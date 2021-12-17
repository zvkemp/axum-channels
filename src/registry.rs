use crate::channel::{Channel, ChannelRunner, NewChannel};
use crate::message::{DecoratedMessage, Message, MessageKind, MessageReply};
use crate::types::{ChannelId, Token};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::info;

pub trait ChannelTemplate: NewChannel + std::fmt::Debug {}

impl<T: NewChannel + std::fmt::Debug> ChannelTemplate for T {}

#[derive(Debug)]
pub struct Registry {
    channels: HashMap<ChannelId, UnboundedSender<DecoratedMessage>>,
    templates: HashMap<String, Box<dyn ChannelTemplate + Send>>,
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

#[derive(Debug)]
pub enum Error {
    NoChannel,
    Transport,
}

pub enum RegistryMessage {
    Dispatch(DecoratedMessage),
    JoinRequest {
        token: Token,
        channel_id: ChannelId,
        mailbox_tx: UnboundedSender<Message>,
        reply_sender: UnboundedSender<MessageReply>,
        msg_ref: String,
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
        let handle = tokio::spawn(async move {
            while let Some(msg) = receiver.recv().await {
                self.handle_message(msg).await;
            }
        });

        (sender, handle)
    }

    async fn handle_message(&mut self, message: RegistryMessage) {
        match message {
            RegistryMessage::Dispatch(inner) => {
                self.dispatch(inner);
            }
            RegistryMessage::JoinRequest {
                token,
                channel_id,
                mailbox_tx,
                reply_sender,
                msg_ref,
                payload,
            } => {
                self.handle_join_request(
                    token,
                    channel_id,
                    mailbox_tx,
                    reply_sender,
                    msg_ref,
                    payload,
                );
            }
            RegistryMessage::Close => todo!(),
            RegistryMessage::Continue => todo!(),
            RegistryMessage::Debug(reply_to) => {
                let reply = format!("{:#?}", self);
                reply_to.send(reply);
            }
            RegistryMessage::Inactivity(channel_id, reply_to) => {
                let should_close = match self.last_join_at.get(&channel_id) {
                    // in case inactivity was triggered at the same time as a join request, provide some wiggle room
                    Some(instant) => instant.elapsed() > Duration::from_secs(5),
                    None => true,
                };

                if should_close {
                    self.channels.remove(&channel_id);
                    reply_to.send(RegistryMessage::Close);
                } else {
                    reply_to.send(RegistryMessage::Continue);
                }
            }
        }
    }

    // the write half of the socket is connected to the receiver, and the sender here will handle
    // channel subscriptions
    pub fn register_template<C: ChannelTemplate + Send + 'static>(
        &mut self,
        key: String,
        channel: C,
    ) {
        self.templates
            .entry(key)
            .or_insert_with(|| Box::new(channel));
    }

    /// Send a message to a channel. Because the reigstry is typically behind a mutex,
    /// this should be reserved for sockets that don't already have a copy of the channel sender.
    fn dispatch(&self, message: DecoratedMessage) -> Result<(), Error> {
        self.channels
            .get(message.channel_id())
            .ok_or(Error::NoChannel)?
            .send(message)
            .map_err(|_| Error::Transport)
    }

    fn handle_join_request(
        &mut self,
        token: Token,
        channel_id: ChannelId,
        mailbox_tx: UnboundedSender<Message>,
        ws_reply_to: UnboundedSender<MessageReply>,
        msg_ref: String,
        payload: serde_json::Value,
    ) {
        info!(
            "handle_join_request: token={}, channel_id={:?}",
            token, channel_id
        );

        if self.channels.get(&channel_id).is_none() {
            // let template = self.templates.get(channel_id.key().unwrap());
            match self.templates.get(channel_id.key().unwrap()) {
                // FIXME: no unwrap
                Some(_) => {
                    self.add_channel_from_template(channel_id.clone());
                }

                None => {
                    eprintln!("registered behavior not found for {:?}", channel_id);
                    return;
                }
            }
        }

        self.set_last_join_at(&channel_id);
        let mut join_msg = Message {
            kind: MessageKind::Join,
            channel_id,
            msg_ref: Some(msg_ref.clone()),
            join_ref: None,
            payload,
            event: "phx_join".to_string(),
            channel_sender: None,
        }
        .decorate(token, mailbox_tx);

        join_msg.ws_reply_to = Some(ws_reply_to);
        join_msg.msg_ref = Some(msg_ref);

        println!("dispatching {:?}", join_msg);
        self.dispatch(join_msg).unwrap();
    }

    fn set_last_join_at(&mut self, channel_id: &ChannelId) {
        self.last_join_at.insert(channel_id.clone(), Instant::now());
    }

    fn add_channel_from_template(&mut self, channel_id: ChannelId) {
        let template = self.templates.get(channel_id.key().unwrap()).unwrap();
        let channel = template.new_channel(channel_id.clone());
        self.add_channel(channel_id, channel);
    }

    pub fn add_channel(&mut self, channel_id: ChannelId, channel: Box<dyn Channel>) {
        let (_, channel_sender) = ChannelRunner::spawn(
            channel_id.clone(),
            channel,
            self.sender.as_ref().unwrap().clone(),
        );
        self.channels.entry(channel_id).or_insert(channel_sender);
    }
}
