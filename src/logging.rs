//! Logging setup.
//!
//! Design for the current (single-user) scope, extendable later:
//!
//! - One global `tracing` subscriber with an env-filter, an fmt layer
//!   whose format (pretty vs JSON) is decided at startup.
//! - Default format: pretty on a TTY, JSON otherwise. Override with
//!   `ZUN_LOG_FORMAT=pretty|json`.
//! - Default filter: `zun_rust_server=info,tower_http=info,audit=info`.
//!   Upgrade either component via the usual `RUST_LOG` env var.
//! - Every handler/worker call already runs inside a `request`/`job`
//!   span (see src/lib.rs and src/worker.rs); those span fields appear
//!   on every emitted event automatically.
//!
//! ## Audit target
//!
//! User-visible lifecycle events live on the `audit` target with a
//! structured `event` field. Emit via:
//!
//! ```ignore
//! tracing::info!(
//!     target: "audit",
//!     event = "job.submitted",
//!     job_id = %id,
//!     prompt_id = %prompt_id,
//! );
//! ```
//!
//! When the service grows multi-user, the same call sites will double-
//! write to a durable `audit_events` DB table — no code changes at the
//! call sites, just a broader sink.
//!
//! ## Error chains
//!
//! When logging an `anyhow::Error`, prefer `error = ?e` (Debug formatter)
//! so the full cause chain is printed, not just the top message.

use std::io::IsTerminal;

use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::LogFormat;

pub const DEFAULT_FILTER: &str = "zun_rust_server=info,tower_http=info,audit=info";

/// Install the global subscriber. Call once from `main`.
pub fn init(format: LogFormat) -> anyhow::Result<()> {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));

    let resolved = match format {
        LogFormat::Pretty => LogFormat::Pretty,
        LogFormat::Json => LogFormat::Json,
        LogFormat::Auto => {
            if std::io::stderr().is_terminal() {
                LogFormat::Pretty
            } else {
                LogFormat::Json
            }
        }
    };

    let registry = tracing_subscriber::registry().with(env_filter);

    match resolved {
        LogFormat::Pretty => registry
            .with(fmt::layer().with_writer(std::io::stderr).with_target(true))
            .try_init()?,
        LogFormat::Json => registry
            .with(
                fmt::layer()
                    .json()
                    .with_writer(std::io::stderr)
                    .with_target(true)
                    .with_current_span(true)
                    .with_span_list(true),
            )
            .try_init()?,
        LogFormat::Auto => unreachable!(),
    }

    tracing::debug!(?resolved, "logging initialised");
    Ok(())
}
