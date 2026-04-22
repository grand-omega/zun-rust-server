use std::net::SocketAddr;
use tracing_subscriber::{EnvFilter, fmt};
use zun_rust_server::router;

#[tokio::main]
async fn main() {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("zun_rust_server=info,tower_http=info")),
        )
        .init();

    let addr: SocketAddr = "127.0.0.1:8080".parse().expect("valid bind address");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind 127.0.0.1:8080");
    tracing::info!(%addr, "zun-rust-server listening");

    axum::serve(listener, router()).await.expect("server error");
}
