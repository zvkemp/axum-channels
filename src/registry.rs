use crate::{
    channel::{Channel, ChannelBehavior},
    message::{DecoratedMessage, Message, MessageReply},
    types::{ChannelId, Token},
};
use std::collections::HashMap;
use tokio::sync::{mpsc::UnboundedSender, oneshot};

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

impl ChannelBehavior for DefaultChannel {
    fn handle_message(&mut self, message: &DecoratedMessage) -> Option<MessageReply> {
        match &message.inner {
            Message::Channel { text, .. } => Some(MessageReply::Broadcast(format!(
                "[{}] {}",
                message.token, text
            ))),
            _ => None,
        }
    }
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

            if let Err(e) = channel.send(msg) {
                eprintln!("unexpected error in subscription confirmation; {:?}", e);
            };
            return Some(rx);
        }

        None
    }

    // FIXME: handle errors
    pub fn add_channel(
        &mut self,
        channel_id: ChannelId,
        behavior: Box<dyn ChannelBehavior + Sync + Send>,
    ) {
        let (_, channel) = Channel::spawn(behavior);
        self.channels.entry(channel_id).or_insert(channel);
    }
}
