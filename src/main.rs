use tracing_subscriber::{EnvFilter, fmt};
use zun_rust_server::{AppState, Config, db, router};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("zun_rust_server=info,tower_http=info")),
        )
        .init();

    let config = Config::from_env();
    tracing::info!(data_dir = %config.data_dir.display(), bind = %config.bind_addr, "starting");

    let pool = db::init(&config.data_dir).await?;
    let state = AppState {
        db: pool,
        config: config.clone(),
    };

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "zun-rust-server listening");

    axum::serve(listener, router(state)).await?;
    Ok(())
}
