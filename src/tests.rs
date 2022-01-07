use axum::{
    extract::{Extension, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    AddExtensionLayer, Router,
};
use futures::{Future, SinkExt, StreamExt};
use http::Extensions;
use std::{
    collections::{HashMap, HashSet},
    net::{SocketAddr, TcpListener},
    time::Duration,
};
use tokio::{
    task::JoinHandle,
    time::{error::Elapsed, timeout as tokio_timeout},
};
use tokio_tungstenite::connect_async;

use crate::{
    channel::{Channel, MessageContext, NewChannel, Presence},
    message::{self, Message, MessageKind},
    registry::{Registry, RegistrySender},
    types::{ChannelId, Token},
    ConnFormat,
};

#[derive(Debug, Default)]
struct DefaultChannel;

#[axum::async_trait]
impl Channel for DefaultChannel {
    async fn handle_presence(
        &mut self,
        channel_id: &ChannelId,
        presence: &Presence,
    ) -> crate::channel::Result<Option<Message>> {
        Ok(Some(Message {
            channel_id: channel_id.clone(),
            event: "presence".into(),
            payload: serde_json::json!({"presence":presence.data}),
            kind: MessageKind::Broadcast,
            msg_ref: None,
            join_ref: None,
            channel_sender: None,
        }))
    }
}

fn run_server(registry: Registry) -> (SocketAddr, JoinHandle<()>, RegistrySender) {
    let (registry_sender, _registry_handle) = registry.start();

    let app = Router::new()
        .route("/ws", get(handler))
        .layer(AddExtensionLayer::new(registry_sender.clone()));

    let listener = TcpListener::bind("0.0.0.0:0".parse::<SocketAddr>().unwrap()).unwrap();
    let socket_addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        axum::Server::from_tcp(listener)
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap();
    });

    (socket_addr, handle, registry_sender)
}

async fn handler(
    ws: WebSocketUpgrade,
    Extension(registry): Extension<RegistrySender>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| crate::handle_connect(socket, ConnFormat::Phoenix, registry))
}

#[tokio::test]
async fn test_presence_join_leave() {
    // FIXME: instead of testing everything via external effects, see if we can subscribe to a channel
    // without a real websocket.

    tracing_subscriber::fmt::init();
    let mut registry = Registry::default();
    registry.add_channel("default".into(), Box::new(DefaultChannel));

    let (address, _server_handle, _registry) = run_server(registry);

    let address = format!("ws://{}/ws", address);
    let (ws_stream, _) = connect_async(&address).await.expect("Failed to connect");
    let (mut write, mut read) = ws_stream.split();

    write
        .send(r#"["1", "1-join", "default", "phx_join", {"arg": "foo1"}]"#.into())
        .await
        .unwrap();

    let (ws_stream2, _) = connect_async(&address).await.expect("Failed to connect");
    let (mut write2, read2) = ws_stream2.split();

    write2
        .send(r#"["2", "2-join", "default", "phx_join", {"arg": "foo2"}]"#.into())
        .await
        .unwrap();

    let _presence1 = read.next().await;
    let _join_confirm = read.next().await;
    let presence2 = read.next().await;
    let decoded2: serde_json::Value =
        serde_json::from_str(presence2.unwrap().unwrap().to_string().as_str()).unwrap();

    let expected = ["foo1", "foo2"].into_iter().collect::<HashSet<&str>>();

    assert_eq!(
        decoded2
            .get(4)
            .and_then(|v| v.get("presence"))
            .and_then(|v| v.as_object())
            .unwrap()
            .iter()
            .map(|(_, obj)| obj.get("arg").unwrap().as_str().unwrap())
            .collect::<HashSet<&str>>(),
        expected
    );

    drop(read2);
    drop(write2);

    let presence3 = read.next().await;

    let decoded3: serde_json::Value =
        serde_json::from_str(presence3.unwrap().unwrap().to_string().as_str()).unwrap();

    let expected2 = ["foo1"].into_iter().collect::<HashSet<&str>>();

    assert_eq!(
        decoded3
            .get(4)
            .and_then(|v| v.get("presence"))
            .and_then(|v| v.as_object())
            .unwrap()
            .iter()
            .map(|(_, obj)| obj.get("arg").unwrap().as_str().unwrap())
            .collect::<HashSet<&str>>(),
        expected2
    );
}

#[derive(Debug)]
struct StatefulChannel {
    socket_state: HashMap<Token, Extensions>,
}

#[axum::async_trait]
impl Channel for StatefulChannel {
    async fn handle_join(
        &mut self,
        context: &MessageContext,
    ) -> crate::channel::Result<Option<Message>> {
        Ok(Some(context.build_push(
            None,
            "join_info".into(),
            serde_json::json!({"token": context.token }),
        )))
    }
}

impl StatefulChannel {
    fn new() -> Self {
        Self {
            socket_state: Default::default(),
        }
    }
}

// NewChannel can be manually implemented for structs that aren't Default
impl NewChannel for StatefulChannel {
    fn new_channel(&self, _channel_id: ChannelId) -> Box<dyn Channel> {
        Box::new(StatefulChannel::new())
    }
}

// NewChannel has a blanket implementation for Channel + Default
#[derive(Debug, Default)]
struct TemplateWithDefault;

impl Channel for TemplateWithDefault {}

#[tokio::test]
async fn test_channel_templates() {
    let mut registry = Registry::default();

    registry.register_template("stateful", StatefulChannel::new());
    registry.register_template("defaultable", TemplateWithDefault::default());

    let (address, _server_handle, _registry) = run_server(registry);

    let address = format!("ws://{}/ws", address);
    let (ws_stream, _) = connect_async(&address).await.expect("Failed to connect");
    let (mut write, mut read) = ws_stream.split();

    write
        .send(r#"["1", "1-join", "defaultable", "phx_join", {"arg": "foo1"}]"#.into())
        .await
        .unwrap();

    // FIXME: when failed join requests return an error, this test will fail.
    assert!(timeout(read.next()).await.is_err());

    write
        .send(r#"["1", "1-join", "stateful:inst_a", "phx_join", {"arg": "foo1"}]"#.into())
        .await
        .unwrap();

    let resp = timeout(read.next()).await.unwrap().unwrap().unwrap();
    let decoded: serde_json::Value = serde_json::from_str(resp.to_string().as_str()).unwrap();
    let status = decoded
        .get(4)
        .and_then(|payload| payload.get("status"))
        .and_then(|status| status.as_str());
    assert_eq!(status, Some("ok"));

    let resp = timeout(read.next()).await.unwrap().unwrap().unwrap();
    let decoded: serde_json::Value = serde_json::from_str(resp.to_string().as_str()).unwrap();
    let token = decoded.get(4).and_then(|payload| payload.get("token"));
    assert!(matches!(token, Some(serde_json::Value::Number(..))));
}

async fn timeout<T>(future: T) -> Result<T::Output, Elapsed>
where
    T: Future,
{
    tokio_timeout(Duration::from_millis(50), future).await
}
