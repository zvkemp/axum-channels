use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc::UnboundedSender, oneshot};

use crate::types::{ChannelId, Token};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Message {
    Channel {
        channel_id: String,
        // token: Token,
        text: String,
    },

    Join {
        channel_id: String,
        // token: Token,
    },
}

impl Message {
    pub fn decorate(self, token: Token, channel_id: ChannelId) -> DecoratedMessage {
        DecoratedMessage {
            token,
            channel_id,
            inner: self,
            reply_to: None,
        }
    }
}

#[derive(Debug)]
pub struct DecoratedMessage {
    pub token: Token,
    channel_id: ChannelId,
    inner: Message,
    pub reply_to: Option<oneshot::Sender<MessageReply>>,
}

impl DecoratedMessage {
    pub fn is_join(&self) -> bool {
        matches!(self.inner, Message::Join { .. })
    }
}

#[derive(Debug, Clone)]
pub enum MessageReply {
    Reply(String),
    Broadcast(String),
    Join(
        Arc<(
            UnboundedSender<DecoratedMessage>,
            broadcast::Receiver<MessageReply>,
        )>,
    ),

    Pong(Vec<u8>),
}

impl MessageReply {
    pub fn is_reply(&self) -> bool {
        matches!(self, MessageReply::Reply(..))
    }

    pub fn is_broadcast(&self) -> bool {
        matches!(self, MessageReply::Broadcast(..))
    }
}
