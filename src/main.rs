use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::ws::{self, WebSocket, WebSocketUpgrade};
use axum::extract::Extension;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{AddExtensionLayer, Router};
use futures::sink::SinkExt;
use futures::stream::{SplitSink, SplitStream, StreamExt};
use message::DecoratedMessage;
use registry::Registry;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use types::Token;

use crate::message::{Message, MessageReply};

// TODO:
// - make this a lib
// - each socket gets a private channel; messages can be sent to that particular socket.
// - a socket can subscribe to any channel by name (join?)

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

pub struct ReaderSubscriptions {
    channels: HashMap<String, UnboundedSender<DecoratedMessage>>,
}

impl ReaderSubscriptions {
    pub fn new() -> Self {
        Self {
            channels: Default::default(),
        }
    }

    pub fn insert(&mut self, channel_id: String, sender: UnboundedSender<DecoratedMessage>) {
        self.channels.entry(channel_id).or_insert(sender);
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
    let mut subscriptions = ReaderSubscriptions::new();
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

                            let msg = Message::Join {
                                channel_id: "default".to_string(),
                            };
                            let serialized = serde_json::to_string(&msg);
                            eprintln!("{}", serialized.unwrap());
                        }
                        _ => todo!(),
                    }

                    // Ok(msg) => match msg {},
                    // Err(_) => todo!(),
                }
                ws::Message::Binary(_) => todo!(),
                ws::Message::Ping(_) => todo!(),
                ws::Message::Pong(_) => todo!(),
                ws::Message::Close(_) => todo!(),
            },
            Err(e) => {
                eprintln!("error={:?}", e);
            }
        }
    }
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
        println!("[write] msg = {:?}", msg);
        writer
            .send(ws::Message::Text(format!("{:?}", msg)))
            .await
            .unwrap();
    }
}
