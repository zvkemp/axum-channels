use std::sync::Arc;

use axum::extract::ws;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc::UnboundedSender, oneshot};

use crate::types::{ChannelId, Token};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Message {
    Channel { channel_id: String, text: String },
    Join { channel_id: String },
    Leave { channel_id: String },
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
    pub channel_id: ChannelId,
    pub inner: Message,
    pub reply_to: Option<oneshot::Sender<MessageReply>>,
}

impl DecoratedMessage {
    pub fn is_join(&self) -> bool {
        matches!(self.inner, Message::Join { .. })
    }

    pub fn is_leave(&self) -> bool {
        matches!(self.inner, Message::Leave { .. })
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

impl From<MessageReply> for ws::Message {
    fn from(msg: MessageReply) -> Self {
        match msg {
            MessageReply::Reply(text) => ws::Message::Text(text),
            MessageReply::Broadcast(text) => ws::Message::Text(text),
            MessageReply::Join(_) => todo!(),
            MessageReply::Pong(data) => ws::Message::Pong(data),
        }
    }
}
