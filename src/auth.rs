use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::header::{AUTHORIZATION, HeaderName},
    middleware::Next,
    response::Response,
};
use ipnet::IpNet;
use parking_lot::Mutex;
use subtle::ConstantTimeEq;

use crate::{AppError, AppState};

/// Sliding-window rate limit on failed auth attempts per remote IP.
/// After `MAX_FAILURES` failures within `WINDOW`, the IP is blocked until
/// the window elapses without new failures.
const WINDOW: Duration = Duration::from_secs(60);
const MAX_FAILURES: u32 = 10;
const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");

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

/// Returns the client IP for limiter / audit purposes. The TCP peer is
/// the primary source. When the peer matches a configured trusted proxy,
/// the leftmost hop of `X-Forwarded-For` is used instead so that
/// per-client throttling and logs survive a reverse proxy.
///
/// XFF from an untrusted peer is ignored — it's just a request header
/// any client can set.
fn peer_ip(req: &Request, trusted_proxies: &[IpNet]) -> Option<IpAddr> {
    let direct = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())?;

    if trusted_proxies.iter().any(|net| net.contains(&direct))
        && let Some(forwarded) = req
            .headers()
            .get(&X_FORWARDED_FOR)
            .and_then(|value| value.to_str().ok())
            .and_then(first_forwarded_ip)
    {
        return Some(forwarded);
    }

    Some(direct)
}

fn first_forwarded_ip(value: &str) -> Option<IpAddr> {
    value
        .split(',')
        .next()
        .map(str::trim)
        .and_then(|ip| ip.parse().ok())
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
    let ip = peer_ip(&req, &state.config.trusted_proxies);

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
    let ip_str = ip.map(|i| i.to_string());
    tracing::warn!(
        target: "audit",
        event = "auth.denied",
        reason,
        path = %path,
        ip = ip_str.as_deref().unwrap_or("unknown"),
        failures = ?failures,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn req_with(peer: IpAddr, xff: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder();
        if let Some(value) = xff {
            builder = builder.header("x-forwarded-for", value);
        }
        let mut req = builder.body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((peer, 49152))));
        req
    }

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

    #[test]
    fn first_forwarded_ip_uses_leftmost_hop() {
        assert_eq!(
            first_forwarded_ip("203.0.113.10, 127.0.0.1"),
            Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10))),
        );
        assert_eq!(
            first_forwarded_ip("  198.51.100.5  ,10.0.0.1"),
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 5))),
        );
        assert_eq!(first_forwarded_ip("not-an-ip, 203.0.113.10"), None);
        assert_eq!(first_forwarded_ip(""), None);
    }

    fn nets(specs: &[&str]) -> Vec<IpNet> {
        specs.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn peer_ip_returns_direct_when_trusted_list_is_empty() {
        // Empty trusted_proxies = "no proxy in front." XFF must be ignored.
        let trusted: Vec<IpNet> = vec![];
        let req = req_with(IpAddr::V4(Ipv4Addr::LOCALHOST), Some("203.0.113.10"));
        assert_eq!(
            peer_ip(&req, &trusted),
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST))
        );
    }

    #[test]
    fn peer_ip_trusts_xff_only_when_peer_is_in_list() {
        let trusted = nets(&["127.0.0.1/32"]);
        let req = req_with(IpAddr::V4(Ipv4Addr::LOCALHOST), Some("203.0.113.10"));
        assert_eq!(
            peer_ip(&req, &trusted),
            Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10))),
        );

        // Peer NOT in trusted list — XFF must be ignored even though present.
        let req = req_with(
            IpAddr::V4(Ipv4Addr::new(198, 51, 100, 20)),
            Some("203.0.113.10"),
        );
        assert_eq!(
            peer_ip(&req, &trusted),
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 20))),
        );
    }

    #[test]
    fn peer_ip_supports_off_host_proxy_via_lan_ip() {
        // Topology: Caddy on a LAN box (192.168.1.5) reverse-proxies to this
        // server bound on 192.168.1.10. trusted_proxies tells us the proxy.
        let proxy = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5));
        let trusted = nets(&["192.168.1.5/32"]);
        let req = req_with(proxy, Some("203.0.113.10"));
        assert_eq!(
            peer_ip(&req, &trusted),
            Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10))),
        );
    }

    #[test]
    fn peer_ip_falls_back_to_peer_when_xff_missing() {
        let trusted = nets(&["127.0.0.1/32"]);
        let req = req_with(IpAddr::V4(Ipv4Addr::LOCALHOST), None);
        assert_eq!(
            peer_ip(&req, &trusted),
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST))
        );
    }

    #[test]
    fn peer_ip_handles_ipv6_loopback_proxy() {
        let trusted = nets(&["::1/128"]);
        let req = req_with(IpAddr::V6(Ipv6Addr::LOCALHOST), Some("2001:db8::1"));
        assert_eq!(
            peer_ip(&req, &trusted),
            Some(IpAddr::V6("2001:db8::1".parse().unwrap())),
        );
    }

    #[test]
    fn peer_ip_matches_proxy_inside_cidr_range() {
        // Tailnet topology: trust any proxy in 100.64.0.0/10 (CGNAT range
        // Tailscale uses). Two different proxy IPs in that range should
        // both be honored without enumerating each one.
        let trusted = nets(&["100.64.0.0/10"]);

        let proxy_a = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 5));
        let req = req_with(proxy_a, Some("203.0.113.10"));
        assert_eq!(
            peer_ip(&req, &trusted),
            Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10))),
        );

        let proxy_b = IpAddr::V4(Ipv4Addr::new(100, 100, 200, 99));
        let req = req_with(proxy_b, Some("203.0.113.10"));
        assert_eq!(
            peer_ip(&req, &trusted),
            Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10))),
        );

        // Outside the CIDR — XFF ignored.
        let outside = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 5));
        let req = req_with(outside, Some("203.0.113.10"));
        assert_eq!(peer_ip(&req, &trusted), Some(outside));
    }
}
