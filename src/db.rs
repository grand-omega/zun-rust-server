use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use std::path::Path;
use std::time::Duration;

/// Initialise the SQLite pool, create the DB file if missing, apply PRAGMAs
/// per connection, run migrations, and seed the default admin user.
pub async fn init(data_dir: &Path) -> anyhow::Result<SqlitePool> {
    std::fs::create_dir_all(data_dir)?;
    let db_path = data_dir.join("jobs.db");

    let opts = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(opts)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;
    seed_default_user(&pool).await?;
    Ok(pool)
}

/// Idempotently seed user id=1 ("admin"). The single-user world only ever
/// references this row; multi-user is a future v3 concern.
async fn seed_default_user(pool: &SqlitePool) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT OR IGNORE INTO users (id, username, display_name, created_at) \
         VALUES (1, 'admin', 'admin', ?)",
    )
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}
