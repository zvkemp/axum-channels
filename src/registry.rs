use crate::{
    channel::{Channel, ChannelBehavior},
    message::{DecoratedMessage, Message, MessageKind, MessageReply},
    types::{ChannelId, Token},
};
use std::collections::HashMap;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info};

#[derive(Debug, Default)]
pub struct Registry {
    channels: HashMap<ChannelId, UnboundedSender<DecoratedMessage>>,
    behaviors: HashMap<String, Box<dyn ChannelBehavior>>,
}

#[derive(Debug)]
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
    pub fn register_behavior(&mut self, key: String, behavior: Box<dyn ChannelBehavior>) {
        self.behaviors.entry(key).or_insert(behavior);
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
        ws_reply_to: UnboundedSender<MessageReply>,
        msg_ref: String,
        payload: serde_json::Value,
    ) {
        info!(
            "handle_join_request: token={}, channel_id={:?}",
            token, channel_id
        );

        if self.channels.get(&channel_id).is_none() {
            match self.behaviors.get(channel_id.key().unwrap()) {
                // FIXME: no unwrap
                Some(behavior) => {
                    self.add_channel(channel_id.clone(), behavior.clone());
                }

                None => {
                    eprintln!("registered behavior not found for {:?}", channel_id);
                    return;
                }
            }
        }

        let mut join_msg = Message {
            kind: MessageKind::Join,
            channel_id: channel_id.clone(),
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

    // FIXME: handle errors
    pub fn add_channel(&mut self, channel_id: ChannelId, behavior: Box<dyn ChannelBehavior>) {
        let (_, channel) = Channel::spawn(behavior);
        self.channels.entry(channel_id).or_insert(channel);
    }
}
