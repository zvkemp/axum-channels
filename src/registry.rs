use crate::channel::{Channel, ChannelRunner, NewChannel};
use crate::message::{DecoratedMessage, Message, MessageKind, MessageReply};
use crate::types::{ChannelId, Token};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;
use tracing::info;

#[derive(Clone, Default, Debug)]
pub struct Registry {
    inner: Arc<Mutex<RegistryInner>>,
}

pub trait ChannelTemplate: NewChannel + std::fmt::Debug {}

impl<T: NewChannel + std::fmt::Debug> ChannelTemplate for T {}

#[derive(Default, Debug)]
pub struct RegistryInner {
    channels: HashMap<ChannelId, UnboundedSender<DecoratedMessage>>,
    templates: HashMap<String, Box<dyn ChannelTemplate + Send>>,
}

#[derive(Debug)]
pub enum Error {
    NoChannel,
    Transport,
}

impl Registry {
    pub fn register_template<C: ChannelTemplate + Send + 'static>(
        &mut self,
        key: String,
        channel: C,
    ) {
        let mut inner = self.inner.lock().unwrap();
        inner.register_template(key, channel)
    }

    pub fn dispatch(&self, message: DecoratedMessage) -> Result<(), Error> {
        let inner = self.inner.lock().unwrap();
        inner.dispatch(message)
    }

    pub fn handle_join_request(
        &mut self,
        token: Token,
        channel_id: ChannelId,
        mailbox_tx: UnboundedSender<Message>,
        ws_reply_to: UnboundedSender<MessageReply>,
        msg_ref: String,
        payload: serde_json::Value,
    ) {
        let mut inner = self.inner.lock().unwrap();
        inner.handle_join_request(token, channel_id, mailbox_tx, ws_reply_to, msg_ref, payload)
    }

    pub fn add_channel(&mut self, channel_id: ChannelId, channel: Box<dyn Channel>) {
        let mut inner = self.inner.lock().unwrap();
        inner.add_channel(channel_id, channel)
    }
}

// This RegistryInner can probably be used in two ways:
// - global registry, which keeps track of all ws channels and socket mpsc channels
// - within the channel itself, to track subscribers
impl RegistryInner {
    // the write half of the socket is connected to the receiver, and the sender here will handle
    // channel subscriptions
    fn register_template<C: ChannelTemplate + Send + 'static>(&mut self, key: String, channel: C) {
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

    fn add_channel_from_template(&mut self, channel_id: ChannelId) {
        let template = self.templates.get(channel_id.key().unwrap()).unwrap();
        let channel = template.new_channel(channel_id.clone());
        self.add_channel(channel_id, channel);
    }

    fn add_channel(&mut self, channel_id: ChannelId, channel: Box<dyn Channel>) {
        let (_, channel_sender) = ChannelRunner::spawn(channel);
        self.channels.entry(channel_id).or_insert(channel_sender);
    }
}
