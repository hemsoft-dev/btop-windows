//! Portable, OS-neutral Fleet telemetry wire contract (`NodeSample`, `FleetSamplesResponse`,
//! and `NodeStatus`), implementing ADR-0004 and `GLOSSARY.md`.
//!
//! By design, Fleet payloads never include `Snapshot`, usernames, process lists, command lines,
//! GPUs, or OS services.

use serde::{Deserialize, Serialize};

use crate::DataState;

/// Major wire schema version for `GET /v1/samples`.
pub const FLEET_SCHEMA_VERSION: u32 = 1;

/// Default TCP port for the Tailscale Fleet Agent HTTP server.
pub const DEFAULT_FLEET_PORT: u16 = 9127;

/// Default Agent sampling interval in seconds (`5s`).
pub const DEFAULT_AGENT_INTERVAL_SECS: u64 = 5;

/// Minimum allowed Agent sampling interval in seconds (`1s`).
pub const MIN_AGENT_INTERVAL_SECS: u64 = 1;

/// Maximum allowed Agent sampling interval in seconds (`60s`).
pub const MAX_AGENT_INTERVAL_SECS: u64 = 60;

/// Default Agent in-memory `Ring` retention window in minutes (`10m`).
pub const DEFAULT_AGENT_RETENTION_MINS: u64 = 10;

/// Minimum allowed Agent `Ring` retention window in minutes (`1m`).
pub const MIN_AGENT_RETENTION_MINS: u64 = 1;

/// Maximum allowed Agent `Ring` retention window in minutes (`60m`).
pub const MAX_AGENT_RETENTION_MINS: u64 = 60;

/// Canonical `DataState::Unavailable` reason surfaced by Fleet View when an Agent reports an
/// unsupported major `schema_version`.
pub const INCOMPATIBLE_AGENT_REASON: &str = "incompatible agent";

/// Health state of one Fleet `Node` as classified by `Fleet View` (per `GLOSSARY.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// Agent is reachable and its newest `NodeSample` is no older than three sampling intervals.
    Online,
    /// A previous `NodeSample` is retained, but its age exceeds `3x interval` or the latest poll
    /// timed out.
    Stale,
    /// Agent cannot be reached (for example while a laptop is asleep).
    #[default]
    Offline,
}

/// Best-effort aggregate disk throughput across local storage volumes (bytes/sec).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiskIoRate {
    pub read_bytes_per_sec: u64,
    pub write_bytes_per_sec: u64,
}

/// Aggregate physical network interface throughput across active hardware NICs (bytes/sec).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NetIoRate {
    pub rx_bytes_per_sec: u64,
    pub tx_bytes_per_sec: u64,
}

/// A single point-in-time, OS-neutral telemetry reading for one Fleet `Node`.
///
/// Covers strictly CPU utilization, memory used (`total - available`) and total, worst-volume
/// disk fullness, best-effort aggregate disk read/write rates, physical NIC RX/TX rates, and
/// uptime. Never includes usernames, processes, command lines, GPUs, or OS services.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeSample {
    /// Agent-clock timestamp in milliseconds since Unix epoch (used as `Cursor`).
    pub timestamp_ms: u64,
    /// Aggregate CPU busy percentage (`0.0..=100.0`). `WarmingUp` on initial tick or sleep gap.
    pub cpu_busy_pct: DataState<f32>,
    /// Physical memory in use (`total - available`) in bytes.
    pub mem_used_bytes: u64,
    /// Total physical memory in bytes.
    pub mem_total_bytes: u64,
    /// Highest fullness percentage (`0.0..=100.0`) among included local storage volumes.
    pub disk_max_used_pct: DataState<f32>,
    /// Best-effort aggregate local disk read/write bytes/sec (`Unavailable` when unsupported by
    /// the host OS; `WarmingUp` on initial tick or across a `> 3x interval` sleep gap).
    pub disk_io: DataState<DiskIoRate>,
    /// Aggregate physical NIC RX/TX bytes/sec (`WarmingUp` on initial tick or across a sleep gap).
    pub net_io: DataState<NetIoRate>,
    /// Host uptime in seconds.
    pub uptime_secs: u64,
}

/// Wire response returned by `GET /v1/samples?since_ms=<cursor>`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FleetSamplesResponse {
    /// Wire schema major version (`FLEET_SCHEMA_VERSION`).
    pub schema_version: u32,
    /// Semantic version of the running `btop` / `btop-fleet` Agent binary.
    pub agent_version: String,
    /// Host operating system identifier (for example `"windows"`, `"linux"`, `"macos"`).
    pub os: String,
    /// Host CPU architecture identifier (for example `"x86_64"`, `"aarch64"`).
    pub arch: String,
    /// Configured Agent sampling interval in milliseconds.
    pub interval_ms: u64,
    /// Agent's own monotonic clock reading in milliseconds when building this response.
    pub now_ms: u64,
    /// Chronological `NodeSample` entries newer than the caller's `since_ms` cursor (or the full
    /// `Ring` when `since_ms == 0`).
    pub samples: Vec<NodeSample>,
}

impl FleetSamplesResponse {
    /// Returns `true` when this response uses the supported Fleet schema version.
    pub const fn is_schema_compatible(&self) -> bool {
        is_compatible_fleet_schema(self.schema_version)
    }

    /// Validates `schema_version`, returning `DataState::Unavailable("incompatible agent")` on
    /// mismatch per ADR-0004.
    pub fn check_compatibility(&self) -> Result<(), DataState<()>> {
        if self.is_schema_compatible() {
            Ok(())
        } else {
            Err(DataState::Unavailable(INCOMPATIBLE_AGENT_REASON.to_owned()))
        }
    }
}

/// Returns `true` when a Fleet Agent `schema_version` matches the supported major version.
pub const fn is_compatible_fleet_schema(schema_version: u32) -> bool {
    schema_version == FLEET_SCHEMA_VERSION
}

/// Classifies a Fleet `Node` as [`NodeStatus::Online`], [`NodeStatus::Stale`], or
/// [`NodeStatus::Offline`] following `GLOSSARY.md` and ADR-0004.
///
/// - `Online`: reachable, latest poll did not time out, and newest sample age `<= 3 * interval_ms`.
/// - `Stale`: a previous sample is retained, and either its age exceeds `3 * interval_ms` or the
///   latest poll timed out.
/// - `Offline`: Agent cannot be reached (or no sample has ever been received while failing/timed out).
pub const fn classify_node_status(
    sample_age_ms: Option<u64>,
    interval_ms: u64,
    reachable: bool,
    timed_out: bool,
) -> NodeStatus {
    let stale_threshold_ms = interval_ms.saturating_mul(3);
    match sample_age_ms {
        Some(age_ms) if reachable && !timed_out && age_ms <= stale_threshold_ms => {
            NodeStatus::Online
        }
        Some(_) if reachable || timed_out => NodeStatus::Stale,
        _ => NodeStatus::Offline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_sample_and_response_roundtrip_without_sensitive_fields() {
        let sample = NodeSample {
            timestamp_ms: 1_732_800_005_000,
            cpu_busy_pct: DataState::Available(27.5),
            mem_used_bytes: 12 * 1024 * 1024 * 1024,
            mem_total_bytes: 32 * 1024 * 1024 * 1024,
            disk_max_used_pct: DataState::Available(68.2),
            disk_io: DataState::Available(DiskIoRate {
                read_bytes_per_sec: 1_048_576,
                write_bytes_per_sec: 524_288,
            }),
            net_io: DataState::Available(NetIoRate {
                rx_bytes_per_sec: 2_000_000,
                tx_bytes_per_sec: 500_000,
            }),
            uptime_secs: 86_400,
        };
        let resp = FleetSamplesResponse {
            schema_version: FLEET_SCHEMA_VERSION,
            agent_version: "0.2.0".to_owned(),
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            interval_ms: 5_000,
            now_ms: 1_732_800_006_000,
            samples: vec![sample],
        };

        let json = serde_json::to_string(&resp).expect("serialize fleet response");
        let parsed_val: serde_json::Value =
            serde_json::from_str(&json).expect("parse fleet response value");

        // Enforce wire privacy boundary: zero usernames, processes, command lines, GPUs, services.
        for forbidden in [
            "username",
            "user",
            "processes",
            "procs",
            "command_line",
            "gpus",
            "services",
            "hostname",
        ] {
            assert!(
                !json.contains(&format!("\"{forbidden}\"")),
                "fleet wire payload must never contain field {forbidden:?}: {json}"
            );
        }
        assert_eq!(parsed_val["schema_version"], FLEET_SCHEMA_VERSION);
        assert_eq!(parsed_val["os"], "linux");
        assert_eq!(parsed_val["arch"], "x86_64");

        let decoded: FleetSamplesResponse =
            serde_json::from_str(&json).expect("deserialize fleet response");
        assert_eq!(decoded, resp);
        assert!(decoded.is_schema_compatible());
        assert!(decoded.check_compatibility().is_ok());
    }

    #[test]
    fn additive_future_fields_are_tolerated_and_incompatible_schema_is_flagged() {
        let future_json = r#"{
            "schema_version": 2,
            "agent_version": "0.9.0",
            "os": "macos",
            "arch": "aarch64",
            "interval_ms": 5000,
            "now_ms": 20000,
            "future_top_level_field": {"nested": true},
            "samples": [{
                "timestamp_ms": 15000,
                "cpu_busy_pct": {"state": "available", "data": 12.5},
                "mem_used_bytes": 1000,
                "mem_total_bytes": 4000,
                "disk_max_used_pct": {"state": "available", "data": 40.0},
                "disk_io": {"state": "unavailable", "data": "unsupported"},
                "net_io": {"state": "available", "data": {"rx_bytes_per_sec": 10, "tx_bytes_per_sec": 20, "extra_net": 1}},
                "uptime_secs": 99,
                "future_sample_metric": 42
            }]
        }"#;

        let decoded: FleetSamplesResponse =
            serde_json::from_str(future_json).expect("deserialize with additive fields");
        assert_eq!(decoded.schema_version, 2);
        assert!(!decoded.is_schema_compatible());
        assert_eq!(
            decoded.check_compatibility(),
            Err(DataState::Unavailable(INCOMPATIBLE_AGENT_REASON.to_owned()))
        );
        assert_eq!(decoded.samples.len(), 1);
        assert_eq!(decoded.samples[0].uptime_secs, 99);
    }

    #[test]
    fn classify_node_status_matches_glossary_thresholds() {
        // Online when reachable, no timeout, age <= 3 * interval (15_000 ms for 5s interval)
        assert_eq!(
            classify_node_status(Some(0), 5_000, true, false),
            NodeStatus::Online
        );
        assert_eq!(
            classify_node_status(Some(15_000), 5_000, true, false),
            NodeStatus::Online
        );

        // Stale when age > 3 * interval or latest poll timed out while retaining a sample
        assert_eq!(
            classify_node_status(Some(15_001), 5_000, true, false),
            NodeStatus::Stale
        );
        assert_eq!(
            classify_node_status(Some(5_000), 5_000, false, true),
            NodeStatus::Stale
        );

        // Offline when unreachable (e.g. connection refused / sleeping laptop) or never sampled
        assert_eq!(
            classify_node_status(Some(5_000), 5_000, false, false),
            NodeStatus::Offline
        );
        assert_eq!(
            classify_node_status(None, 5_000, true, false),
            NodeStatus::Offline
        );
        assert_eq!(
            classify_node_status(None, 5_000, false, true),
            NodeStatus::Offline
        );
    }
}
