use crate::channel::{Channel, ChannelRunner, MessageContext, NewChannel};
use crate::message::{Message, MessageKind, MessageReply, MsgRef};
use crate::spawn_named;
use crate::types::{ChannelId, Token};
use snafu::Snafu;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc::error::SendError;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{error, info};

pub trait ChannelTemplate: NewChannel + std::fmt::Debug {}

impl<T: NewChannel + std::fmt::Debug> ChannelTemplate for T {}

#[derive(Debug)]
pub struct Registry {
    channels: HashMap<ChannelId, UnboundedSender<MessageContext>>,
    templates: HashMap<&'static str, Box<dyn ChannelTemplate + Send>>,
    sender: Option<RegistrySender>,
    receiver: Option<RegistryReceiver>,
    last_join_at: HashMap<ChannelId, Instant>,
}

impl Default for Registry {
    fn default() -> Self {
        let (reg_sender, reg_receiver) = unbounded_channel();

        Registry {
            channels: Default::default(),
            templates: Default::default(),
            sender: Some(reg_sender),
            receiver: Some(reg_receiver),
            last_join_at: Default::default(),
        }
    }
}

#[derive(Debug, Snafu)]
pub enum Error {
    NoChannel,
    ChannelDead,
    NoTemplate,
    Transport,
}

#[derive(Debug)]
pub enum RegistryMessage {
    JoinRequest {
        token: Token,
        channel_id: ChannelId,
        mailbox_tx: UnboundedSender<Message>,
        reply_sender: UnboundedSender<MessageReply>,
        msg_ref: MsgRef,
        payload: serde_json::Value,
    },
    // fixme
    Close,
    Continue,
    Debug(oneshot::Sender<String>),
    Inactivity(ChannelId, oneshot::Sender<RegistryMessage>),
}

pub type RegistrySender = UnboundedSender<RegistryMessage>;
pub type RegistryReceiver = UnboundedReceiver<RegistryMessage>;

// This RegistryInner can probably be used in two ways:
// - global registry, which keeps track of all ws channels and socket mpsc channels
// - within the channel itself, to track subscribers
impl Registry {
    pub fn start(mut self) -> (RegistrySender, JoinHandle<()>) {
        let sender = self.sender.clone().unwrap();
        let mut receiver = self.receiver.take().unwrap();
        let handle = spawn_named(
            async move {
                while let Some(msg) = receiver.recv().await {
                    if let Err(e) = self.handle_message(msg).await {
                        error!("Error in Registry task; e={}", e);
                    }
                }
            },
            "registry",
        );

        (sender, handle)
    }

    async fn handle_message(&mut self, message: RegistryMessage) -> Result<(), Error> {
        match message {
            RegistryMessage::JoinRequest {
                token,
                channel_id,
                mailbox_tx,
                reply_sender,
                msg_ref,
                payload,
            } => self.handle_join_request(
                token,
                channel_id,
                mailbox_tx,
                reply_sender,
                msg_ref,
                payload,
            ),
            RegistryMessage::Close => todo!(),
            RegistryMessage::Continue => todo!(),
            RegistryMessage::Debug(reply_to) => {
                let reply = format!("{:#?}", self);
                let _ = reply_to.send(reply);
                Ok(())
            }
            RegistryMessage::Inactivity(channel_id, reply_to) => {
                let should_close = match self.last_join_at.get(&channel_id) {
                    // in case inactivity was triggered at the same time as a join request, provide some wiggle room
                    Some(instant) => instant.elapsed() > Duration::from_secs(5),
                    None => true,
                };

                if should_close {
                    self.channels.remove(&channel_id);
                    let _ = reply_to.send(RegistryMessage::Close);
                } else {
                    let _ = reply_to.send(RegistryMessage::Continue);
                }

                Ok(())
            }
        }
    }

    // the write half of the socket is connected to the receiver, and the sender here will handle
    // channel subscriptions
    pub fn register_template<C: ChannelTemplate + Send + 'static>(
        &mut self,
        key: &'static str,
        channel: C,
    ) {
        self.templates
            .entry(key)
            .or_insert_with(|| Box::new(channel));
    }

    /// Send a message to a channel. Because the registry is typically behind a mutex,
    /// this should be reserved for sockets that don't already have a copy of the channel sender.
    fn dispatch(&mut self, message: MessageContext) -> Result<(), Error> {
        if let Err(SendError(returned)) = self
            .channels
            .get(message.channel_id())
            .ok_or(Error::NoChannel)?
            .send(message)
        {
            // channel unexpectedly dead (probably something panicked in the channel task)
            error!("channel unexpectedly closed");
            // FIXME: need to disconnect the active sockets
            self.channels.remove(returned.channel_id());

            return Err(Error::ChannelDead);
        }

        Ok(())
    }

    fn handle_join_request(
        &mut self,
        token: Token,
        channel_id: ChannelId,
        mailbox_tx: UnboundedSender<Message>,
        ws_reply_to: UnboundedSender<MessageReply>,
        msg_ref: MsgRef,
        payload: serde_json::Value,
    ) -> Result<(), Error> {
        info!(
            "handle_join_request: token={}, channel_id={:?}",
            token, channel_id
        );

        if self.channels.get(&channel_id).is_none() {
            match channel_id.key().and_then(|key| self.templates.get(key)) {
                Some(_) => {
                    self.add_channel_from_template(channel_id.clone())?;
                }

                None => {
                    error!("registered behavior not found for {:?}", channel_id);
                    // FIXME: send an error response to the socket here
                    return Err(Error::NoTemplate);
                }
            }
        }

        self.set_last_join_at(&channel_id);
        let mut join_msg = Message {
            kind: MessageKind::JoinRequest,
            channel_id,
            msg_ref: Some(msg_ref.clone()),
            join_ref: None,
            payload,
            event: "phx_join".into(),
            channel_sender: None,
        }
        .decorate(token, mailbox_tx);

        join_msg.ws_reply_to = Some(ws_reply_to);
        join_msg.msg_ref = Some(msg_ref);

        println!("dispatching {:?}", join_msg);
        self.dispatch(join_msg)
    }

    fn set_last_join_at(&mut self, channel_id: &ChannelId) {
        self.last_join_at.insert(channel_id.clone(), Instant::now());
    }

    fn add_channel_from_template(&mut self, channel_id: ChannelId) -> Result<(), Error> {
        let template = self.templates.get(channel_id.key().unwrap()).unwrap();
        let channel = template.new_channel(channel_id.clone());
        self.add_channel_inner(channel_id, channel, true)
    }

    pub fn add_channel(
        &mut self,
        channel_id: ChannelId,
        channel: Box<dyn Channel>,
    ) -> Result<(), Error> {
        self.add_channel_inner(channel_id, channel, false)
    }

    // FIXME: change inactivity_timeout to a Option<Duration>
    fn add_channel_inner(
        &mut self,
        channel_id: ChannelId,
        channel: Box<dyn Channel>,
        inactivity_timeout: bool,
    ) -> Result<(), Error> {
        let (_, channel_sender) = ChannelRunner::spawn(
            channel_id.clone(),
            channel,
            self.sender.as_ref().unwrap().clone(),
            inactivity_timeout,
        );
        self.channels.entry(channel_id).or_insert(channel_sender);

        Ok(())
    }
}
