//! Versioned headless JSON output envelopes and subsystem selection for CLI and agent consumers.

use serde::{Deserialize, Serialize};

use crate::diagnose::{DiagnosticsReport, diagnose_snapshot};
use crate::proc_query::{
    HeadlessQuery, ProcessTreeNode, ProcsQueryMeta, QueryFilter, ServiceJsonEntry,
    ServicesQueryMeta, query_flat_procs, query_proc_tree, query_services,
};
use crate::{
    BatterySnapshot, CpuSnapshot, DataState, DiskSnapshot, GpuSnapshot, MemSnapshot,
    NetInterfaceSnapshot, ProcessEntry, Snapshot,
};

/// Current schema version for headless `btop --json` output envelopes.
pub const JSON_SCHEMA_VERSION: u32 = 1;

/// Valid section tokens accepted by `--sections`.
pub const VALID_SECTION_NAMES: &[&str] = &[
    "cpu", "mem", "disks", "net", "procs", "services", "gpus", "battery", "all",
];

/// Bitset-like selection of telemetry subsystems included in headless JSON output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JsonSections {
    pub cpu: bool,
    pub mem: bool,
    pub disks: bool,
    pub net: bool,
    pub procs: bool,
    pub services: bool,
    pub gpus: bool,
    pub battery: bool,
}

impl JsonSections {
    /// Selects every telemetry subsystem.
    pub const fn all() -> Self {
        Self {
            cpu: true,
            mem: true,
            disks: true,
            net: true,
            procs: true,
            services: true,
            gpus: true,
            battery: true,
        }
    }

    /// Parses a comma-separated list of section names (`cpu,mem,disks,net,procs,services,gpus,battery,all`).
    pub fn parse_csv(spec: &str) -> Result<Self, String> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            return Err(format!(
                "invalid --sections value '': expected comma-separated list from [{}]",
                VALID_SECTION_NAMES.join(", ")
            ));
        }

        let mut selected = Self::default();
        for raw_token in trimmed.split(',') {
            let token = raw_token.trim().to_ascii_lowercase();
            if token.is_empty() {
                return Err(format!(
                    "invalid --sections value '{spec}': empty section token; expected values from [{}]",
                    VALID_SECTION_NAMES.join(", ")
                ));
            }
            match token.as_str() {
                "all" => selected = Self::all(),
                "cpu" => selected.cpu = true,
                "mem" => selected.mem = true,
                "disks" => selected.disks = true,
                "net" => selected.net = true,
                "procs" => selected.procs = true,
                "services" => selected.services = true,
                "gpus" => selected.gpus = true,
                "battery" => selected.battery = true,
                other => {
                    return Err(format!(
                        "unknown --sections value '{other}': expected values from [{}]",
                        VALID_SECTION_NAMES.join(", ")
                    ));
                }
            }
        }
        Ok(selected)
    }

    /// Returns selected section names in deterministic canonical order.
    pub fn selected_names(self) -> Vec<&'static str> {
        let mut names = Vec::with_capacity(8);
        if self.cpu {
            names.push("cpu");
        }
        if self.mem {
            names.push("mem");
        }
        if self.disks {
            names.push("disks");
        }
        if self.net {
            names.push("net");
        }
        if self.procs {
            names.push("procs");
        }
        if self.services {
            names.push("services");
        }
        if self.gpus {
            names.push("gpus");
        }
        if self.battery {
            names.push("battery");
        }
        names
    }
}

/// Top-level JSON response emitted to stdout by `btop --json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonSnapshotEnvelope {
    pub schema_version: u32,
    pub btop_version: String,
    pub captured_at_ms: u64,
    pub sample_window_ms: u64,
    pub hostname: String,
    pub username: String,
    pub sections: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_phys_mem_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<DataState<CpuSnapshot>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem: Option<DataState<MemSnapshot>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disks: Option<DataState<Vec<DiskSnapshot>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<DataState<Vec<NetInterfaceSnapshot>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub procs: Option<DataState<Vec<ProcessEntry>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proc_tree: Option<DataState<Vec<ProcessTreeNode>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub procs_meta: Option<ProcsQueryMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub services: Option<DataState<Vec<ServiceJsonEntry>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub services_meta: Option<ServicesQueryMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpus: Option<Vec<DataState<GpuSnapshot>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<DataState<BatterySnapshot>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<DiagnosticsReport>,
}

impl JsonSnapshotEnvelope {
    /// Projects an immutable [`Snapshot`] into a versioned JSON envelope containing only `sections`
    /// using default (unfiltered, flat) query options.
    pub fn from_snapshot(
        snapshot: Snapshot,
        btop_version: impl Into<String>,
        sample_window_ms: u64,
        sections: JsonSections,
    ) -> Self {
        Self::from_snapshot_with_query(
            snapshot,
            btop_version,
            sample_window_ms,
            sections,
            &HeadlessQuery::default(),
            &QueryFilter::None,
        )
    }

    /// Projects an immutable [`Snapshot`] into a versioned JSON envelope applying `query` and
    /// `compiled_filter` to `procs`/`proc_tree` and `services`.
    pub fn from_snapshot_with_query(
        snapshot: Snapshot,
        btop_version: impl Into<String>,
        sample_window_ms: u64,
        sections: JsonSections,
        query: &HeadlessQuery,
        compiled_filter: &QueryFilter,
    ) -> Self {
        Self::from_snapshot_with_options(
            snapshot,
            btop_version,
            sample_window_ms,
            sections,
            query,
            compiled_filter,
            false,
        )
    }

    /// Projects an immutable [`Snapshot`] into a versioned JSON envelope, optionally attaching
    /// deterministic health diagnostics computed over the full un-truncated snapshot.
    pub fn from_snapshot_with_options(
        snapshot: Snapshot,
        btop_version: impl Into<String>,
        sample_window_ms: u64,
        sections: JsonSections,
        query: &HeadlessQuery,
        compiled_filter: &QueryFilter,
        include_diagnostics: bool,
    ) -> Self {
        let diagnostics = include_diagnostics.then(|| diagnose_snapshot(&snapshot, sections));

        let Snapshot {
            timestamp_ms,
            hostname,
            username,
            total_phys_mem_bytes,
            cpu,
            mem,
            disks,
            net,
            processes,
            services,
            gpus,
            battery,
        } = snapshot;

        let live_procs_slice = match &processes {
            DataState::Available(list) | DataState::Stale { value: list, .. } => {
                Some(list.as_slice())
            }
            _ => None,
        };

        let (services_out, services_meta) = if sections.services {
            project_services(services, live_procs_slice, query, compiled_filter)
        } else {
            (None, None)
        };

        let projected_procs = if sections.procs {
            project_processes(processes, query, compiled_filter)
        } else {
            ProjectedProcs::default()
        };

        Self {
            schema_version: JSON_SCHEMA_VERSION,
            btop_version: btop_version.into(),
            captured_at_ms: timestamp_ms,
            sample_window_ms,
            hostname,
            username,
            sections: sections
                .selected_names()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            total_phys_mem_bytes: if sections.mem {
                total_phys_mem_bytes
            } else {
                None
            },
            cpu: sections.cpu.then_some(cpu),
            mem: sections.mem.then_some(mem),
            disks: sections.disks.then_some(disks),
            net: sections.net.then_some(net),
            procs: projected_procs.procs,
            proc_tree: projected_procs.proc_tree,
            procs_meta: projected_procs.meta,
            services: services_out,
            services_meta,
            gpus: sections.gpus.then_some(gpus),
            battery: sections.battery.then_some(battery),
            diagnostics,
        }
    }
}

#[derive(Default)]
struct ProjectedProcs {
    procs: Option<DataState<Vec<ProcessEntry>>>,
    proc_tree: Option<DataState<Vec<ProcessTreeNode>>>,
    meta: Option<ProcsQueryMeta>,
}

fn project_processes(
    processes: DataState<Vec<ProcessEntry>>,
    query: &HeadlessQuery,
    compiled_filter: &QueryFilter,
) -> ProjectedProcs {
    if query.tree {
        match processes {
            DataState::Available(list) => {
                let (nodes, meta) = query_proc_tree(&list, query, compiled_filter);
                ProjectedProcs {
                    procs: None,
                    proc_tree: Some(DataState::Available(nodes)),
                    meta: Some(meta),
                }
            }
            DataState::Stale { value, age_ms } => {
                let (nodes, meta) = query_proc_tree(&value, query, compiled_filter);
                ProjectedProcs {
                    procs: None,
                    proc_tree: Some(DataState::Stale {
                        value: nodes,
                        age_ms,
                    }),
                    meta: Some(meta),
                }
            }
            DataState::WarmingUp => ProjectedProcs {
                proc_tree: Some(DataState::WarmingUp),
                ..ProjectedProcs::default()
            },
            DataState::Denied => ProjectedProcs {
                proc_tree: Some(DataState::Denied),
                ..ProjectedProcs::default()
            },
            DataState::Unavailable(msg) => ProjectedProcs {
                proc_tree: Some(DataState::Unavailable(msg)),
                ..ProjectedProcs::default()
            },
        }
    } else {
        match processes {
            DataState::Available(list) => {
                let (rows, meta) = query_flat_procs(&list, query, compiled_filter);
                ProjectedProcs {
                    procs: Some(DataState::Available(rows)),
                    proc_tree: None,
                    meta: Some(meta),
                }
            }
            DataState::Stale { value, age_ms } => {
                let (rows, meta) = query_flat_procs(&value, query, compiled_filter);
                ProjectedProcs {
                    procs: Some(DataState::Stale {
                        value: rows,
                        age_ms,
                    }),
                    proc_tree: None,
                    meta: Some(meta),
                }
            }
            DataState::WarmingUp => ProjectedProcs {
                procs: Some(DataState::WarmingUp),
                ..ProjectedProcs::default()
            },
            DataState::Denied => ProjectedProcs {
                procs: Some(DataState::Denied),
                ..ProjectedProcs::default()
            },
            DataState::Unavailable(msg) => ProjectedProcs {
                procs: Some(DataState::Unavailable(msg)),
                ..ProjectedProcs::default()
            },
        }
    }
}

fn project_services(
    services: DataState<Vec<crate::ServiceEntry>>,
    procs: Option<&[ProcessEntry]>,
    query: &HeadlessQuery,
    compiled_filter: &QueryFilter,
) -> (
    Option<DataState<Vec<ServiceJsonEntry>>>,
    Option<ServicesQueryMeta>,
) {
    match services {
        DataState::Available(list) => {
            let (rows, meta) = query_services(&list, procs, query, compiled_filter);
            (Some(DataState::Available(rows)), Some(meta))
        }
        DataState::Stale { value, age_ms } => {
            let (rows, meta) = query_services(&value, procs, query, compiled_filter);
            (
                Some(DataState::Stale {
                    value: rows,
                    age_ms,
                }),
                Some(meta),
            )
        }
        DataState::WarmingUp => (Some(DataState::WarmingUp), None),
        DataState::Denied => (Some(DataState::Denied), None),
        DataState::Unavailable(msg) => (Some(DataState::Unavailable(msg)), None),
    }
}

/// Structured error body emitted to stderr when `--json` mode fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonErrorBody {
    pub code: String,
    pub message: String,
}

/// Top-level JSON error envelope emitted to stderr when `--json` mode fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonErrorEnvelope {
    pub schema_version: u32,
    pub error: JsonErrorBody,
}

impl JsonErrorEnvelope {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            schema_version: JSON_SCHEMA_VERSION,
            error: JsonErrorBody {
                code: code.into(),
                message: message.into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::win11_16core_dual_gpu_desktop;

    #[test]
    fn parse_sections_handles_all_subsets_case_and_rejects_invalid() {
        assert_eq!(
            JsonSections::parse_csv("all").expect("all"),
            JsonSections::all()
        );
        let sub = JsonSections::parse_csv("CPU, mem , GPUs").expect("subset");
        assert_eq!(sub.selected_names(), vec!["cpu", "mem", "gpus"]);
        assert!(!sub.procs);
        assert!(!sub.services);

        assert!(JsonSections::parse_csv("").is_err());
        assert!(JsonSections::parse_csv("   ").is_err());
        assert!(JsonSections::parse_csv("cpu,,mem").is_err());
        assert!(JsonSections::parse_csv("cpu,").is_err());
        assert!(JsonSections::parse_csv("cpu,bogus").is_err());
    }

    #[test]
    fn envelope_projects_only_selected_sections_and_preserves_data_state() {
        let mut snap = win11_16core_dual_gpu_desktop()[0].clone();
        snap.battery = DataState::Unavailable("desktop without battery".to_owned());

        let sections = JsonSections::parse_csv("cpu,gpus,battery").expect("parse");
        let env = JsonSnapshotEnvelope::from_snapshot(snap, "0.1.1", 500, sections);
        let value = serde_json::to_value(&env).expect("to value");

        assert_eq!(value["schema_version"], JSON_SCHEMA_VERSION);
        assert_eq!(value["btop_version"], "0.1.1");
        assert_eq!(value["sample_window_ms"], 500);
        assert_eq!(
            value["sections"],
            serde_json::json!(["cpu", "gpus", "battery"])
        );
        assert_eq!(value["cpu"]["state"], "available");
        assert!(value["gpus"].is_array());
        assert_eq!(value["battery"]["state"], "unavailable");
        assert!(value.get("mem").is_none());
        assert!(value.get("disks").is_none());
        assert!(value.get("net").is_none());
        assert!(value.get("procs").is_none());
        assert!(value.get("services").is_none());
    }

    #[test]
    fn error_envelope_serializes_cleanly() {
        let err = JsonErrorEnvelope::new("cli_usage", "unknown option");
        let val = serde_json::to_value(&err).expect("serialize error");
        assert_eq!(val["schema_version"], JSON_SCHEMA_VERSION);
        assert_eq!(val["error"]["code"], "cli_usage");
        assert_eq!(val["error"]["message"], "unknown option");
    }
}
