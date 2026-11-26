//! Draft 2020-12 JSON Schema generation and conformance validation for headless `btop --json` outputs.

use serde_json::{Map, Value, json};

use crate::json_output::JSON_SCHEMA_VERSION;

/// Draft 2020-12 JSON Schema meta-schema URI.
pub const JSON_SCHEMA_DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// Valid mode tokens accepted by `--schema [MODE]`.
pub const VALID_SCHEMA_MODES: &[&str] = &["all", "snapshot", "summary", "diagnose", "error"];

/// Output contract subset selectable via `btop --schema [MODE]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchemaMode {
    #[default]
    All,
    Snapshot,
    Summary,
    Diagnose,
    Error,
}

impl SchemaMode {
    /// Parses a `--schema` mode string (`all`, `snapshot`, `summary`, `diagnose`, `error`).
    pub fn parse_str(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "snapshot" => Ok(Self::Snapshot),
            "summary" => Ok(Self::Summary),
            "diagnose" => Ok(Self::Diagnose),
            "error" => Ok(Self::Error),
            other => Err(format!(
                "unknown --schema mode '{other}': expected one of [{}]",
                VALID_SCHEMA_MODES.join(", ")
            )),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Snapshot => "snapshot",
            Self::Summary => "summary",
            Self::Diagnose => "diagnose",
            Self::Error => "error",
        }
    }
}

/// Generates a deterministic Draft 2020-12 JSON Schema document for the requested `mode`.
pub fn build_json_schema(mode: SchemaMode) -> Value {
    let defs = build_all_defs();
    let (title, description, root_body) = match mode {
        SchemaMode::All => (
            "btop-windows headless JSON contract (all modes)",
            "Draft 2020-12 schema covering btop --json snapshot, --summary, --diagnose, and stderr error envelopes.",
            json!({
                "oneOf": [
                    { "$ref": "#/$defs/JsonSnapshotEnvelope" },
                    { "$ref": "#/$defs/JsonSummaryEnvelope" },
                    { "$ref": "#/$defs/DiagnosticsReport" },
                    { "$ref": "#/$defs/JsonErrorEnvelope" }
                ]
            }),
        ),
        SchemaMode::Snapshot => (
            "btop-windows JsonSnapshotEnvelope",
            "Single-snapshot or NDJSON streaming envelope emitted by `btop --json`.",
            json!({ "$ref": "#/$defs/JsonSnapshotEnvelope" }),
        ),
        SchemaMode::Summary => (
            "btop-windows JsonSummaryEnvelope",
            "Aggregated time-window summary emitted by `btop --json --summary`.",
            json!({ "$ref": "#/$defs/JsonSummaryEnvelope" }),
        ),
        SchemaMode::Diagnose => (
            "btop-windows DiagnosticsReport",
            "Structured deterministic health and bottleneck diagnostics attached by `--diagnose`.",
            json!({ "$ref": "#/$defs/DiagnosticsReport" }),
        ),
        SchemaMode::Error => (
            "btop-windows JsonErrorEnvelope",
            "Structured error object emitted to stderr when `btop --json` fails.",
            json!({ "$ref": "#/$defs/JsonErrorEnvelope" }),
        ),
    };

    let mut root = Map::new();
    root.insert("$schema".to_owned(), json!(JSON_SCHEMA_DRAFT_2020_12));
    root.insert(
        "$id".to_owned(),
        json!(format!(
            "https://github.com/hemsoft-dev/btop-windows/schemas/v{JSON_SCHEMA_VERSION}/{}.schema.json",
            mode.as_str()
        )),
    );
    root.insert("title".to_owned(), json!(title));
    root.insert("description".to_owned(), json!(description));
    if let Value::Object(body_map) = root_body {
        for (k, v) in body_map {
            root.insert(k, v);
        }
    }
    root.insert("$defs".to_owned(), Value::Object(defs));
    Value::Object(root)
}

fn data_state_schema(inner: &Value) -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "required": ["state"],
                "additionalProperties": false,
                "properties": {
                    "state": { "const": "warming_up" }
                }
            },
            {
                "type": "object",
                "required": ["state", "data"],
                "additionalProperties": false,
                "properties": {
                    "state": { "const": "available" },
                    "data": inner
                }
            },
            {
                "type": "object",
                "required": ["state", "data"],
                "additionalProperties": false,
                "properties": {
                    "state": { "const": "stale" },
                    "data": {
                        "type": "object",
                        "required": ["value", "age_ms"],
                        "additionalProperties": false,
                        "properties": {
                            "value": inner,
                            "age_ms": { "type": "integer", "minimum": 0 }
                        }
                    }
                }
            },
            {
                "type": "object",
                "required": ["state"],
                "additionalProperties": false,
                "properties": {
                    "state": { "const": "denied" }
                }
            },
            {
                "type": "object",
                "required": ["state", "data"],
                "additionalProperties": false,
                "properties": {
                    "state": { "const": "unavailable" },
                    "data": { "type": "string" }
                }
            }
        ]
    })
}

fn build_all_defs() -> Map<String, Value> {
    let mut defs = Map::new();
    insert_envelope_defs(&mut defs);
    insert_core_telemetry_defs(&mut defs);
    insert_proc_and_service_defs(&mut defs);
    insert_summary_defs(&mut defs);
    insert_diagnostics_defs(&mut defs);
    defs
}

fn insert_envelope_defs(defs: &mut Map<String, Value>) {
    defs.insert(
        "JsonSnapshotEnvelope".to_owned(),
        json!({
            "type": "object",
            "required": [
                "schema_version",
                "btop_version",
                "captured_at_ms",
                "sample_window_ms",
                "hostname",
                "username",
                "sections"
            ],
            "additionalProperties": false,
            "properties": {
                "schema_version": { "const": JSON_SCHEMA_VERSION },
                "btop_version": { "type": "string" },
                "captured_at_ms": { "type": "integer", "minimum": 0 },
                "sample_window_ms": { "type": "integer", "minimum": 0 },
                "hostname": { "type": "string" },
                "username": { "type": "string" },
                "sections": { "type": "array", "items": { "type": "string" } },
                "total_phys_mem_bytes": { "type": "integer", "minimum": 0 },
                "cpu": data_state_schema(&json!({ "$ref": "#/$defs/CpuSnapshot" })),
                "mem": data_state_schema(&json!({ "$ref": "#/$defs/MemSnapshot" })),
                "disks": data_state_schema(&json!({ "type": "array", "items": { "$ref": "#/$defs/DiskSnapshot" } })),
                "net": data_state_schema(&json!({ "type": "array", "items": { "$ref": "#/$defs/NetInterfaceSnapshot" } })),
                "procs": data_state_schema(&json!({ "type": "array", "items": { "$ref": "#/$defs/ProcessEntry" } })),
                "proc_tree": data_state_schema(&json!({ "type": "array", "items": { "$ref": "#/$defs/ProcessTreeNode" } })),
                "procs_meta": { "$ref": "#/$defs/ProcsQueryMeta" },
                "services": data_state_schema(&json!({ "type": "array", "items": { "$ref": "#/$defs/ServiceJsonEntry" } })),
                "services_meta": { "$ref": "#/$defs/ServicesQueryMeta" },
                "gpus": {
                    "type": "array",
                    "items": data_state_schema(&json!({ "$ref": "#/$defs/GpuSnapshot" }))
                },
                "battery": data_state_schema(&json!({ "$ref": "#/$defs/BatterySnapshot" })),
                "diagnostics": { "$ref": "#/$defs/DiagnosticsReport" }
            }
        }),
    );

    defs.insert(
        "JsonSummaryEnvelope".to_owned(),
        json!({
            "type": "object",
            "required": [
                "schema_version",
                "btop_version",
                "started_at_ms",
                "ended_at_ms",
                "window_ms",
                "sample_count",
                "hostname",
                "username",
                "sections"
            ],
            "additionalProperties": false,
            "properties": {
                "schema_version": { "const": JSON_SCHEMA_VERSION },
                "btop_version": { "type": "string" },
                "started_at_ms": { "type": "integer", "minimum": 0 },
                "ended_at_ms": { "type": "integer", "minimum": 0 },
                "window_ms": { "type": "integer", "minimum": 0 },
                "sample_count": { "type": "integer", "minimum": 0 },
                "hostname": { "type": "string" },
                "username": { "type": "string" },
                "sections": { "type": "array", "items": { "type": "string" } },
                "cpu": { "$ref": "#/$defs/CpuSummary" },
                "mem": { "$ref": "#/$defs/MemSummary" },
                "disks": { "type": "array", "items": { "$ref": "#/$defs/DiskSummary" } },
                "net": { "type": "array", "items": { "$ref": "#/$defs/NetSummary" } },
                "procs": { "$ref": "#/$defs/ProcsWindowSummary" },
                "gpus": { "type": "array", "items": { "$ref": "#/$defs/GpuSummary" } },
                "battery": { "$ref": "#/$defs/BatterySummary" },
                "series": { "type": "array", "items": { "$ref": "#/$defs/SummarySeriesPoint" } },
                "diagnostics": { "$ref": "#/$defs/DiagnosticsReport" }
            }
        }),
    );

    defs.insert(
        "JsonErrorEnvelope".to_owned(),
        json!({
            "type": "object",
            "required": ["schema_version", "error"],
            "additionalProperties": false,
            "properties": {
                "schema_version": { "const": JSON_SCHEMA_VERSION },
                "error": {
                    "type": "object",
                    "required": ["code", "message"],
                    "additionalProperties": false,
                    "properties": {
                        "code": { "type": "string" },
                        "message": { "type": "string" }
                    }
                }
            }
        }),
    );
}

fn insert_core_telemetry_defs(defs: &mut Map<String, Value>) {
    defs.insert(
        "LogicalCpuId".to_owned(),
        json!({
            "type": "object",
            "required": ["group", "number"],
            "additionalProperties": false,
            "properties": {
                "group": { "type": "integer", "minimum": 0 },
                "number": { "type": "integer", "minimum": 0 }
            }
        }),
    );

    defs.insert(
        "CoreSample".to_owned(),
        json!({
            "type": "object",
            "required": ["id", "busy_pct", "kind", "temp_celsius"],
            "additionalProperties": false,
            "properties": {
                "id": { "$ref": "#/$defs/LogicalCpuId" },
                "busy_pct": { "type": "number" },
                "kind": { "type": "string", "enum": ["uniform", "performance", "efficiency"] },
                "temp_celsius": { "type": ["number", "null"] }
            }
        }),
    );

    defs.insert(
        "CpuSnapshot".to_owned(),
        json!({
            "type": "object",
            "required": [
                "model_name", "total_busy_pct", "utility_pct", "freq_mhz",
                "package_temp_celsius", "package_watts", "run_queue_ewma", "uptime_secs", "cores"
            ],
            "additionalProperties": false,
            "properties": {
                "model_name": { "type": "string" },
                "total_busy_pct": { "type": "number" },
                "utility_pct": { "type": ["number", "null"] },
                "freq_mhz": { "type": ["integer", "null"] },
                "freq_range_mhz": {
                    "type": ["array", "null"],
                    "prefixItems": [
                        { "type": "integer", "minimum": 0 },
                        { "type": "integer", "minimum": 0 }
                    ],
                    "minItems": 2,
                    "maxItems": 2
                },
                "package_temp_celsius": { "type": ["number", "null"] },
                "package_watts": { "type": ["number", "null"] },
                "run_queue_ewma": {
                    "type": "array",
                    "prefixItems": [
                        { "type": "number" },
                        { "type": "number" },
                        { "type": "number" }
                    ],
                    "minItems": 3,
                    "maxItems": 3
                },
                "uptime_secs": { "type": "integer", "minimum": 0 },
                "cores": { "type": "array", "items": { "$ref": "#/$defs/CoreSample" } }
            }
        }),
    );

    defs.insert(
        "MemSnapshot".to_owned(),
        json!({
            "type": "object",
            "required": [
                "total_bytes", "used_bytes", "available_bytes", "cached_bytes", "free_bytes",
                "commit_used_bytes", "commit_limit_bytes", "pagefile_used_bytes",
                "pagefile_total_bytes", "compressed_bytes"
            ],
            "additionalProperties": false,
            "properties": {
                "total_bytes": { "type": "integer", "minimum": 0 },
                "used_bytes": { "type": "integer", "minimum": 0 },
                "available_bytes": { "type": "integer", "minimum": 0 },
                "cached_bytes": { "type": "integer", "minimum": 0 },
                "free_bytes": { "type": "integer", "minimum": 0 },
                "commit_used_bytes": { "type": "integer", "minimum": 0 },
                "commit_limit_bytes": { "type": "integer", "minimum": 0 },
                "pagefile_used_bytes": { "type": "integer", "minimum": 0 },
                "pagefile_total_bytes": { "type": "integer", "minimum": 0 },
                "compressed_bytes": { "type": ["integer", "null"] }
            }
        }),
    );

    insert_device_telemetry_defs(defs);
}

fn insert_device_telemetry_defs(defs: &mut Map<String, Value>) {
    defs.insert(
        "DiskSnapshot".to_owned(),
        json!({
            "type": "object",
            "required": ["mount", "label", "fs_type", "total_bytes", "used_bytes", "free_bytes"],
            "additionalProperties": false,
            "properties": {
                "mount": { "type": "string" },
                "label": { "type": "string" },
                "fs_type": { "type": "string" },
                "physical_drive": { "type": "integer", "minimum": 0 },
                "total_bytes": { "type": "integer", "minimum": 0 },
                "used_bytes": { "type": "integer", "minimum": 0 },
                "free_bytes": { "type": "integer", "minimum": 0 },
                "read_bytes_per_sec": { "type": "integer", "minimum": 0 },
                "write_bytes_per_sec": { "type": "integer", "minimum": 0 },
                "busy_pct": { "type": "number" }
            }
        }),
    );

    defs.insert(
        "NetInterfaceSnapshot".to_owned(),
        json!({
            "type": "object",
            "required": [
                "luid", "name", "description", "is_up",
                "rx_bytes_per_sec", "tx_bytes_per_sec", "rx_total_bytes", "tx_total_bytes"
            ],
            "additionalProperties": false,
            "properties": {
                "luid": { "type": "integer", "minimum": 0 },
                "name": { "type": "string" },
                "description": { "type": "string" },
                "is_up": { "type": "boolean" },
                "ipv4": { "type": "string" },
                "ipv6": { "type": "string" },
                "rx_bytes_per_sec": { "type": "integer", "minimum": 0 },
                "tx_bytes_per_sec": { "type": "integer", "minimum": 0 },
                "rx_total_bytes": { "type": "integer", "minimum": 0 },
                "tx_total_bytes": { "type": "integer", "minimum": 0 },
                "link_speed_bps": { "type": "integer", "minimum": 0 }
            }
        }),
    );

    defs.insert(
        "GpuSnapshot".to_owned(),
        json!({
            "type": "object",
            "required": [
                "index", "name", "vendor", "pci_bdf", "utilization_pct", "vram_used_bytes",
                "vram_total_bytes", "core_clock_mhz", "mem_clock_mhz", "temp_celsius",
                "power_watts", "power_limit_watts", "pcie_tx_kbs", "pcie_rx_kbs",
                "encoder_pct", "decoder_pct"
            ],
            "additionalProperties": false,
            "properties": {
                "index": { "type": "integer", "minimum": 0 },
                "name": { "type": "string" },
                "vendor": { "type": "string" },
                "pci_bdf": { "type": ["string", "null"] },
                "utilization_pct": { "type": ["number", "null"] },
                "vram_used_bytes": { "type": ["integer", "null"] },
                "vram_total_bytes": { "type": ["integer", "null"] },
                "shared_vram_used_bytes": { "type": "integer", "minimum": 0 },
                "shared_vram_total_bytes": { "type": "integer", "minimum": 0 },
                "luid": { "type": "integer", "minimum": 0 },
                "core_clock_mhz": { "type": ["integer", "null"] },
                "mem_clock_mhz": { "type": ["integer", "null"] },
                "temp_celsius": { "type": ["number", "null"] },
                "temp_max_celsius": { "type": "number" },
                "power_watts": { "type": ["number", "null"] },
                "power_limit_watts": { "type": ["number", "null"] },
                "pstate": { "type": "string" },
                "pcie_tx_kbs": { "type": ["integer", "null"] },
                "pcie_rx_kbs": { "type": ["integer", "null"] },
                "encoder_pct": { "type": ["number", "null"] },
                "decoder_pct": { "type": ["number", "null"] }
            }
        }),
    );

    defs.insert(
        "BatterySnapshot".to_owned(),
        json!({
            "type": "object",
            "required": ["charge_pct", "status", "watts", "remaining_secs"],
            "additionalProperties": false,
            "properties": {
                "charge_pct": { "type": "integer", "minimum": 0, "maximum": 100 },
                "status": { "type": "string" },
                "watts": { "type": ["number", "null"] },
                "remaining_secs": { "type": ["integer", "null"] }
            }
        }),
    );
}

fn insert_proc_and_service_defs(defs: &mut Map<String, Value>) {
    defs.insert(
        "ProcessIdentity".to_owned(),
        json!({
            "type": "object",
            "required": ["pid", "create_time_100ns"],
            "additionalProperties": false,
            "properties": {
                "pid": { "type": "integer", "minimum": 0 },
                "create_time_100ns": { "type": "integer", "minimum": 0 }
            }
        }),
    );

    defs.insert(
        "ProcessEntry".to_owned(),
        json!({
            "type": "object",
            "required": [
                "identity", "parent_pid", "name", "command_line", "user",
                "threads", "cpu_pct", "working_set_private_bytes",
                "io_read_bytes", "io_write_bytes", "is_protected"
            ],
            "additionalProperties": false,
            "properties": {
                "identity": { "$ref": "#/$defs/ProcessIdentity" },
                "parent_pid": { "type": "integer", "minimum": 0 },
                "name": { "type": "string" },
                "image_path": { "type": "string" },
                "command_line": { "type": ["string", "null"] },
                "command_line_denied": { "type": "boolean" },
                "user": { "type": ["string", "null"] },
                "user_denied": { "type": "boolean" },
                "threads": { "type": "integer", "minimum": 0 },
                "cpu_pct": { "type": "number" },
                "working_set_private_bytes": { "type": "integer", "minimum": 0 },
                "io_read_bytes": { "type": "integer", "minimum": 0 },
                "io_write_bytes": { "type": "integer", "minimum": 0 },
                "is_protected": { "type": "boolean" }
            }
        }),
    );

    defs.insert(
        "ProcessTreeNode".to_owned(),
        json!({
            "type": "object",
            "required": [
                "identity", "parent_pid", "name", "command_line", "user",
                "threads", "cpu_pct", "working_set_private_bytes",
                "io_read_bytes", "io_write_bytes", "is_protected",
                "depth", "subtree_cpu_pct", "subtree_working_set_bytes", "children"
            ],
            "additionalProperties": false,
            "properties": {
                "identity": { "$ref": "#/$defs/ProcessIdentity" },
                "parent_pid": { "type": "integer", "minimum": 0 },
                "name": { "type": "string" },
                "image_path": { "type": "string" },
                "command_line": { "type": ["string", "null"] },
                "command_line_denied": { "type": "boolean" },
                "user": { "type": ["string", "null"] },
                "user_denied": { "type": "boolean" },
                "threads": { "type": "integer", "minimum": 0 },
                "cpu_pct": { "type": "number" },
                "working_set_private_bytes": { "type": "integer", "minimum": 0 },
                "io_read_bytes": { "type": "integer", "minimum": 0 },
                "io_write_bytes": { "type": "integer", "minimum": 0 },
                "is_protected": { "type": "boolean" },
                "depth": { "type": "integer", "minimum": 0 },
                "subtree_cpu_pct": { "type": "number" },
                "subtree_working_set_bytes": { "type": "integer", "minimum": 0 },
                "children": { "type": "array", "items": { "$ref": "#/$defs/ProcessTreeNode" } }
            }
        }),
    );

    defs.insert(
        "ProcsQueryMeta".to_owned(),
        json!({
            "type": "object",
            "required": ["total_processes", "total_matched", "returned", "truncated", "sort", "reversed", "tree"],
            "additionalProperties": false,
            "properties": {
                "total_processes": { "type": "integer", "minimum": 0 },
                "total_matched": { "type": "integer", "minimum": 0 },
                "returned": { "type": "integer", "minimum": 0 },
                "truncated": { "type": "boolean" },
                "sort": { "type": "string", "enum": ["cpu", "mem", "pid", "name", "threads", "io"] },
                "reversed": { "type": "boolean" },
                "tree": { "type": "boolean" },
                "top": { "type": "integer", "minimum": 0 },
                "filter": { "type": "string" },
                "pid": { "type": "integer", "minimum": 0 }
            }
        }),
    );

    insert_service_defs(defs);
}

fn insert_service_defs(defs: &mut Map<String, Value>) {
    defs.insert(
        "ServiceLinkedProcess".to_owned(),
        json!({
            "type": "object",
            "required": ["pid", "name", "cpu_pct", "working_set_private_bytes", "threads"],
            "additionalProperties": false,
            "properties": {
                "pid": { "type": "integer", "minimum": 0 },
                "name": { "type": "string" },
                "cpu_pct": { "type": "number" },
                "working_set_private_bytes": { "type": "integer", "minimum": 0 },
                "threads": { "type": "integer", "minimum": 0 }
            }
        }),
    );

    defs.insert(
        "ServiceJsonEntry".to_owned(),
        json!({
            "type": "object",
            "required": ["name", "display_name", "state", "start_type", "pid"],
            "additionalProperties": false,
            "properties": {
                "name": { "type": "string" },
                "display_name": { "type": "string" },
                "state": { "type": "string" },
                "start_type": { "type": "string" },
                "pid": { "type": ["integer", "null"] },
                "process": { "$ref": "#/$defs/ServiceLinkedProcess" }
            }
        }),
    );

    defs.insert(
        "ServicesQueryMeta".to_owned(),
        json!({
            "type": "object",
            "required": ["total_services", "total_matched", "returned", "state_filter"],
            "additionalProperties": false,
            "properties": {
                "total_services": { "type": "integer", "minimum": 0 },
                "total_matched": { "type": "integer", "minimum": 0 },
                "returned": { "type": "integer", "minimum": 0 },
                "state_filter": { "type": "string", "enum": ["all", "running", "stopped"] },
                "filter": { "type": "string" }
            }
        }),
    );
}

fn insert_summary_defs(defs: &mut Map<String, Value>) {
    defs.insert(
        "ScalarStats".to_owned(),
        json!({
            "type": "object",
            "required": ["count", "min", "avg", "p95", "max"],
            "additionalProperties": false,
            "properties": {
                "count": { "type": "integer", "minimum": 0 },
                "min": { "type": "number" },
                "avg": { "type": "number" },
                "p95": { "type": "number" },
                "max": { "type": "number" }
            }
        }),
    );

    defs.insert(
        "U64Stats".to_owned(),
        json!({
            "type": "object",
            "required": ["count", "min", "avg", "p95", "max"],
            "additionalProperties": false,
            "properties": {
                "count": { "type": "integer", "minimum": 0 },
                "min": { "type": "integer", "minimum": 0 },
                "avg": { "type": "integer", "minimum": 0 },
                "p95": { "type": "integer", "minimum": 0 },
                "max": { "type": "integer", "minimum": 0 }
            }
        }),
    );

    defs.insert(
        "CpuSummary".to_owned(),
        json!({
            "type": "object",
            "required": ["model_name", "logical_cores", "samples_available", "total_busy_pct", "run_queue_1m"],
            "additionalProperties": false,
            "properties": {
                "model_name": { "type": "string" },
                "logical_cores": { "type": "integer", "minimum": 0 },
                "samples_available": { "type": "integer", "minimum": 0 },
                "total_busy_pct": { "$ref": "#/$defs/ScalarStats" },
                "utility_pct": { "$ref": "#/$defs/ScalarStats" },
                "freq_mhz": { "$ref": "#/$defs/ScalarStats" },
                "package_temp_celsius": { "$ref": "#/$defs/ScalarStats" },
                "package_watts": { "$ref": "#/$defs/ScalarStats" },
                "run_queue_1m": { "$ref": "#/$defs/ScalarStats" }
            }
        }),
    );

    defs.insert(
        "MemSummary".to_owned(),
        json!({
            "type": "object",
            "required": [
                "total_bytes", "samples_available", "used_bytes", "available_bytes",
                "commit_used_bytes", "commit_limit_bytes", "pagefile_used_bytes", "pagefile_total_bytes"
            ],
            "additionalProperties": false,
            "properties": {
                "total_bytes": { "type": "integer", "minimum": 0 },
                "samples_available": { "type": "integer", "minimum": 0 },
                "used_bytes": { "$ref": "#/$defs/U64Stats" },
                "available_bytes": { "$ref": "#/$defs/U64Stats" },
                "commit_used_bytes": { "$ref": "#/$defs/U64Stats" },
                "commit_limit_bytes": { "$ref": "#/$defs/U64Stats" },
                "pagefile_used_bytes": { "$ref": "#/$defs/U64Stats" },
                "pagefile_total_bytes": { "type": "integer", "minimum": 0 },
                "compressed_bytes": { "$ref": "#/$defs/U64Stats" }
            }
        }),
    );

    insert_summary_device_and_series_defs(defs);
}

fn insert_summary_device_and_series_defs(defs: &mut Map<String, Value>) {
    defs.insert(
        "DiskSummary".to_owned(),
        json!({
            "type": "object",
            "required": ["mount", "label", "fs_type", "samples_available", "total_bytes", "free_bytes"],
            "additionalProperties": false,
            "properties": {
                "mount": { "type": "string" },
                "label": { "type": "string" },
                "fs_type": { "type": "string" },
                "physical_drive": { "type": "integer", "minimum": 0 },
                "samples_available": { "type": "integer", "minimum": 0 },
                "total_bytes": { "type": "integer", "minimum": 0 },
                "free_bytes": { "$ref": "#/$defs/U64Stats" },
                "read_bytes_per_sec": { "$ref": "#/$defs/U64Stats" },
                "write_bytes_per_sec": { "$ref": "#/$defs/U64Stats" },
                "busy_pct": { "$ref": "#/$defs/ScalarStats" }
            }
        }),
    );

    defs.insert(
        "NetSummary".to_owned(),
        json!({
            "type": "object",
            "required": [
                "luid", "name", "description", "is_up", "samples_available",
                "rx_bytes_per_sec", "tx_bytes_per_sec", "rx_delta_bytes", "tx_delta_bytes"
            ],
            "additionalProperties": false,
            "properties": {
                "luid": { "type": "integer", "minimum": 0 },
                "name": { "type": "string" },
                "description": { "type": "string" },
                "is_up": { "type": "boolean" },
                "link_speed_bps": { "type": "integer", "minimum": 0 },
                "samples_available": { "type": "integer", "minimum": 0 },
                "rx_bytes_per_sec": { "$ref": "#/$defs/U64Stats" },
                "tx_bytes_per_sec": { "$ref": "#/$defs/U64Stats" },
                "rx_delta_bytes": { "type": "integer", "minimum": 0 },
                "tx_delta_bytes": { "type": "integer", "minimum": 0 }
            }
        }),
    );

    defs.insert(
        "GpuSummary".to_owned(),
        json!({
            "type": "object",
            "required": ["index", "name", "vendor", "samples_available"],
            "additionalProperties": false,
            "properties": {
                "index": { "type": "integer", "minimum": 0 },
                "name": { "type": "string" },
                "vendor": { "type": "string" },
                "samples_available": { "type": "integer", "minimum": 0 },
                "utilization_pct": { "$ref": "#/$defs/ScalarStats" },
                "vram_used_bytes": { "$ref": "#/$defs/U64Stats" },
                "vram_total_bytes": { "type": "integer", "minimum": 0 },
                "power_watts": { "$ref": "#/$defs/ScalarStats" },
                "power_limit_watts": { "type": "number" },
                "temp_celsius": { "$ref": "#/$defs/ScalarStats" },
                "temp_max_celsius": { "type": "number" },
                "core_clock_mhz": { "$ref": "#/$defs/ScalarStats" }
            }
        }),
    );

    defs.insert(
        "BatterySummary".to_owned(),
        json!({
            "type": "object",
            "required": ["samples_available", "charge_pct", "last_status"],
            "additionalProperties": false,
            "properties": {
                "samples_available": { "type": "integer", "minimum": 0 },
                "charge_pct": { "$ref": "#/$defs/ScalarStats" },
                "watts": { "$ref": "#/$defs/ScalarStats" },
                "last_status": { "type": "string" }
            }
        }),
    );

    insert_summary_proc_and_series_defs(defs);
}

fn insert_summary_proc_and_series_defs(defs: &mut Map<String, Value>) {
    defs.insert(
        "ProcessWindowEntry".to_owned(),
        json!({
            "type": "object",
            "required": [
                "pid", "create_time_100ns", "name", "samples_seen",
                "avg_cpu_pct", "peak_cpu_pct", "avg_working_set_bytes",
                "peak_working_set_bytes", "io_read_delta_bytes", "io_write_delta_bytes"
            ],
            "additionalProperties": false,
            "properties": {
                "pid": { "type": "integer", "minimum": 0 },
                "create_time_100ns": { "type": "integer", "minimum": 0 },
                "name": { "type": "string" },
                "command_line": { "type": "string" },
                "user": { "type": "string" },
                "samples_seen": { "type": "integer", "minimum": 0 },
                "avg_cpu_pct": { "type": "number" },
                "peak_cpu_pct": { "type": "number" },
                "avg_working_set_bytes": { "type": "integer", "minimum": 0 },
                "peak_working_set_bytes": { "type": "integer", "minimum": 0 },
                "io_read_delta_bytes": { "type": "integer", "minimum": 0 },
                "io_write_delta_bytes": { "type": "integer", "minimum": 0 }
            }
        }),
    );

    defs.insert(
        "ProcsWindowSummary".to_owned(),
        json!({
            "type": "object",
            "required": ["samples_available", "total_unique_processes", "top_limit", "by_cpu", "by_mem"],
            "additionalProperties": false,
            "properties": {
                "samples_available": { "type": "integer", "minimum": 0 },
                "total_unique_processes": { "type": "integer", "minimum": 0 },
                "top_limit": { "type": "integer", "minimum": 0 },
                "by_cpu": { "type": "array", "items": { "$ref": "#/$defs/ProcessWindowEntry" } },
                "by_mem": { "type": "array", "items": { "$ref": "#/$defs/ProcessWindowEntry" } }
            }
        }),
    );

    defs.insert(
        "SummarySeriesPoint".to_owned(),
        json!({
            "type": "object",
            "required": ["captured_at_ms"],
            "additionalProperties": false,
            "properties": {
                "captured_at_ms": { "type": "integer", "minimum": 0 },
                "cpu_busy_pct": { "type": "number" },
                "cpu_freq_mhz": { "type": "integer", "minimum": 0 },
                "mem_used_bytes": { "type": "integer", "minimum": 0 },
                "mem_available_bytes": { "type": "integer", "minimum": 0 },
                "mem_commit_used_bytes": { "type": "integer", "minimum": 0 },
                "disk_read_bytes_per_sec": { "type": "integer", "minimum": 0 },
                "disk_write_bytes_per_sec": { "type": "integer", "minimum": 0 },
                "disk_max_busy_pct": { "type": "number" },
                "net_rx_bytes_per_sec": { "type": "integer", "minimum": 0 },
                "net_tx_bytes_per_sec": { "type": "integer", "minimum": 0 },
                "gpu_max_util_pct": { "type": "number" },
                "gpu_max_temp_celsius": { "type": "number" },
                "gpu_total_power_watts": { "type": "number" }
            }
        }),
    );
}

fn insert_diagnostics_defs(defs: &mut Map<String, Value>) {
    defs.insert(
        "DiagnosticEntity".to_owned(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "pid": { "type": "integer", "minimum": 0 },
                "create_time_100ns": { "type": "integer", "minimum": 0 },
                "name": { "type": "string" },
                "mount": { "type": "string" },
                "gpu_index": { "type": "integer", "minimum": 0 },
                "gpu_name": { "type": "string" },
                "interface": { "type": "string" },
                "luid": { "type": "integer", "minimum": 0 }
            }
        }),
    );

    defs.insert(
        "DiagnosticEvidence".to_owned(),
        json!({
            "type": "object",
            "required": ["metric", "observed", "threshold", "unit"],
            "additionalProperties": false,
            "properties": {
                "metric": { "type": "string" },
                "observed": { "type": "number" },
                "threshold": { "type": "number" },
                "unit": { "type": "string" },
                "entity": { "$ref": "#/$defs/DiagnosticEntity" }
            }
        }),
    );

    defs.insert(
        "DiagnosticFinding".to_owned(),
        json!({
            "type": "object",
            "required": ["code", "severity", "category", "summary", "evidence"],
            "additionalProperties": false,
            "properties": {
                "code": { "type": "string" },
                "severity": { "type": "string", "enum": ["info", "warning", "critical"] },
                "category": { "type": "string", "enum": ["cpu", "memory", "gpu", "disk", "network", "process"] },
                "summary": { "type": "string" },
                "evidence": { "$ref": "#/$defs/DiagnosticEvidence" }
            }
        }),
    );

    defs.insert(
        "SkippedCheck".to_owned(),
        json!({
            "type": "object",
            "required": ["check", "category", "reason"],
            "additionalProperties": false,
            "properties": {
                "check": { "type": "string" },
                "category": { "type": "string", "enum": ["cpu", "memory", "gpu", "disk", "network", "process"] },
                "reason": { "type": "string" }
            }
        }),
    );

    defs.insert(
        "DiagnosticsReport".to_owned(),
        json!({
            "type": "object",
            "required": ["overall_status", "checks_evaluated", "skipped_checks", "findings"],
            "additionalProperties": false,
            "properties": {
                "overall_status": { "type": "string", "enum": ["healthy", "warning", "critical"] },
                "checks_evaluated": { "type": "array", "items": { "type": "string" } },
                "skipped_checks": { "type": "array", "items": { "$ref": "#/$defs/SkippedCheck" } },
                "findings": { "type": "array", "items": { "$ref": "#/$defs/DiagnosticFinding" } }
            }
        }),
    );
}

/// Validates a serialized JSON `instance` against a Draft 2020-12 schema emitted by
/// [`build_json_schema`], enforcing `$ref`, `oneOf`, `const`, `enum`, `type`, `required`,
/// `properties`, `additionalProperties: false`, `items`, `prefixItems`, and numeric bounds.
pub fn validate_against_schema(schema: &Value, instance: &Value) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    validate_node(schema, schema, instance, "$", &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn resolve_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let name = reference.strip_prefix("#/$defs/")?;
    root.get("$defs")?.get(name)
}

fn validate_node(
    root: &Value,
    node: &Value,
    instance: &Value,
    path: &str,
    errors: &mut Vec<String>,
) {
    let Some(obj) = node.as_object() else {
        return;
    };

    if let Some(ref_str) = obj.get("$ref").and_then(Value::as_str) {
        if let Some(target) = resolve_ref(root, ref_str) {
            validate_node(root, target, instance, path, errors);
        } else {
            errors.push(format!("{path}: unresolved $ref '{ref_str}'"));
        }
        return;
    }

    if let Some(branches) = obj.get("oneOf").and_then(Value::as_array) {
        let mut matched = 0usize;
        for branch in branches {
            let mut branch_errs = Vec::new();
            validate_node(root, branch, instance, path, &mut branch_errs);
            if branch_errs.is_empty() {
                matched += 1;
            }
        }
        if matched != 1 {
            errors.push(format!(
                "{path}: expected exactly 1 matching oneOf branch, got {matched}"
            ));
        }
        return;
    }

    if let Some(expected) = obj.get("const")
        && instance != expected
    {
        errors.push(format!("{path}: expected const {expected}, got {instance}"));
        return;
    }

    if let Some(allowed) = obj.get("enum").and_then(Value::as_array)
        && !allowed.contains(instance)
    {
        errors.push(format!("{path}: value {instance} not in enum {allowed:?}"));
        return;
    }

    if let Some(type_spec) = obj.get("type")
        && !matches_type(type_spec, instance)
    {
        errors.push(format!("{path}: expected type {type_spec}, got {instance}"));
        return;
    }

    validate_numeric_bounds(obj, instance, path, errors);
    validate_object(root, obj, instance, path, errors);
    validate_array(root, obj, instance, path, errors);
}

fn matches_type(spec: &Value, instance: &Value) -> bool {
    match spec {
        Value::String(s) => matches_single_type(s, instance),
        Value::Array(arr) => arr
            .iter()
            .filter_map(Value::as_str)
            .any(|t| matches_single_type(t, instance)),
        _ => true,
    }
}

fn matches_single_type(name: &str, instance: &Value) -> bool {
    match name {
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        "boolean" => instance.is_boolean(),
        "null" => instance.is_null(),
        "integer" => instance.is_i64() || instance.is_u64(),
        "number" => instance.is_number(),
        _ => false,
    }
}

fn validate_numeric_bounds(
    obj: &Map<String, Value>,
    instance: &Value,
    path: &str,
    errors: &mut Vec<String>,
) {
    let Some(val) = instance.as_f64() else {
        return;
    };
    if let Some(min) = obj.get("minimum").and_then(Value::as_f64)
        && val < min
    {
        errors.push(format!("{path}: value {val} < minimum {min}"));
    }
    if let Some(max) = obj.get("maximum").and_then(Value::as_f64)
        && val > max
    {
        errors.push(format!("{path}: value {val} > maximum {max}"));
    }
}

fn validate_object(
    root: &Value,
    schema_obj: &Map<String, Value>,
    instance: &Value,
    path: &str,
    errors: &mut Vec<String>,
) {
    let Some(map) = instance.as_object() else {
        return;
    };
    if let Some(required) = schema_obj.get("required").and_then(Value::as_array) {
        for req in required.iter().filter_map(Value::as_str) {
            if !map.contains_key(req) {
                errors.push(format!("{path}: missing required property '{req}'"));
            }
        }
    }
    let props = schema_obj.get("properties").and_then(Value::as_object);
    let forbid_extra = schema_obj
        .get("additionalProperties")
        .and_then(Value::as_bool)
        == Some(false);

    for (key, val) in map {
        let child_path = format!("{path}.{key}");
        if let Some(prop_schema) = props.and_then(|p| p.get(key)) {
            validate_node(root, prop_schema, val, &child_path, errors);
        } else if forbid_extra {
            errors.push(format!("{path}: unexpected additional property '{key}'"));
        }
    }
}

fn validate_array(
    root: &Value,
    schema_obj: &Map<String, Value>,
    instance: &Value,
    path: &str,
    errors: &mut Vec<String>,
) {
    let Some(arr) = instance.as_array() else {
        return;
    };
    if let Some(min_items) = schema_obj.get("minItems").and_then(Value::as_u64)
        && (arr.len() as u64) < min_items
    {
        errors.push(format!(
            "{path}: array len {} < minItems {min_items}",
            arr.len()
        ));
    }
    if let Some(max_items) = schema_obj.get("maxItems").and_then(Value::as_u64)
        && (arr.len() as u64) > max_items
    {
        errors.push(format!(
            "{path}: array len {} > maxItems {max_items}",
            arr.len()
        ));
    }
    if let Some(prefix) = schema_obj.get("prefixItems").and_then(Value::as_array) {
        for (idx, (item_schema, item_val)) in prefix.iter().zip(arr.iter()).enumerate() {
            validate_node(
                root,
                item_schema,
                item_val,
                &format!("{path}[{idx}]"),
                errors,
            );
        }
    }
    if let Some(item_schema) = schema_obj.get("items") {
        for (idx, item_val) in arr.iter().enumerate() {
            validate_node(
                root,
                item_schema,
                item_val,
                &format!("{path}[{idx}]"),
                errors,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::win11_16core_dual_gpu_desktop;
    use crate::{
        BatterySnapshot, DataState, HeadlessQuery, JsonErrorEnvelope, JsonSections,
        JsonSnapshotEnvelope, JsonSummaryEnvelope, QueryFilter, ServiceEntry,
    };

    #[test]
    fn schema_validates_all_data_state_variants_envelopes_and_catches_drift() {
        let all_schema = build_json_schema(SchemaMode::All);
        assert_eq!(all_schema["$schema"], JSON_SCHEMA_DRAFT_2020_12);

        // 1. Committed fixture snapshot with tree + diagnostics + every DataState variant.
        let mut fixtures = win11_16core_dual_gpu_desktop();
        fixtures[1].battery = DataState::Available(BatterySnapshot {
            charge_pct: 84,
            status: "Discharging".to_owned(),
            watts: Some(18.5),
            remaining_secs: Some(7200),
        });
        fixtures[1].services = DataState::Available(vec![ServiceEntry {
            name: "Spooler".to_owned(),
            display_name: "Print Spooler".to_owned(),
            state: "Running".to_owned(),
            start_type: "Auto".to_owned(),
            pid: Some(4216),
        }]);
        fixtures[2].cpu = DataState::Stale {
            value: match &fixtures[1].cpu {
                DataState::Available(c) => c.clone(),
                _ => unreachable!(),
            },
            age_ms: 1500,
        };
        fixtures[2].disks = DataState::Denied;
        fixtures[2].net = DataState::WarmingUp;
        fixtures[2].battery = DataState::Unavailable("desktop".to_owned());

        let snap_schema = build_json_schema(SchemaMode::Snapshot);
        for tree in [false, true] {
            let query = HeadlessQuery {
                tree,
                ..HeadlessQuery::default()
            };
            for snap in [&fixtures[1], &fixtures[2]] {
                let env = JsonSnapshotEnvelope::from_snapshot_with_options(
                    snap.clone(),
                    "0.1.1",
                    500,
                    JsonSections::all(),
                    &query,
                    &QueryFilter::None,
                    true,
                );
                let val = serde_json::to_value(&env).expect("serialize snapshot");
                validate_against_schema(&snap_schema, &val).expect("snapshot schema conformance");
                validate_against_schema(&all_schema, &val).expect("all schema conformance");
            }
        }

        // 2. Summary + series + diagnostics envelope.
        let summary = JsonSummaryEnvelope::from_snapshots(
            &fixtures,
            "0.1.1",
            None,
            JsonSections::all(),
            &HeadlessQuery::default(),
            &QueryFilter::None,
            true,
        )
        .with_window_diagnostics(&fixtures, JsonSections::all());
        let sum_val = serde_json::to_value(&summary).expect("serialize summary");
        validate_against_schema(&build_json_schema(SchemaMode::Summary), &sum_val)
            .expect("summary schema conformance");
        validate_against_schema(&all_schema, &sum_val).expect("summary in all schema");

        let diag_val = serde_json::to_value(summary.diagnostics.expect("diag")).expect("diag");
        validate_against_schema(&build_json_schema(SchemaMode::Diagnose), &diag_val)
            .expect("diagnose schema conformance");

        // 3. Error envelope.
        let err_val =
            serde_json::to_value(JsonErrorEnvelope::new("cli_usage", "bad flag")).expect("err");
        validate_against_schema(&build_json_schema(SchemaMode::Error), &err_val)
            .expect("error schema conformance");
        validate_against_schema(&all_schema, &err_val).expect("error in all schema");

        // 4. Negative drift check: adding an undocumented field MUST fail validation.
        let mut drifted = err_val;
        drifted
            .as_object_mut()
            .expect("obj")
            .insert("undocumented_field".to_owned(), json!(123));
        assert!(
            validate_against_schema(&build_json_schema(SchemaMode::Error), &drifted).is_err(),
            "validator must reject undocumented properties"
        );
    }
}
