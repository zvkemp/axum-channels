use axum::{
    extract::{Extension, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    AddExtensionLayer, Router,
};
use futures::{SinkExt, StreamExt};
use std::net::{SocketAddr, TcpListener};
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;

use crate::{
    channel::{Channel, Presence},
    message::{self, Message, MessageKind},
    registry::{Registry, RegistrySender},
    types::ChannelId,
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
        Ok(Some(message::broadcast(
            channel_id.clone(),
            "presence".into(),
            serde_json::json!({ "presence": presence.data }),
        )))
    }
}

fn run_server() -> (SocketAddr, JoinHandle<()>, RegistrySender) {
    let mut registry = Registry::default();
    registry.add_channel("default".parse().unwrap(), Box::new(DefaultChannel));

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
    let (address, _server_handle, _registry) = run_server();

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
