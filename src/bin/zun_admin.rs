//! Admin CLI for one-off ops on the v2 store.
//!
//! Subcommands:
//!   seed-prompts <username> --from <toml>   Insert prompt rows for a user.
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
    /// Insert prompt rows from a TOML file into a user's prompt catalog.
    SeedPrompts {
        /// Target username (e.g. "admin").
        username: String,
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
    /// User management.
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
}

#[derive(Subcommand)]
enum UserCmd {
    /// List all users (including disabled).
    List,
    /// Create a new user.
    Create {
        username: String,
        /// Display name. Defaults to the username.
        #[arg(long)]
        display_name: Option<String>,
    },
    /// Disable a user (soft). Auth as that user fails; rows preserved.
    Disable { username: String },
    /// Re-enable a previously disabled user.
    Enable { username: String },
    /// Permanently delete a user and ALL their data (inputs, prompts, jobs).
    /// FK cascades remove DB rows; per-user files under data/users/<id>/
    /// are removed too if --cleanup-files is set.
    Delete {
        username: String,
        #[arg(long)]
        cleanup_files: bool,
        /// Required to actually delete; otherwise this is a dry-run preview.
        #[arg(long)]
        confirm: bool,
    },
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
        Cmd::SeedPrompts { username, from } => {
            let pool = open_pool(&config.data_dir).await?;
            seed_prompts(&pool, &config, &username, &from).await
        }
        Cmd::Purge { dry_run } => {
            let pool = open_pool(&config.data_dir).await?;
            purge(&pool, &config, dry_run).await
        }
        Cmd::User { cmd } => {
            let pool = open_pool(&config.data_dir).await?;
            user(&pool, &config, cmd).await
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
    username: &str,
    from: &std::path::Path,
) -> anyhow::Result<()> {
    let user_id: Option<i64> = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(pool)
        .await?;
    let user_id = user_id.ok_or_else(|| anyhow::anyhow!("user '{username}' not found"))?;

    let raw = std::fs::read_to_string(from)?;
    let parsed: PromptsFile = toml::from_str(&raw)?;
    let registry = zun_rust_server::workflow::load_registry(&config.resolved_workflows_dir())?;
    let now = chrono::Utc::now().timestamp();

    let mut inserted = 0usize;
    for p in parsed.prompts {
        registry
            .supports(&p.workflow)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        sqlx::query(
            "INSERT INTO custom_prompts \
             (user_id, label, description, text, workflow, timeout_seconds, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(user_id)
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
    println!("seeded {inserted} prompts into user_id={user_id} ({username})");
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
    let registry = zun_rust_server::workflow::load_registry(&workflows_dir)?;
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

async fn user(
    pool: &SqlitePool,
    config: &zun_rust_server::Config,
    cmd: UserCmd,
) -> anyhow::Result<()> {
    match cmd {
        UserCmd::List => {
            let rows: Vec<(i64, String, String, i64, Option<i64>)> = sqlx::query_as(
                "SELECT id, username, display_name, created_at, disabled_at \
                 FROM users ORDER BY id ASC",
            )
            .fetch_all(pool)
            .await?;
            println!(
                "{:<4}  {:<20}  {:<20}  {:<12}  status",
                "id", "username", "display_name", "created_at"
            );
            for (id, username, display_name, created_at, disabled_at) in rows {
                let status = if disabled_at.is_some() {
                    "disabled"
                } else {
                    "active"
                };
                println!("{id:<4}  {username:<20}  {display_name:<20}  {created_at:<12}  {status}");
            }
        }
        UserCmd::Create {
            username,
            display_name,
        } => {
            let display_name = display_name.unwrap_or_else(|| username.clone());
            let now = chrono::Utc::now().timestamp();
            let res = sqlx::query(
                "INSERT INTO users (username, display_name, created_at) VALUES (?, ?, ?)",
            )
            .bind(&username)
            .bind(&display_name)
            .bind(now)
            .execute(pool)
            .await?;
            println!(
                "created user_id={} username={username}",
                res.last_insert_rowid()
            );
        }
        UserCmd::Disable { username } => {
            let now = chrono::Utc::now().timestamp();
            let res = sqlx::query(
                "UPDATE users SET disabled_at = ? WHERE username = ? AND disabled_at IS NULL",
            )
            .bind(now)
            .bind(&username)
            .execute(pool)
            .await?;
            if res.rows_affected() == 0 {
                anyhow::bail!("user not found or already disabled: {username}");
            }
            println!("disabled {username}");
        }
        UserCmd::Enable { username } => {
            let res = sqlx::query("UPDATE users SET disabled_at = NULL WHERE username = ?")
                .bind(&username)
                .execute(pool)
                .await?;
            if res.rows_affected() == 0 {
                anyhow::bail!("user not found: {username}");
            }
            println!("enabled {username}");
        }
        UserCmd::Delete {
            username,
            cleanup_files,
            confirm,
        } => {
            let row: Option<(i64,)> = sqlx::query_as("SELECT id FROM users WHERE username = ?")
                .bind(&username)
                .fetch_optional(pool)
                .await?;
            let user_id = row
                .ok_or_else(|| anyhow::anyhow!("user not found: {username}"))?
                .0;
            let counts: (i64, i64, i64) = sqlx::query_as(
                "SELECT \
                   (SELECT COUNT(*) FROM jobs           WHERE user_id = ?), \
                   (SELECT COUNT(*) FROM inputs         WHERE user_id = ?), \
                   (SELECT COUNT(*) FROM custom_prompts WHERE user_id = ?)",
            )
            .bind(user_id)
            .bind(user_id)
            .bind(user_id)
            .fetch_one(pool)
            .await?;
            let prefix = if confirm {
                ""
            } else {
                "DRY-RUN (re-run with --confirm to delete): "
            };
            println!(
                "{prefix}user_id={user_id} username={username} → would delete \
                 {} jobs, {} inputs, {} prompts",
                counts.0, counts.1, counts.2
            );
            if !confirm {
                return Ok(());
            }
            sqlx::query("DELETE FROM users WHERE id = ?")
                .bind(user_id)
                .execute(pool)
                .await?;
            if cleanup_files {
                let user_dir = config.data_dir.join("users").join(user_id.to_string());
                if let Err(e) = tokio::fs::remove_dir_all(&user_dir).await {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(path = %user_dir.display(), error = %e, "failed to remove user dir");
                    }
                } else {
                    println!("removed {}", user_dir.display());
                }
            }
            println!("deleted user {username}");
        }
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
