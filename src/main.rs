use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use zun_rust_server::{
    AppState, Config, auth::AuthLimiter, backup, comfy::ComfyClient, comfy_monitor, db,
    hash::sha256_hex, logging, purge, router, worker, workflow,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path = parse_config_path()?;
    let config = Config::load(&config_path)?;
    logging::init(config.log_format)?;
    // Identify the token by a sha256 prefix rather than dumping its bytes;
    // log files end up in journals, screenshots, and bug reports.
    let token_id = format!("sha256:{}", &sha256_hex(config.token.as_bytes())[..12]);
    tracing::info!(
        data_dir = %config.data_dir.display(),
        bind = %config.bind,
        comfy = %config.comfy_url,
        token = %token_id,
        "starting"
    );

    let pool = db::init(&config.data_dir).await?;

    let workflows = workflow::load_registry(
        config.workflows_dir.as_deref(),
        &config.enabled_workflows,
        &config.default_workflow,
    )?;
    let source = config
        .workflows_dir
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<embedded>".into());
    tracing::info!(
        n = workflows.templates.len(),
        supported = workflows.supported_count(),
        source = %source,
        "workflow templates loaded"
    );

    let comfy = ComfyClient::new(&config.comfy_url)?;
    let comfy_health = comfy_monitor::new_handle();
    let (worker_tx, worker_rx) = mpsc::channel::<()>(1);

    let state = AppState {
        db: pool,
        config: config.clone(),
        workflows: Arc::new(workflows),
        comfy: comfy.clone(),
        comfy_health: comfy_health.clone(),
        worker_tx,
        auth_limiter: AuthLimiter::new(),
        disk_usage_cache: Arc::new(parking_lot::Mutex::new(None)),
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    comfy_monitor::spawn(comfy, comfy_health, shutdown_rx.clone());
    worker::spawn(state.clone(), worker_rx, shutdown_rx.clone());
    purge::spawn(state.clone(), shutdown_rx.clone());
    backup::spawn(
        state.db.clone(),
        state.config.data_dir.clone(),
        shutdown_rx.clone(),
    );

    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        tracing::info!("shutdown signal received");
        let _ = shutdown_tx.send(true);
    });

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!(addr = %config.bind, "zun-rust-server listening");

    let mut axum_shutdown_rx = shutdown_rx;
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
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

/// Tiny hand-rolled flag parser. Looks for `--config <path>`; falls back
/// to `./config.toml`. Avoids pulling in clap for one knob.
fn parse_config_path() -> anyhow::Result<PathBuf> {
    let mut args = std::env::args().skip(1);
    let Some(arg) = args.next() else {
        return Ok(PathBuf::from("config.toml"));
    };
    match arg.as_str() {
        "--config" => {
            let path = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--config requires a path argument"))?;
            Ok(PathBuf::from(path))
        }
        other => anyhow::bail!("unknown argument: {other}"),
    }
}

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
