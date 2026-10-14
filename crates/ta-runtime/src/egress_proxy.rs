// egress_proxy.rs: per-goal CONNECT egress proxy (v0.17.12.7).
//
// The OS sandbox cannot filter by hostname (Seatbelt sees only addresses and
// ports), so hostname policy lives here. The sandbox lets the agent reach
// exactly one loopback port, this proxy's, and nothing else. Every connection
// attempt is decided here and written to an audit log, allowed or not.
//
// Platform-neutral on purpose: Linux (v0.17.12.8) and Windows (v0.17.12.9)
// reach the same proxy through their own transport (bind-mounted socket,
// named pipe relay).
//
// ## What it enforces
//
// - Only `CONNECT host:port` (HTTPS). Plain HTTP forwarding is refused with an
//   explanation, because it would let the proxy see and alter request bodies.
// - The target host must match an allowlist entry. Entries are `host`,
//   `*.suffix` (subdomains only; list the apex separately) or either with
//   `:port`. Without a port only 443 is allowed.
// - DNS rebinding and SSRF: the name is resolved by the proxy and every
//   address is checked. Loopback, unspecified, link-local, multicast and
//   broadcast addresses are never reachable (the daemon on 127.0.0.1:7700 and
//   NATS on :4222 must stay out of reach even for an allowlisted name).
//   Private ranges are reachable only when that exact IP literal is listed.
// - Bounded: connection count, request-head size, header and connect timeouts.
//
// The allowlist is fixed at start. It comes from the trusted sandbox profile,
// never from anything the agent can write.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Serialize;

/// Largest request head the proxy will buffer.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// Model API host that a sandboxed Claude Code agent always needs.
pub const MODEL_API_HOSTS: &[&str] = &["api.anthropic.com"];

// ── Allowlist ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum HostPattern {
    Exact(String),
    /// `*.example.com`, stored as `.example.com`.
    Suffix(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostRule {
    pattern: HostPattern,
    /// `None` means 443 only.
    port: Option<u16>,
    source: String,
}

/// The parsed hostname allowlist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EgressAllowlist {
    rules: Vec<HostRule>,
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

impl EgressAllowlist {
    /// Parse allowlist entries. `*` alone is rejected: an open network is the
    /// owner's choice and is expressed by not running a proxy at all.
    pub fn parse<S: AsRef<str>>(entries: &[S]) -> Result<Self, String> {
        let mut rules = Vec::new();
        for raw in entries {
            let raw = raw.as_ref().trim();
            if raw.is_empty() {
                continue;
            }
            if raw == "*" {
                return Err(
                    "`*` is not a hostname allowlist entry; it means unrestricted \
                            network and runs without the egress proxy"
                        .to_string(),
                );
            }
            let (host_part, port) = split_host_port(raw)
                .map_err(|e| format!("invalid allow_network entry {raw:?}: {e}"))?;
            let host = normalize_host(&host_part);
            if host.is_empty() {
                return Err(format!("invalid allow_network entry {raw:?}: empty host"));
            }
            let pattern = match host.strip_prefix("*.") {
                Some(rest) if !rest.is_empty() && !rest.contains('*') => {
                    HostPattern::Suffix(format!(".{rest}"))
                }
                _ if host.contains('*') => {
                    return Err(format!(
                        "invalid allow_network entry {raw:?}: `*` is only allowed as a leading \
                         `*.` label (for example `*.github.com`)"
                    ))
                }
                _ => HostPattern::Exact(host),
            };
            rules.push(HostRule {
                pattern,
                port,
                source: raw.to_string(),
            });
        }
        Ok(Self { rules })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The entries as written, for the audit log and banner.
    pub fn entries(&self) -> Vec<String> {
        self.rules.iter().map(|r| r.source.clone()).collect()
    }

    /// The first rule that permits `host:port`, if any.
    pub fn matching_rule(&self, host: &str, port: u16) -> Option<&str> {
        let host = normalize_host(host);
        self.rules
            .iter()
            .find(|r| {
                let port_ok = match r.port {
                    Some(p) => p == port,
                    None => port == 443,
                };
                let host_ok = match &r.pattern {
                    HostPattern::Exact(h) => *h == host,
                    HostPattern::Suffix(s) => host.len() > s.len() && host.ends_with(s.as_str()),
                };
                port_ok && host_ok
            })
            .map(|r| r.source.as_str())
    }

    /// True when `ip` is listed as an exact IP literal.
    fn lists_literal(&self, ip: &IpAddr) -> bool {
        let text = ip.to_string();
        self.rules
            .iter()
            .any(|r| matches!(&r.pattern, HostPattern::Exact(h) if *h == text))
    }
}

fn split_host_port(s: &str) -> Result<(String, Option<u16>), String> {
    if let Some(rest) = s.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| "unterminated `[` in IPv6 literal".to_string())?;
        return match tail.strip_prefix(':') {
            Some(p) => Ok((host.to_string(), Some(parse_port(p)?))),
            None if tail.is_empty() => Ok((host.to_string(), None)),
            None => Err("unexpected text after `]`".to_string()),
        };
    }
    // A bare IPv6 literal has several colons and no brackets: no port.
    if s.matches(':').count() > 1 {
        return Ok((s.to_string(), None));
    }
    match s.rsplit_once(':') {
        Some((h, p)) => Ok((h.to_string(), Some(parse_port(p)?))),
        None => Ok((s.to_string(), None)),
    }
}

fn parse_port(p: &str) -> Result<u16, String> {
    match p.parse::<u16>() {
        Ok(0) | Err(_) => Err(format!("{p:?} is not a valid port")),
        Ok(n) => Ok(n),
    }
}

/// Addresses the proxy never connects to, whatever the allowlist says.
pub fn is_always_blocked(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6_mapped(v6) {
                return is_always_blocked(&IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Private ranges: reachable only when the exact IP literal is allowlisted.
pub fn is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private() || (o[0] == 100 && (o[1] & 0xc0) == 64) || o[0] == 0
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6_mapped(v6) {
                return is_private(&IpAddr::V4(v4));
            }
            (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

fn v6_mapped(v6: &Ipv6Addr) -> Option<Ipv4Addr> {
    v6.to_ipv4_mapped()
}

// ── Audit ──────────────────────────────────────────────────────────────────

/// One audit line. `event` is `connect`, `close` or `deny`.
#[derive(Debug, Clone, Serialize)]
pub struct EgressRecord {
    pub ts: String,
    pub goal_id: String,
    pub event: &'static str,
    pub host: String,
    pub port: u16,
    pub allowed: bool,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_up: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_down: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

type DenyHook = Arc<dyn Fn(&EgressRecord) + Send + Sync>;

struct Audit {
    goal_id: String,
    file: Mutex<Option<std::fs::File>>,
    path: Option<PathBuf>,
    on_deny: Option<DenyHook>,
}

impl Audit {
    fn record(&self, mut rec: EgressRecord) {
        rec.goal_id = self.goal_id.clone();
        if !rec.allowed {
            tracing::warn!(
                goal_id = %rec.goal_id,
                host = %rec.host,
                port = rec.port,
                reason = %rec.reason,
                "egress proxy denied a connection from the sandboxed agent"
            );
            if let Some(hook) = &self.on_deny {
                hook(&rec);
            }
        } else {
            tracing::debug!(goal_id = %rec.goal_id, host = %rec.host, port = rec.port,
                event = rec.event, "egress proxy");
        }
        let Ok(mut line) = serde_json::to_string(&rec) else {
            return;
        };
        line.push('\n');
        if let Ok(mut guard) = self.file.lock() {
            if let Some(f) = guard.as_mut() {
                if let Err(e) = f.write_all(line.as_bytes()) {
                    tracing::warn!(
                        path = ?self.path, error = %e,
                        "could not append to the egress audit log; check free disk space and \
                         permissions on the audit directory"
                    );
                }
            }
        }
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn rec(
    event: &'static str,
    host: &str,
    port: u16,
    allowed: bool,
    reason: impl Into<String>,
) -> EgressRecord {
    EgressRecord {
        ts: now(),
        goal_id: String::new(),
        event,
        host: host.to_string(),
        port,
        allowed,
        reason: reason.into(),
        rule: None,
        resolved: None,
        bytes_up: None,
        bytes_down: None,
        duration_ms: None,
    }
}

// ── Proxy ──────────────────────────────────────────────────────────────────

/// Settings for [`EgressProxy::start`].
pub struct EgressProxyConfig {
    pub goal_id: String,
    pub allowlist: EgressAllowlist,
    /// JSON-lines audit log (one [`EgressRecord`] per line). `None` logs through `tracing` only.
    pub audit_path: Option<PathBuf>,
    /// Called for every denied connection (the launcher turns these into violation events).
    pub on_deny: Option<DenyHook>,
    pub max_connections: usize,
    pub header_timeout: Duration,
    pub connect_timeout: Duration,
}

impl EgressProxyConfig {
    pub fn new(goal_id: impl Into<String>, allowlist: EgressAllowlist) -> Self {
        Self {
            goal_id: goal_id.into(),
            allowlist,
            audit_path: None,
            on_deny: None,
            max_connections: 128,
            header_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(10),
        }
    }
}

/// Counters, readable while the proxy runs.
#[derive(Debug, Default)]
pub struct EgressStats {
    pub allowed: AtomicU64,
    pub denied: AtomicU64,
    pub bytes_up: AtomicU64,
    pub bytes_down: AtomicU64,
}

struct Shared {
    cfg_allowlist: EgressAllowlist,
    audit: Audit,
    stats: Arc<EgressStats>,
    active: AtomicUsize,
    max_connections: usize,
    header_timeout: Duration,
    connect_timeout: Duration,
    stop: AtomicBool,
    next_id: AtomicU64,
    open: Mutex<HashMap<u64, (TcpStream, Option<TcpStream>)>>,
}

/// A running proxy. Dropping it stops the listener and closes open tunnels.
pub struct EgressProxy {
    addr: SocketAddr,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl EgressProxy {
    /// Bind `127.0.0.1:0` and start serving.
    pub fn start(cfg: EgressProxyConfig) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!(
                    "could not bind the egress proxy on 127.0.0.1 (goal {}): {e}. \
                     Check that loopback networking is available.",
                    cfg.goal_id
                ),
            )
        })?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;

        let file = match &cfg.audit_path {
            Some(path) => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| {
                        std::io::Error::new(
                            e.kind(),
                            format!(
                                "could not create the egress audit directory {}: {e}",
                                dir.display()
                            ),
                        )
                    })?;
                }
                let mut opts = std::fs::OpenOptions::new();
                opts.create(true).append(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                Some(opts.open(path).map_err(|e| {
                    std::io::Error::new(
                        e.kind(),
                        format!(
                            "could not open the egress audit log {}: {e}. A proxy without an \
                             audit log is not started; fix the path or its permissions.",
                            path.display()
                        ),
                    )
                })?)
            }
            None => None,
        };

        let shared = Arc::new(Shared {
            cfg_allowlist: cfg.allowlist,
            audit: Audit {
                goal_id: cfg.goal_id,
                file: Mutex::new(file),
                path: cfg.audit_path,
                on_deny: cfg.on_deny,
            },
            stats: Arc::new(EgressStats::default()),
            active: AtomicUsize::new(0),
            max_connections: cfg.max_connections.max(1),
            header_timeout: cfg.header_timeout,
            connect_timeout: cfg.connect_timeout,
            stop: AtomicBool::new(false),
            next_id: AtomicU64::new(0),
            open: Mutex::new(HashMap::new()),
        });

        let accept_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("ta-egress-proxy".into())
            .spawn(move || accept_loop(listener, accept_shared))?;

        tracing::info!(
            goal_id = %shared.audit.goal_id,
            port = addr.port(),
            entries = ?shared.cfg_allowlist.entries(),
            "egress proxy started"
        );
        Ok(Self {
            addr,
            shared,
            thread: Some(thread),
        })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    pub fn stats(&self) -> Arc<EgressStats> {
        Arc::clone(&self.shared.stats)
    }

    pub fn audit_path(&self) -> Option<&std::path::Path> {
        self.shared.audit.path.as_deref()
    }

    /// Stop accepting and close open tunnels. Idempotent.
    pub fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Ok(open) = self.shared.open.lock() {
            for (client, upstream) in open.values() {
                let _ = client.shutdown(Shutdown::Both);
                if let Some(u) = upstream {
                    let _ = u.shutdown(Shutdown::Both);
                }
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Environment that points ordinary tooling at the proxy.
pub fn proxy_env(url: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for k in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        out.push((k.to_string(), url.to_string()));
    }
    // Node 24+ only honors the proxy variables with this switch.
    out.push(("NODE_USE_ENV_PROXY".to_string(), "1".to_string()));
    out
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                let _ = stream.set_nonblocking(false);
                if shared.active.fetch_add(1, Ordering::SeqCst) >= shared.max_connections {
                    shared.active.fetch_sub(1, Ordering::SeqCst);
                    shared.stats.denied.fetch_add(1, Ordering::Relaxed);
                    shared.audit.record(rec(
                        "deny",
                        "",
                        0,
                        false,
                        format!(
                            "too many concurrent connections (limit {})",
                            shared.max_connections
                        ),
                    ));
                    let mut s = stream;
                    let _ = respond(
                        &mut s,
                        503,
                        "Service Unavailable",
                        "TA egress proxy is at \
                        its connection limit; retry shortly.",
                    );
                    continue;
                }
                let conn_shared = Arc::clone(&shared);
                let spawned = std::thread::Builder::new()
                    .name("ta-egress-conn".into())
                    .spawn(move || {
                        handle(stream, &conn_shared);
                        conn_shared.active.fetch_sub(1, Ordering::SeqCst);
                    });
                if spawned.is_err() {
                    shared.active.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                tracing::warn!(error = %e, "egress proxy accept failed; retrying");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn respond(stream: &mut TcpStream, code: u16, reason: &str, body: &str) -> std::io::Result<()> {
    let msg = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}\n",
        body.len() + 1
    );
    stream.write_all(msg.as_bytes())
}

/// Read until the blank line ending the request head. Returns the head and
/// any bytes read past it.
fn read_head(stream: &mut TcpStream, timeout: Duration) -> Result<(Vec<u8>, Vec<u8>), String> {
    let deadline = Instant::now() + timeout;
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("timed out reading the request head".into());
        }
        let _ = stream.set_read_timeout(Some(left));
        match stream.read(&mut chunk) {
            Ok(0) => return Err("client closed before sending a request".into()),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(format!(
                    "timed out after {}s reading the request head",
                    timeout.as_secs_f32().max(0.1)
                ))
            }
            Err(e) => return Err(format!("could not read the request head: {e}")),
        }
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(pos + 4);
            return Ok((buf, rest));
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(format!("request head exceeds {MAX_HEAD_BYTES} bytes"));
        }
    }
}

fn handle(mut client: TcpStream, shared: &Shared) {
    let (head, leftover) = match read_head(&mut client, shared.header_timeout) {
        Ok(v) => v,
        Err(reason) => {
            shared.stats.denied.fetch_add(1, Ordering::Relaxed);
            shared
                .audit
                .record(rec("deny", "", 0, false, reason.clone()));
            let _ = respond(
                &mut client,
                400,
                "Bad Request",
                &format!("TA egress proxy: {reason}"),
            );
            return;
        }
    };
    let first = String::from_utf8_lossy(&head);
    let first_line = first.lines().next().unwrap_or("").to_string();
    let mut parts = first_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));

    if !method.eq_ignore_ascii_case("CONNECT") {
        shared.stats.denied.fetch_add(1, Ordering::Relaxed);
        let shown: String = target.chars().take(120).collect();
        shared.audit.record(rec(
            "deny",
            &shown,
            0,
            false,
            format!("method {method:?} is not supported; only CONNECT (HTTPS) is proxied"),
        ));
        let _ = respond(
            &mut client,
            405,
            "Method Not Allowed",
            "TA egress proxy only tunnels HTTPS (CONNECT). Plain HTTP requests are not \
             forwarded; use an https:// URL.",
        );
        return;
    }

    let (host, port) = match split_host_port(target) {
        Ok((h, Some(p))) => (normalize_host(&h), p),
        _ => {
            shared.stats.denied.fetch_add(1, Ordering::Relaxed);
            let shown: String = target.chars().take(120).collect();
            shared.audit.record(rec(
                "deny",
                &shown,
                0,
                false,
                "CONNECT target is not host:port",
            ));
            let _ = respond(
                &mut client,
                400,
                "Bad Request",
                "TA egress proxy: CONNECT target must be host:port.",
            );
            return;
        }
    };

    let deny = |client: &mut TcpStream, reason: String, status: (u16, &str)| {
        shared.stats.denied.fetch_add(1, Ordering::Relaxed);
        shared
            .audit
            .record(rec("deny", &host, port, false, reason.clone()));
        let _ = respond(
            client,
            status.0,
            status.1,
            &format!(
                "TA egress proxy denied CONNECT to {host}:{port}: {reason}. Allowed hosts come \
                 from the trusted sandbox policy (`allow_network` in the state-dir policy.toml); \
                 ask the project owner to add {host} if this agent needs it."
            ),
        );
    };

    let rule = match shared.cfg_allowlist.matching_rule(&host, port) {
        Some(r) => r.to_string(),
        None => {
            deny(
                &mut client,
                "host is not on the allowlist".into(),
                (403, "Forbidden"),
            );
            return;
        }
    };

    // Resolve here and vet every address (DNS rebinding / SSRF to the daemon).
    let literal: Option<IpAddr> = host.parse().ok();
    let candidates: Vec<SocketAddr> = match literal {
        Some(ip) => vec![SocketAddr::new(ip, port)],
        None => match (host.as_str(), port).to_socket_addrs() {
            Ok(it) => it.collect(),
            Err(e) => {
                deny(
                    &mut client,
                    format!("could not resolve host: {e}"),
                    (502, "Bad Gateway"),
                );
                return;
            }
        },
    };
    let allowed: Vec<SocketAddr> = candidates
        .iter()
        .copied()
        .filter(|sa| {
            let ip = sa.ip();
            !is_always_blocked(&ip)
                && (!is_private(&ip)
                    || (literal.is_some() && shared.cfg_allowlist.lists_literal(&ip)))
        })
        .collect();
    if allowed.is_empty() {
        let seen: Vec<String> = candidates.iter().map(|s| s.ip().to_string()).collect();
        deny(
            &mut client,
            format!(
                "host resolves only to blocked addresses ({}): loopback, link-local, \
                 multicast and unlisted private ranges are never reachable",
                seen.join(", ")
            ),
            (403, "Forbidden"),
        );
        return;
    }

    let started = Instant::now();
    let mut upstream = None;
    let mut last_err = String::new();
    for sa in &allowed {
        match TcpStream::connect_timeout(sa, shared.connect_timeout) {
            Ok(s) => {
                upstream = Some((s, *sa));
                break;
            }
            Err(e) => last_err = format!("{sa}: {e}"),
        }
    }
    let Some((mut upstream, peer)) = upstream else {
        deny(
            &mut client,
            format!(
                "could not connect within {}s ({last_err})",
                shared.connect_timeout.as_secs()
            ),
            (502, "Bad Gateway"),
        );
        return;
    };

    shared.stats.allowed.fetch_add(1, Ordering::Relaxed);
    let mut open_rec = rec("connect", &host, port, true, "allowlisted");
    open_rec.rule = Some(rule);
    open_rec.resolved = Some(peer.ip().to_string());
    shared.audit.record(open_rec);

    if client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .is_err()
    {
        return;
    }
    if !leftover.is_empty() && upstream.write_all(&leftover).is_err() {
        return;
    }
    let _ = client.set_read_timeout(None);

    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    if let (Ok(c), Ok(u)) = (client.try_clone(), upstream.try_clone()) {
        if let Ok(mut open) = shared.open.lock() {
            open.insert(id, (c, Some(u)));
        }
    }
    if shared.stop.load(Ordering::SeqCst) {
        let _ = client.shutdown(Shutdown::Both);
        let _ = upstream.shutdown(Shutdown::Both);
    }

    let (up, down) = pipe(client, upstream);
    if let Ok(mut open) = shared.open.lock() {
        open.remove(&id);
    }
    shared.stats.bytes_up.fetch_add(up, Ordering::Relaxed);
    shared.stats.bytes_down.fetch_add(down, Ordering::Relaxed);
    let mut close = rec("close", &host, port, true, "closed");
    close.bytes_up = Some(up + leftover.len() as u64);
    close.bytes_down = Some(down);
    close.duration_ms = Some(started.elapsed().as_millis() as u64);
    shared.audit.record(close);
}

/// Copy both directions until either side finishes. Returns (client→upstream, upstream→client) bytes.
fn pipe(client: TcpStream, upstream: TcpStream) -> (u64, u64) {
    let (Ok(mut c_read), Ok(mut u_write)) = (client.try_clone(), upstream.try_clone()) else {
        return (0, 0);
    };
    let (mut u_read, mut c_write) = (upstream, client);
    let down = std::thread::spawn(move || {
        let n = std::io::copy(&mut u_read, &mut c_write).unwrap_or(0);
        let _ = c_write.shutdown(Shutdown::Both);
        let _ = u_read.shutdown(Shutdown::Both);
        n
    });
    let up = std::io::copy(&mut c_read, &mut u_write).unwrap_or(0);
    let _ = u_write.shutdown(Shutdown::Both);
    let _ = c_read.shutdown(Shutdown::Both);
    (up, down.join().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    fn list(entries: &[&str]) -> EgressAllowlist {
        EgressAllowlist::parse(entries).unwrap()
    }

    #[test]
    fn exact_host_defaults_to_443_only() {
        let a = list(&["api.anthropic.com"]);
        assert!(a.matching_rule("api.anthropic.com", 443).is_some());
        assert!(a.matching_rule("API.Anthropic.com.", 443).is_some());
        assert!(a.matching_rule("api.anthropic.com", 80).is_none());
        assert!(a.matching_rule("evil-api.anthropic.com", 443).is_none());
        assert!(a.matching_rule("api.anthropic.com.evil.io", 443).is_none());
    }

    #[test]
    fn explicit_port_replaces_the_default() {
        let a = list(&["registry.local:8443"]);
        assert!(a.matching_rule("registry.local", 8443).is_some());
        assert!(a.matching_rule("registry.local", 443).is_none());
    }

    #[test]
    fn wildcard_matches_subdomains_not_the_apex_or_lookalikes() {
        let a = list(&["*.github.com"]);
        assert!(a.matching_rule("api.github.com", 443).is_some());
        assert!(a.matching_rule("a.b.github.com", 443).is_some());
        assert!(a.matching_rule("github.com", 443).is_none());
        assert!(a.matching_rule("notgithub.com", 443).is_none());
        assert!(a.matching_rule("evilgithub.com", 443).is_none());
    }

    #[test]
    fn bare_star_and_inner_wildcards_are_rejected_with_a_reason() {
        assert!(EgressAllowlist::parse(&["*"])
            .unwrap_err()
            .contains("unrestricted"));
        assert!(EgressAllowlist::parse(&["a*.com"])
            .unwrap_err()
            .contains("leading"));
        assert!(EgressAllowlist::parse(&["host:0"])
            .unwrap_err()
            .contains("port"));
        assert!(EgressAllowlist::parse(&["host:notaport"]).is_err());
    }

    #[test]
    fn ipv6_literals_parse() {
        let a = list(&["[2001:db8::1]:8443", "2001:db8::2"]);
        assert!(a.matching_rule("2001:db8::1", 8443).is_some());
        assert!(a.matching_rule("2001:db8::2", 443).is_some());
    }

    #[test]
    fn always_blocked_and_private_ranges() {
        for ip in [
            "127.0.0.1",
            "127.9.9.9",
            "0.0.0.0",
            "169.254.169.254",
            "224.0.0.1",
            "::1",
            "::",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_always_blocked(&ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "2606:4700::1111"] {
            assert!(!is_always_blocked(&ip.parse().unwrap()), "{ip}");
            assert!(!is_private(&ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "10.0.0.1",
            "172.16.5.5",
            "192.168.1.1",
            "100.64.0.1",
            "fd00::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(is_private(&ip.parse().unwrap()), "{ip}");
        }
    }

    // ── end to end over real sockets ───────────────────────────────────────

    fn echo_server() -> (u16, JoinHandle<()>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let mut buf = [0u8; 256];
                while let Ok(n) = s.read(&mut buf) {
                    if n == 0 || s.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        });
        (port, h)
    }

    fn connect_via(proxy: &EgressProxy, target: &str) -> (String, TcpStream) {
        let mut s = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(s, "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut status = String::new();
        r.read_line(&mut status).unwrap();
        (status, s)
    }

    fn start(entries: &[&str], audit: Option<PathBuf>) -> EgressProxy {
        let mut cfg = EgressProxyConfig::new("goal-test", list(entries));
        cfg.audit_path = audit;
        cfg.header_timeout = Duration::from_millis(500);
        EgressProxy::start(cfg).unwrap()
    }

    #[test]
    fn unlisted_host_is_denied_403_and_audited() {
        let dir = tempfile::tempdir().unwrap();
        let audit = dir.path().join("a/egress.jsonl");
        let proxy = start(&["api.anthropic.com"], Some(audit.clone()));
        let (status, _s) = connect_via(&proxy, "example.org:443");
        assert!(status.contains("403"), "{status}");
        let log = std::fs::read_to_string(&audit).unwrap();
        let v: serde_json::Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
        assert_eq!(v["event"], "deny");
        assert_eq!(v["host"], "example.org");
        assert_eq!(v["goal_id"], "goal-test");
        assert_eq!(v["allowed"], false);
        assert_eq!(proxy.stats().denied.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn allowlisted_name_resolving_to_loopback_is_blocked() {
        // `localhost` is on the allowlist but resolves to loopback: the daemon
        // must stay unreachable even for an allowlisted name.
        let (echo_port, _h) = echo_server();
        let proxy = start(&[&format!("localhost:{echo_port}")], None);
        let (status, _s) = connect_via(&proxy, &format!("localhost:{echo_port}"));
        assert!(status.contains("403"), "{status}");
    }

    #[test]
    fn listed_loopback_ip_literal_is_still_blocked() {
        let (echo_port, _h) = echo_server();
        let proxy = start(&[&format!("127.0.0.1:{echo_port}")], None);
        let (status, _s) = connect_via(&proxy, &format!("127.0.0.1:{echo_port}"));
        assert!(status.contains("403"), "{status}");
    }

    #[test]
    fn plain_http_is_refused_with_405() {
        let proxy = start(&["example.org"], None);
        let mut s = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(
            s,
            "GET http://example.org/ HTTP/1.1\r\nHost: example.org\r\n\r\n"
        )
        .unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        assert!(out.starts_with("HTTP/1.1 405"), "{out}");
        assert!(out.contains("CONNECT"));
    }

    #[test]
    fn malformed_and_oversized_heads_are_rejected() {
        let proxy = start(&["example.org"], None);
        let (status, _s) = connect_via(&proxy, "no-port-here");
        assert!(status.contains("400"), "{status}");

        let mut s = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let junk = vec![b'a'; MAX_HEAD_BYTES + 2048];
        let _ = s.write_all(&junk);
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        assert!(out.starts_with("HTTP/1.1 400"), "{out}");
    }

    #[test]
    fn silent_client_times_out_instead_of_hanging() {
        let proxy = start(&["example.org"], None);
        let mut s = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        assert!(out.starts_with("HTTP/1.1 400"), "{out}");
        assert!(out.contains("timed out"));
    }

    #[test]
    fn private_ip_literal_tunnels_only_when_listed() {
        // Loopback is the only address we can listen on in a test, so check the
        // decision function that gates private ranges instead.
        let a = list(&["10.1.2.3"]);
        let ip: IpAddr = "10.1.2.3".parse().unwrap();
        assert!(a.lists_literal(&ip));
        assert!(!a.lists_literal(&"10.1.2.4".parse().unwrap()));
        assert!(is_private(&ip) && !is_always_blocked(&ip));
    }

    #[test]
    fn connection_limit_returns_503() {
        let mut cfg = EgressProxyConfig::new("g", list(&["example.org"]));
        cfg.max_connections = 1;
        cfg.header_timeout = Duration::from_secs(3);
        let proxy = EgressProxy::start(cfg).unwrap();
        // First connection sits silent, holding the only slot.
        let _hold = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let mut s = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        assert!(out.starts_with("HTTP/1.1 503"), "{out}");
    }

    #[test]
    fn deny_hook_fires_for_denials_only() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen2 = Arc::clone(&seen);
        let mut cfg = EgressProxyConfig::new("g", list(&["api.anthropic.com"]));
        cfg.on_deny = Some(Arc::new(move |r| {
            seen2.lock().unwrap().push(r.host.clone())
        }));
        let proxy = EgressProxy::start(cfg).unwrap();
        let (status, _s) = connect_via(&proxy, "blocked.example:443");
        assert!(status.contains("403"));
        assert_eq!(seen.lock().unwrap().as_slice(), ["blocked.example"]);
    }

    #[test]
    fn unwritable_audit_path_refuses_to_start_with_guidance() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "x").unwrap();
        let mut cfg = EgressProxyConfig::new("g", list(&["example.org"]));
        cfg.audit_path = Some(blocker.join("sub/egress.jsonl"));
        let err = EgressProxy::start(cfg)
            .err()
            .expect("must refuse")
            .to_string();
        assert!(err.contains("audit"), "{err}");
    }

    #[test]
    fn shutdown_stops_the_listener() {
        let mut proxy = start(&["example.org"], None);
        let port = proxy.port();
        proxy.shutdown();
        assert!(TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_millis(300)
        )
        .is_err());
    }

    #[test]
    fn proxy_env_sets_both_cases_and_node_switch() {
        let env: HashMap<_, _> = proxy_env("http://127.0.0.1:9").into_iter().collect();
        assert_eq!(env["HTTPS_PROXY"], "http://127.0.0.1:9");
        assert_eq!(env["https_proxy"], "http://127.0.0.1:9");
        assert_eq!(env["NODE_USE_ENV_PROXY"], "1");
    }
}
