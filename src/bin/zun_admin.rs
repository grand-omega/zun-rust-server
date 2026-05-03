//! Admin CLI for one-off ops on the v2 store.
//!
//! Subcommands:
//!   seed-prompts --from <toml>              Insert prompt rows.
//!   purge --dry-run                         Run/preview cache + soft-delete purge.
//!   check-comfy                              Check ComfyUI reachability + workflows.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde::Deserialize;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

#[derive(Parser)]
#[command(name = "zun-admin", about = "zun-rust-server admin CLI")]
struct Cli {
    /// Path to config.toml. Defaults to ./config.toml.
    #[arg(long, default_value = "config.toml")]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Insert prompt rows from a TOML file into the prompt catalog.
    SeedPrompts {
        /// Path to a TOML file in the same shape as the v1 prompts.toml.
        #[arg(long)]
        from: PathBuf,
    },
    /// Purge soft-deleted jobs and stale input cache files.
    Purge {
        /// If set, print what would be purged without modifying anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Check ComfyUI reachability and workflow support status.
    CheckComfy,
}

#[derive(Debug, Deserialize)]
struct PromptsFile {
    prompts: Vec<PromptSeed>,
}

#[derive(Debug, Deserialize)]
struct PromptSeed {
    label: String,
    #[serde(default)]
    description: Option<String>,
    text: String,
    workflow: String,
    #[serde(default)]
    timeout_seconds: Option<i64>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = zun_rust_server::Config::from_file(&cli.config)?;

    match cli.cmd {
        Cmd::CheckComfy => check_comfy(&config).await,
        Cmd::SeedPrompts { from } => {
            let pool = open_pool(&config.data_dir).await?;
            seed_prompts(&pool, &config, &from).await
        }
        Cmd::Purge { dry_run } => {
            let pool = open_pool(&config.data_dir).await?;
            purge(&pool, &config, dry_run).await
        }
    }
}

async fn open_pool(data_dir: &std::path::Path) -> anyhow::Result<SqlitePool> {
    let db_path = data_dir.join("jobs.db");
    let opts = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(false)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true);
    Ok(SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(opts)
        .await?)
}

async fn seed_prompts(
    pool: &SqlitePool,
    config: &zun_rust_server::Config,
    from: &std::path::Path,
) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(from)?;
    let parsed: PromptsFile = toml::from_str(&raw)?;
    let registry = zun_rust_server::workflow::load_registry(
        &config.resolved_workflows_dir(),
        &config.enabled_workflows,
        &config.default_workflow,
    )?;
    let now = chrono::Utc::now().timestamp();

    let mut inserted = 0usize;
    for p in parsed.prompts {
        registry
            .supports(&p.workflow)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        sqlx::query(
            "INSERT INTO custom_prompts \
             (label, description, text, workflow, timeout_seconds, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&p.label)
        .bind(&p.description)
        .bind(&p.text)
        .bind(&p.workflow)
        .bind(p.timeout_seconds)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await?;
        inserted += 1;
    }
    println!("seeded {inserted} prompts");
    Ok(())
}

async fn purge(
    pool: &SqlitePool,
    config: &zun_rust_server::Config,
    dry_run: bool,
) -> anyhow::Result<()> {
    use zun_rust_server::purge::PurgeOpts;
    let opts = PurgeOpts {
        dry_run,
        ..PurgeOpts::defaults_now()
    };
    let state = build_state(pool, config).await?;
    let report = zun_rust_server::purge::run(&state, opts).await?;
    let prefix = if dry_run { "dry-run: would " } else { "" };
    println!(
        "{prefix}hard-delete {} jobs ({} files); purge {} inputs ({} files)",
        report.jobs_hard_deleted,
        report.job_files_removed,
        report.inputs_purged,
        report.input_files_removed,
    );
    Ok(())
}

async fn check_comfy(config: &zun_rust_server::Config) -> anyhow::Result<()> {
    let workflows_dir = config.resolved_workflows_dir();
    let registry = zun_rust_server::workflow::load_registry(
        &workflows_dir,
        &config.enabled_workflows,
        &config.default_workflow,
    )?;
    println!(
        "workflows: {} loaded, {} supported ({})",
        registry.templates.len(),
        registry.supported_count(),
        workflows_dir.display()
    );
    for wf in registry.support_list() {
        let status = if wf.supported { "OK" } else { "DISABLED" };
        let reason = wf.reason.unwrap_or_else(|| "supported".into());
        println!("{status:<8} {:<32} {reason}", wf.name);
    }

    let comfy = zun_rust_server::comfy::ComfyClient::new(&config.comfy_url)?;
    match comfy.health().await {
        Ok(()) => println!("comfy: reachable ({})", config.comfy_url),
        Err(e) => println!("comfy: unreachable ({}) - {e}", config.comfy_url),
    }
    Ok(())
}

async fn build_state(
    pool: &SqlitePool,
    config: &zun_rust_server::Config,
) -> anyhow::Result<zun_rust_server::AppState> {
    use std::sync::Arc;
    let comfy = zun_rust_server::comfy::ComfyClient::new(&config.comfy_url)?;
    let (worker_tx, _worker_rx) = tokio::sync::mpsc::channel::<()>(1);
    Ok(zun_rust_server::AppState {
        db: pool.clone(),
        config: config.clone(),
        workflows: Arc::new(zun_rust_server::workflow::WorkflowRegistry::empty()),
        comfy,
        comfy_health: zun_rust_server::comfy_monitor::new_handle(),
        worker_tx,
        auth_limiter: zun_rust_server::auth::AuthLimiter::new(),
        disk_usage_cache: std::sync::Arc::new(parking_lot::Mutex::new(None)),
    })
}
