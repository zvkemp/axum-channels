use axum::{
    extract::{Extension, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    AddExtensionLayer, Router,
};
use axum_channels::{
    registry::{Registry, RegistrySender},
    ConnFormat,
};
use tracing::debug;

#[tokio::main]
async fn main() {
    let registry = Registry::default();
    let (registry_sender, _handle) = registry.start();

    let app = Router::new()
        .route("/ws", get(handler))
        .layer(AddExtensionLayer::new(registry_sender));

    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn handler(
    ws: WebSocketUpgrade,
    Extension(registry): Extension<RegistrySender>,
) -> impl IntoResponse {
    debug!("handler");
    ws.on_upgrade(move |socket| {
        axum_channels::handle_connect(socket, ConnFormat::Phoenix, registry)
    })
}
