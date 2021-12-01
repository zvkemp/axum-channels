use crate::{
    channel::{Channel, ChannelBehavior},
    message::{DecoratedMessage, Message, MessageReply},
    types::{ChannelId, Token},
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender},
    oneshot,
};

#[derive(Debug)]
pub struct Registry {
    channels: HashMap<ChannelId, UnboundedSender<DecoratedMessage>>,
    sockets: HashMap<Token, UnboundedSender<MessageReply>>,
}

impl Default for Registry {
    fn default() -> Self {
        let mut channels = HashMap::new();
        let (_, default_channel) = Channel::spawn(Box::new(DefaultChannel));
        channels
            .entry("default".to_string())
            .or_insert(default_channel);

        let sockets = HashMap::new();

        Registry { channels, sockets }
    }
}

// FIXME: move
struct DefaultChannel;

impl ChannelBehavior for DefaultChannel {}

// This Registry can probably be used in two ways:
// - global registry, which keeps track of all ws channels and socket mpsc channels
// - within the channel itself, to track subscribers
impl Registry {
    // the write half of the socket is connected to the receiver, and the sender here will handle
    // channel subscriptions
    pub fn register_writer(&mut self, token: usize, sender: UnboundedSender<MessageReply>) {
        self.sockets.entry(token).or_insert(sender); // this will be a way to communicate with sockets
    }

    pub fn deregister_writer(&mut self, token: usize) {
        self.sockets.remove(&token);
    }

    pub fn subscribe(
        &self,
        channel_id: ChannelId,
        token: Token,
    ) -> Option<oneshot::Receiver<MessageReply>> {
        println!("subscribing {} to {}...", token, channel_id);

        if let Some(channel) = self.channels.get(&channel_id) {
            let (tx, rx) = oneshot::channel();
            let mut msg = Message::Join {
                channel_id: channel_id.clone(),
            }
            .decorate(token, channel_id);
            msg.reply_to = Some(tx);

            channel.send(msg);
            return Some(rx);
        }

        None
    }
}
