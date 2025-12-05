use axum::{
    Router,
    extract::{Extension, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
};
use axum_channels::{
    ConnFormat,
    registry::{Registry, RegistrySender},
};
use tracing::debug;

#[tokio::main]
async fn main() {
    let registry = Registry::default();
    let (registry_sender, _handle) = registry.start();

    let app = Router::new()
        .route("/ws", get(handler))
        .layer(Extension(registry_sender));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();

    axum::serve(listener, app.into_make_service())
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
