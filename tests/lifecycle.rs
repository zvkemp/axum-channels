use axum::{
    extract::{Extension, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    AddExtensionLayer, Router,
};
use axum_channels::{
    channel::ChannelBehavior,
    message::{DecoratedMessage, Message},
    registry::Registry,
    ConnFormat,
};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::{
    net::SocketAddr,
    net::TcpListener,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{task::JoinHandle, time::sleep};
use tokio_tungstenite::connect_async;
use tracing::{debug, info};
use tungstenite::protocol::Message as TgMessage;

#[tokio::test]
async fn test_websocket_lifecycle() {
    std::env::set_var("RUST_LOG", "debug");
    tracing_subscriber::fmt::init();

    println!("RUST_LOG={:?}", std::env::var("RUST_LOG"));

    info!("hello");
    debug!("hello");
    let (address, _server_handle) = run_server();

    let address = format!("ws://{}/ws", address);
    let (ws_stream, _) = connect_async(&address).await.expect("Failed to connect");
    let (mut write, mut read) = ws_stream.split();

    write
        .send(r#"["1", "1", "default:*", "phx_join", {}]"#.into())
        .await
        .unwrap();

    let msg1_1 = read.next().await.unwrap().unwrap();

    assert_eq!(
        msg1_1,
        TgMessage::Text(
            json!([null, "1", "default:*", "phx_reply", {"response": {}, "status": "ok"}])
                .to_string()
        )
    );

    let (ws_stream2, _) = connect_async(&address).await.expect("Failed to connect");
    let (mut write2, mut read2) = ws_stream2.split();

    write2
        .send(r#"["2", "2", "default:*", "phx_join", {}]"#.into())
        .await
        .unwrap();

    let msg2_1 = read2.next().await.unwrap().unwrap();

    assert_eq!(
        msg2_1,
        TgMessage::Text(
            json!([null, "2", "default:*", "phx_reply", {"response": {}, "status": "ok"}])
                .to_string()
        )
    );

    write2
        .send(r#"["1", "1", "default:*", "msg", { "text": "hello world"}]"#.into())
        .await
        .unwrap();

    let msg1_2 = read.next().await.unwrap().unwrap();
    let msg2_2 = read2.next().await.unwrap().unwrap();

    assert_eq!(
        msg1_2,
        TgMessage::Text(r#"[null,null,"default:*","msg",{"text":"hello world"}]"#.into())
    );
    assert_eq!(msg1_2, msg2_2);
}

fn run_server() -> (SocketAddr, JoinHandle<()>) {
    let registry = Arc::new(Mutex::new(Registry::default()));
    let mut locked = registry.lock().unwrap();

    locked.add_channel("default:*".parse().unwrap(), Box::new(DefaultChannel));

    drop(locked);

    let app = Router::new()
        .route("/ws", get(handler))
        .layer(AddExtensionLayer::new(registry));

    let listener = TcpListener::bind("0.0.0.0:0".parse::<SocketAddr>().unwrap()).unwrap();
    let socket_addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        axum::Server::from_tcp(listener)
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap();
    });

    (socket_addr, handle)
}

async fn handler(
    ws: WebSocketUpgrade,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| {
        axum_channels::handle_connect(socket, ConnFormat::Message, registry.clone())
    })
}

#[derive(Debug, Clone)]
struct DefaultChannel;

impl ChannelBehavior for DefaultChannel {
    fn handle_message(&mut self, message: &DecoratedMessage) -> Option<Message> {
        match &message.inner {
            Message::Event { event, payload, .. } => Some(Message::Broadcast {
                event: event.to_string(),
                payload: payload.clone(),
                channel_id: message.channel_id().clone(),
            }),
            _ => None,
        }
    }

    fn handle_join(
        &mut self,
        _message: &DecoratedMessage,
    ) -> Result<(), axum_channels::channel::JoinError> {
        Ok(())
    }
}
