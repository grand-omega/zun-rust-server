//! Logging setup.
//!
//! Design for the current single-user scope:
//!
//! - One global `tracing` subscriber with an env-filter, an fmt layer
//!   whose format (pretty vs JSON) is decided at startup.
//! - Default format: pretty on a TTY, JSON otherwise. Override with
//!   `log_format = "pretty" | "json"` in `config.toml` — there is no
//!   environment variable for the format.
//! - Default filter: `zun_rust_server=info,tower_http=info,audit=info`.
//!   Upgrade either component via the usual `RUST_LOG` env var.
//! - HTTP events sit inside the `request` span that `tower_http`'s
//!   `TraceLayer` opens per request. The worker has no span of its own:
//!   its events carry `job_id` as an explicit field on each call site.
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
//! These same call sites serve as a local audit trail for the operator.
//!
//! ## Error chains
//!
//! When logging an `anyhow::Error`, prefer `error = ?e` (Debug formatter)
//! so the full cause chain is printed, not just the top message.

use std::io::IsTerminal;

use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::LogFormat;

pub const DEFAULT_FILTER: &str = "zun_rust_server=info,tower_http=info,audit=info";

/// [`LogFormat`] with `Auto` already resolved against the TTY check. Having
/// no `Auto` variant is what keeps the format match below total — the older
/// shape needed an `unreachable!()` arm that a refactor could have turned
/// into a process abort.
#[derive(Debug, Clone, Copy)]
enum Resolved {
    Pretty,
    Json,
}

impl From<LogFormat> for Resolved {
    fn from(format: LogFormat) -> Self {
        match format {
            LogFormat::Pretty => Self::Pretty,
            LogFormat::Json => Self::Json,
            LogFormat::Auto => {
                if std::io::stderr().is_terminal() {
                    Self::Pretty
                } else {
                    Self::Json
                }
            }
        }
    }
}

/// Install the global subscriber. Call once from `main`.
pub fn init(format: LogFormat) -> anyhow::Result<()> {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));

    let resolved = Resolved::from(format);

    let registry = tracing_subscriber::registry().with(env_filter);

    match resolved {
        Resolved::Pretty => registry
            .with(fmt::layer().with_writer(std::io::stderr).with_target(true))
            .try_init()?,
        Resolved::Json => registry
            .with(
                fmt::layer()
                    .json()
                    .with_writer(std::io::stderr)
                    .with_target(true)
                    .with_current_span(true)
                    .with_span_list(true),
            )
            .try_init()?,
    }

    tracing::debug!(?resolved, "logging initialised");
    Ok(())
}
