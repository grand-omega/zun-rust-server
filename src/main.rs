use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use zun_rust_server::{
    AppState, Config, comfy::ComfyClient, comfy_monitor, db, logging, prompts, router, worker,
    workflow,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    logging::init()?;

    let config = Config::from_env()?;
    tracing::info!(
        data_dir = %config.data_dir.display(),
        bind = %config.bind_addr,
        comfy = %config.comfy_url,
        "starting"
    );

    let pool = db::init(&config.data_dir).await?;

    let prompts = prompts::load(&config.prompts_path)?;
    tracing::info!(n = prompts.len(), path = %config.prompts_path.display(), "prompts loaded");

    let workflows_dir = config.data_dir.join("workflows");
    let workflows = workflow::load_templates(&workflows_dir)?;
    tracing::info!(
        n = workflows.len(),
        dir = %workflows_dir.display(),
        "workflow templates loaded"
    );

    let comfy = ComfyClient::new(&config.comfy_url)?;
    let comfy_health = comfy_monitor::new_handle();
    let (worker_tx, worker_rx) = mpsc::channel::<()>(1);

    let state = AppState {
        db: pool,
        config: config.clone(),
        prompts: Arc::new(prompts),
        workflows: Arc::new(workflows),
        comfy: comfy.clone(),
        comfy_health: comfy_health.clone(),
        worker_tx,
    };

    // Broadcast-once shutdown channel. Axum, the worker, and the comfy
    // monitor all subscribe. Signal handler flips it to true.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    comfy_monitor::spawn(comfy, comfy_health, shutdown_rx.clone());
    worker::spawn(state.clone(), worker_rx, shutdown_rx.clone());

    // Install signal handler. First SIGTERM or Ctrl+C triggers graceful
    // shutdown; a second signal after 30s would need a forced kill.
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        tracing::info!("shutdown signal received");
        let _ = shutdown_tx.send(true);
    });

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "zun-rust-server listening");

    let mut axum_shutdown_rx = shutdown_rx;
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            // Wait until the watched value flips.
            while !*axum_shutdown_rx.borrow() {
                if axum_shutdown_rx.changed().await.is_err() {
                    return;
                }
            }
            tracing::info!("server no longer accepting new connections; draining");
        })
        .await?;

    tracing::info!("zun-rust-server exited cleanly");
    Ok(())
}

/// Wait for Ctrl+C (SIGINT) or SIGTERM, whichever arrives first. On
/// non-unix platforms SIGTERM is unavailable and only Ctrl+C is handled.
async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %e, "ctrl_c handler failed");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "SIGTERM handler failed");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
