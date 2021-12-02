use std::{collections::HashMap, sync::Arc};

use tokio::{
    sync::{
        broadcast,
        mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    },
    task::JoinHandle,
};

use crate::message::MessageReply;
use crate::message::{DecoratedMessage, Message};

pub struct Channel {
    incoming_sender: UnboundedSender<DecoratedMessage>,
    incoming_receiver: UnboundedReceiver<DecoratedMessage>,
    broadcast_sender: broadcast::Sender<MessageReply>,
    // broadcast_receiver: broadcast::Receiver<MessageReply>,
    behavior: Box<dyn ChannelBehavior + Sync + Send>,
}

pub trait ChannelBehavior {
    fn handle_message(&mut self, _message: &DecoratedMessage) -> Option<MessageReply> {
        None
    }
}

// FIXME: add channel id
impl Channel {
    pub fn new(behavior: Box<dyn ChannelBehavior + Sync + Send>) -> Self {
        let (incoming_sender, incoming_receiver) = unbounded_channel();
        let (broadcast_sender, _broadcast_receiver) = broadcast::channel(1024);

        Self {
            incoming_sender,
            incoming_receiver,
            // broadcast_receiver,
            broadcast_sender,
            behavior,
        }
    }

    // FIXME: need a way to receive a shutdown message
    pub fn spawn(
        behavior: Box<dyn ChannelBehavior + Sync + Send>,
    ) -> (JoinHandle<()>, UnboundedSender<DecoratedMessage>) {
        let channel = Self::new(behavior);
        let sender = channel.incoming_sender.clone();

        let handle = channel.start();

        (handle, sender)
    }

    pub fn start(mut self) -> JoinHandle<()> {
        println!("starting this channel");

        tokio::spawn(async move {
            while let Some(message) = self.incoming_receiver.recv().await {
                println!("msg={:#?}", message);

                // FIXME: also pass Join to callback to allow behavior to do things
                if message.is_join() {
                    self.handle_join(message);
                } else if message.is_leave() {
                    self.handle_leave(message);
                } else {
                    match self.behavior.handle_message(&message) {
                        None => {
                            println!("got MessageReply::None");
                        }
                        Some(MessageReply::Reply(inner)) => {
                            if let Some(reply_to) = message.reply_to {
                                println!("sending reply...");
                                if let Err(e) = reply_to.send(Message::Reply(inner)) {
                                    eprintln!("unexpected error in reply; error={:?}", e);
                                };
                            }
                        }
                        Some(MessageReply::Broadcast(msg)) => {
                            println!("broadcasting...");
                            if let Err(e) = self.broadcast_sender.send(MessageReply::Broadcast(msg))
                            {
                                eprintln!("unexpected error in broadcast; err={:?}", e);
                            };
                        }

                        _ => {
                            todo!()
                        }
                    }
                }
            }
        })
    }

    // FIXME: join should be a fallible operation (e.g. allow for Authorization).
    // The volume of join requests would probably be sufficient to have everything go
    // through the locked Registry mutex, though it would be nice not to have to.
    fn handle_join(&self, message: DecoratedMessage) -> () {
        match message {
            DecoratedMessage {
                channel_id,
                inner: Message::Join { .. },
                reply_to: Some(tx),
                broadcast_reply_to: Some(broadcast_reply_to),
                ..
            } => {
                tx.send(Message::DidJoin {
                    channel_id,
                    channel_sender: self.incoming_sender.clone(),
                    // FIXME: spawn the task here
                    broadcast_handle: spawn_broadcast_subscriber(
                        broadcast_reply_to,
                        self.broadcast_sender.subscribe(),
                    ),
                })
                .unwrap(); // FIXME: unwrap

                self.broadcast_sender
                    .send(MessageReply::Broadcast(format!(
                        "socket {} joined the channel. Welcome!",
                        message.token
                    )))
                    .unwrap(); // FIXME
            }
            _ => todo!("handle errors; this should not happen"),
        }
    }

    // FIXME: also pass this to behavior?
    // Some behaviors may want to set presence
    fn handle_leave(&self, message: DecoratedMessage) -> () {
        self.broadcast_sender
            .send(MessageReply::Broadcast(format!(
                "socket {} left the channel. Goodbye!",
                message.token
            )))
            .unwrap();
    }

    // FIXME: what should this look like?
    // - subscriber is a socket
    // - should receive a clone of the incoming sender to send messages to the channel
    // - should
    // fn subscribe(&self, token: usize) -> broadcast::Receiver<DecoratedMessage> {}
}

use futures::stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tokio_util::sync::PollSender;

fn spawn_broadcast_subscriber(
    socket_sender: UnboundedSender<MessageReply>,
    mut broadcast: broadcast::Receiver<MessageReply>,
) -> JoinHandle<()> {
    // This would work for a Sender, but not UnboundedSender
    // tokio::spawn(BroadcastStream::new(broadcast).forward(PollSender::new(socket_sender)))

    tokio::spawn(async move {
        while let Ok(msg_reply) = broadcast.recv().await {
            socket_sender.send(msg_reply);
        }
    })
}
