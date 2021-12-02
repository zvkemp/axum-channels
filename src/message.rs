use std::sync::Arc;

use axum::extract::ws;
// use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc::UnboundedSender};

use crate::types::{ChannelId, Token};

// FIXME: should be something more like:
// pub enum Event {
//   JoinRequest
//   JoinResult
//   LeaveRequest
//   LeaveResult
//   Message
//   PutData/Assign/Remove/etc
// }
#[derive(Debug)]
pub enum Message {
    Channel {
        channel_id: String,
        text: String,
    },
    Join {
        channel_id: String,
    },
    // FIXME: also include broadcaster?
    DidJoin {
        channel_id: String,
        channel_sender: UnboundedSender<DecoratedMessage>,
        // FIXME: is this arc an appropriate way to send this through the channel?
        broadcast_handle: tokio::task::JoinHandle<()>,
    },
    Leave {
        channel_id: String,
    },

    // FIXME: duplicate of MessageReply::Reply
    Reply(String),
}

impl Message {
    pub fn decorate(self, token: Token, channel_id: ChannelId) -> DecoratedMessage {
        DecoratedMessage {
            token,
            channel_id,
            inner: self,
            reply_to: None,
            broadcast_reply_to: None,
        }
    }
}

#[derive(Debug)]
pub struct DecoratedMessage {
    pub token: Token,
    pub channel_id: ChannelId,
    pub inner: Message,
    pub reply_to: Option<UnboundedSender<Message>>,
    pub broadcast_reply_to: Option<UnboundedSender<MessageReply>>,
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
            MessageReply::Pong(data) => ws::Message::Pong(data),
        }
    }
}
