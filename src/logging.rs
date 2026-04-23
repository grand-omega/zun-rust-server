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

pub const DEFAULT_FILTER: &str = "zun_rust_server=info,tower_http=info,audit=info";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable multi-line records with ANSI colors.
    Pretty,
    /// One JSON object per line — for `jq`, journald, or any aggregator.
    Json,
}

impl LogFormat {
    fn from_env_or_tty() -> Self {
        match std::env::var("ZUN_LOG_FORMAT").ok().as_deref() {
            Some("json") => Self::Json,
            Some("pretty") => Self::Pretty,
            _ => {
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
pub fn init() -> anyhow::Result<()> {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let format = LogFormat::from_env_or_tty();

    let registry = tracing_subscriber::registry().with(env_filter);

    match format {
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
    }

    tracing::debug!(?format, filter = %std::env::var("RUST_LOG").unwrap_or_else(|_| DEFAULT_FILTER.to_string()), "logging initialised");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_format_env_override_to_json() {
        // SAFETY: tests in this module don't share env state with parallel
        // integration test binaries (they're in the unit-test binary).
        // Still, serialize via a unique value scope.
        // Use a closure to ensure we unset afterwards.
        let prev = std::env::var("ZUN_LOG_FORMAT").ok();
        unsafe {
            std::env::set_var("ZUN_LOG_FORMAT", "json");
        }
        assert_eq!(LogFormat::from_env_or_tty(), LogFormat::Json);
        unsafe {
            std::env::set_var("ZUN_LOG_FORMAT", "pretty");
        }
        assert_eq!(LogFormat::from_env_or_tty(), LogFormat::Pretty);
        unsafe {
            match prev {
                Some(v) => std::env::set_var("ZUN_LOG_FORMAT", v),
                None => std::env::remove_var("ZUN_LOG_FORMAT"),
            }
        }
    }
}
