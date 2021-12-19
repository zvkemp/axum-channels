use axum::extract::ws::{self, CloseFrame, WebSocket};
use channel::MessageContext;
use futures::sink::SinkExt;
use futures::stream::{SplitSink, StreamExt};
use futures::Stream;
use message::{Event, MessageKind};
use registry::{RegistryMessage, RegistrySender};
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tracing::{debug, error, warn};
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

// pub mod examples;
#[cfg(test)]
pub mod tests;

// FIXME: this doesn't provide much utility
pub struct Conn {
    format: ConnFormat,
    mailbox_tx: UnboundedSender<Message>,
    mailbox_rx: UnboundedReceiver<Message>,
}

#[derive(Debug, Clone, Copy)]
pub enum ConnFormat {
    Phoenix,
}

fn get_token() -> Token {
    static COUNTER: AtomicUsize = AtomicUsize::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed).into()
}

pub async fn handle_connect(socket: WebSocket, format: ConnFormat, registry: RegistrySender) {
    let token = get_token();

    // the raw websocket stream
    let (writer, reader) = socket.split();

    // This channel receiver is consumed by the websocket writer;
    // `sender` sends messages to be serialized and written into the websocket.
    let (sender, receiver) = unbounded_channel();

    // the `mailbox` is consumed in the socket reader function; raw data from the websocket
    // is parsed and then sent to this channel. Additionally, clones of the mailbox_tx can
    // be attached to messages to allow for responses to be send directly to the socket task for further processing.
    let (mailbox_tx, mailbox_rx) = unbounded_channel();

    let conn = Conn {
        mailbox_tx: mailbox_tx.clone(),
        mailbox_rx,
        format,
    };

    spawn_named(
        write(token, format, writer, receiver, mailbox_tx),
        &format!("socket_writer:{}", token),
    );
    spawn_named(
        read(token, conn, reader, sender, registry),
        &format!("socket_reader:{}", token),
    );
}

// A set of senders pointing to the subscribed channels.
// Avoids a central message bus, instad allows sockets to more-or-less directly send messages
// to the intended channels.
#[derive(Debug)]
pub struct ReaderSubscriptions {
    channels: HashMap<ChannelId, UnboundedSender<MessageContext>>,
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

    pub fn insert(&mut self, channel_id: ChannelId, sender: UnboundedSender<MessageContext>) {
        self.channels.entry(channel_id).or_insert(sender);
    }
}

impl Drop for ReaderSubscriptions {
    fn drop(&mut self) {
        debug!("dropping ReaderSubscriptions, {:?}", self);
        for (channel_id, sender) in &self.channels {
            let _ = sender.send(
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

fn parse_message<'a>(
    message: &'a str,
    format: &ConnFormat,
) -> Result<Message, Box<dyn std::error::Error + Send + Sync + 'a>> {
    match *format {
        ConnFormat::Phoenix => PhoenixParser::from_str(message).map_err(Into::into),
    }
}

struct PhoenixParser;

impl PhoenixParser {
    pub fn from_str(input: &str) -> Result<Message, ParseError> {
        let value: serde_json::Value = serde_json::from_str(input).unwrap();

        let join_ref = value[0].as_str().map(Into::into);
        let msg_ref = value[1].as_str().map(Into::into);

        let channel_id: ChannelId = value[2].as_str().unwrap().parse().unwrap();
        let event: Event = value[3].as_str().unwrap().into();
        let payload = value[4].to_owned();

        let kind = match event.as_ref() {
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

fn handle_close(mailbox_tx: UnboundedSender<Message>, _close_frame: Option<CloseFrame>) {
    if let Err(e) = mailbox_tx.send(Message {
        kind: MessageKind::Closed,
        channel_id: "_closed".parse().unwrap(),
        msg_ref: None,
        join_ref: None,
        payload: serde_json::Value::Null,
        event: "closed".into(),
        channel_sender: None,
    }) {
        error!("error encountered closing socket; err={:?}", e);
    }
}

// reading data from remote
// FIXME: continue genericizing this
async fn read<S: Stream<Item = Result<ws::Message, axum::Error>> + Unpin + Send + 'static>(
    token: Token,
    mut conn: Conn,
    mut ws_receiver: S,
    reply_sender: UnboundedSender<MessageReply>,
    registry_sender: RegistrySender,
) {
    let mut subscriptions = ReaderSubscriptions::new(token, conn.mailbox_tx.clone());

    let format = conn.format;
    let mailbox_tx = conn.mailbox_tx.clone();
    let ws_reply_sender = reply_sender.clone(); // directly send a response to the ws writer

    // this task maps ws::Message to Message and sends them to mailbox_tx
    let _ws_handle = spawn_named(
        async move {
            while let Some(msg) = ws_receiver.next().await {
                match msg {
                    Ok(inner) => match inner {
                        ws::Message::Text(inner) => {
                            // FIXME: eep
                            let msg: Result<Message, _> = parse_message(&inner, &format);

                            match msg {
                                Ok(msg) => mailbox_tx.send(msg).unwrap(),
                                Err(e) => {
                                    error!("message could not be parsed; err={:?}", e);
                                }
                            }
                        }
                        ws::Message::Binary(_) => todo!(),
                        ws::Message::Ping(data) => {
                            ws_reply_sender.send(MessageReply::Pong(data)).unwrap()
                        } // FIXME unwrap
                        ws::Message::Pong(_) => todo!(),
                        ws::Message::Close(frame) => {
                            return handle_close(mailbox_tx, frame);
                        }
                    },
                    Err(e) => {
                        error!("unexpected error in websocket reader; err={:?}", e);
                        return handle_close(mailbox_tx, None);
                    }
                }
            }

            warn!("websocket mapper task shutting down");
        },
        &format!("websocket_mapper:{}", token),
    );

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

                registry_sender.send(RegistryMessage::JoinRequest {
                    token,
                    channel_id: msg.channel_id,
                    mailbox_tx: conn.mailbox_tx.clone(),
                    reply_sender: reply_sender.clone(),
                    msg_ref: msg.msg_ref.unwrap(),
                    payload: msg.payload,
                });
            }
            MessageKind::DidJoin => {
                debug!("received join confirmation");
                subscriptions
                    .channels
                    .entry(msg.channel_id.clone())
                    .or_insert_with(|| msg.channel_sender.unwrap());

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

            MessageKind::BroadcastPresence => {
                todo!()
            }

            MessageKind::PresenceChange => {
                todo!()
            }

            MessageKind::Event | MessageKind::BroadcastIntercept => {
                if let Some(tx) = subscriptions.channels.get(&msg.channel_id) {
                    let _ = tx.send(msg.decorate(token, conn.mailbox_tx.clone()));
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

            MessageKind::Closed => {
                return;
            }
        }
    }
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

// FIXME: how to genericize the writer?
async fn write(
    _token: Token,
    _format: ConnFormat,
    mut writer: SplitSink<WebSocket, ws::Message>,
    mut receiver: UnboundedReceiver<MessageReply>,
    mailbox_tx: UnboundedSender<Message>,
) {
    while let Some(msg) = receiver.recv().await {
        let ws_msg: ws::Message = msg.into();

        debug!("[write] msg = {:?}", ws_msg);
        match writer.send(ws_msg).await {
            Ok(..) => {}
            Err(e) => {
                error!("error in socket writer; e={:?}", e);
                return handle_close(mailbox_tx, None);
            }
        }
    }
}

#[track_caller]
pub(crate) fn spawn_named<T>(
    task: impl std::future::Future<Output = T> + Send + 'static,
    _name: &str,
) -> tokio::task::JoinHandle<T>
where
    T: Send + 'static,
{
    #[cfg(tokio_unstable)]
    return tokio::task::Builder::new().name(_name).spawn(task);

    #[cfg(not(tokio_unstable))]
    tokio::spawn(task)
}
