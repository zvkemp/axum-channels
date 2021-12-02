use axum::extract::ws::{self, CloseFrame, WebSocket};
use futures::sink::SinkExt;
use futures::stream::{SplitSink, SplitStream, StreamExt};
use message::DecoratedMessage;
use registry::Registry;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use types::Token;

use crate::message::{Message, MessageReply};

// TODO:
// - make this a lib
// - each socket gets a private channel; messages can be sent to that particular socket.
// - a socket can subscribe to any channel by name (join?)
// - periodically send PING to client, await PONG (should register a timeout and cancel it when the pong is received?)

pub mod channel;
pub mod message;
pub mod registry;
pub mod types;

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
#[derive(Debug, Clone, Copy)]
pub enum ConnFormat {
    JSON,
    Simple,
}

fn get_token() -> Token {
    static COUNTER: AtomicUsize = AtomicUsize::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed).into()
}

pub async fn handle_connect(socket: WebSocket, format: ConnFormat, registry: Arc<Mutex<Registry>>) {
    let token = get_token();
    let (mut writer, mut reader) = socket.split();
    let (sender, receiver) = unbounded_channel();
    tokio::spawn(write(
        token,
        format,
        writer,
        receiver,
        sender.clone(),
        registry.clone(),
    ));
    tokio::spawn(read(token, format, reader, sender, registry.clone()));
}

// A set of senders pointing to the subscribed channels.
// Avoids a central message bus, instad allows sockets to more-or-less directly send messages
// to the intended channels.
pub struct ReaderSubscriptions {
    channels: HashMap<String, UnboundedSender<DecoratedMessage>>,
    token: Token,
}

impl ReaderSubscriptions {
    pub fn new(token: Token) -> Self {
        Self {
            channels: Default::default(),
            token: token,
        }
    }

    pub fn insert(&mut self, channel_id: String, sender: UnboundedSender<DecoratedMessage>) {
        self.channels.entry(channel_id).or_insert(sender);
    }
}

impl Drop for ReaderSubscriptions {
    fn drop(&mut self) {
        for (channel_id, sender) in &self.channels {
            sender.send(
                Message::Leave {
                    channel_id: channel_id.to_string(),
                }
                .decorate(self.token, channel_id.to_string()),
            );
        }
    }
}

fn spawn_subscriber(
    mut broadcast: broadcast::Receiver<MessageReply>,
    reply_sender: UnboundedSender<MessageReply>,
) {
    // FIXME: this task needs cancellation
    tokio::spawn(async move {
        while let Ok(msg) = broadcast.recv().await {
            // FIXME: also need the socket writer here
            println!("received {:?} destined for socket", msg);

            reply_sender.send(msg).unwrap();
        }
    });
}

// FIXME: should be temporary?
fn parse_message(
    message: &str,
    format: &ConnFormat,
) -> Result<Message, Box<dyn std::error::Error + Send + Sync>> {
    match format {
        &ConnFormat::JSON => serde_json::from_str(message).map_err(Into::into),
        &ConnFormat::Simple => SimpleParser::from_str(message).map_err(Into::into),
    }
}

struct SimpleParser;

// join default
// message | hello
//
impl SimpleParser {
    pub fn from_str(input: &str) -> Result<Message, String> {
        let mut segments = input.split("|");
        let mut token_args = segments.next().unwrap().split_whitespace();

        println!(
            "token_args = {:?}",
            token_args.clone().collect::<Vec<&str>>()
        );

        let command = token_args.next();
        let channel_id = token_args.next().or(Some("default")).unwrap().to_string();

        if command.is_some() {
            match command.unwrap() {
                "join" | "j" => {
                    return Ok(Message::Join { channel_id });
                }
                "message" | "m" | "msg" => {
                    return Ok(Message::Channel {
                        channel_id,
                        text: segments.next().unwrap().to_string(),
                    });
                }
                "leave" | "l" => {
                    return Ok(Message::Leave { channel_id });
                }

                _ => {}
            }
        }

        // in all other cases, just echo the input
        Ok(Message::Channel {
            channel_id,
            text: input.to_string(),
        })
    }
}
// reading data from remote
async fn read(
    token: Token,
    format: ConnFormat,
    mut ws_receiver: SplitStream<WebSocket>,
    reply_sender: UnboundedSender<MessageReply>,
    registry: Arc<Mutex<Registry>>,
) {
    let mut subscriptions = ReaderSubscriptions::new(token);
    while let Some(msg) = ws_receiver.next().await {
        match msg {
            Ok(inner) => match inner {
                ws::Message::Text(inner) => {
                    // eep
                    let msg: Result<Message, _> = parse_message(&inner, &format);

                    match msg {
                        /*
                        {"Join":{"channel_id":"default"}}
                        */
                        Ok(Message::Join { channel_id }) => {
                            println!("joining token={}, channel={}", token, channel_id);
                            println!("{:#?}", registry);
                            // FIXME: if possible `subscribe` should do all of the work necessary to
                            // run this subscription, without the additional processing steps. This would
                            // allow channels to spawn subscriptions to other channels.

                            // Alternately,
                            //  - change Token to Token(usize, sender)
                            //  - change this loop to merge the associated receiver and ws receiver streams
                            //  - allow channels to send messages back to this loop.

                            // Alternately,
                            //  - move `spawn_subscriber` into Registry
                            //  - lazily update ReaderSubscriptions when a message is sent
                            // (this might allow getting rid of the oneshot callbacks)
                            let rx = {
                                let locked = registry.lock().unwrap();
                                locked.subscribe(channel_id.clone(), token)
                            };

                            if rx.is_some() {
                                if let Ok(MessageReply::Join(arc)) = rx.unwrap().await {
                                    let (sender, bx) = Arc::try_unwrap(arc).unwrap();
                                    subscriptions.insert(channel_id, sender);
                                    spawn_subscriber(bx, reply_sender.clone());
                                }
                            } else {
                                eprintln!("channel not active");
                            }
                        }
                        Ok(Message::Channel { channel_id, text }) => {
                            if let Some(tx) = subscriptions.channels.get(&channel_id) {
                                tx.send(
                                    Message::Channel {
                                        channel_id: channel_id.clone(),
                                        text,
                                    }
                                    .decorate(token, channel_id),
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!("{:?}", e);
                        }
                        _ => todo!(),
                    }
                }
                ws::Message::Binary(_) => todo!(),
                ws::Message::Ping(data) => reply_sender.send(MessageReply::Pong(data)).unwrap(), // FIXME unwrap
                ws::Message::Pong(_) => todo!(),
                ws::Message::Close(frame) => {
                    handle_write_close(token, registry); // it's entirely possible this will get called more than once
                    return handle_read_close(token, frame);
                }
            },
            Err(e) => {
                eprintln!("error={:?}", e);
            }
        }
    }
}

fn handle_read_close(token: Token, _frame: Option<CloseFrame>) {
    println!("socket {} closed", token)
}

fn handle_write_close(token: Token, registry: Arc<Mutex<Registry>>) {
    println!("socket writer {} closed", token);

    registry.lock().unwrap().deregister_writer(token);
}

async fn write(
    token: Token,
    format: ConnFormat,
    mut writer: SplitSink<WebSocket, ws::Message>,
    mut receiver: UnboundedReceiver<MessageReply>,
    sender: UnboundedSender<MessageReply>,
    registry: Arc<Mutex<Registry>>,
) {
    // let mut subscriber = registry.lock().unwrap().register_writer(token);
    registry.lock().unwrap().register_writer(token, sender);

    while let Some(msg) = receiver.recv().await {
        let ws_msg: ws::Message = msg.into();

        println!("[write] msg = {:?}", ws_msg);
        match writer.send(ws_msg).await {
            Ok(..) => {}
            Err(..) => {
                return handle_write_close(token, registry);
                // FIXME: this assumes ConnectionClosed (axum errors somewhat opaque, can't be matched)
            }
        }
    }
}
