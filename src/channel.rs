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

use crate::message::{self, Event, Message, MsgRef};
use crate::message::{MessageKind, MessageReply};
use crate::registry::{RegistryMessage, RegistrySender};
use crate::spawn_named;
use crate::types::{ChannelId, Token};
use serde_json::json;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio::time::interval;
use tracing::{debug, error, warn};

pub struct ChannelRunner {
    incoming_sender: UnboundedSender<MessageContext>,
    incoming_receiver: UnboundedReceiver<MessageContext>,
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

// FIXME: design: there's a conceptual overlap between MessageKind and these individual message handler callbacks
#[axum::async_trait]
pub trait Channel: std::fmt::Debug + Send + Sync {
    async fn handle_message(&mut self, _context: &MessageContext) -> Option<Message> {
        None
    }

    /// receives a BroadcastIntercept for socket-specific processing
    async fn handle_out(&mut self, _context: &MessageContext) -> Option<Message> {
        None
    }

    /// authorize new socket connections; return Ok(..) to allow the socket to join the channel, with
    /// an optional response
    async fn handle_join(&mut self, _context: &MessageContext) -> Result<Option<Message>> {
        Ok(None)
    }

    // FIXME: use MessageContext
    async fn handle_presence(
        &mut self,
        _channel_id: &crate::types::ChannelId,
        _presence: &Presence,
    ) -> Result<Option<Message>> {
        Ok(None)
    }

    async fn handle_info(&mut self, _context: &MessageContext) -> Result<Option<Message>> {
        Ok(None)
    }

    async fn handle_leave(&mut self, _context: &MessageContext) -> Result<Option<Message>> {
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
        inactivity_timeout: bool,
    ) -> (JoinHandle<()>, UnboundedSender<MessageContext>) {
        let channel = Self::new(channel_id, channel);
        let sender = channel.incoming_sender.clone();

        let handle = channel.start(registry_sender, inactivity_timeout);

        (handle, sender)
    }

    // If inactivity_timeout is false (default false for calls to add_channel) the
    // inactivity tick is ignored.
    pub fn start(
        mut self,
        registry_sender: RegistrySender,
        inactivity_timeout: bool,
    ) -> JoinHandle<()> {
        let channel_id = self.channel_id.clone();
        debug!("starting {}", channel_id);

        spawn_named(
            async move {
                let timeout = interval(Duration::from_secs(60));

                tokio::pin!(timeout);

                timeout.tick().await; // skip the first tick, which happens immediately

                // FIXME: may just need an additional receiver for registry messages
                loop {
                    tokio::select! {
                        m = self.incoming_receiver.recv() => {
                            match m {
                                Some(mut message) => {
                                    self.handle_incoming(message).await;
                                }
                                None => { break; }
                            }

                            timeout.reset();
                        }
                        _ = timeout.tick() => {
                            warn!("[{:?}] inactivity check {:?}", self.channel_id, self.presence);

                            // FIXME: we shouldn't shut down for manually-added channels (ie, those not lazily created from a template)
                            if inactivity_timeout && self.presence.is_empty() {
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
            },
            &format!("{}:mailbox", channel_id),
        )
    }

    async fn handle_incoming(&mut self, mut context: MessageContext) {
        context.broadcast = Some(self.broadcast_sender.clone());
        // FIXME: also pass Join to callback to allow channel to do things
        if context.is_leave() {
            self.handle_leave(context).await;
        } else {
            let response = if context.is_intercept() {
                self.channel.handle_out(&context).await
            } else if context.is_join() {
                match self.channel.handle_join(&context).await {
                    Ok(join_response) => {
                        debug!("join_response={:#?}", join_response);
                        self.handle_join(&context).await;
                        join_response
                    }
                    Err(e) => {
                        error!("{:?}", e);
                        None
                    }
                }
            } else {
                self.channel.handle_message(&context).await
            };

            match response {
                None => {
                    debug!("got handle_message => None");
                }

                Some(msg) => context.send(msg),
            }
        }
    }

    // The volume of join requests would probably be sufficient to have everything go
    // through the locked Registry mutex, though it would be nice not to have to.
    // FIXME: avoid duplicate subscriptions
    // FIXME: add user presence tracking
    async fn handle_join(&mut self, context: &MessageContext) -> Result<()> {
        match context {
            MessageContext {
                inner:
                    Message {
                        kind: MessageKind::JoinRequest,
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
                    &context.token,
                    &self.channel_id,
                );

                self.presence.track(context.token, payload);
                self.handle_presence(channel_id).await;

                tx.send(context.did_join(context.msg_ref.clone(), self.incoming_sender.clone()))
                    .map_err(|e| Error::Send(e.to_string()))
            }
            _ => {
                todo!("handle errors; this should not happen");
            }
        }
    }

    async fn handle_leave(&mut self, context: MessageContext) {
        self.presence.leave(context.token);
        let _ = self.channel.handle_leave(&context).await;
        let _ = self.handle_presence(context.channel_id()).await;
    }

    async fn handle_presence(&mut self, channel_id: &crate::types::ChannelId) {
        match self
            .channel
            .handle_presence(channel_id, &self.presence)
            .await
        {
            Ok(Some(msg)) => match msg.kind {
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
    token: &Token,
    channel_id: &ChannelId,
) -> JoinHandle<()> {
    debug!("spawn_broadcast_subscriber");
    // This would work for a Sender, but not UnboundedSender
    // tokio::spawn(BroadcastStream::new(broadcast).forward(PollSender::new(socket_sender)))

    spawn_named(
        async move {
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
                            channel_id,
                            kind: MessageKind::BroadcastIntercept,
                            msg_ref: None,
                            join_ref: None,
                            payload,
                            event,
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
        },
        &format!("broadcast:{}:{}", channel_id, token),
    )
}

// Sort of a hybrid between a channel and a message;
// i.e. a message with channel/socket context around it
#[derive(Debug)]
pub struct MessageContext {
    pub token: Token,
    pub inner: Message,
    pub reply_to: Option<UnboundedSender<Message>>,
    pub ws_reply_to: Option<UnboundedSender<MessageReply>>,
    pub broadcast: Option<broadcast::Sender<MessageReply>>,
    pub msg_ref: Option<MsgRef>,
}

impl MessageContext {
    pub fn is_join(&self) -> bool {
        matches!(self.inner.kind, MessageKind::JoinRequest)
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

    /// Replies from the channel trait methods are normally piped through here;
    /// additionally, this method is available on the MessageContext argument to those methods.
    pub fn send(&self, msg: Message) {
        match msg.kind {
            MessageKind::Push => {
                if let Some(reply_to) = &self.reply_to {
                    if let Err(e) = reply_to.send(msg) {
                        error!("unexpected error in reply; error={:?}", e);
                    };
                }
            }
            MessageKind::Broadcast | MessageKind::BroadcastIntercept => {
                debug!("broadcasting...");

                let reply: Option<MessageReply> = match msg.try_into() {
                    Ok(reply) => Some(reply),
                    Err(e) => {
                        error!("could not convert message to broadcast; reason={:?}", e);
                        None
                    }
                };

                if reply.is_some() {
                    match self.broadcast.as_ref() {
                        Some(broadcast) => {
                            if let Err(e) = broadcast.send(reply.unwrap()) {
                                error!("unexpected error in broadcast; err={:?}", e);
                            }
                        }

                        None => {
                            error!(
                            "No broadcast channel found in this context; channel_id={}, kind={:?}",
                            self.channel_id(),
                            self.inner.kind
                        );
                        }
                    }
                }
            }

            _ => {
                todo!()
            }
        }
    }

    pub fn broadcast_intercept(&self, event: Event, payload: serde_json::Value) -> Message {
        Message {
            join_ref: None,
            msg_ref: None,
            kind: MessageKind::BroadcastIntercept,
            channel_id: self.channel_id().clone(),
            event,
            payload,
            channel_sender: None,
        }
    }

    pub fn push(
        &self,
        // include a msg_ref to indicate a reply
        msg_ref: Option<MsgRef>,
        event: Event,
        payload: serde_json::Value,
    ) -> Message {
        Message {
            kind: MessageKind::Push,
            channel_id: self.channel_id().clone(),
            msg_ref,
            join_ref: None,
            payload,
            event,
            channel_sender: None,
        }
    }

    pub fn broadcast(&self, event: Event, payload: serde_json::Value) -> Message {
        Message {
            kind: MessageKind::Broadcast,
            channel_id: self.channel_id().clone(),
            msg_ref: None,
            join_ref: None,
            payload,
            event,
            channel_sender: None,
        }
    }

    pub(crate) fn did_join(
        &self,
        msg_ref: Option<MsgRef>,
        channel_sender: UnboundedSender<MessageContext>,
    ) -> Message {
        Message {
            join_ref: None, // FIXME: return the token ID here?
            kind: MessageKind::DidJoin,
            msg_ref,
            event: "phx_join".into(),
            channel_id: self.channel_id().clone(),
            channel_sender: Some(channel_sender),
            payload: json!(null),
        }
    }
}
