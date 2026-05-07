use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
};
use parking_lot::Mutex;
use subtle::ConstantTimeEq;

use crate::{AppError, AppState};

/// Sliding-window rate limit on failed auth attempts per remote IP.
/// After `MAX_FAILURES` failures within `WINDOW`, the IP is blocked until
/// the window elapses without new failures.
const WINDOW: Duration = Duration::from_secs(60);
const MAX_FAILURES: u32 = 10;

#[derive(Debug, Default)]
struct FailState {
    /// Count of failures inside the current window.
    failures: u32,
    /// When the current window started (first failure).
    window_start: Option<Instant>,
}

impl FailState {
    /// Returns true if this IP is currently rate-limited (>= MAX_FAILURES
    /// within WINDOW). Also rolls the window forward if it's expired.
    fn is_blocked(&mut self, now: Instant) -> bool {
        if let Some(start) = self.window_start
            && now.duration_since(start) >= WINDOW
        {
            // Window expired: reset.
            self.failures = 0;
            self.window_start = None;
        }
        self.failures >= MAX_FAILURES
    }

    fn record_failure(&mut self, now: Instant) {
        if self.window_start.is_none() {
            self.window_start = Some(now);
        }
        self.failures = self.failures.saturating_add(1);
    }
}

/// Shared state for the per-IP auth rate limiter.
#[derive(Clone, Default)]
pub struct AuthLimiter {
    inner: Arc<Mutex<HashMap<IpAddr, FailState>>>,
}

impl AuthLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_blocked(&self, ip: IpAddr) -> bool {
        // Use get_mut, NOT entry().or_default(): every authenticated request
        // would otherwise insert an empty row, growing the map without bound
        // in steady state. An IP we've never seen is by definition not
        // blocked, so absence-as-false is correct.
        let mut guard = self.inner.lock();
        match guard.get_mut(&ip) {
            Some(state) => state.is_blocked(Instant::now()),
            None => false,
        }
    }

    fn record_failure(&self, ip: IpAddr) -> u32 {
        let mut guard = self.inner.lock();
        let now = Instant::now();
        // Bound the map by recent activity rather than all-time IPs: drop
        // entries whose window has already expired. Cheap because the map is
        // only as large as the set of IPs failing within the last WINDOW.
        guard.retain(|_, state| match state.window_start {
            Some(start) => now.duration_since(start) < WINDOW,
            None => false,
        });
        let entry = guard.entry(ip).or_default();
        entry.record_failure(now);
        entry.failures
    }
}

/// Constant-time bytes comparison to avoid trivial timing side channels.
fn token_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn peer_ip(req: &Request) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
}

/// Rejects any request missing or presenting a non-matching
/// `Authorization: Bearer <token>` header. After repeated failures from
/// the same peer IP, returns 429 for the remainder of the sliding window.
pub async fn require_bearer(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let path = req.uri().path().to_string();
    let ip = peer_ip(&req);

    // Fast path: if this IP is already blocked, short-circuit before
    // reading the header at all.
    if let Some(ip) = ip
        && state.auth_limiter.check_blocked(ip)
    {
        tracing::warn!(
            target: "audit",
            event = "auth.rate_limited",
            %ip,
            %path,
        );
        return Err(AppError::TooManyRequests);
    }

    let presented = match req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
    {
        Some(t) => t,
        None => {
            record_failure(&state, ip, &path, "missing_or_malformed_header");
            return Err(AppError::Unauthorized);
        }
    };

    if !token_eq(presented, &state.config.token) {
        record_failure(&state, ip, &path, "token_mismatch");
        return Err(AppError::Unauthorized);
    }

    // Authenticated. This is a single-user server, so downstream handlers
    // do not need any per-request identity.
    Ok(next.run(req).await)
}

fn record_failure(state: &AppState, ip: Option<IpAddr>, path: &str, reason: &'static str) {
    let failures = ip.map(|ip| state.auth_limiter.record_failure(ip));
    tracing::warn!(
        target: "audit",
        event = "auth.denied",
        reason,
        path = %path,
        ip = ?ip,
        failures = ?failures,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn limiter_blocks_after_max_failures_and_unblocks_after_window() {
        let limiter = AuthLimiter::new();
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        // Under the threshold: never blocked.
        for _ in 0..(MAX_FAILURES - 1) {
            assert!(!limiter.check_blocked(ip));
            limiter.record_failure(ip);
        }
        assert!(!limiter.check_blocked(ip));

        // Hit the threshold — now blocked.
        limiter.record_failure(ip);
        assert!(limiter.check_blocked(ip));

        // Forcibly age the window start so the sliding window elapses,
        // then the next check should clear and unblock.
        {
            let mut guard = limiter.inner.lock();
            let entry = guard.get_mut(&ip).unwrap();
            entry.window_start = Some(Instant::now() - (WINDOW + Duration::from_secs(1)));
        }
        assert!(!limiter.check_blocked(ip));
    }

    #[test]
    fn check_blocked_does_not_insert_for_unseen_ip() {
        // Authenticated requests funnel through check_blocked. If that path
        // inserts an entry, the map grows unbounded in steady state. The
        // sweep on record_failure only fires on failures, so without this
        // guarantee the bound regresses silently.
        let limiter = AuthLimiter::new();
        let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1));
        assert!(!limiter.check_blocked(ip));
        assert!(
            limiter.inner.lock().is_empty(),
            "check_blocked must not materialise an entry for an unseen IP",
        );
    }

    #[test]
    fn limiter_evicts_expired_entries_on_record_failure() {
        // Build up state for two IPs whose windows have already expired,
        // then record a failure for a third. Expired entries should be
        // gone after the call, leaving only the active one.
        let limiter = AuthLimiter::new();
        let stale1 = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        let stale2 = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2));
        let active = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 3));
        limiter.record_failure(stale1);
        limiter.record_failure(stale2);
        {
            let mut guard = limiter.inner.lock();
            for ip in [&stale1, &stale2] {
                let entry = guard.get_mut(ip).unwrap();
                entry.window_start = Some(Instant::now() - (WINDOW + Duration::from_secs(1)));
            }
        }
        limiter.record_failure(active);
        let guard = limiter.inner.lock();
        assert!(!guard.contains_key(&stale1));
        assert!(!guard.contains_key(&stale2));
        assert!(guard.contains_key(&active));
    }

    #[test]
    fn limiter_is_per_ip() {
        let limiter = AuthLimiter::new();
        let a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        for _ in 0..MAX_FAILURES {
            limiter.record_failure(a);
        }
        assert!(limiter.check_blocked(a));
        // B has no failures recorded — must not be blocked by A's failures.
        assert!(!limiter.check_blocked(b));
    }
}
