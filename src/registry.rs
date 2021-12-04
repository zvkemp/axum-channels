use crate::{
    channel::{Channel, ChannelBehavior},
    message::{DecoratedMessage, Message, MessageReply},
    types::{ChannelId, Token},
};
use std::collections::HashMap;
use tokio::sync::mpsc::UnboundedSender;
use tracing::error;

#[derive(Debug)]
pub struct Registry {
    channels: HashMap<ChannelId, UnboundedSender<DecoratedMessage>>,
    sockets: HashMap<Token, UnboundedSender<MessageReply>>,
    behaviors: HashMap<String, Box<dyn ChannelBehavior>>,
}

impl Default for Registry {
    fn default() -> Self {
        let mut channels = HashMap::new();
        let (_, default_channel) = Channel::spawn(Box::new(DefaultChannel));
        channels
            .entry("default:*".parse().unwrap())
            .or_insert(default_channel);

        let behaviors = HashMap::new();
        let sockets = HashMap::new();

        Registry {
            channels,
            sockets,
            behaviors,
        }
    }
}

// FIXME: move
#[derive(Debug, Clone)]
struct DefaultChannel;

impl ChannelBehavior for DefaultChannel {
    fn handle_message(&mut self, message: &DecoratedMessage) -> Option<Message> {
        match &message.inner {
            Message::Channel { text, .. } => {
                Some(Message::Broadcast(format!("[{}] {}", message.token, text)))
            }
            _ => None,
        }
    }
}

pub enum Error {
    NoChannel,
    Transport, // FIXME: use snafu?
}

// This Registry can probably be used in two ways:
// - global registry, which keeps track of all ws channels and socket mpsc channels
// - within the channel itself, to track subscribers
impl Registry {
    // the write half of the socket is connected to the receiver, and the sender here will handle
    // channel subscriptions
    pub fn register_writer(&mut self, token: Token, sender: UnboundedSender<MessageReply>) {
        self.sockets.entry(token).or_insert(sender); // this will be a way to communicate with sockets
    }

    pub fn deregister_writer(&mut self, token: Token) {
        self.sockets.remove(&token);
    }

    /// Send a message to a channel. Because the reigstry is typically behind a mutex,
    /// this should be reserved for sockets that don't already have a copy of the channel sender.
    pub fn dispatch(&self, message: DecoratedMessage) -> Result<(), Error> {
        self.channels
            .get(message.channel_id())
            .ok_or(Error::NoChannel)?
            .send(message)
            .map_err(|_| Error::Transport)
    }

    pub fn handle_join_request(
        &mut self,
        token: Token,
        channel_id: ChannelId,
        mailbox_tx: UnboundedSender<Message>,
    ) {
        if self.channels.get(&channel_id).is_none() {
            match self.behaviors.get(channel_id.key().unwrap()) {
                // FIXME: no unwrap
                Some(behavior) => {
                    self.add_channel(channel_id.clone(), behavior.clone());

                    let join_msg = Message::Join {
                        channel_id: channel_id.clone(),
                    }
                    .decorate(token, mailbox_tx);
                }

                None => {
                    error!("registered behavior not found for {:?}", channel_id);
                }
            }
        }
    }

    // FIXME: handle errors
    pub fn add_channel(&mut self, channel_id: ChannelId, behavior: Box<dyn ChannelBehavior>) {
        let (_, channel) = Channel::spawn(behavior);
        self.channels.entry(channel_id).or_insert(channel);
    }
}
