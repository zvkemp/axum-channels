use axum::{
    extract::{Extension, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    AddExtensionLayer, Router,
};
use axum_channels::{registry::Registry, ConnFormat};
use tracing::debug;

#[tokio::main]
async fn main() {
    let registry = Registry::default();

    let app = Router::new()
        .route("/ws", get(handler))
        .layer(AddExtensionLayer::new(registry));

    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn handler(
    ws: WebSocketUpgrade,
    Extension(registry): Extension<Registry>,
) -> impl IntoResponse {
    debug!("handler");
    ws.on_upgrade(move |socket| {
        axum_channels::handle_connect(socket, ConnFormat::Phoenix, registry.clone())
    })
}
