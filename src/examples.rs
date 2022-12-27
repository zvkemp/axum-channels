use crate::{
    channel::Channel,
    message::{MessageContext, Message, MessageKind},
};

#[derive(Clone, Debug)]
pub struct PresenceChannel;

#[axum::async_trait]
impl Channel for PresenceChannel {
    async fn handle_message(&mut self, _message: &MessageContext) -> Option<Message> {
        None
    }

    async fn handle_out(&mut self, _message: &MessageContext) -> Option<Message> {
        None
    }

    async fn handle_join(
        &mut self,
        _message: &MessageContext,
    ) -> crate::channel::Result<Option<Message>> {
        Ok(None)
    }

    async fn handle_info(&mut self, message: &Message) -> crate::channel::Result<Option<Message>> {
        match message.kind {
            crate::message::MessageKind::PresenceChange => {
                let message = Message {
                    kind: MessageKind::BroadcastPresence,
                    channel_id: message.channel_id.clone(),
                    msg_ref: None,
                    join_ref: None,
                    payload: serde_json::json!(null),
                    event: message.event.clone(),
                    channel_sender: None,
                };

                return Ok(Some(message));
            }
            _ => {}
        }

        Ok(None)
    }

    async fn handle_leave(
        &mut self,
        _message: &MessageContext,
    ) -> crate::channel::Result<Option<Message>> {
        Ok(None)
    }
}
