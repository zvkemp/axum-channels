use std::collections::HashMap;
use std::time::Duration;

// High-level FIXME:
// a pattern like the Axum extractor macros may be worthwhile exploring here

// FIXME:
// this design probably has an upper limit to channel members,
// as the mutable state means there's no real channel concurrency.
// This is probably mitigated somewhat by broadcasts, but `BroadcastIntercept`s may end up being expensive to process.

// FIXME:
// It's probably necessary to have some sort of pub-sub abstraction, where if the server is run as a cluster,
// there is a single channel agent responsible for replying to messages. Maybe a good use case for <secret project>
// - introduce MessageAddress
// - introduce traits for routing messages
// - introduce traits AddressEndpoint (maybe this is just Channel)

use crate::message::{DecoratedMessage, Message};
use crate::message::{MessageKind, MessageReply};
use crate::registry::{RegistryMessage, RegistrySender};
use crate::types::{ChannelId, Token};
use serde_json::json;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio::time::interval;
use tracing::{debug, error, warn};

pub struct ChannelRunner {
    incoming_sender: UnboundedSender<DecoratedMessage>,
    incoming_receiver: UnboundedReceiver<DecoratedMessage>,
    broadcast_sender: broadcast::Sender<MessageReply>,
    channel: Box<dyn Channel>,
    presence: Presence,
    channel_id: ChannelId,
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

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

#[derive(Debug)]
pub enum Error {
    Join { reason: String },
    Send(String),
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[axum::async_trait]
pub trait Channel: std::fmt::Debug + Send + Sync {
    async fn handle_message(&mut self, _message: &DecoratedMessage) -> Option<Message> {
        None
    }

    // receives a BroadcastIntercept for socket-specific processing
    async fn handle_out(&mut self, _message: &DecoratedMessage) -> Option<Message> {
        None
    }

    // authorize new socket connections
    async fn handle_join(&mut self, _message: &DecoratedMessage) -> Result<Option<Message>> {
        Ok(None)
    }

    async fn handle_presence(
        &mut self,
        _channel_id: &crate::types::ChannelId,
        _presence: &Presence,
    ) -> Result<Option<Message>> {
        Ok(None)
    }

    async fn handle_info(&mut self, _message: &Message) -> Result<Option<Message>> {
        Ok(None)
    }

    async fn handle_leave(&mut self, _message: &DecoratedMessage) -> Result<Option<Message>> {
        Ok(None)
    }
}

pub trait NewChannel {
    fn new_channel(&self, channel_id: ChannelId) -> Box<dyn Channel>;
}

impl<T: Default + Channel + 'static> NewChannel for T {
    fn new_channel(&self, _channel_id: ChannelId) -> Box<dyn Channel> {
        Box::new(Self::default())
    }
}

// impl<T: Clone + Channel + 'static> NewChannel for T {}

// FIXME: add channel id
impl ChannelRunner {
    pub fn new(channel_id: ChannelId, channel: Box<dyn Channel>) -> Self {
        let (incoming_sender, incoming_receiver) = unbounded_channel();
        let (broadcast_sender, _broadcast_receiver) = broadcast::channel(1024);

        Self {
            incoming_sender,
            incoming_receiver,
            broadcast_sender,
            channel,
            channel_id,
            presence: Default::default(),
        }
    }

    // FIXME: need a way to receive a shutdown message
    pub fn spawn(
        channel_id: ChannelId,
        channel: Box<dyn Channel>,
        registry_sender: RegistrySender,
    ) -> (JoinHandle<()>, UnboundedSender<DecoratedMessage>) {
        let channel = Self::new(channel_id, channel);
        let sender = channel.incoming_sender.clone();

        let handle = channel.start(registry_sender);

        (handle, sender)
    }

    pub fn start(mut self, registry_sender: RegistrySender) -> JoinHandle<()> {
        debug!("starting this channel");

        tokio::spawn(async move {
            let timeout = interval(Duration::from_secs(5));

            tokio::pin!(timeout);

            loop {
                tokio::select! {
                    m = self.incoming_receiver.recv() => {
                        match m {
                            Some(message) => { self.handle_incoming(message).await; }
                            None => { break; }
                        }

                        timeout.reset();
                    }
                    _ = timeout.tick() => {
                        warn!("[{:?}] inactivity check {:?}", self.channel_id, self.presence);

                        if self.presence.is_empty() {
                            let (tx, rx) = oneshot::channel();
                            // Inform the registry that we plan to disable this channel
                            registry_sender.send(RegistryMessage::Inactivity(self.channel_id.clone(), tx));

                            // If the registry has confirmed that no new sockets have connect since the Inactivity notice was sent,
                            // we can shut down.
                            if let Ok(RegistryMessage::Close) = rx.await {
                                warn!("{:?} shutting down due to inactivity", self.channel_id);
                                break;
                            }
                        }
                    }
                }
            }
        })
    }

    async fn handle_incoming(&mut self, message: DecoratedMessage) {
        // FIXME: also pass Join to callback to allow channel to do things
        if message.is_leave() {
            self.handle_leave(message).await;
        } else {
            let response = if message.is_intercept() {
                self.channel.handle_out(&message).await
            } else if message.is_join() {
                match self.channel.handle_join(&message).await {
                    Ok(join_response) => {
                        debug!("join_response={:#?}", join_response);
                        self.handle_join(&message).await;
                        join_response
                    }
                    Err(e) => {
                        error!("{:?}", e);
                        None
                    }
                }
            } else {
                self.channel.handle_message(&message).await
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
                        if let Err(e) = self.broadcast_sender.send(MessageReply::Broadcast {
                            channel_id: msg.channel_id,
                            event: msg.event,
                            payload: msg.payload,
                        }) {
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

    // The volume of join requests would probably be sufficient to have everything go
    // through the locked Registry mutex, though it would be nice not to have to.
    // FIXME: avoid duplicate subscriptions
    // FIXME: add user presence tracking
    async fn handle_join(&mut self, message: &DecoratedMessage) -> Result<()> {
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

                self.presence.track(message.token, payload);
                self.handle_presence(channel_id).await;

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

    async fn handle_leave(&mut self, message: DecoratedMessage) {
        self.presence.leave(message.token);
        let _ = self.channel.handle_leave(&message).await;
        let _ = self.handle_presence(message.channel_id()).await;
    }

    async fn handle_presence(&mut self, channel_id: &crate::types::ChannelId) {
        match self
            .channel
            .handle_presence(channel_id, &self.presence)
            .await
        {
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

    #[allow(dead_code)]
    fn handle_info(&mut self, _message: Message) {
        todo!()
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
