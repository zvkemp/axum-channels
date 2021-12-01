use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::ws::{self, CloseFrame, WebSocket, WebSocketUpgrade};
use axum::extract::Extension;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{AddExtensionLayer, Router};
use futures::sink::SinkExt;
use futures::stream::{SplitSink, SplitStream, StreamExt};
use message::DecoratedMessage;
use registry::Registry;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use types::Token;

use crate::message::{Message, MessageReply};

// TODO:
// - make this a lib
// - each socket gets a private channel; messages can be sent to that particular socket.
// - a socket can subscribe to any channel by name (join?)
// - periodically send PING to client, await PONG (should register a timeout and cancel it when the pong is received?)

mod channel;
mod message;
mod registry;
mod types;

#[tokio::main]
async fn main() {
    let mut registry = Arc::new(Mutex::new(Registry::default()));
    let app = Router::new()
        .route("/ws", get(handler))
        .layer(AddExtensionLayer::new(registry));

    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

fn get_id() -> usize {
    static COUNTER: AtomicUsize = AtomicUsize::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
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
async fn handler(
    ws: WebSocketUpgrade,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    println!("handler");
    ws.on_upgrade(move |socket| handle_connect(socket, registry.clone()))
}

async fn handle_connect(mut socket: WebSocket, registry: Arc<Mutex<Registry>>) {
    let token = get_id();
    let (mut writer, mut reader) = socket.split();
    let (sender, receiver) = unbounded_channel();
    tokio::spawn(write(
        token,
        writer,
        receiver,
        sender.clone(),
        registry.clone(),
    ));
    tokio::spawn(read(token, reader, sender, registry.clone()));
}

// A set of senders pointing to the subscribed channels.
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
    tokio::spawn(async move {
        while let Ok(msg) = broadcast.recv().await {
            // FIXME: also need the socket writer here
            println!("received {:?} destined for socket", msg);

            reply_sender.send(msg).unwrap();
        }
    });
}

// reading data from remote
async fn read(
    token: usize,
    mut receiver: SplitStream<WebSocket>,
    reply_sender: UnboundedSender<MessageReply>,
    registry: Arc<Mutex<Registry>>,
) {
    let mut subscriptions = ReaderSubscriptions::new(token);
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(inner) => match inner {
                ws::Message::Text(inner) => {
                    // eep
                    let msg: Result<Message, _> = serde_json::from_str(&inner);

                    match msg {
                        Ok(Message::Join { channel_id }) => {
                            println!("joining token={}, channel={}", token, channel_id);
                            println!("{:#?}", registry);
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
                        Ok(Message::Channel { .. }) => todo!(),
                        Err(e) => {
                            eprintln!("{:?}", e);

                            // FIXME: remove this
                            let msg = Message::Join {
                                channel_id: "default".to_string(),
                            };
                            let serialized = serde_json::to_string(&msg);
                            eprintln!("{}", serialized.unwrap());
                        }
                        _ => todo!(),
                    }
                }
                ws::Message::Binary(_) => todo!(),
                ws::Message::Ping(data) => reply_sender.send(MessageReply::Pong(data)).unwrap(), // FIXME unwrap
                ws::Message::Pong(_) => todo!(),
                ws::Message::Close(frame) => return handle_read_close(token, frame),
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
    token: usize,
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
