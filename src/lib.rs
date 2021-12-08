use axum::extract::ws::{self, CloseFrame, WebSocket};
use futures::sink::SinkExt;
use futures::stream::{SplitSink, SplitStream, StreamExt};
use message::{DecoratedMessage, MessageKind};
use registry::Registry;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::Display;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tracing::{debug, error, info};
use types::Token;

use crate::message::{Message, MessageReply};
use crate::types::ChannelId;

// TODO:
// - each socket gets a private channel; messages can be sent to that particular socket.
// - a socket can subscribe to any channel by name (join?)
// - periodically send PING to client, await PONG (should register a timeout and cancel it when the pong is received?)

pub mod channel;
pub mod message;
pub mod registry;
pub mod types;

// FIXME: so far this provides minimal value;
// something like https://docs.rs/http/0.2.5/http/struct.Extensions.html
// would allow some form of mutable state without a lot of futzing with generics
pub struct Conn {
    format: ConnFormat,
    mailbox_tx: UnboundedSender<Message>,
    mailbox_rx: UnboundedReceiver<Message>,
    extensions: http::Extensions,
}

// channel responses should be:
//  - reply (send a reply to the sender)
//  - broadcast (send a reply to all members of the channel)
//  - None

// example join flow:
// - conn sends join:my_channel
// - my_channel is created if it doesn't exist, with a new broadcast pair
// - conn subscribes to the broadcast

// FIXME: it may be beneficial to use RwLock instead of Mutex, though the locking operations
// will probably only happen when mpsc channels are opened for new connections / subscriptions.

// FIXME: this is more or less for testing purposes; the endpoint selected
// will establish which serialization format will be used for the client.
// 'message' format is similar to Phoenix socketry, where the event name is accompanied by a json payload,
// e.g. join,{ "token": "foo" }
#[derive(Debug, Clone, Copy)]
pub enum ConnFormat {
    Phoenix,
}

fn get_token() -> Token {
    static COUNTER: AtomicUsize = AtomicUsize::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed).into()
}

pub async fn handle_connect(socket: WebSocket, format: ConnFormat, registry: Arc<Mutex<Registry>>) {
    let token = get_token();
    let (writer, reader) = socket.split();
    let (sender, receiver) = unbounded_channel();

    let (mailbox_tx, mailbox_rx) = unbounded_channel();

    let mut conn = Conn {
        mailbox_tx,
        mailbox_rx,
        format,
        extensions: Default::default(),
    };

    conn.extensions.insert(token);

    tokio::spawn(write(
        token,
        format,
        writer,
        receiver,
        sender.clone(),
        registry.clone(),
    ));
    tokio::spawn(read(token, conn, reader, sender, registry.clone()));
}

// A set of senders pointing to the subscribed channels.
// Avoids a central message bus, instad allows sockets to more-or-less directly send messages
// to the intended channels.
pub struct ReaderSubscriptions {
    channels: HashMap<ChannelId, UnboundedSender<DecoratedMessage>>,
    token: Token,
    mailbox_tx: UnboundedSender<Message>,
}

impl ReaderSubscriptions {
    pub fn new(token: Token, mailbox_tx: UnboundedSender<Message>) -> Self {
        Self {
            channels: Default::default(),
            token,
            mailbox_tx,
        }
    }

    pub fn insert(&mut self, channel_id: ChannelId, sender: UnboundedSender<DecoratedMessage>) {
        self.channels.entry(channel_id).or_insert(sender);
    }
}

impl Drop for ReaderSubscriptions {
    fn drop(&mut self) {
        for (channel_id, sender) in &self.channels {
            sender.send(
                Message {
                    kind: MessageKind::Leave,
                    channel_id: channel_id.clone(),
                    channel_sender: None,
                    event: "phx_leave".into(),
                    join_ref: None,
                    msg_ref: None,
                    payload: Default::default(),
                }
                .decorate(self.token, self.mailbox_tx.clone()),
            );
        }
    }
}

// FIXME: should be temporary?
fn parse_message<'a>(
    message: &'a str,
    format: &ConnFormat,
) -> Result<Message, Box<dyn std::error::Error + Send + Sync + 'a>> {
    match format {
        &ConnFormat::Phoenix => PhoenixParser::from_str(message).map_err(Into::into),
    }
}

struct PhoenixParser;

impl PhoenixParser {
    pub fn from_str(input: &str) -> Result<Message, ParseError> {
        let value: serde_json::Value = serde_json::from_str(input).unwrap();

        let join_ref = value[0].as_str().map(Into::into);
        let msg_ref = value[1].as_str().map(Into::into);

        let channel_id: ChannelId = value[2].as_str().unwrap().parse().unwrap();
        let event = value[3].as_str().unwrap().to_string();
        let payload = value[4].to_owned();

        let kind = match event.as_str() {
            "join" | "phx_join" => MessageKind::JoinRequest,
            "heartbeat" => MessageKind::Heartbeat,
            _ => MessageKind::Event,
        };

        Ok(Message {
            kind,
            join_ref,
            msg_ref,
            channel_id,
            event,
            payload,
            channel_sender: None,
        })
    }
}

// reading data from remote
async fn read(
    token: Token,
    mut conn: Conn,
    mut ws_receiver: SplitStream<WebSocket>,
    reply_sender: UnboundedSender<MessageReply>,
    registry: Arc<Mutex<Registry>>,
) {
    let mut subscriptions = ReaderSubscriptions::new(token, conn.mailbox_tx.clone());

    let format = conn.format.clone();
    let mailbox_tx = conn.mailbox_tx.clone();
    let ws_reply_sender = reply_sender.clone(); // directly send a response to the ws writer
    let registry_c = registry.clone();

    // this task maps ws::Message to Message and sends them to mailbox_tx
    let _ws_handle = tokio::spawn(async move {
        while let Some(msg) = ws_receiver.next().await {
            info!("{:?}", msg);
            match msg {
                Ok(inner) => match inner {
                    ws::Message::Text(inner) => {
                        // FIXME: eep
                        let msg: Result<Message, _> = parse_message(&inner, &format);

                        debug!("message parsed; {:?}", msg);
                        match msg {
                            Ok(msg) => mailbox_tx.send(msg).unwrap(),
                            Err(e) => {
                                error!("{:?}", e);
                            }
                        }
                    }
                    ws::Message::Binary(_) => todo!(),
                    ws::Message::Ping(data) => {
                        ws_reply_sender.send(MessageReply::Pong(data)).unwrap()
                    } // FIXME unwrap
                    ws::Message::Pong(_) => todo!(),
                    ws::Message::Close(frame) => {
                        handle_write_close(token, registry_c); // it's entirely possible this will get called more than once
                        return handle_read_close(token, frame);
                    }
                },
                Err(e) => {
                    error!("error={:?}", e);
                }
            }
        }
    });

    while let Some(msg) = conn.mailbox_rx.recv().await {
        match &msg.kind {
            // FIXME: drop this variant?
            MessageKind::Join => {
                debug!("joining token={}, channel={}", token, msg.channel_id.id());
            }
            MessageKind::JoinRequest => {
                debug!(
                    "sending JoinRequest to registry; channel_id={}",
                    msg.channel_id.id()
                );

                let mut locked = registry.lock().unwrap();
                locked.handle_join_request(
                    token,
                    msg.channel_id,
                    conn.mailbox_tx.clone(),
                    reply_sender.clone(),
                    msg.msg_ref.unwrap(),
                );
            }
            MessageKind::DidJoin => {
                debug!("received join confirmation");
                subscriptions
                    .channels
                    .entry(msg.channel_id.clone())
                    .or_insert(msg.channel_sender.unwrap());

                reply_sender
                    .send(MessageReply::Join {
                        channel_id: msg.channel_id,
                        msg_ref: msg.msg_ref.unwrap(),
                    })
                    .unwrap();
            }

            MessageKind::Leave => todo!(),

            MessageKind::Reply => {
                todo!()
            }

            MessageKind::Broadcast => {
                todo!() // This probably shouldn't be sent here
            }

            MessageKind::Event | MessageKind::BroadcastIntercept => {
                if let Some(tx) = subscriptions.channels.get(&msg.channel_id) {
                    tx.send(msg.decorate(token, conn.mailbox_tx.clone()));
                }
            }

            MessageKind::Heartbeat => {
                reply_sender
                    .send(MessageReply::Heartbeat {
                        msg_ref: msg.msg_ref.unwrap(),
                    })
                    .unwrap();
            }

            MessageKind::Push => {
                reply_sender
                    .send(MessageReply::Push {
                        channel_id: msg.channel_id,
                        event: msg.event,
                        payload: msg.payload,
                    })
                    .unwrap();
            }
        }
    }
}

fn handle_read_close(token: Token, _frame: Option<CloseFrame>) {
    debug!("socket {} closed", token)
}

fn handle_write_close(token: Token, registry: Arc<Mutex<Registry>>) {
    debug!("socket writer {} closed", token);

    registry.lock().unwrap().deregister_writer(token);
}

#[derive(Debug)]
pub struct ParseError {
    message: Option<String>,
}

impl Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for ParseError {}

async fn write(
    token: Token,
    _format: ConnFormat,
    mut writer: SplitSink<WebSocket, ws::Message>,
    mut receiver: UnboundedReceiver<MessageReply>,
    sender: UnboundedSender<MessageReply>,
    registry: Arc<Mutex<Registry>>,
) {
    // let mut subscriber = registry.lock().unwrap().register_writer(token);
    registry.lock().unwrap().register_writer(token, sender);

    while let Some(msg) = receiver.recv().await {
        let ws_msg: ws::Message = msg.into();

        debug!("[write] msg = {:?}", ws_msg);
        match writer.send(ws_msg).await {
            Ok(..) => {}
            Err(..) => {
                return handle_write_close(token, registry);
                // FIXME: this assumes ConnectionClosed (axum errors somewhat opaque, can't be matched)
            }
        }
    }
}
