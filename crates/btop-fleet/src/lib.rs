//! Permanently read-only Tailscale Fleet Agent `Ring`, Tailscale address scoping, and
//! synchronous HTTP server (`GET /v1/samples?since_ms=<cursor>`), implementing ADR-0004.
//!
//! This crate enforces `#![forbid(unsafe_code)]` and depends strictly on `btop-model` (never
//! `btop-sys` or any process-termination or service-control modules).

#![forbid(unsafe_code)]

pub mod net;
pub mod ring;
pub mod server;

use thiserror::Error;

pub use net::{
    BackoffConfig, DEFAULT_BACKOFF_INITIAL_MS, DEFAULT_BACKOFF_MAX_MS, canonicalize_ip,
    is_allowed_bind_ip, is_allowed_peer_ip, is_tailscale_ip, is_tailscale_ipv4, is_tailscale_ipv6,
    resolve_tailscale_bind_addr_with_backoff, select_tailscale_bind_ip, validate_bind_addr,
};
pub use ring::{AgentClock, Ring, RingConfig, apply_sleep_gap_guard, is_sleep_gap};
pub use server::{
    AgentMetadata, AgentServer, DEFAULT_IO_TIMEOUT, MAX_REQUEST_BYTES, ParsedRequest, ServerConfig,
    SharedRing, parse_http_request,
};

#[derive(Debug, Error)]
pub enum FleetError {
    #[error("invalid fleet configuration: {0}")]
    InvalidConfig(String),
    #[error("forbidden bind address {addr}: {reason}")]
    ForbiddenBindAddr { addr: String, reason: String },
    #[error("Tailscale interface detection stopped before a Tailscale address became available")]
    BindCancelled,
    #[error("failed to bind fleet agent listener on {addr}: {source}")]
    BindFailed {
        addr: String,
        #[source]
        source: std::io::Error,
    },
    #[error("fleet server I/O error: {0}")]
    Io(#[from] std::io::Error),
}
