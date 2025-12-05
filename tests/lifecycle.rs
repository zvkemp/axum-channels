use axum::{
    Router,
    extract::{Extension, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
};
use axum_channels::{
    ConnFormat,
    channel::{Channel, ChannelFuture, MessageContext},
    message::{Message, MessageKind},
    registry::{Registry, RegistrySender},
};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::{
    future::ready,
    net::{SocketAddr, TcpListener},
    pin::Pin,
};
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tracing::{debug, info};
use tungstenite::protocol::Message as TgMessage;

#[tokio::test]
async fn test_websocket_lifecycle() {
    unsafe { std::env::set_var("RUST_LOG", "debug") };
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
                .into()
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
                .into()
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
    let mut registry = Registry::default();

    registry.add_channel("default:*".into(), Box::new(DefaultChannel));
    let (registry_sender, _registry_handle) = registry.start();

    let app = Router::new()
        .route("/ws", get(handler))
        .layer(Extension(registry_sender));

    let listener = TcpListener::bind("0.0.0.0:0".parse::<SocketAddr>().unwrap()).unwrap();
    let socket_addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        let async_listener = tokio::net::TcpListener::from_std(listener).unwrap();
        axum::serve(async_listener, app.into_make_service())
            .await
            .unwrap();
    });

    (socket_addr, handle)
}

async fn handler(
    ws: WebSocketUpgrade,
    Extension(registry): Extension<RegistrySender>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| {
        axum_channels::handle_connect(socket, ConnFormat::Phoenix, registry)
    })
}

#[derive(Debug, Clone)]
struct DefaultChannel;

impl Channel for DefaultChannel {
    fn handle_message<'a>(
        &'a mut self,
        message: &'a MessageContext,
    ) -> ChannelFuture<'a, Option<Message>> {
        Box::pin(ready(match &message.inner.kind {
            MessageKind::Event => Some(Message {
                msg_ref: None,
                join_ref: None,
                kind: MessageKind::Broadcast,
                event: message.inner.event.clone(),
                payload: message.inner.payload.clone(),
                channel_id: message.channel_id().clone(),
                channel_sender: None,
            }),
            _ => None,
        }))
    }

    fn handle_join(
        &mut self,
        _message: &MessageContext,
    ) -> Pin<Box<dyn Future<Output = axum_channels::channel::Result<Option<Message>>> + Send>> {
        Box::pin(ready(Ok(None)))
    }
}
