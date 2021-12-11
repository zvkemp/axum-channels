use axum::{
    extract::{Extension, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    AddExtensionLayer, Router,
};
use futures::{SinkExt, StreamExt};
use std::{
    marker::PhantomData,
    net::{SocketAddr, TcpListener},
    sync::{Arc, Mutex},
};
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;

use crate::{
    channel::{ChannelBehavior, Presence},
    message::{Message, MessageKind},
    registry::Registry,
    types::ChannelId,
    ConnFormat,
};

#[derive(Debug, Clone)]
struct DefaultChannel;

impl ChannelBehavior for DefaultChannel {
    fn handle_presence(
        &mut self,
        channel_id: &ChannelId,
        presence: &Presence,
    ) -> crate::channel::Result<Option<Message>> {
        Ok(Some(Message {
            kind: MessageKind::Broadcast,
            channel_id: channel_id.clone(),
            msg_ref: None,
            join_ref: None,
            payload: serde_json::json!({ "presence": presence.data }),
            event: "presence".into(),
            channel_sender: None,
        }))
    }
}

fn run_server() -> (SocketAddr, JoinHandle<()>, Arc<Mutex<Registry>>) {
    let registry = Arc::new(Mutex::new(Registry::default()));
    let mut locked = registry.lock().unwrap();

    locked.add_channel("default".parse().unwrap(), Box::new(DefaultChannel));

    drop(locked);

    let app = Router::new()
        .route("/ws", get(handler))
        .layer(AddExtensionLayer::new(registry.clone()));

    let listener = TcpListener::bind("0.0.0.0:0".parse::<SocketAddr>().unwrap()).unwrap();
    let socket_addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        axum::Server::from_tcp(listener)
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap();
    });

    (socket_addr, handle, registry)
}

async fn handler(
    ws: WebSocketUpgrade,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| {
        crate::handle_connect(socket, ConnFormat::Phoenix, registry.clone())
    })
}

#[tokio::test]
async fn test_presence_join_leave() {
    // FIXME: instead of testing everything via external effects, see if we can subscribe to a channel
    // without a real websocket.

    // tracing_subscriber::fmt::init();
    let (address, _server_handle, registry) = run_server();

    let address = format!("ws://{}/ws", address);
    let (ws_stream, _) = connect_async(&address).await.expect("Failed to connect");
    let (mut write, mut read) = ws_stream.split();

    write
        .send(r#"["1", "1-join", "default", "phx_join", {"arg": "foo1"}]"#.into())
        .await
        .unwrap();

    let (ws_stream2, _) = connect_async(&address).await.expect("Failed to connect");
    let (mut write2, mut read2) = ws_stream2.split();

    write2
        .send(r#"["2", "2-join", "default", "phx_join", {"arg": "foo2"}]"#.into())
        .await
        .unwrap();

    let _presence1 = read.next().await;
    let _join_confirm = read.next().await;
    let presence2 = read.next().await;

    let decoded2: serde_json::Value =
        serde_json::from_str(presence2.unwrap().unwrap().to_string().as_str()).unwrap();

    assert_eq!(
        decoded2
            .get(4)
            .and_then(|v| v.get("presence"))
            .and_then(|v| v.get("2"))
            .and_then(|v| v.get("arg"))
            .and_then(|v| v.as_str()),
        Some("foo2")
    );

    drop(read2);
    drop(write2);

    let presence3 = read.next().await;

    let decoded3: serde_json::Value =
        serde_json::from_str(presence3.unwrap().unwrap().to_string().as_str()).unwrap();

    assert!(decoded3
        .get(4)
        .and_then(|v| v.get("presence"))
        .and_then(|v| v.get("2"))
        .is_none());
}

struct PhantomDefault<T>(PhantomData<T>);

fn default_from_phantom<T: Default>(template: PhantomDefault<T>) -> T {
    Default::default()
}

// FIXME: this is a better constraint for channel templating; clone doesn't make a lot of sense
// and requires a lot of irritating boxing nonsense
#[test]
fn test_phantom_default() {
    let phantom: PhantomDefault<Vec<String>> = PhantomDefault(PhantomData);

    let mut v = default_from_phantom(phantom);

    v.push("foo".into());
}
