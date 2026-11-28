//! Synchronous, permanently read-only HTTP server serving `GET /v1/samples?since_ms=<cursor>`
//! with strict request size, timeout, concurrency, and peer source-IP caps.

use std::collections::VecDeque;
use std::io::{self, Read as _, Write as _};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use btop_model::{FLEET_SCHEMA_VERSION, FleetSamplesResponse, NodeSample};

use crate::FleetError;
use crate::net::{is_allowed_peer_ip, validate_bind_addr};
use crate::ring::{AgentClock, Ring, RingConfig};

/// Maximum HTTP request bytes accepted before rejecting with `413 Payload Too Large`.
pub const MAX_REQUEST_BYTES: usize = 4_096;

/// Default per-connection read/write and overall request deadline (`2s`).
pub const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(2);

const WORKER_COUNT: usize = 4;
const MAX_QUEUED_CONNECTIONS: usize = 16;
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Immutable metadata attached to every [`FleetSamplesResponse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMetadata {
    pub agent_version: String,
    pub os: String,
    pub arch: String,
}

impl AgentMetadata {
    pub fn current(agent_version: impl Into<String>) -> Self {
        Self {
            agent_version: agent_version.into(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
        }
    }
}

/// Runtime tuning for [`AgentServer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerConfig {
    pub io_timeout: Duration,
    pub max_request_bytes: usize,
    pub worker_count: usize,
    pub max_queued_connections: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            io_timeout: DEFAULT_IO_TIMEOUT,
            max_request_bytes: MAX_REQUEST_BYTES,
            worker_count: WORKER_COUNT,
            max_queued_connections: MAX_QUEUED_CONNECTIONS,
        }
    }
}

#[derive(Debug)]
struct RingState {
    ring: Ring,
    clock: AgentClock,
    meta: AgentMetadata,
}

/// Thread-safe handle shared between the Agent sampling loop and the HTTP server workers.
#[derive(Debug, Clone)]
pub struct SharedRing {
    inner: Arc<Mutex<RingState>>,
}

impl SharedRing {
    pub fn new(config: RingConfig, meta: AgentMetadata) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RingState {
                ring: Ring::new(config),
                clock: AgentClock::new(),
                meta,
            })),
        }
    }

    /// Returns the next strictly monotonic sample timestamp from the Agent's own clock.
    pub fn next_sample_timestamp_ms(&self) -> u64 {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let last = guard.ring.latest_timestamp_ms();
        guard.clock.next_sample_timestamp_ms(last)
    }

    /// Pushes a collected [`NodeSample`] into the bounded `Ring`, applying sleep-gap protection.
    pub fn push_sample(&self, sample: NodeSample, mono_elapsed_ms: Option<u64>) -> bool {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.ring.push(sample, mono_elapsed_ms)
    }

    /// Builds a [`FleetSamplesResponse`] using the Agent's own clock and cursor filtering.
    pub fn build_response(&self, since_ms: u64) -> FleetSamplesResponse {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now_ms = guard.clock.now_ms();
        let samples = guard.ring.samples_since(since_ms);
        FleetSamplesResponse {
            schema_version: FLEET_SCHEMA_VERSION,
            agent_version: guard.meta.agent_version.clone(),
            os: guard.meta.os.clone(),
            arch: guard.meta.arch.clone(),
            interval_ms: guard.ring.config().interval_ms(),
            now_ms,
            samples,
        }
    }

    /// Returns the configured [`RingConfig`].
    pub fn config(&self) -> RingConfig {
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.ring.config()
    }
}

/// Validated `GET /v1/samples` request parsed from raw HTTP header bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedRequest {
    pub since_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpReject {
    BadRequest(&'static str),
    NotFound,
    MethodNotAllowed,
    RequestTimeout,
    PayloadTooLarge,
    HttpVersionNotSupported,
}

impl HttpReject {
    const fn status_line(self) -> &'static str {
        match self {
            Self::BadRequest(_) => "400 Bad Request",
            Self::NotFound => "404 Not Found",
            Self::MethodNotAllowed => "405 Method Not Allowed",
            Self::RequestTimeout => "408 Request Timeout",
            Self::PayloadTooLarge => "413 Payload Too Large",
            Self::HttpVersionNotSupported => "505 HTTP Version Not Supported",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::BadRequest(msg) => msg,
            Self::NotFound => "not found",
            Self::MethodNotAllowed => "method not allowed; only GET is supported",
            Self::RequestTimeout => "request timeout",
            Self::PayloadTooLarge => "request exceeds maximum size",
            Self::HttpVersionNotSupported => "unsupported HTTP version",
        }
    }
}

/// Parses and strictly validates one HTTP/1.x request header block (`\r\n\r\n`-terminated).
pub fn parse_http_request(raw: &[u8]) -> Result<ParsedRequest, HttpReject> {
    let text = std::str::from_utf8(raw).map_err(|_| HttpReject::BadRequest("invalid UTF-8"))?;
    let header_block = text
        .split_once("\r\n\r\n")
        .map(|(head, _)| head)
        .ok_or(HttpReject::BadRequest("missing header terminator"))?;

    let mut lines = header_block.split("\r\n");
    let request_line = lines
        .next()
        .ok_or(HttpReject::BadRequest("empty request line"))?;

    let mut parts = request_line.split(' ');
    let method = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or(HttpReject::BadRequest("missing HTTP method"))?;
    let target = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or(HttpReject::BadRequest("missing request target"))?;
    let version = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or(HttpReject::BadRequest("missing HTTP version"))?;
    if parts.next().is_some() {
        return Err(HttpReject::BadRequest("malformed request line"));
    }

    if !method
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b == b'-' || b == b'_')
    {
        return Err(HttpReject::BadRequest("invalid HTTP method"));
    }

    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(HttpReject::HttpVersionNotSupported);
    }

    let mut has_host = false;
    let mut seen_content_length = false;
    let mut has_transfer_encoding = false;
    let mut content_length = 0u64;

    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(HttpReject::BadRequest("malformed header line"));
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(HttpReject::BadRequest("invalid header name"));
        }
        if value.bytes().any(|b| (b < 0x20 && b != b'\t') || b == 0x7f) {
            return Err(HttpReject::BadRequest(
                "invalid control character in header",
            ));
        }

        if name.eq_ignore_ascii_case("host") {
            if value.trim().is_empty() {
                return Err(HttpReject::BadRequest("empty Host header"));
            }
            has_host = true;
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            has_transfer_encoding = true;
        } else if name.eq_ignore_ascii_case("content-length") {
            if seen_content_length {
                return Err(HttpReject::BadRequest("duplicate Content-Length header"));
            }
            seen_content_length = true;
            content_length = value
                .trim()
                .parse()
                .map_err(|_| HttpReject::BadRequest("invalid Content-Length"))?;
        }
    }

    if version == "HTTP/1.1" && !has_host {
        return Err(HttpReject::BadRequest(
            "missing required Host header for HTTP/1.1",
        ));
    }

    if method != "GET" {
        return Err(HttpReject::MethodNotAllowed);
    }

    if has_transfer_encoding {
        return Err(HttpReject::BadRequest(
            "request bodies and Transfer-Encoding are not accepted",
        ));
    }
    if content_length > 0 {
        return Err(HttpReject::BadRequest("GET request must not carry a body"));
    }

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (target, None),
    };

    if path != "/v1/samples" {
        return Err(HttpReject::NotFound);
    }

    let since_ms = parse_since_ms_query(query)?;
    Ok(ParsedRequest { since_ms })
}

fn parse_since_ms_query(query: Option<&str>) -> Result<u64, HttpReject> {
    let Some(q) = query else {
        return Ok(0);
    };
    if q.is_empty() {
        return Ok(0);
    }
    let mut since_ms: Option<u64> = None;
    for pair in q.split('&') {
        let Some((key, val)) = pair.split_once('=') else {
            return Err(HttpReject::BadRequest("malformed query parameter"));
        };
        if key != "since_ms" {
            return Err(HttpReject::BadRequest("unsupported query parameter"));
        }
        if since_ms.is_some() {
            return Err(HttpReject::BadRequest("duplicate since_ms parameter"));
        }
        if val.is_empty() || !val.bytes().all(|b| b.is_ascii_digit()) {
            return Err(HttpReject::BadRequest(
                "since_ms must be a non-negative integer",
            ));
        }
        let parsed = val
            .parse::<u64>()
            .map_err(|_| HttpReject::BadRequest("since_ms overflow"))?;
        since_ms = Some(parsed);
    }
    Ok(since_ms.unwrap_or(0))
}

#[derive(Debug)]
struct ConnectionQueue {
    items: Mutex<VecDeque<TcpStream>>,
    available: Condvar,
    max_queued: usize,
}

impl ConnectionQueue {
    fn new(max_queued: usize) -> Self {
        Self {
            items: Mutex::new(VecDeque::with_capacity(max_queued)),
            available: Condvar::new(),
            max_queued: max_queued.max(1),
        }
    }

    fn try_push(&self, stream: TcpStream) -> bool {
        let mut guard = self
            .items
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.len() >= self.max_queued {
            return false;
        }
        guard.push_back(stream);
        self.available.notify_one();
        true
    }

    fn pop(&self, stop: &AtomicBool) -> Option<TcpStream> {
        let mut guard = self
            .items
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if stop.load(Ordering::SeqCst) {
                return None;
            }
            if let Some(stream) = guard.pop_front() {
                return Some(stream);
            }
            guard = self
                .available
                .wait(guard)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn wake_all(&self) {
        // Hold the queue mutex around `notify_all` so a worker cannot miss the `stop` transition
        // between checking `stop.load` and entering `Condvar::wait`.
        let _guard = self
            .items
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.available.notify_all();
    }
}

/// Running synchronous HTTP server serving `GET /v1/samples?since_ms=<cursor>`.
#[derive(Debug)]
pub struct AgentServer {
    local_addr: SocketAddr,
    stop: Arc<AtomicBool>,
    queue: Arc<ConnectionQueue>,
    accept_thread: Option<JoinHandle<()>>,
    workers: Vec<JoinHandle<()>>,
}

impl AgentServer {
    /// Validates `bind_addr`, binds a synchronous TCP listener, and spawns the accept loop plus
    /// fixed worker pool.
    pub fn start(
        bind_addr: SocketAddr,
        shared: &SharedRing,
        stop: Arc<AtomicBool>,
        config: ServerConfig,
    ) -> Result<Self, FleetError> {
        let validated_addr = validate_bind_addr(bind_addr)?;
        let listener =
            TcpListener::bind(validated_addr).map_err(|source| FleetError::BindFailed {
                addr: validated_addr.to_string(),
                source,
            })?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;

        let queue = Arc::new(ConnectionQueue::new(config.max_queued_connections));
        let worker_count = config.worker_count.max(1);
        let mut workers = Vec::with_capacity(worker_count);

        for idx in 0..worker_count {
            let worker_queue = Arc::clone(&queue);
            let worker_stop = Arc::clone(&stop);
            let worker_ring = shared.clone();
            let handle = thread::Builder::new()
                .name(format!("btop-fleet-http-{idx}"))
                .spawn(move || {
                    while let Some(stream) = worker_queue.pop(&worker_stop) {
                        handle_connection(stream, &worker_ring, config);
                    }
                })?;
            workers.push(handle);
        }

        let accept_queue = Arc::clone(&queue);
        let accept_stop = Arc::clone(&stop);
        let bound_ip = local_addr.ip();
        let accept_thread = thread::Builder::new()
            .name("btop-fleet-accept".to_owned())
            .spawn(move || {
                run_accept_loop(&listener, bound_ip, &accept_queue, &accept_stop);
            })?;

        Ok(Self {
            local_addr,
            stop,
            queue,
            accept_thread: Some(accept_thread),
            workers,
        })
    }

    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Signals shutdown and joins the accept thread and all HTTP worker threads cleanly.
    pub fn shutdown(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.queue.wake_all();
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
        self.queue.wake_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

impl Drop for AgentServer {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

fn run_accept_loop(
    listener: &TcpListener,
    bound_ip: IpAddr,
    queue: &ConnectionQueue,
    stop: &AtomicBool,
) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer_addr)) => {
                if !is_allowed_peer_ip(peer_addr.ip(), bound_ip) {
                    // Fail closed without sending bytes or banners to unauthorized peers.
                    drop(stream);
                    continue;
                }
                if stream.set_nonblocking(false).is_err() {
                    drop(stream);
                    continue;
                }
                if !queue.try_push(stream) {
                    // Queue saturated; drop excess connection immediately to cap resource usage.
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
        }
    }
}

fn handle_connection(mut stream: TcpStream, shared: &SharedRing, config: ServerConfig) {
    let _ = stream.set_read_timeout(Some(config.io_timeout));
    let _ = stream.set_write_timeout(Some(config.io_timeout));

    let raw = match read_request_headers(&mut stream, config.max_request_bytes, config.io_timeout) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return, // Client closed connection without sending bytes.
        Err(reject) => {
            let _ = write_error_response(&mut stream, reject);
            return;
        }
    };

    match parse_http_request(&raw) {
        Ok(req) => {
            let response = shared.build_response(req.since_ms);
            match serde_json::to_vec(&response) {
                Ok(body) => {
                    let _ = write_json_ok_response(&mut stream, &body);
                }
                Err(_) => {
                    let _ = write_raw_response(
                        &mut stream,
                        "500 Internal Server Error",
                        None,
                        br#"{"error":"serialization failed"}"#,
                    );
                }
            }
        }
        Err(reject) => {
            let _ = write_error_response(&mut stream, reject);
        }
    }
}

fn read_request_headers(
    stream: &mut TcpStream,
    max_bytes: usize,
    timeout: Duration,
) -> Result<Option<Vec<u8>>, HttpReject> {
    let deadline = Instant::now() + timeout;
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 512];

    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(HttpReject::RequestTimeout);
        }
        let remaining = deadline.saturating_duration_since(now);
        let _ = stream.set_read_timeout(Some(remaining));

        match stream.read(&mut chunk) {
            Ok(0) => {
                if buf.is_empty() {
                    return Ok(None);
                }
                return Err(HttpReject::BadRequest("truncated HTTP request"));
            }
            Ok(n) => {
                let Some(next_len) = buf.len().checked_add(n) else {
                    return Err(HttpReject::PayloadTooLarge);
                };
                if next_len > max_bytes {
                    return Err(HttpReject::PayloadTooLarge);
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    return Ok(Some(buf));
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(HttpReject::RequestTimeout);
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(HttpReject::BadRequest("connection read error")),
        }
    }
}

fn write_json_ok_response(stream: &mut TcpStream, body: &[u8]) -> io::Result<()> {
    write_raw_response(stream, "200 OK", None, body)
}

fn write_error_response(stream: &mut TcpStream, reject: HttpReject) -> io::Result<()> {
    let extra = matches!(reject, HttpReject::MethodNotAllowed).then_some("Allow: GET\r\n");
    let body = serde_json::json!({ "error": reject.message() }).to_string();
    write_raw_response(stream, reject.status_line(), extra, body.as_bytes())
}

fn write_raw_response(
    stream: &mut TcpStream,
    status_line: &str,
    extra_header: Option<&str>,
    body: &[u8],
) -> io::Result<()> {
    let extra = extra_header.unwrap_or("");
    let header = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n{extra}Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use btop_model::{DataState, DiskIoRate, NetIoRate};

    use super::*;

    fn test_sample(ts_ms: u64, cpu: f32) -> NodeSample {
        NodeSample {
            timestamp_ms: ts_ms,
            cpu_busy_pct: DataState::Available(cpu),
            mem_used_bytes: 4_096,
            mem_total_bytes: 16_384,
            disk_max_used_pct: DataState::Available(42.0),
            disk_io: DataState::Available(DiskIoRate {
                read_bytes_per_sec: 100,
                write_bytes_per_sec: 50,
            }),
            net_io: DataState::Available(NetIoRate {
                rx_bytes_per_sec: 200,
                tx_bytes_per_sec: 100,
            }),
            uptime_secs: 777,
        }
    }

    fn send_raw_http(addr: SocketAddr, request: &[u8]) -> String {
        let mut stream = TcpStream::connect(addr).expect("connect to test agent");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("set read timeout");
        stream.write_all(request).expect("write request");
        stream.flush().expect("flush");
        let mut out = String::new();
        stream.read_to_string(&mut out).expect("read response");
        out
    }

    #[test]
    fn parse_http_request_validates_method_path_query_and_headers() {
        assert_eq!(
            parse_http_request(b"GET /v1/samples HTTP/1.1\r\nHost: mini:9127\r\n\r\n"),
            Ok(ParsedRequest { since_ms: 0 })
        );
        assert_eq!(
            parse_http_request(
                b"GET /v1/samples?since_ms=12345 HTTP/1.1\r\nHost: mini:9127\r\n\r\n"
            ),
            Ok(ParsedRequest { since_ms: 12_345 })
        );
        assert_eq!(
            parse_http_request(b"POST /v1/samples HTTP/1.1\r\nHost: mini:9127\r\n\r\n"),
            Err(HttpReject::MethodNotAllowed)
        );
        assert_eq!(
            parse_http_request(b"DELETE /v1/samples HTTP/1.1\r\nHost: mini:9127\r\n\r\n"),
            Err(HttpReject::MethodNotAllowed)
        );
        assert_eq!(
            parse_http_request(b"GET /v1/other HTTP/1.1\r\nHost: mini:9127\r\n\r\n"),
            Err(HttpReject::NotFound)
        );
        assert!(matches!(
            parse_http_request(b"GET /v1/samples?since_ms=-1 HTTP/1.1\r\nHost: mini:9127\r\n\r\n"),
            Err(HttpReject::BadRequest(_))
        ));
        assert!(matches!(
            parse_http_request(b"GET /v1/samples?foo=1 HTTP/1.1\r\nHost: mini:9127\r\n\r\n"),
            Err(HttpReject::BadRequest(_))
        ));
        assert!(matches!(
            parse_http_request(b"GET /v1/samples HTTP/1.1\r\n\r\n"),
            Err(HttpReject::BadRequest(_))
        ));
        assert!(matches!(
            parse_http_request(
                b"GET /v1/samples HTTP/1.1\r\nHost: mini\r\nContent-Length: 5\r\n\r\n"
            ),
            Err(HttpReject::BadRequest(_))
        ));
    }

    #[test]
    fn server_serves_full_and_incremental_samples_and_rejects_invalid_requests() {
        let shared = SharedRing::new(RingConfig::default(), AgentMetadata::current("0.2.0-test"));
        assert!(shared.push_sample(test_sample(10_000, 11.0), Some(5_000)));
        assert!(shared.push_sample(test_sample(15_000, 22.0), Some(5_000)));
        assert!(shared.push_sample(test_sample(20_000, 33.0), Some(5_000)));

        let stop = Arc::new(AtomicBool::new(false));
        let server = AgentServer::start(
            "127.0.0.1:0".parse().expect("loopback ephemeral"),
            &shared,
            Arc::clone(&stop),
            ServerConfig::default(),
        )
        .expect("start agent server");
        let addr = server.local_addr();

        // 1. Full Ring when since_ms=0
        let full_raw = send_raw_http(
            addr,
            b"GET /v1/samples?since_ms=0 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(full_raw.starts_with("HTTP/1.1 200 OK\r\n"));
        let (_, full_body) = full_raw.split_once("\r\n\r\n").expect("split body");
        let full_resp: FleetSamplesResponse =
            serde_json::from_str(full_body).expect("parse full response");
        assert_eq!(full_resp.schema_version, FLEET_SCHEMA_VERSION);
        assert_eq!(full_resp.agent_version, "0.2.0-test");
        assert_eq!(full_resp.interval_ms, 5_000);
        assert_eq!(full_resp.samples.len(), 3);
        assert_eq!(full_resp.samples[0].timestamp_ms, 10_000);
        assert_eq!(full_resp.samples[2].timestamp_ms, 20_000);

        // 2. Incremental cursor returns only newer samples
        let delta_raw = send_raw_http(
            addr,
            b"GET /v1/samples?since_ms=15000 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(delta_raw.starts_with("HTTP/1.1 200 OK\r\n"));
        let (_, delta_body) = delta_raw.split_once("\r\n\r\n").expect("split delta body");
        let delta_resp: FleetSamplesResponse =
            serde_json::from_str(delta_body).expect("parse delta response");
        assert_eq!(delta_resp.samples.len(), 1);
        assert_eq!(delta_resp.samples[0].timestamp_ms, 20_000);

        // 3. POST rejected with 405 Method Not Allowed + Allow: GET
        let post_raw = send_raw_http(
            addr,
            b"POST /v1/samples HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        );
        assert!(post_raw.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));
        assert!(post_raw.contains("Allow: GET\r\n"));

        // 4. Unknown path rejected with 404
        let missing_raw = send_raw_http(addr, b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(missing_raw.starts_with("HTTP/1.1 404 Not Found\r\n"));

        // 5. Oversized request rejected with 413
        let huge_header = format!(
            "GET /v1/samples HTTP/1.1\r\nHost: localhost\r\nX-Pad: {}\r\n\r\n",
            "a".repeat(MAX_REQUEST_BYTES + 128)
        );
        let too_big_raw = send_raw_http(addr, huge_header.as_bytes());
        assert!(too_big_raw.starts_with("HTTP/1.1 413 Payload Too Large\r\n"));

        server.shutdown();
    }

    #[test]
    fn server_enforces_request_timeout_and_forbids_wildcard_bind() {
        let shared = SharedRing::new(RingConfig::default(), AgentMetadata::current("0.2.0-test"));
        let stop = Arc::new(AtomicBool::new(false));

        // Wildcard 0.0.0.0 must be rejected before bind
        assert!(matches!(
            AgentServer::start(
                "0.0.0.0:0".parse().expect("wildcard"),
                &shared,
                Arc::clone(&stop),
                ServerConfig::default(),
            ),
            Err(FleetError::ForbiddenBindAddr { .. })
        ));

        // Short timeout test for stalled client
        let cfg = ServerConfig {
            io_timeout: Duration::from_millis(150),
            ..ServerConfig::default()
        };
        let server =
            AgentServer::start("127.0.0.1:0".parse().expect("loopback"), &shared, stop, cfg)
                .expect("start server");
        let addr = server.local_addr();

        let mut stream = TcpStream::connect(addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        // Send incomplete header and stall
        stream
            .write_all(b"GET /v1/samples HTTP/1.1\r\nHost: localhost\r\n")
            .expect("write partial");
        let mut resp = String::new();
        stream.read_to_string(&mut resp).expect("read 408");
        assert!(resp.starts_with("HTTP/1.1 408 Request Timeout\r\n"));

        server.shutdown();
    }
}
