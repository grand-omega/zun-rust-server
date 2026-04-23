use std::sync::Arc;
use tokio::sync::mpsc;
use tracing_subscriber::{EnvFilter, fmt};
use zun_rust_server::{
    AppState, Config, comfy::ComfyClient, db, prompts, router, worker, workflow,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("zun_rust_server=info,tower_http=info")),
        )
        .init();

    let config = Config::from_env()?;
    tracing::info!(
        data_dir = %config.data_dir.display(),
        bind = %config.bind_addr,
        comfy = %config.comfy_url,
        "starting"
    );

    let pool = db::init(&config.data_dir).await?;

    let prompts_path = config.data_dir.join("prompts.yaml");
    let prompts = prompts::load(&prompts_path)?;
    tracing::info!(n = prompts.len(), path = %prompts_path.display(), "prompts loaded");

    let workflows_dir = config.data_dir.join("workflows");
    let workflows = workflow::load_templates(&workflows_dir)?;
    tracing::info!(
        n = workflows.len(),
        dir = %workflows_dir.display(),
        "workflow templates loaded"
    );

    let comfy = ComfyClient::new(&config.comfy_url)?;
    let (worker_tx, worker_rx) = mpsc::channel::<()>(1);

    let state = AppState {
        db: pool,
        config: config.clone(),
        prompts: Arc::new(prompts),
        workflows: Arc::new(workflows),
        comfy,
        worker_tx,
    };

    // Spawn the worker first so it can pick up any already-queued jobs
    // before we start accepting new submissions.
    worker::spawn(state.clone(), worker_rx);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "zun-rust-server listening");

    axum::serve(listener, router(state)).await?;
    Ok(())
}
