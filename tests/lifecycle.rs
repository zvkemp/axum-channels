use axum::{
    extract::{Extension, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    AddExtensionLayer, Router,
};
use axum_channels::{registry::Registry, ConnFormat};
use futures::{SinkExt, StreamExt};
use std::{
    net::SocketAddr,
    net::TcpListener,
    sync::{Arc, Mutex},
};
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tungstenite::protocol::Message as TgMessage;

#[tokio::test]
async fn test_websocket_lifecycle() {
    let (address, server_handle) = run_server();

    let address = format!("ws://{}/ws", address);
    let (ws_stream, _) = connect_async(&address).await.expect("Failed to connect");
    let (mut write, mut read) = ws_stream.split();

    write.send("join default".into()).await.unwrap();

    let msg1_1 = read.next().await.unwrap().unwrap();

    assert_eq!(
        msg1_1,
        TgMessage::Text("socket Token(1) joined the channel. Welcome!".to_string())
    );

    let (ws_stream2, _) = connect_async(&address).await.expect("Failed to connect");
    let (mut write2, mut read2) = ws_stream2.split();

    write2.send("join default".into()).await.unwrap();

    let msg1_2 = read.next().await.unwrap().unwrap();
    let msg2_1 = read2.next().await.unwrap().unwrap();

    assert_eq!(
        msg1_2,
        TgMessage::Text("socket Token(2) joined the channel. Welcome!".to_string())
    );

    assert_eq!(msg1_2, msg2_1);

    write2
        .send("msg default | hello world".into())
        .await
        .unwrap();

    let msg1_3 = read.next().await.unwrap().unwrap();
    let msg2_2 = read2.next().await.unwrap().unwrap();

    assert_eq!(msg1_3, TgMessage::Text("[Token(2)]  hello world".into()));
    assert_eq!(msg1_3, msg2_2);
}

fn run_server() -> (SocketAddr, JoinHandle<()>) {
    let registry = Arc::new(Mutex::new(Registry::default()));
    let app = Router::new()
        .route("/ws", get(simple_handler))
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

async fn simple_handler(
    ws: WebSocketUpgrade,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    println!("simple_handler");
    ws.on_upgrade(move |socket| {
        axum_channels::handle_connect(socket, ConnFormat::Simple, registry.clone())
    })
}
