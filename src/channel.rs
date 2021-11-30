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
    broadcast_receiver: broadcast::Receiver<MessageReply>,
    behavior: Box<dyn ChannelBehavior + Sync + Send>,
}

pub trait ChannelBehavior {
    fn handle_message(&mut self, message: &DecoratedMessage) -> MessageReply {
        MessageReply::None
    }
}

impl Channel {
    pub fn new(behavior: Box<dyn ChannelBehavior + Sync + Send>) -> Self {
        let (incoming_sender, incoming_receiver) = unbounded_channel();
        let (broadcast_sender, broadcast_receiver) = broadcast::channel(1024);

        Self {
            incoming_sender,
            incoming_receiver,
            broadcast_receiver,
            broadcast_sender,
            behavior,
        }
    }

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

                if message.is_join() {
                    self.handle_join(message);
                } else {
                    match self.behavior.handle_message(&message) {
                        MessageReply::None => {
                            println!("got MessageReply::None");
                        }
                        // Reply should send a Token-keyed message back to some place; either a central broker
                        // or a oneshot channel (this would necessitate the caller knowing that a reply is sent, and should listen for it)
                        MessageReply::Reply(inner) => {
                            if let Some(reply_to) = message.reply_to {
                                println!("sending reply...");
                                reply_to.send(MessageReply::Reply(inner));
                            }
                        }
                        MessageReply::Broadcast(msg) => {
                            println!("broadcasting...");
                            self.broadcast_sender.send(MessageReply::Broadcast(msg));
                        }
                        _ => {
                            todo!()
                        }
                    }
                }
            }
        })
    }

    fn handle_join(&self, message: DecoratedMessage) -> () {
        match message.reply_to {
            Some(tx) => {
                tx.send(MessageReply::Join(Arc::new((
                    self.incoming_sender.clone(),
                    self.broadcast_sender.subscribe(),
                ))))
                .unwrap(); // FIXME: unwrap

                self.broadcast_sender
                    .send(MessageReply::Broadcast(format!(
                        "socket {} joined the channel. Welcome!",
                        message.token
                    )))
                    .unwrap(); // FIXME
            }
            _ => todo!(),
        }
    }

    // FIXME: what should this look like?
    // - subscriber is a socket
    // - should receive a clone of the incoming sender to send messages to the channel
    // - should
    // fn subscribe(&self, token: usize) -> broadcast::Receiver<DecoratedMessage> {}
}
