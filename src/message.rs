use std::sync::Arc;

use crate::types::{ChannelId, Token};
use axum::extract::ws;
use serde_json::json;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug)]
pub enum MessageKind {
    Join,
    JoinRequest,
    DidJoin,
    Leave,
    Event,
    Broadcast,
    Heartbeat,
    BroadcastIntercept,
    Reply, // request-response
    Push,  // response only
    PresenceChange,
    BroadcastPresence,
    Closed,
}

// Immutable string references - these values are frequently cloned but never mutated.
pub type MsgRef = Arc<str>;
pub type JoinRef = Arc<str>;
pub type Event = Arc<str>;

#[derive(Debug)]
pub struct Message {
    pub kind: MessageKind,
    pub channel_id: ChannelId,
    pub msg_ref: Option<MsgRef>,
    pub join_ref: Option<JoinRef>, // this should probably be conceptually merged with Token
    pub payload: serde_json::Value,
    pub event: Event,
    pub channel_sender: Option<UnboundedSender<DecoratedMessage>>, // FIXME only used for DidJoin
}

impl Message {
    pub fn decorate(self, token: Token, reply_to: UnboundedSender<Message>) -> DecoratedMessage {
        DecoratedMessage {
            token,
            inner: self,
            reply_to: Some(reply_to),
            ws_reply_to: None,
            msg_ref: None,
        }
    }
}

#[derive(Debug)]
pub struct DecoratedMessage {
    pub token: Token,
    pub inner: Message,
    pub reply_to: Option<UnboundedSender<Message>>,
    pub ws_reply_to: Option<UnboundedSender<MessageReply>>,
    pub msg_ref: Option<MsgRef>,
}

lazy_static::lazy_static! {
    pub static ref PHX_CHANNEL: ChannelId = "phoenix".parse().unwrap();
}

impl DecoratedMessage {
    pub fn is_join(&self) -> bool {
        matches!(self.inner.kind, MessageKind::Join)
    }

    pub fn is_leave(&self) -> bool {
        matches!(self.inner.kind, MessageKind::Leave)
    }

    pub fn is_intercept(&self) -> bool {
        matches!(self.inner.kind, MessageKind::BroadcastIntercept)
    }

    pub fn channel_id(&self) -> &ChannelId {
        &self.inner.channel_id
    }
}

pub fn broadcast_intercept(
    channel_id: ChannelId,
    event: Event,
    payload: serde_json::Value,
) -> Message {
    Message {
        join_ref: None,
        msg_ref: None,
        kind: MessageKind::BroadcastIntercept,
        channel_id,
        event,
        payload,
        channel_sender: None,
    }
}

pub fn push(
    channel_id: ChannelId,
    msg_ref: Option<MsgRef>,
    event: Event,
    payload: serde_json::Value,
) -> Message {
    Message {
        kind: MessageKind::Push,
        channel_id,
        msg_ref,
        join_ref: None,
        payload,
        event,
        channel_sender: None,
    }
}

pub fn broadcast(channel_id: ChannelId, event: Event, payload: serde_json::Value) -> Message {
    Message {
        kind: MessageKind::Broadcast,
        channel_id,
        msg_ref: None,
        join_ref: None,
        payload,
        event,
        channel_sender: None,
    }
}

pub(crate) fn did_join(
    msg_ref: Option<MsgRef>,
    channel_id: ChannelId,
    channel_sender: UnboundedSender<DecoratedMessage>,
) -> Message {
    Message {
        join_ref: None, // FIXME: return the token ID here?
        kind: MessageKind::DidJoin,
        msg_ref,
        event: "phx_join".into(),
        channel_id,
        channel_sender: Some(channel_sender),
        payload: json!(null),
    }
}

#[derive(Debug, Clone)]
pub enum MessageReply {
    Reply(String),
    Broadcast {
        event: Event,
        payload: serde_json::Value,
        channel_id: ChannelId,
    },
    Pong(Vec<u8>),
    Event {
        event: Event,
        payload: serde_json::Value,
        channel_id: ChannelId,
    },
    Heartbeat {
        msg_ref: MsgRef,
    },
    Join {
        msg_ref: MsgRef,
        channel_id: ChannelId,
    },
    BroadcastIntercept {
        channel_id: ChannelId,
        event: Event,
        payload: serde_json::Value,
    },
    Push {
        channel_id: ChannelId,
        event: Event,
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
                let json_value = json!([null, null, channel_id.id(), &*event, payload]);
                ws::Message::Text(serde_json::to_string(&json_value).unwrap())
            }
            MessageReply::Pong(data) => ws::Message::Pong(data),
            MessageReply::Heartbeat { msg_ref } => {
                // FIXME: msg_ref is escape-quoted; needs to be just "8" instead of "\"8\""
                // FIXME: we also need to send a phx_reply for phx_join events
                let json_value = json!([null, &*msg_ref, PHX_CHANNEL.id(), "phx_reply", {"response": {}, "status": "ok"}]);
                ws::Message::Text(serde_json::to_string(&json_value).unwrap())
            }
            MessageReply::Join {
                msg_ref,
                channel_id,
            } => {
                let json_value = json!([null, &*msg_ref, channel_id.id(), "phx_reply", {"response": {}, "status": "ok"}]);
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
                let json_value = json!([null, null, channel_id.id(), &*event, payload]);
                ws::Message::Text(serde_json::to_string(&json_value).unwrap())
            }
            MessageReply::BroadcastIntercept { .. } => {
                todo!()
                // FIXME: this should never happen
            }
        }
    }
}
