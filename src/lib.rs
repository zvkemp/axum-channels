use axum::extract::ws::{self, CloseFrame, WebSocket};
use futures::sink::SinkExt;
use futures::stream::{SplitSink, SplitStream, StreamExt};
use message::DecoratedMessage;
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
    JSON,
    Simple,
    Message,
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
                Message::Leave {
                    channel_id: channel_id.clone(), // FIXME unwrap
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
        &ConnFormat::JSON => todo!(), // serde_json::from_str(message).map_err(Into::into),
        &ConnFormat::Simple => SimpleParser::from_str(message).map_err(Into::into),
        &ConnFormat::Message => MessageParser::from_str(message).map_err(Into::into),
    }
}

struct SimpleParser;

// join default
// message | hello
//
impl SimpleParser {
    pub fn from_str(input: &str) -> Result<Message, ParseError> {
        Err(ParseError {
            message: Some("deprecated parser".to_string()),
        })
        // let mut segments = input.split("|");
        // let mut token_args = segments.next().unwrap().split_whitespace();

        // debug!(
        //     "token_args = {:?}",
        //     token_args.clone().collect::<Vec<&str>>()
        // );

        // let command = token_args.next();
        // let channel_id = token_args.next().or(Some("default")).unwrap().to_string();

        // if command.is_some() {
        //     match command.unwrap() {
        //         // "join" | "j" => {
        //         //     return Ok(Message::Join { channel_id });
        //         // }
        //         // "message" | "m" | "msg" => {
        //         //     return Ok(Message::Channel {
        //         //         channel_id,
        //         //         text: segments.next().unwrap().to_string(),
        //         //     });
        //         // }
        //         // "leave" | "l" => {
        //         //     return Ok(Message::Leave { channel_id });
        //         // }
        //         _ => {}
        //     }
        // }

        // // in all other cases, just echo the input
        // Ok(Message::Channel {
        //     channel_id,
        //     text: input.to_string(),
        // })
    }
}

// #[derive(Deserialize)]
// struct Params<'a> {
//     #[serde(borrow)]
//     raw_value: &'a RawValue,
// }

struct MessageParser;
impl MessageParser {
    pub fn from_str(input: &str) -> Result<Message, ParseError> {
        let value: serde_json::Value = serde_json::from_str(input).unwrap();

        let _join_ref = value[0].as_str();
        let msg_ref = value[1].as_str().unwrap().to_string();

        let channel_id: ChannelId = value[2].as_str().unwrap().parse().unwrap();
        let event = value[3].as_str().unwrap();
        let payload = value[4].to_owned();

        match event {
            "join" | "phx_join" => Ok(Message::JoinRequest {
                channel_id,
                msg_ref,
            }),
            "heartbeat" => Ok(Message::Heartbeat { msg_ref }),
            // basically any other event name should be handled by the behavior
            _ => Ok(Message::Event {
                msg_ref,
                channel_id,
                event: event.to_string(),
                payload,
            }),
        }
    }
}

pub(crate) struct JoinRequest {}
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
        match msg {
            /*
            {"Join":{"channel_id":"default"}}
            */
            Message::Join { channel_id } => {
                debug!("joining token={}, channel={}", token, channel_id.id());
                // let join_request = JoinRequest {
                //     channel_key:
                // };
                //     Message::Join { channel_id }.decorate(token, conn.mailbox_tx.clone());
                // decorated.broadcast_reply_to = Some(reply_sender.clone());

                // let locked = registry.lock().unwrap();
                // locked.dispatch(decorated);
            }
            Message::JoinRequest {
                channel_id,
                msg_ref,
            } => {
                debug!(
                    "sending JoinRequest to registry; channel_id={}",
                    channel_id.id()
                );

                let mut locked = registry.lock().unwrap();
                locked.handle_join_request(
                    token,
                    channel_id,
                    conn.mailbox_tx.clone(),
                    reply_sender.clone(),
                    msg_ref,
                );
            }
            Message::DidJoin {
                channel_id,
                channel_sender,
                msg_ref,
                ..
            } => {
                debug!("received join confirmation");
                subscriptions
                    .channels
                    .entry(channel_id.clone())
                    .or_insert(channel_sender);

                reply_sender
                    .send(MessageReply::Join {
                        channel_id,
                        msg_ref,
                    })
                    .unwrap();
            }

            Message::Channel { channel_id, text } => {
                if let Some(tx) = subscriptions.channels.get(&channel_id) {
                    tx.send(
                        Message::Channel { channel_id, text }
                            .decorate(token, conn.mailbox_tx.clone()),
                    );
                }
            }
            Message::Leave { .. } => todo!(),
            Message::Reply(text) => {
                reply_sender.send(MessageReply::Reply(text));
            }

            Message::Broadcast { .. } => {
                todo!() // This probably shouldn't be sent here
            }

            Message::Event {
                channel_id,
                event,
                payload,
                msg_ref,
            } => {
                if let Some(tx) = subscriptions.channels.get(&channel_id) {
                    tx.send(
                        Message::Event {
                            channel_id,
                            event,
                            payload,
                            msg_ref,
                        }
                        .decorate(token, conn.mailbox_tx.clone()),
                    );
                }
            }

            Message::Heartbeat { msg_ref } => {
                reply_sender
                    .send(MessageReply::Heartbeat { msg_ref })
                    .unwrap();
            }

            Message::Push {
                channel_id,
                event,
                payload,
            } => reply_sender
                .send(MessageReply::Push {
                    channel_id,
                    event,
                    payload,
                })
                .unwrap(),

            Message::BroadcastIntercept { ref channel_id, .. } => {
                if let Some(tx) = subscriptions.channels.get(&channel_id) {
                    tx.send(msg.decorate(token, conn.mailbox_tx.clone()));
                }
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
