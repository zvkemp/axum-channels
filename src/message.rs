use crate::types::{ChannelId, Token};
use axum::extract::ws;
use serde_json::json;
use tokio::sync::mpsc::UnboundedSender;

pub enum MessageKind {
    Channel,
    Join,
    JoinRequest,
    DidJoin,
    Leave,
    Event(String),
    Broadcast,
    Heartbeat,
    BroadcastIntercept,
    Push,
}

// FIXME: this is getting silly; it's probably type to split of MessageKind, and standardize the other fields
#[derive(Debug)]
pub enum Message {
    Channel {
        channel_id: ChannelId,
        text: String,
    },
    Join {
        channel_id: ChannelId,
    },
    JoinRequest {
        channel_id: ChannelId, // topic name
        msg_ref: String,
    },
    DidJoin {
        channel_id: ChannelId,
        channel_sender: UnboundedSender<DecoratedMessage>,
        broadcast_handle: tokio::task::JoinHandle<()>,
        msg_ref: String,
    },
    Leave {
        channel_id: ChannelId,
    },
    Event {
        channel_id: ChannelId,
        event: String,
        payload: serde_json::Value,
        msg_ref: String,
    },
    // FIXME: duplicate of MessageReply::Reply
    Reply(String),
    Broadcast {
        channel_id: ChannelId,
        event: String,
        payload: serde_json::Value,
    },
    Heartbeat {
        msg_ref: String,
    },
    BroadcastIntercept {
        channel_id: ChannelId,
        event: String,
        payload: serde_json::Value,
    },

    Push {
        channel_id: ChannelId,
        event: String,
        payload: serde_json::Value,
    }, // Push FIXME: add this variant; message that is sent to one websocket, but not as a specific reply
}

impl Message {
    pub fn decorate(
        self,
        token: Token,
        // channel_id: ChannelId,
        reply_to: UnboundedSender<Message>,
    ) -> DecoratedMessage {
        DecoratedMessage {
            token,
            // channel_id,
            inner: self,
            reply_to: Some(reply_to),
            broadcast_reply_to: None,
            msg_ref: None,
        }
    }
}

#[derive(Debug)]
pub struct DecoratedMessage {
    pub token: Token,
    pub inner: Message,
    pub reply_to: Option<UnboundedSender<Message>>,
    pub broadcast_reply_to: Option<UnboundedSender<MessageReply>>,
    pub msg_ref: Option<String>,
}

lazy_static::lazy_static! {
    pub static ref PHX_CHANNEL: ChannelId = "phoenix".parse().unwrap();
}

impl DecoratedMessage {
    pub fn is_join(&self) -> bool {
        matches!(self.inner, Message::Join { .. })
    }

    pub fn is_leave(&self) -> bool {
        matches!(self.inner, Message::Leave { .. })
    }

    pub fn is_intercept(&self) -> bool {
        matches!(self.inner, Message::BroadcastIntercept { .. })
    }

    pub fn channel_id(&self) -> &ChannelId {
        match &self.inner {
            Message::Channel { channel_id, .. } => channel_id,
            Message::Join { channel_id } => channel_id,
            Message::DidJoin { channel_id, .. } => channel_id,
            Message::Leave { channel_id } => channel_id,
            Message::JoinRequest { channel_id, .. } => channel_id,
            // FIXME: probably ok to add channel_id to these two
            Message::Reply(_) => todo!(),
            Message::Broadcast { channel_id, .. } => channel_id,
            Message::Event { channel_id, .. } => channel_id,
            Message::Heartbeat { .. } => &PHX_CHANNEL,
            Message::BroadcastIntercept { channel_id, .. } => channel_id,
            Message::Push { channel_id, .. } => channel_id,
        }
    }
}

#[derive(Debug, Clone)]
pub enum MessageReply {
    Reply(String),
    Broadcast {
        event: String,
        payload: serde_json::Value,
        channel_id: ChannelId,
    },
    Pong(Vec<u8>),
    Event {
        event: String,
        payload: serde_json::Value,
        channel_id: ChannelId,
    },
    Heartbeat {
        msg_ref: String,
    },
    Join {
        msg_ref: String,
        channel_id: ChannelId,
    },
    BroadcastIntercept {
        channel_id: ChannelId,
        event: String,
        payload: serde_json::Value,
    },
    Push {
        channel_id: ChannelId,
        event: String,
        payload: serde_json::Value,
    },
}

impl MessageReply {
    pub fn is_reply(&self) -> bool {
        matches!(self, MessageReply::Reply(..))
    }

    pub fn is_broadcast(&self) -> bool {
        matches!(self, MessageReply::Broadcast { .. })
    }
}

impl From<MessageReply> for ws::Message {
    fn from(msg: MessageReply) -> Self {
        match msg {
            MessageReply::Reply(text) => ws::Message::Text(text),
            MessageReply::Broadcast {
                event,
                payload,
                channel_id,
            } => {
                let json_value = json!([null, null, channel_id.id(), event, payload]);
                ws::Message::Text(serde_json::to_string(&json_value).unwrap())
            }
            MessageReply::Pong(data) => ws::Message::Pong(data),
            MessageReply::Heartbeat { msg_ref } => {
                // FIXME: msg_ref is escape-quoted; needs to be just "8" instead of "\"8\""
                // FIXME: we also need to send a phx_reply for phx_join events
                let json_value = json!([null, msg_ref, PHX_CHANNEL.id(), "phx_reply", {"response": {}, "status": "ok"}]);
                ws::Message::Text(serde_json::to_string(&json_value).unwrap())
            }
            MessageReply::Join {
                msg_ref,
                channel_id,
            } => {
                let json_value = json!([null, msg_ref, channel_id.id(), "phx_reply", {"response": {}, "status": "ok"}]);
                ws::Message::Text(serde_json::to_string(&json_value).unwrap())
            }
            MessageReply::Event {
                event,
                payload,
                channel_id,
            }
            | MessageReply::Push {
                event,
                payload,
                channel_id,
            } => {
                let json_value = json!([null, null, channel_id.id(), event, payload]);
                ws::Message::Text(serde_json::to_string(&json_value).unwrap())
            }
            MessageReply::BroadcastIntercept { .. } => {
                todo!()
                // FIXME: this should never happen
            }
        }
    }
}
