// High-level FIXME:
// a pattern like the Axum extractor macros may be worthwhile exploring here
use crate::message::{DecoratedMessage, Message};
use crate::message::{MessageKind, MessageReply};
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
}

pub enum JoinError {
    Unauthorized,
    Unknown,
}

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
    fn handle_join(&mut self, _message: &DecoratedMessage) -> Result<(), JoinError> {
        Ok(())
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
                debug!("msg={:#?}", message);

                // FIXME: also pass Join to callback to allow behavior to do things
                if message.is_join() {
                    self.behavior
                        .handle_join(&message)
                        .and_then(|_| Ok(self.handle_join(message)));
                } else if message.is_leave() {
                    self.handle_leave(message);
                } else {
                    let response = if message.is_intercept() {
                        self.behavior.handle_out(&message)
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
    fn handle_join(&self, message: DecoratedMessage) -> () {
        match message {
            DecoratedMessage {
                inner:
                    Message {
                        kind: MessageKind::Join,
                        channel_id,
                        ..
                    },
                reply_to: Some(tx),
                broadcast_reply_to: Some(broadcast_reply_to),
                ..
            } => {
                spawn_broadcast_subscriber(
                    broadcast_reply_to,
                    tx.clone(),
                    self.broadcast_sender.subscribe(),
                );

                tx.send(Message {
                    join_ref: None, // FIXME: return the token ID here?
                    kind: MessageKind::DidJoin,
                    msg_ref: message.msg_ref,
                    event: "phx_join".into(),
                    channel_id,
                    channel_sender: Some(self.incoming_sender.clone()),
                    payload: json!(null),
                })
                .unwrap();
            }
            _ => {
                eprintln!("unexpected={:?}", message);
                todo!("handle errors; this should not happen");
            }
        }
    }

    // FIXME: also pass this to behavior?
    // Some behaviors may want to set presence
    fn handle_leave(&self, message: DecoratedMessage) -> () {
        // self.broadcast_sender
        //     .send(MessageReply::Broadcast(format!(
        //         "socket {} left the channel. Goodbye!",
        //         message.token
        //     )))
        //     .unwrap();
    }

    // FIXME: what should this look like?
    // - subscriber is a socket
    // - should receive a clone of the incoming sender to send messages to the channel
    // - should
    // fn subscribe(&self, token: usize) -> broadcast::Receiver<DecoratedMessage> {}
}

fn spawn_broadcast_subscriber(
    socket_sender: UnboundedSender<MessageReply>,
    mailbox_sender: UnboundedSender<Message>,
    mut broadcast: broadcast::Receiver<MessageReply>,
) -> JoinHandle<()> {
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
                socket_sender.send(msg_reply);
            }
        }
    })
}
