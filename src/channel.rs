use std::collections::HashMap;

// High-level FIXME:
// a pattern like the Axum extractor macros may be worthwhile exploring here

// FIXME:
// this design probably has an upper limit to channel members,
// as the mutable state means there's no real channel concurrency.
// This is probably mitigated somewhat by broadcasts, but `BroadcastIntercept`s may end up being expensive to process.
use crate::message::{DecoratedMessage, Message};
use crate::message::{MessageKind, MessageReply};
use crate::types::Token;
use serde_json::json;
use tokio::sync::broadcast;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tracing::{debug, error};

pub struct Channel {
    incoming_sender: UnboundedSender<DecoratedMessage>,
    incoming_receiver: UnboundedReceiver<DecoratedMessage>,
    broadcast_sender: broadcast::Sender<MessageReply>,
    behavior: Box<dyn ChannelBehavior>,
    presence: Presence,
    // socket_state: SocketState,
}

#[derive(Default, Debug)]
pub struct Presence {
    pub data: HashMap<Token, serde_json::Value>,
}

impl Presence {
    pub fn track(&mut self, token: Token, join_data: &serde_json::Value) {
        self.data
            .entry(token)
            .and_modify(|e| *e = join_data.clone())
            .or_insert_with(|| join_data.clone());
    }

    pub fn leave(&mut self, token: Token) {
        self.data.remove(&token);
    }
}

#[derive(Debug)]
pub enum Error {
    Join { reason: String },
    Send(String),
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub trait CloneChannelBehavior {
    fn clone_box(&self) -> Box<dyn ChannelBehavior>;
}

pub trait ChannelBehavior: std::fmt::Debug + Send + Sync + CloneChannelBehavior {
    fn handle_message(&mut self, _message: &DecoratedMessage) -> Option<Message> {
        None
    }

    // receives a BroadcastIntercept for socket-specific processing
    fn handle_out(&mut self, _message: &DecoratedMessage) -> Option<Message> {
        None
    }

    // authorize new socket connections
    fn handle_join(&mut self, _message: &DecoratedMessage) -> Result<Option<Message>> {
        Ok(None)
    }

    fn handle_presence(
        &mut self,
        channel_id: &crate::types::ChannelId,
        _presence: &Presence,
    ) -> Result<Option<Message>> {
        Ok(None)
    }

    fn handle_info(&mut self, _message: &Message) -> Result<Option<Message>> {
        Ok(None)
    }

    fn handle_leave(&mut self, _message: &DecoratedMessage) -> Result<Option<Message>> {
        Ok(None)
    }
}

impl<T> CloneChannelBehavior for T
where
    T: 'static + ChannelBehavior + Clone,
{
    fn clone_box(&self) -> Box<dyn ChannelBehavior> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn ChannelBehavior> {
    fn clone(&self) -> Box<dyn ChannelBehavior> {
        self.clone_box()
    }
}

// FIXME: add channel id
impl Channel {
    pub fn new(behavior: Box<dyn ChannelBehavior>) -> Self {
        let (incoming_sender, incoming_receiver) = unbounded_channel();
        let (broadcast_sender, _broadcast_receiver) = broadcast::channel(1024);

        Self {
            incoming_sender,
            incoming_receiver,
            broadcast_sender,
            behavior,
            presence: Default::default(),
        }
    }

    // FIXME: need a way to receive a shutdown message
    pub fn spawn(
        behavior: Box<dyn ChannelBehavior>,
    ) -> (JoinHandle<()>, UnboundedSender<DecoratedMessage>) {
        let channel = Self::new(behavior);
        let sender = channel.incoming_sender.clone();

        let handle = channel.start();

        (handle, sender)
    }

    pub fn start(mut self) -> JoinHandle<()> {
        debug!("starting this channel");

        tokio::spawn(async move {
            while let Some(message) = self.incoming_receiver.recv().await {
                // FIXME: also pass Join to callback to allow behavior to do things
                if message.is_leave() {
                    self.handle_leave(message);
                } else {
                    let response = if message.is_intercept() {
                        self.behavior.handle_out(&message)
                    } else if message.is_join() {
                        match self.behavior.handle_join(&message) {
                            Ok(join_response) => {
                                debug!("join_response={:#?}", join_response);
                                self.handle_join(&message);
                                join_response
                            }
                            Err(e) => {
                                error!("{:?}", e);
                                None
                            }
                        }
                    } else {
                        self.behavior.handle_message(&message)
                    };

                    match response {
                        None => {
                            debug!("got handle_message => None");
                        }

                        Some(msg) => match msg.kind {
                            MessageKind::Reply | MessageKind::Push => {
                                if let Some(reply_to) = message.reply_to {
                                    debug!("sending reply...");
                                    if let Err(e) = reply_to.send(msg) {
                                        error!("unexpected error in reply; error={:?}", e);
                                    };
                                }
                            }
                            MessageKind::Broadcast => {
                                debug!("broadcasting...");
                                if let Err(e) =
                                    self.broadcast_sender.send(MessageReply::Broadcast {
                                        channel_id: msg.channel_id,
                                        event: msg.event,
                                        payload: msg.payload,
                                    })
                                {
                                    error!("unexpected error in broadcast; err={:?}", e);
                                };
                            }

                            MessageKind::BroadcastIntercept => {
                                debug!("broadcasting...");
                                if let Err(e) =
                                    self.broadcast_sender
                                        .send(MessageReply::BroadcastIntercept {
                                            channel_id: msg.channel_id,
                                            event: msg.event,
                                            payload: msg.payload,
                                        })
                                {
                                    error!("unexpected error in broadcast; err={:?}", e);
                                };
                            }

                            _ => {
                                todo!()
                            }
                        },
                    }
                }
            }
        })
    }

    // The volume of join requests would probably be sufficient to have everything go
    // through the locked Registry mutex, though it would be nice not to have to.
    // FIXME: avoid duplicate subscriptions
    // FIXME: add user presence tracking
    fn handle_join(&mut self, message: &DecoratedMessage) -> Result<()> {
        match message {
            DecoratedMessage {
                inner:
                    Message {
                        kind: MessageKind::Join,
                        ref channel_id,
                        payload,
                        ..
                    },
                reply_to: Some(tx),
                ws_reply_to: Some(ws_reply_to),
                ..
            } => {
                spawn_broadcast_subscriber(
                    ws_reply_to.clone(),
                    tx.clone(),
                    self.broadcast_sender.subscribe(),
                );

                self.presence.track(message.token, &payload);
                self.handle_presence(channel_id);

                tx.send(Message {
                    join_ref: None, // FIXME: return the token ID here?
                    kind: MessageKind::DidJoin,
                    msg_ref: message.msg_ref.clone(),
                    event: "phx_join".into(),
                    channel_id: channel_id.clone(),
                    channel_sender: Some(self.incoming_sender.clone()),
                    payload: json!(null),
                })
                .map_err(|e| Error::Send(e.to_string()))
            }
            _ => {
                eprintln!("unexpected={:?}", message);
                todo!("handle errors; this should not happen");
            }
        }
    }

    fn handle_leave(&mut self, message: DecoratedMessage) -> () {
        self.presence.leave(message.token);
        self.behavior.handle_leave(&message);
        self.handle_presence(&message.channel_id());
    }

    fn handle_presence(&mut self, channel_id: &crate::types::ChannelId) -> () {
        match self.behavior.handle_presence(channel_id, &self.presence) {
            Ok(Some(msg)) => match msg.kind {
                MessageKind::Join => todo!(),
                MessageKind::JoinRequest => todo!(),
                MessageKind::DidJoin => todo!(),
                MessageKind::Leave => todo!(),
                MessageKind::Event => todo!(),
                MessageKind::Broadcast => {
                    let _ = self.broadcast_sender.send(MessageReply::Broadcast {
                        event: msg.event,
                        payload: msg.payload,
                        channel_id: msg.channel_id,
                    });
                }
                MessageKind::Heartbeat => todo!(),
                MessageKind::BroadcastIntercept => todo!(),
                MessageKind::Reply => todo!(),
                MessageKind::Push => todo!(),
                MessageKind::PresenceChange => todo!(),
                MessageKind::BroadcastPresence => todo!(),
                MessageKind::Closed => todo!(),
            },

            Err(e) => {
                error!("{:?}", e);
            }
            _ => {}
        }
    }

    fn handle_info(&mut self, message: Message) -> () {
        todo!()
        // match self.behavior.handle_info(&message) {
        //     Ok(Some(msg)) => {
        //         if matches!(msg.kind, MessageKind::BroadcastPresence) {
        //             let _ = self.broadcast_sender.send(MessageReply::Broadcast {
        //                 event: "presence".into(),
        //                 payload: serde_json::json!(self.presence),
        //                 channel_id: msg.channel_id,
        //             });
        //         }
        //     }
        //     Err(e) => {
        //         error!("{:?}", e);
        //     }
        //     _ => {}
        // }
    }
}

fn spawn_broadcast_subscriber(
    ws_sender: UnboundedSender<MessageReply>,
    mailbox_sender: UnboundedSender<Message>,
    mut broadcast: broadcast::Receiver<MessageReply>,
) -> JoinHandle<()> {
    debug!("spawn_broadcast_subscriber");
    // This would work for a Sender, but not UnboundedSender
    // tokio::spawn(BroadcastStream::new(broadcast).forward(PollSender::new(socket_sender)))

    tokio::spawn(async move {
        while let Ok(msg_reply) = broadcast.recv().await {
            if let MessageReply::BroadcastIntercept {
                channel_id,
                event,
                payload,
            } = msg_reply
            {
                // send Intercepts back to the individual sockets for further processing
                mailbox_sender
                    .send(Message {
                        join_ref: None,
                        msg_ref: None,
                        kind: MessageKind::BroadcastIntercept,
                        channel_id,
                        event,
                        payload,
                        channel_sender: None,
                    })
                    .unwrap()
            } else {
                // Other messages get sent directly to the websocket writer
                if let Err(e) = ws_sender.send(msg_reply) {
                    error!("{:?}", e);
                    break;
                }
            }
        }
    })
}
