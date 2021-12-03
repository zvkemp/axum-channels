use axum::{
    extract::{Extension, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    AddExtensionLayer, Router,
};
use axum_channels::{registry::Registry, ConnFormat};
use std::sync::{Arc, Mutex};
use tracing::debug;

#[tokio::main]
async fn main() {
    let registry = Arc::new(Mutex::new(Registry::default()));
    let app = Router::new()
        .route("/ws", get(json_handler))
        .route("/simple", get(simple_handler))
        .layer(AddExtensionLayer::new(registry));

    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn json_handler(
    ws: WebSocketUpgrade,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    debug!("handler");
    ws.on_upgrade(move |socket| {
        axum_channels::handle_connect(socket, ConnFormat::JSON, registry.clone())
    })
}

async fn simple_handler(
    ws: WebSocketUpgrade,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    debug!("simple_handler");
    ws.on_upgrade(move |socket| {
        axum_channels::handle_connect(socket, ConnFormat::Simple, registry.clone())
    })
}
