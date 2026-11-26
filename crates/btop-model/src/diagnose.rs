//! Pure, deterministic system health and bottleneck diagnostics for headless `--json --diagnose`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::json_output::JsonSections;
use crate::proc_query::{
    HeadlessQuery, QueryFilter, build_acyclic_adjacency, compute_subtree_totals,
};
use crate::summary::{JsonSummaryEnvelope, ProcessWindowEntry, ScalarStats};
use crate::{
    CpuSnapshot, DataState, DiskSnapshot, GpuSnapshot, MemSnapshot, NetInterfaceSnapshot,
    ProcessEntry, ProcessIdentity, Snapshot,
};

/// Overall system health status derived deterministically from diagnostic findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Healthy,
    Warning,
    Critical,
}

/// Severity level for an individual diagnostic finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Critical,
}

impl DiagnosticSeverity {
    const fn rank(self) -> u8 {
        match self {
            Self::Critical => 0,
            Self::Warning => 1,
            Self::Info => 2,
        }
    }
}

/// Telemetry subsystem category for a diagnostic check or finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCategory {
    Cpu,
    Memory,
    Gpu,
    Disk,
    Network,
    Process,
}

/// Identifies the offending entity (process, disk volume, GPU, or network adapter) in a finding.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiagnosticEntity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_time_100ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_index: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub luid: Option<u64>,
}

impl DiagnosticEntity {
    fn is_empty(&self) -> bool {
        self.pid.is_none()
            && self.create_time_100ns.is_none()
            && self.name.is_none()
            && self.mount.is_none()
            && self.gpu_index.is_none()
            && self.gpu_name.is_none()
            && self.interface.is_none()
            && self.luid.is_none()
    }

    fn sort_key(&self) -> (Option<u32>, Option<&str>, Option<u8>, Option<&str>) {
        (
            self.pid,
            self.mount.as_deref(),
            self.gpu_index,
            self.interface.as_deref().or(self.name.as_deref()),
        )
    }
}

/// Exact numeric evidence supporting a diagnostic finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticEvidence {
    pub metric: String,
    pub observed: f64,
    pub threshold: f64,
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<DiagnosticEntity>,
}

/// A single structured health or bottleneck finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticFinding {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub category: DiagnosticCategory,
    pub summary: String,
    pub evidence: DiagnosticEvidence,
}

/// Records a diagnostic check that could not be assessed due to missing, warming-up, denied,
/// unavailable, or unselected telemetry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedCheck {
    pub check: String,
    pub category: DiagnosticCategory,
    pub reason: String,
}

/// Structured diagnostics object attached to `--json --diagnose` and `--json --summary --diagnose`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsReport {
    pub overall_status: HealthStatus,
    pub checks_evaluated: Vec<String>,
    pub skipped_checks: Vec<SkippedCheck>,
    pub findings: Vec<DiagnosticFinding>,
}

impl DiagnosticsReport {
    fn finalize(
        checks_evaluated: Vec<String>,
        skipped_checks: Vec<SkippedCheck>,
        mut findings: Vec<DiagnosticFinding>,
    ) -> Self {
        findings.sort_by(|a, b| {
            a.severity
                .rank()
                .cmp(&b.severity.rank())
                .then_with(|| a.category.cmp(&b.category))
                .then_with(|| a.code.cmp(&b.code))
                .then_with(|| {
                    let a_key = a.evidence.entity.as_ref().map(DiagnosticEntity::sort_key);
                    let b_key = b.evidence.entity.as_ref().map(DiagnosticEntity::sort_key);
                    a_key.cmp(&b_key)
                })
                .then_with(|| b.evidence.observed.total_cmp(&a.evidence.observed))
        });

        let overall_status = findings
            .iter()
            .map(|f| match f.severity {
                DiagnosticSeverity::Critical => HealthStatus::Critical,
                DiagnosticSeverity::Warning => HealthStatus::Warning,
                DiagnosticSeverity::Info => HealthStatus::Healthy,
            })
            .max()
            .unwrap_or(HealthStatus::Healthy);

        Self {
            overall_status,
            checks_evaluated,
            skipped_checks,
            findings,
        }
    }
}

#[derive(Default)]
struct EvalContext {
    evaluated: Vec<String>,
    skipped: Vec<SkippedCheck>,
    findings: Vec<DiagnosticFinding>,
}

impl EvalContext {
    fn mark_evaluated(&mut self, check: &str) {
        self.evaluated.push(check.to_owned());
    }

    fn mark_skipped(
        &mut self,
        check: &str,
        category: DiagnosticCategory,
        reason: impl Into<String>,
    ) {
        self.skipped.push(SkippedCheck {
            check: check.to_owned(),
            category,
            reason: reason.into(),
        });
    }

    fn push_finding(
        &mut self,
        code: &str,
        severity: DiagnosticSeverity,
        category: DiagnosticCategory,
        summary: String,
        evidence: DiagnosticEvidence,
    ) {
        let mut evidence = evidence;
        if let Some(ref ent) = evidence.entity
            && ent.is_empty()
        {
            evidence.entity = None;
        }
        self.findings.push(DiagnosticFinding {
            code: code.to_owned(),
            severity,
            category,
            summary,
            evidence,
        });
    }

    fn finish(self) -> DiagnosticsReport {
        DiagnosticsReport::finalize(self.evaluated, self.skipped, self.findings)
    }
}

fn data_state_skip_reason<T>(state: &DataState<T>) -> Option<String> {
    match state {
        DataState::Available(_) | DataState::Stale { .. } => None,
        DataState::WarmingUp => Some("subsystem warming up".to_owned()),
        DataState::Denied => Some("access denied".to_owned()),
        DataState::Unavailable(msg) => Some(if msg.is_empty() {
            "subsystem unavailable".to_owned()
        } else {
            msg.clone()
        }),
    }
}

fn state_ref<T>(state: &DataState<T>) -> Option<&T> {
    match state {
        DataState::Available(v) | DataState::Stale { value: v, .. } => Some(v),
        DataState::WarmingUp | DataState::Denied | DataState::Unavailable(_) => None,
    }
}

#[allow(clippy::cast_precision_loss)]
fn ratio_pct_u64(part: u64, total: u64) -> Option<f64> {
    if total == 0 {
        return None;
    }
    Some((part as f64 / total as f64) * 100.0)
}

fn round1(val: f64) -> f64 {
    (val * 10.0).round() / 10.0
}

fn eval_high_threshold(
    observed: f64,
    warn_threshold: f64,
    crit_threshold: f64,
) -> Option<(DiagnosticSeverity, f64)> {
    if !observed.is_finite() {
        return None;
    }
    if observed >= crit_threshold {
        Some((DiagnosticSeverity::Critical, crit_threshold))
    } else if observed >= warn_threshold {
        Some((DiagnosticSeverity::Warning, warn_threshold))
    } else {
        None
    }
}

fn eval_low_threshold(
    observed: f64,
    warn_threshold: f64,
    crit_threshold: f64,
) -> Option<(DiagnosticSeverity, f64)> {
    if !observed.is_finite() {
        return None;
    }
    if observed <= crit_threshold {
        Some((DiagnosticSeverity::Critical, crit_threshold))
    } else if observed <= warn_threshold {
        Some((DiagnosticSeverity::Warning, warn_threshold))
    } else {
        None
    }
}

/// Evaluates pure deterministic health and bottleneck diagnostics on a single immutable [`Snapshot`].
pub fn diagnose_snapshot(snapshot: &Snapshot, sections: JsonSections) -> DiagnosticsReport {
    let mut ctx = EvalContext::default();

    // 1. CPU
    if !sections.cpu {
        ctx.mark_skipped(
            "cpu_health",
            DiagnosticCategory::Cpu,
            "section not selected",
        );
    } else if let Some(cpu) = state_ref(&snapshot.cpu) {
        diagnose_cpu_snapshot(&mut ctx, cpu);
    } else if let Some(reason) = data_state_skip_reason(&snapshot.cpu) {
        ctx.mark_skipped("cpu_health", DiagnosticCategory::Cpu, reason);
    }

    // 2. Memory
    let total_phys_mem = state_ref(&snapshot.mem)
        .map(|m| m.total_bytes)
        .filter(|&b| b > 0)
        .or(snapshot.total_phys_mem_bytes);

    if !sections.mem {
        ctx.mark_skipped(
            "memory_health",
            DiagnosticCategory::Memory,
            "section not selected",
        );
    } else if let Some(mem) = state_ref(&snapshot.mem) {
        diagnose_mem_snapshot(&mut ctx, mem);
    } else if let Some(reason) = data_state_skip_reason(&snapshot.mem) {
        ctx.mark_skipped("memory_health", DiagnosticCategory::Memory, reason);
    }

    // 3. Disks
    if !sections.disks {
        ctx.mark_skipped(
            "disk_health",
            DiagnosticCategory::Disk,
            "section not selected",
        );
    } else if let Some(disks) = state_ref(&snapshot.disks) {
        diagnose_disks_snapshot(&mut ctx, disks);
    } else if let Some(reason) = data_state_skip_reason(&snapshot.disks) {
        ctx.mark_skipped("disk_health", DiagnosticCategory::Disk, reason);
    }

    // 4. Network
    if !sections.net {
        ctx.mark_skipped(
            "network_health",
            DiagnosticCategory::Network,
            "section not selected",
        );
    } else if let Some(net) = state_ref(&snapshot.net) {
        diagnose_net_snapshot(&mut ctx, net);
    } else if let Some(reason) = data_state_skip_reason(&snapshot.net) {
        ctx.mark_skipped("network_health", DiagnosticCategory::Network, reason);
    }

    // 5. GPUs
    if sections.gpus {
        diagnose_gpus_snapshot(&mut ctx, &snapshot.gpus);
    } else {
        ctx.mark_skipped(
            "gpu_health",
            DiagnosticCategory::Gpu,
            "section not selected",
        );
    }

    // 6. Processes
    if !sections.procs {
        ctx.mark_skipped(
            "process_health",
            DiagnosticCategory::Process,
            "section not selected",
        );
    } else if let Some(procs) = state_ref(&snapshot.processes) {
        diagnose_procs_snapshot(&mut ctx, procs, total_phys_mem);
    } else if let Some(reason) = data_state_skip_reason(&snapshot.processes) {
        ctx.mark_skipped("process_health", DiagnosticCategory::Process, reason);
    }

    ctx.finish()
}

fn diagnose_cpu_snapshot(ctx: &mut EvalContext, cpu: &CpuSnapshot) {
    let busy = f64::from(cpu.total_busy_pct);
    if busy.is_finite() {
        ctx.mark_evaluated("cpu_saturation");
        if let Some((severity, threshold)) = eval_high_threshold(busy, 85.0, 95.0) {
            ctx.push_finding(
                "cpu_saturation",
                severity,
                DiagnosticCategory::Cpu,
                format!("CPU utilization is high at {:.1}%", round1(busy)),
                DiagnosticEvidence {
                    metric: "cpu.total_busy_pct".to_owned(),
                    observed: round1(busy),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: None,
                },
            );
        }
    } else {
        ctx.mark_skipped(
            "cpu_saturation",
            DiagnosticCategory::Cpu,
            "non-finite cpu busy reading",
        );
    }

    let cores = cpu.cores.len();
    let run_q = f64::from(cpu.run_queue_ewma[0]);
    if cores > 0 && run_q.is_finite() {
        ctx.mark_evaluated("cpu_run_queue_pressure");
        #[allow(clippy::cast_precision_loss)]
        let cores_f = cores as f64;
        let warn = cores_f * 1.5;
        let crit = cores_f * 2.5;
        if let Some((severity, threshold)) = eval_high_threshold(run_q, warn, crit) {
            ctx.push_finding(
                "cpu_run_queue_pressure",
                severity,
                DiagnosticCategory::Cpu,
                format!(
                    "CPU 1m run queue ({:.1}) exceeds healthy capacity for {cores} logical cores",
                    round1(run_q)
                ),
                DiagnosticEvidence {
                    metric: "cpu.run_queue_ewma[0]".to_owned(),
                    observed: round1(run_q),
                    threshold: round1(threshold),
                    unit: "threads".to_owned(),
                    entity: None,
                },
            );
        }
    } else {
        ctx.mark_skipped(
            "cpu_run_queue_pressure",
            DiagnosticCategory::Cpu,
            "logical core count or run queue unavailable",
        );
    }

    if let Some(temp) = cpu.package_temp_celsius.filter(|t| t.is_finite()) {
        ctx.mark_evaluated("cpu_thermal_pressure");
        let t = f64::from(temp);
        if let Some((severity, threshold)) = eval_high_threshold(t, 85.0, 95.0) {
            ctx.push_finding(
                "cpu_thermal_pressure",
                severity,
                DiagnosticCategory::Cpu,
                format!("CPU package temperature is elevated at {:.1} C", round1(t)),
                DiagnosticEvidence {
                    metric: "cpu.package_temp_celsius".to_owned(),
                    observed: round1(t),
                    threshold,
                    unit: "celsius".to_owned(),
                    entity: None,
                },
            );
        }
    } else {
        ctx.mark_skipped(
            "cpu_thermal_pressure",
            DiagnosticCategory::Cpu,
            "CPU package temperature sensor unavailable",
        );
    }
}

fn diagnose_mem_snapshot(ctx: &mut EvalContext, mem: &MemSnapshot) {
    diagnose_mem_values(
        ctx,
        mem.total_bytes,
        mem.available_bytes,
        mem.commit_used_bytes,
        mem.commit_limit_bytes,
        mem.pagefile_used_bytes,
        mem.pagefile_total_bytes,
        mem.compressed_bytes,
        "mem",
    );
}

#[allow(clippy::too_many_arguments)]
fn diagnose_mem_values(
    ctx: &mut EvalContext,
    total_bytes: u64,
    available_bytes: u64,
    commit_used_bytes: u64,
    commit_limit_bytes: u64,
    pagefile_used_bytes: u64,
    pagefile_total_bytes: u64,
    compressed_bytes: Option<u64>,
    prefix: &str,
) {
    if let Some(avail_pct) = ratio_pct_u64(available_bytes, total_bytes) {
        ctx.mark_evaluated("memory_low_available");
        if let Some((severity, threshold)) = eval_low_threshold(avail_pct, 15.0, 7.0) {
            ctx.push_finding(
                "memory_low_available",
                severity,
                DiagnosticCategory::Memory,
                format!("Available physical RAM is low at {:.1}%", round1(avail_pct)),
                DiagnosticEvidence {
                    metric: format!("{prefix}.available_pct"),
                    observed: round1(avail_pct),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: None,
                },
            );
        }
    } else {
        ctx.mark_skipped(
            "memory_low_available",
            DiagnosticCategory::Memory,
            "total physical memory is zero",
        );
    }

    if let Some(commit_pct) = ratio_pct_u64(commit_used_bytes, commit_limit_bytes) {
        ctx.mark_evaluated("memory_commit_pressure");
        if let Some((severity, threshold)) = eval_high_threshold(commit_pct, 80.0, 92.0) {
            ctx.push_finding(
                "memory_commit_pressure",
                severity,
                DiagnosticCategory::Memory,
                format!(
                    "Windows commit charge is high at {:.1}% of commit limit",
                    round1(commit_pct)
                ),
                DiagnosticEvidence {
                    metric: format!("{prefix}.commit_pct"),
                    observed: round1(commit_pct),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: None,
                },
            );
        }
    } else {
        ctx.mark_skipped(
            "memory_commit_pressure",
            DiagnosticCategory::Memory,
            "commit limit unavailable",
        );
    }

    diagnose_mem_paging(
        ctx,
        total_bytes,
        pagefile_used_bytes,
        pagefile_total_bytes,
        compressed_bytes,
        prefix,
    );
}

fn diagnose_mem_paging(
    ctx: &mut EvalContext,
    total_bytes: u64,
    pagefile_used_bytes: u64,
    pagefile_total_bytes: u64,
    compressed_bytes: Option<u64>,
    prefix: &str,
) {
    if let Some(pf_pct) = ratio_pct_u64(pagefile_used_bytes, pagefile_total_bytes) {
        ctx.mark_evaluated("memory_pagefile_pressure");
        if let Some((severity, threshold)) = eval_high_threshold(pf_pct, 75.0, 90.0) {
            ctx.push_finding(
                "memory_pagefile_pressure",
                severity,
                DiagnosticCategory::Memory,
                format!("Pagefile utilization is high at {:.1}%", round1(pf_pct)),
                DiagnosticEvidence {
                    metric: format!("{prefix}.pagefile_pct"),
                    observed: round1(pf_pct),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: None,
                },
            );
        }
    } else {
        ctx.mark_skipped(
            "memory_pagefile_pressure",
            DiagnosticCategory::Memory,
            "pagefile absent or zero capacity",
        );
    }

    if let Some(comp) = compressed_bytes
        && let Some(comp_pct) = ratio_pct_u64(comp, total_bytes)
    {
        ctx.mark_evaluated("memory_high_compressed");
        if let Some((severity, threshold)) = eval_high_threshold(comp_pct, 15.0, 25.0) {
            ctx.push_finding(
                "memory_high_compressed",
                severity,
                DiagnosticCategory::Memory,
                format!(
                    "Compressed memory accounts for {:.1}% of physical RAM",
                    round1(comp_pct)
                ),
                DiagnosticEvidence {
                    metric: format!("{prefix}.compressed_pct"),
                    observed: round1(comp_pct),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: None,
                },
            );
        }
    } else {
        ctx.mark_skipped(
            "memory_high_compressed",
            DiagnosticCategory::Memory,
            "compressed memory metric unavailable",
        );
    }
}

fn diagnose_disks_snapshot(ctx: &mut EvalContext, disks: &[DiskSnapshot]) {
    if disks.is_empty() {
        ctx.mark_skipped(
            "disk_health",
            DiagnosticCategory::Disk,
            "no fixed local volumes enumerated",
        );
        return;
    }

    let mut evaluated_space = false;
    let mut evaluated_busy = false;

    for disk in disks {
        if let Some(free_pct) = ratio_pct_u64(disk.free_bytes, disk.total_bytes) {
            evaluated_space = true;
            if let Some((severity, threshold)) = eval_low_threshold(free_pct, 15.0, 5.0) {
                ctx.push_finding(
                    "disk_low_free_space",
                    severity,
                    DiagnosticCategory::Disk,
                    format!(
                        "Volume {} has low free space ({:.1}% free)",
                        disk.mount,
                        round1(free_pct)
                    ),
                    DiagnosticEvidence {
                        metric: "disk.free_pct".to_owned(),
                        observed: round1(free_pct),
                        threshold,
                        unit: "percent".to_owned(),
                        entity: Some(DiagnosticEntity {
                            mount: Some(disk.mount.clone()),
                            ..DiagnosticEntity::default()
                        }),
                    },
                );
            }
        }

        if let Some(busy) = disk.busy_pct.filter(|b| b.is_finite()) {
            evaluated_busy = true;
            let b = f64::from(busy);
            if let Some((severity, threshold)) = eval_high_threshold(b, 80.0, 95.0) {
                ctx.push_finding(
                    "disk_high_busy",
                    severity,
                    DiagnosticCategory::Disk,
                    format!(
                        "Volume {} is I/O saturated ({:.1}% busy)",
                        disk.mount,
                        round1(b)
                    ),
                    DiagnosticEvidence {
                        metric: "disk.busy_pct".to_owned(),
                        observed: round1(b),
                        threshold,
                        unit: "percent".to_owned(),
                        entity: Some(DiagnosticEntity {
                            mount: Some(disk.mount.clone()),
                            ..DiagnosticEntity::default()
                        }),
                    },
                );
            }
        }
    }

    if evaluated_space {
        ctx.mark_evaluated("disk_low_free_space");
    } else {
        ctx.mark_skipped(
            "disk_low_free_space",
            DiagnosticCategory::Disk,
            "volume capacities unavailable",
        );
    }

    if evaluated_busy {
        ctx.mark_evaluated("disk_high_busy");
    } else {
        ctx.mark_skipped(
            "disk_high_busy",
            DiagnosticCategory::Disk,
            "disk busy percentage unavailable",
        );
    }
}

fn diagnose_net_snapshot(ctx: &mut EvalContext, net: &[NetInterfaceSnapshot]) {
    let mut evaluated_bw = false;
    for iface in net {
        if !iface.is_up {
            continue;
        }
        let Some(link_bps) = iface.link_speed_bps.filter(|&bps| bps > 0) else {
            continue;
        };
        evaluated_bw = true;
        let peak_bytes_per_sec = iface.rx_bytes_per_sec.max(iface.tx_bytes_per_sec);
        let peak_bps = peak_bytes_per_sec.saturating_mul(8);
        if let Some(util_pct) = ratio_pct_u64(peak_bps, link_bps)
            && let Some((severity, threshold)) = eval_high_threshold(util_pct, 80.0, 95.0)
        {
            ctx.push_finding(
                "net_high_bandwidth_utilization",
                severity,
                DiagnosticCategory::Network,
                format!(
                    "Network interface {} is at {:.1}% of link speed",
                    iface.name,
                    round1(util_pct)
                ),
                DiagnosticEvidence {
                    metric: "net.bandwidth_utilization_pct".to_owned(),
                    observed: round1(util_pct),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: Some(DiagnosticEntity {
                        interface: Some(iface.name.clone()),
                        luid: Some(iface.luid),
                        ..DiagnosticEntity::default()
                    }),
                },
            );
        }
    }

    if evaluated_bw {
        ctx.mark_evaluated("net_high_bandwidth_utilization");
    } else {
        ctx.mark_skipped(
            "net_high_bandwidth_utilization",
            DiagnosticCategory::Network,
            "no active network interface with known link speed",
        );
    }
}

#[derive(Default)]
struct GpuCheckCoverage {
    vram: bool,
    util: bool,
    temp: bool,
    power: bool,
}

struct GpuEvalInputs<'a> {
    index: u8,
    name: &'a str,
    vram_used: Option<u64>,
    vram_total: Option<u64>,
    util_pct: Option<f64>,
    temp_c: Option<f64>,
    temp_crit_c: f64,
    power_w: Option<f64>,
    power_limit_w: Option<f64>,
    metric_suffix: &'a str,
}

fn evaluate_single_gpu(ctx: &mut EvalContext, cov: &mut GpuCheckCoverage, inp: &GpuEvalInputs<'_>) {
    let entity = DiagnosticEntity {
        gpu_index: Some(inp.index),
        gpu_name: Some(inp.name.to_owned()),
        ..DiagnosticEntity::default()
    };

    if let (Some(used), Some(total)) = (inp.vram_used, inp.vram_total)
        && let Some(vram_pct) = ratio_pct_u64(used, total)
    {
        cov.vram = true;
        if let Some((severity, threshold)) = eval_high_threshold(vram_pct, 85.0, 95.0) {
            ctx.push_finding(
                "gpu_vram_pressure",
                severity,
                DiagnosticCategory::Gpu,
                format!(
                    "GPU {} ({}) dedicated VRAM is {:.1}% full",
                    inp.index,
                    inp.name,
                    round1(vram_pct)
                ),
                DiagnosticEvidence {
                    metric: format!("gpu.vram_used_pct{}", inp.metric_suffix),
                    observed: round1(vram_pct),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: Some(entity.clone()),
                },
            );
        }
    }

    if let Some(u) = inp.util_pct.filter(|v| v.is_finite()) {
        cov.util = true;
        if let Some((severity, threshold)) = eval_high_threshold(u, 95.0, 99.0) {
            ctx.push_finding(
                "gpu_compute_saturation",
                severity,
                DiagnosticCategory::Gpu,
                format!(
                    "GPU {} ({}) engine utilization is saturated at {:.1}%",
                    inp.index,
                    inp.name,
                    round1(u)
                ),
                DiagnosticEvidence {
                    metric: format!("gpu.utilization_pct{}", inp.metric_suffix),
                    observed: round1(u),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: Some(entity.clone()),
                },
            );
        }
    }

    evaluate_gpu_thermals_and_power(ctx, cov, inp, entity);
}

fn evaluate_gpu_thermals_and_power(
    ctx: &mut EvalContext,
    cov: &mut GpuCheckCoverage,
    inp: &GpuEvalInputs<'_>,
    entity: DiagnosticEntity,
) {
    if let Some(t) = inp.temp_c.filter(|v| v.is_finite()) {
        cov.temp = true;
        let warn = (inp.temp_crit_c - 8.0).max(40.0);
        if let Some((severity, threshold)) = eval_high_threshold(t, warn, inp.temp_crit_c) {
            ctx.push_finding(
                "gpu_thermal_pressure",
                severity,
                DiagnosticCategory::Gpu,
                format!(
                    "GPU {} ({}) temperature is elevated at {:.1} C",
                    inp.index,
                    inp.name,
                    round1(t)
                ),
                DiagnosticEvidence {
                    metric: format!("gpu.temp_celsius{}", inp.metric_suffix),
                    observed: round1(t),
                    threshold: round1(threshold),
                    unit: "celsius".to_owned(),
                    entity: Some(entity.clone()),
                },
            );
        }
    }

    if let (Some(power), Some(limit)) = (inp.power_w, inp.power_limit_w)
        && power.is_finite()
        && limit.is_finite()
        && limit > 0.0
    {
        cov.power = true;
        let pct = (power / limit) * 100.0;
        if let Some((severity, threshold)) = eval_high_threshold(pct, 92.0, 98.0) {
            ctx.push_finding(
                "gpu_power_limit_saturation",
                severity,
                DiagnosticCategory::Gpu,
                format!(
                    "GPU {} ({}) power draw is at {:.1}% of power limit",
                    inp.index,
                    inp.name,
                    round1(pct)
                ),
                DiagnosticEvidence {
                    metric: format!("gpu.power_limit_pct{}", inp.metric_suffix),
                    observed: round1(pct),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: Some(entity),
                },
            );
        }
    }
}

fn diagnose_gpus_snapshot(ctx: &mut EvalContext, gpus: &[DataState<GpuSnapshot>]) {
    if gpus.is_empty() {
        ctx.mark_skipped(
            "gpu_health",
            DiagnosticCategory::Gpu,
            "no hardware GPUs enumerated",
        );
        return;
    }

    let available: Vec<&GpuSnapshot> = gpus.iter().filter_map(state_ref).collect();
    if available.is_empty() {
        let reason = gpus
            .iter()
            .find_map(data_state_skip_reason)
            .unwrap_or_else(|| "GPUs unavailable".to_owned());
        ctx.mark_skipped("gpu_health", DiagnosticCategory::Gpu, reason);
        return;
    }

    let mut cov = GpuCheckCoverage::default();
    for gpu in available {
        let temp_crit_c = gpu
            .temp_max_celsius
            .filter(|m| m.is_finite() && *m > 50.0)
            .map_or(90.0, f64::from);
        let inp = GpuEvalInputs {
            index: gpu.index,
            name: &gpu.name,
            vram_used: gpu.vram_used_bytes,
            vram_total: gpu.vram_total_bytes,
            util_pct: gpu.utilization_pct.map(f64::from),
            temp_c: gpu.temp_celsius.map(f64::from),
            temp_crit_c,
            power_w: gpu.power_watts.map(f64::from),
            power_limit_w: gpu.power_limit_watts.map(f64::from),
            metric_suffix: "",
        };
        evaluate_single_gpu(ctx, &mut cov, &inp);
    }
    record_optional_gpu_checks(ctx, &cov);
}

fn record_optional_gpu_checks(ctx: &mut EvalContext, cov: &GpuCheckCoverage) {
    if cov.vram {
        ctx.mark_evaluated("gpu_vram_pressure");
    } else {
        ctx.mark_skipped(
            "gpu_vram_pressure",
            DiagnosticCategory::Gpu,
            "dedicated VRAM telemetry unavailable",
        );
    }
    if cov.util {
        ctx.mark_evaluated("gpu_compute_saturation");
    } else {
        ctx.mark_skipped(
            "gpu_compute_saturation",
            DiagnosticCategory::Gpu,
            "GPU engine utilization unavailable",
        );
    }
    if cov.temp {
        ctx.mark_evaluated("gpu_thermal_pressure");
    } else {
        ctx.mark_skipped(
            "gpu_thermal_pressure",
            DiagnosticCategory::Gpu,
            "GPU temperature sensor unavailable",
        );
    }
    if cov.power {
        ctx.mark_evaluated("gpu_power_limit_saturation");
    } else {
        ctx.mark_skipped(
            "gpu_power_limit_saturation",
            DiagnosticCategory::Gpu,
            "GPU power/limit sensors unavailable",
        );
    }
}

fn diagnose_procs_snapshot(
    ctx: &mut EvalContext,
    procs: &[ProcessEntry],
    total_phys_mem: Option<u64>,
) {
    if procs.is_empty() {
        ctx.mark_skipped(
            "process_health",
            DiagnosticCategory::Process,
            "no processes enumerated",
        );
        return;
    }

    ctx.mark_evaluated("process_cpu_dominance");
    for proc_entry in procs {
        // PID 0 is Windows System Idle Process; its percentage reflects idle capacity, not load.
        if proc_entry.identity.pid == 0 {
            continue;
        }
        let cpu = f64::from(proc_entry.cpu_pct);
        if let Some((severity, threshold)) = eval_high_threshold(cpu, 60.0, 85.0) {
            ctx.push_finding(
                "process_cpu_dominance",
                severity,
                DiagnosticCategory::Process,
                format!(
                    "Process {} (PID {}) is consuming {:.1}% CPU",
                    proc_entry.name,
                    proc_entry.identity.pid,
                    round1(cpu)
                ),
                DiagnosticEvidence {
                    metric: "process.cpu_pct".to_owned(),
                    observed: round1(cpu),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: Some(proc_entity(proc_entry)),
                },
            );
        }
    }

    diagnose_proc_subtrees(ctx, procs);
    diagnose_proc_memory(ctx, procs, total_phys_mem);
}

fn proc_entity(proc_entry: &ProcessEntry) -> DiagnosticEntity {
    DiagnosticEntity {
        pid: Some(proc_entry.identity.pid),
        create_time_100ns: Some(proc_entry.identity.create_time_100ns),
        name: Some(proc_entry.name.clone()),
        ..DiagnosticEntity::default()
    }
}

fn find_dominant_subtrees(procs: &[ProcessEntry]) -> Vec<(usize, f64)> {
    let (_parent_idx, children) = build_acyclic_adjacency(procs);
    let subtree_totals = compute_subtree_totals(procs, &children);
    let n = procs.len();

    // First identify candidate multi-process workload subtrees (excluding Windows Idle PID 0
    // and kernel System PID 4) where CPU is genuinely spread across multiple processes rather
    // than dominated by a single child (which is already caught by `process_cpu_dominance`).
    let mut is_candidate = vec![false; n];
    for (idx, proc_entry) in procs.iter().enumerate() {
        let pid = proc_entry.identity.pid;
        if pid == 0 || pid == 4 || children[idx].is_empty() {
            continue;
        }
        let sub_cpu = f64::from(subtree_totals[idx].0);
        if !sub_cpu.is_finite() || sub_cpu < 70.0 {
            continue;
        }
        let self_cpu = f64::from(proc_entry.cpu_pct.max(0.0));
        if sub_cpu <= self_cpu + 5.0 {
            continue;
        }
        let max_child_sub = children[idx]
            .iter()
            .map(|&c| f64::from(subtree_totals[c].0))
            .fold(0.0_f64, f64::max);
        if max_child_sub >= sub_cpu * 0.85 {
            continue;
        }
        is_candidate[idx] = true;
    }

    // Keep only the deepest (most specific) candidate in each branch so top-level ancestors
    // like `explorer.exe` or `pwsh.exe` are not blamed for a child workload subtree.
    let mut has_candidate_descendant = vec![false; n];
    let mut memo: Vec<Option<bool>> = vec![None; n];
    for (idx, has_desc) in has_candidate_descendant.iter_mut().enumerate() {
        *has_desc = subtree_has_candidate_descendant(idx, &children, &is_candidate, &mut memo);
    }

    (0..n)
        .filter(|&idx| is_candidate[idx] && !has_candidate_descendant[idx])
        .map(|idx| (idx, f64::from(subtree_totals[idx].0)))
        .collect()
}

fn subtree_has_candidate_descendant(
    idx: usize,
    children: &[Vec<usize>],
    is_candidate: &[bool],
    memo: &mut [Option<bool>],
) -> bool {
    if let Some(cached) = memo[idx] {
        return cached;
    }
    let mut found = false;
    for &c in &children[idx] {
        if is_candidate[c] || subtree_has_candidate_descendant(c, children, is_candidate, memo) {
            found = true;
            break;
        }
    }
    memo[idx] = Some(found);
    found
}

fn diagnose_proc_subtrees(ctx: &mut EvalContext, procs: &[ProcessEntry]) {
    ctx.mark_evaluated("process_subtree_cpu_dominance");
    for (idx, sub_cpu) in find_dominant_subtrees(procs) {
        let proc_entry = &procs[idx];
        if let Some((severity, threshold)) = eval_high_threshold(sub_cpu, 70.0, 90.0) {
            ctx.push_finding(
                "process_subtree_cpu_dominance",
                severity,
                DiagnosticCategory::Process,
                format!(
                    "Process subtree rooted at {} (PID {}) is consuming {:.1}% CPU",
                    proc_entry.name,
                    proc_entry.identity.pid,
                    round1(sub_cpu)
                ),
                DiagnosticEvidence {
                    metric: "process.subtree_cpu_pct".to_owned(),
                    observed: round1(sub_cpu),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: Some(proc_entity(proc_entry)),
                },
            );
        }
    }
}

fn diagnose_proc_memory(
    ctx: &mut EvalContext,
    procs: &[ProcessEntry],
    total_phys_mem: Option<u64>,
) {
    let Some(total_mem) = total_phys_mem.filter(|&b| b > 0) else {
        ctx.mark_skipped(
            "process_memory_dominance",
            DiagnosticCategory::Process,
            "total physical memory unavailable",
        );
        return;
    };

    ctx.mark_evaluated("process_memory_dominance");
    for proc_entry in procs {
        if proc_entry.identity.pid == 0 {
            continue;
        }
        if let Some(mem_pct) = ratio_pct_u64(proc_entry.working_set_private_bytes, total_mem)
            && let Some((severity, threshold)) = eval_high_threshold(mem_pct, 25.0, 45.0)
        {
            ctx.push_finding(
                "process_memory_dominance",
                severity,
                DiagnosticCategory::Process,
                format!(
                    "Process {} (PID {}) holds {:.1}% of physical RAM",
                    proc_entry.name,
                    proc_entry.identity.pid,
                    round1(mem_pct)
                ),
                DiagnosticEvidence {
                    metric: "process.working_set_pct".to_owned(),
                    observed: round1(mem_pct),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: Some(proc_entity(proc_entry)),
                },
            );
        }
    }
}

fn diagnose_cpu_summary(
    ctx: &mut EvalContext,
    cpu: Option<&crate::summary::CpuSummary>,
    selected: bool,
) {
    if !selected {
        ctx.mark_skipped(
            "cpu_health",
            DiagnosticCategory::Cpu,
            "section not selected",
        );
        return;
    }
    let Some(cpu) = cpu else {
        ctx.mark_skipped(
            "cpu_health",
            DiagnosticCategory::Cpu,
            "no available CPU samples in window",
        );
        return;
    };

    ctx.mark_evaluated("cpu_saturation");
    if let Some((severity, threshold)) = eval_high_threshold(cpu.total_busy_pct.p95, 85.0, 95.0) {
        ctx.push_finding(
            "cpu_saturation",
            severity,
            DiagnosticCategory::Cpu,
            format!(
                "Window p95 CPU utilization is high at {:.1}%",
                round1(cpu.total_busy_pct.p95)
            ),
            DiagnosticEvidence {
                metric: "cpu.total_busy_pct.p95".to_owned(),
                observed: round1(cpu.total_busy_pct.p95),
                threshold,
                unit: "percent".to_owned(),
                entity: None,
            },
        );
    }

    if cpu.logical_cores > 0 && cpu.run_queue_1m.count > 0 {
        ctx.mark_evaluated("cpu_run_queue_pressure");
        #[allow(clippy::cast_precision_loss)]
        let cores_f = cpu.logical_cores as f64;
        if let Some((severity, threshold)) =
            eval_high_threshold(cpu.run_queue_1m.p95, cores_f * 1.5, cores_f * 2.5)
        {
            ctx.push_finding(
                "cpu_run_queue_pressure",
                severity,
                DiagnosticCategory::Cpu,
                format!(
                    "Window p95 run queue ({:.1}) exceeds healthy capacity for {} logical cores",
                    round1(cpu.run_queue_1m.p95),
                    cpu.logical_cores
                ),
                DiagnosticEvidence {
                    metric: "cpu.run_queue_1m.p95".to_owned(),
                    observed: round1(cpu.run_queue_1m.p95),
                    threshold: round1(threshold),
                    unit: "threads".to_owned(),
                    entity: None,
                },
            );
        }
    } else {
        ctx.mark_skipped(
            "cpu_run_queue_pressure",
            DiagnosticCategory::Cpu,
            "logical core count or run queue unavailable",
        );
    }

    if let Some(temp) = cpu.package_temp_celsius {
        ctx.mark_evaluated("cpu_thermal_pressure");
        if let Some((severity, threshold)) = eval_high_threshold(temp.max, 85.0, 95.0) {
            ctx.push_finding(
                "cpu_thermal_pressure",
                severity,
                DiagnosticCategory::Cpu,
                format!(
                    "Peak CPU package temperature reached {:.1} C",
                    round1(temp.max)
                ),
                DiagnosticEvidence {
                    metric: "cpu.package_temp_celsius.max".to_owned(),
                    observed: round1(temp.max),
                    threshold,
                    unit: "celsius".to_owned(),
                    entity: None,
                },
            );
        }
    } else {
        ctx.mark_skipped(
            "cpu_thermal_pressure",
            DiagnosticCategory::Cpu,
            "CPU package temperature sensor unavailable",
        );
    }
}

fn diagnose_disks_summary(
    ctx: &mut EvalContext,
    disks: Option<&[crate::summary::DiskSummary]>,
    selected: bool,
) {
    if !selected {
        ctx.mark_skipped(
            "disk_health",
            DiagnosticCategory::Disk,
            "section not selected",
        );
        return;
    }
    let Some(disks) = disks.filter(|d| !d.is_empty()) else {
        ctx.mark_skipped(
            "disk_health",
            DiagnosticCategory::Disk,
            "no available disk samples in window",
        );
        return;
    };

    let mut eval_space = false;
    let mut eval_busy = false;
    for d in disks {
        if let Some(free_pct) = ratio_pct_u64(d.free_bytes.min, d.total_bytes) {
            eval_space = true;
            if let Some((severity, threshold)) = eval_low_threshold(free_pct, 15.0, 5.0) {
                ctx.push_finding(
                    "disk_low_free_space",
                    severity,
                    DiagnosticCategory::Disk,
                    format!(
                        "Volume {} minimum free space is low ({:.1}% free)",
                        d.mount,
                        round1(free_pct)
                    ),
                    DiagnosticEvidence {
                        metric: "disk.free_pct.min".to_owned(),
                        observed: round1(free_pct),
                        threshold,
                        unit: "percent".to_owned(),
                        entity: Some(DiagnosticEntity {
                            mount: Some(d.mount.clone()),
                            ..DiagnosticEntity::default()
                        }),
                    },
                );
            }
        }
        if let Some(busy) = d.busy_pct {
            eval_busy = true;
            if let Some((severity, threshold)) = eval_high_threshold(busy.p95, 80.0, 95.0) {
                ctx.push_finding(
                    "disk_high_busy",
                    severity,
                    DiagnosticCategory::Disk,
                    format!(
                        "Volume {} p95 busy time is saturated ({:.1}%)",
                        d.mount,
                        round1(busy.p95)
                    ),
                    DiagnosticEvidence {
                        metric: "disk.busy_pct.p95".to_owned(),
                        observed: round1(busy.p95),
                        threshold,
                        unit: "percent".to_owned(),
                        entity: Some(DiagnosticEntity {
                            mount: Some(d.mount.clone()),
                            ..DiagnosticEntity::default()
                        }),
                    },
                );
            }
        }
    }
    if eval_space {
        ctx.mark_evaluated("disk_low_free_space");
    } else {
        ctx.mark_skipped(
            "disk_low_free_space",
            DiagnosticCategory::Disk,
            "volume capacities unavailable",
        );
    }
    if eval_busy {
        ctx.mark_evaluated("disk_high_busy");
    } else {
        ctx.mark_skipped(
            "disk_high_busy",
            DiagnosticCategory::Disk,
            "disk busy percentage unavailable",
        );
    }
}

#[derive(Default)]
struct GpuWindowAccum {
    index: u8,
    name: String,
    vram_pcts: Vec<f64>,
    utils: Vec<f64>,
    temps: Vec<f64>,
    temp_max_c: Option<f32>,
    power_pcts: Vec<f64>,
}

fn diagnose_gpus_window(ctx: &mut EvalContext, snapshots: &[Snapshot], selected: bool) {
    if !selected {
        ctx.mark_skipped(
            "gpu_health",
            DiagnosticCategory::Gpu,
            "section not selected",
        );
        return;
    }

    let mut per_gpu: HashMap<u8, GpuWindowAccum> = HashMap::new();
    for snap in snapshots {
        for state in &snap.gpus {
            let Some(gpu) = state_ref(state) else {
                continue;
            };
            push_gpu_window_sample(&mut per_gpu, gpu);
        }
    }

    if per_gpu.is_empty() {
        ctx.mark_skipped(
            "gpu_health",
            DiagnosticCategory::Gpu,
            "no available GPU samples in window",
        );
        return;
    }

    let mut accs: Vec<GpuWindowAccum> = per_gpu.into_values().collect();
    accs.sort_by_key(|a| a.index);

    let mut cov = GpuCheckCoverage::default();
    for acc in accs {
        evaluate_gpu_window(ctx, &mut cov, &acc);
    }
    record_optional_gpu_checks(ctx, &cov);
}

fn push_gpu_window_sample(per_gpu: &mut HashMap<u8, GpuWindowAccum>, gpu: &GpuSnapshot) {
    let acc = per_gpu.entry(gpu.index).or_insert_with(|| GpuWindowAccum {
        index: gpu.index,
        ..GpuWindowAccum::default()
    });
    acc.name.clone_from(&gpu.name);
    if let (Some(used), Some(total)) = (gpu.vram_used_bytes, gpu.vram_total_bytes)
        && let Some(pct) = ratio_pct_u64(used, total)
    {
        acc.vram_pcts.push(pct);
    }
    if let Some(util) = gpu.utilization_pct.filter(|v| v.is_finite()) {
        acc.utils.push(f64::from(util));
    }
    if let Some(temp) = gpu.temp_celsius.filter(|v| v.is_finite()) {
        acc.temps.push(f64::from(temp));
    }
    if let Some(max_t) = gpu.temp_max_celsius.filter(|v| v.is_finite() && *v > 50.0) {
        acc.temp_max_c = Some(acc.temp_max_c.map_or(max_t, |prev| prev.min(max_t)));
    }
    if let (Some(power), Some(limit)) = (gpu.power_watts, gpu.power_limit_watts)
        && power.is_finite()
        && limit.is_finite()
        && limit > 0.0
    {
        acc.power_pcts
            .push((f64::from(power) / f64::from(limit)) * 100.0);
    }
}

fn evaluate_gpu_window(ctx: &mut EvalContext, cov: &mut GpuCheckCoverage, acc: &GpuWindowAccum) {
    let entity = DiagnosticEntity {
        gpu_index: Some(acc.index),
        gpu_name: Some(acc.name.clone()),
        ..DiagnosticEntity::default()
    };
    if let Some(vram) = ScalarStats::from_f64(&acc.vram_pcts) {
        cov.vram = true;
        if let Some((severity, threshold)) = eval_high_threshold(vram.max, 85.0, 95.0) {
            ctx.push_finding(
                "gpu_vram_pressure",
                severity,
                DiagnosticCategory::Gpu,
                format!(
                    "GPU {} ({}) peak VRAM reached {:.1}%",
                    acc.index,
                    acc.name,
                    round1(vram.max)
                ),
                DiagnosticEvidence {
                    metric: "gpu.vram_used_pct.max".to_owned(),
                    observed: round1(vram.max),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: Some(entity.clone()),
                },
            );
        }
    }
    if let Some(util) = ScalarStats::from_f64(&acc.utils) {
        cov.util = true;
        if let Some((severity, threshold)) = eval_high_threshold(util.p95, 95.0, 99.0) {
            ctx.push_finding(
                "gpu_compute_saturation",
                severity,
                DiagnosticCategory::Gpu,
                format!(
                    "GPU {} ({}) p95 utilization is saturated at {:.1}%",
                    acc.index,
                    acc.name,
                    round1(util.p95)
                ),
                DiagnosticEvidence {
                    metric: "gpu.utilization_pct.p95".to_owned(),
                    observed: round1(util.p95),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: Some(entity.clone()),
                },
            );
        }
    }
    if let Some(temp) = ScalarStats::from_f64(&acc.temps) {
        cov.temp = true;
        let crit = acc.temp_max_c.map_or(90.0, f64::from);
        let warn = (crit - 8.0).max(40.0);
        if let Some((severity, threshold)) = eval_high_threshold(temp.max, warn, crit) {
            ctx.push_finding(
                "gpu_thermal_pressure",
                severity,
                DiagnosticCategory::Gpu,
                format!(
                    "GPU {} ({}) peak temperature reached {:.1} C",
                    acc.index,
                    acc.name,
                    round1(temp.max)
                ),
                DiagnosticEvidence {
                    metric: "gpu.temp_celsius.max".to_owned(),
                    observed: round1(temp.max),
                    threshold: round1(threshold),
                    unit: "celsius".to_owned(),
                    entity: Some(entity.clone()),
                },
            );
        }
    }
    if let Some(power) = ScalarStats::from_f64(&acc.power_pcts) {
        cov.power = true;
        if let Some((severity, threshold)) = eval_high_threshold(power.p95, 92.0, 98.0) {
            ctx.push_finding(
                "gpu_power_limit_saturation",
                severity,
                DiagnosticCategory::Gpu,
                format!(
                    "GPU {} ({}) p95 power draw reached {:.1}% of power limit",
                    acc.index,
                    acc.name,
                    round1(power.p95)
                ),
                DiagnosticEvidence {
                    metric: "gpu.power_limit_pct.p95".to_owned(),
                    observed: round1(power.p95),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: Some(entity),
                },
            );
        }
    }
}

/// Evaluates pure deterministic health and bottleneck diagnostics directly over a raw un-truncated
/// slice of [`Snapshot`] frames (used by `--summary --diagnose`).
///
/// Computing memory, network, and GPU ratios per sample before window aggregation prevents
/// cross-sample extrema distortion (e.g., mid-window pagefile expansion or GPU power-limit changes)
/// and ensures `--filter`/`--pid`/`--top` projections never hide runaway processes or subtrees.
pub fn diagnose_window(snapshots: &[Snapshot], sections: JsonSections) -> DiagnosticsReport {
    let unfiltered_query = HeadlessQuery {
        top: Some(0),
        ..HeadlessQuery::default()
    };
    let full_summary = JsonSummaryEnvelope::from_snapshots(
        snapshots,
        "",
        None,
        sections,
        &unfiltered_query,
        &QueryFilter::None,
        false,
    );

    let mut ctx = EvalContext::default();
    diagnose_cpu_summary(&mut ctx, full_summary.cpu.as_ref(), sections.cpu);
    diagnose_mem_window(&mut ctx, snapshots, sections.mem);
    diagnose_disks_summary(&mut ctx, full_summary.disks.as_deref(), sections.disks);
    diagnose_net_window(&mut ctx, snapshots, sections.net);
    diagnose_gpus_window(&mut ctx, snapshots, sections.gpus);

    let total_phys_mem = snapshots.iter().rev().find_map(|s| {
        state_ref(&s.mem)
            .map(|m| m.total_bytes)
            .filter(|&b| b > 0)
            .or(s.total_phys_mem_bytes)
    });
    diagnose_procs_window(
        &mut ctx,
        snapshots,
        full_summary.procs.as_ref(),
        total_phys_mem,
        sections.procs,
    );

    ctx.finish()
}

fn diagnose_mem_window(ctx: &mut EvalContext, snapshots: &[Snapshot], selected: bool) {
    if !selected {
        ctx.mark_skipped(
            "memory_health",
            DiagnosticCategory::Memory,
            "section not selected",
        );
        return;
    }
    let mems: Vec<&MemSnapshot> = snapshots.iter().filter_map(|s| state_ref(&s.mem)).collect();
    if mems.is_empty() {
        ctx.mark_skipped(
            "memory_health",
            DiagnosticCategory::Memory,
            "no available memory samples in window",
        );
        return;
    }

    let min_avail_pct = mems
        .iter()
        .filter_map(|m| ratio_pct_u64(m.available_bytes, m.total_bytes))
        .reduce(f64::min);
    if let Some(avail_pct) = min_avail_pct {
        ctx.mark_evaluated("memory_low_available");
        if let Some((severity, threshold)) = eval_low_threshold(avail_pct, 15.0, 7.0) {
            ctx.push_finding(
                "memory_low_available",
                severity,
                DiagnosticCategory::Memory,
                format!(
                    "Minimum available physical RAM in window was {:.1}%",
                    round1(avail_pct)
                ),
                DiagnosticEvidence {
                    metric: "mem_window.available_pct.min".to_owned(),
                    observed: round1(avail_pct),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: None,
                },
            );
        }
    } else {
        ctx.mark_skipped(
            "memory_low_available",
            DiagnosticCategory::Memory,
            "total physical memory is zero",
        );
    }

    let max_commit_pct = mems
        .iter()
        .filter_map(|m| ratio_pct_u64(m.commit_used_bytes, m.commit_limit_bytes))
        .reduce(f64::max);
    if let Some(commit_pct) = max_commit_pct {
        ctx.mark_evaluated("memory_commit_pressure");
        if let Some((severity, threshold)) = eval_high_threshold(commit_pct, 80.0, 92.0) {
            ctx.push_finding(
                "memory_commit_pressure",
                severity,
                DiagnosticCategory::Memory,
                format!(
                    "Peak Windows commit charge in window reached {:.1}% of commit limit",
                    round1(commit_pct)
                ),
                DiagnosticEvidence {
                    metric: "mem_window.commit_pct.max".to_owned(),
                    observed: round1(commit_pct),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: None,
                },
            );
        }
    } else {
        ctx.mark_skipped(
            "memory_commit_pressure",
            DiagnosticCategory::Memory,
            "commit limit unavailable",
        );
    }

    diagnose_mem_window_paging(ctx, &mems);
}

fn diagnose_mem_window_paging(ctx: &mut EvalContext, mems: &[&MemSnapshot]) {
    let max_pf_pct = mems
        .iter()
        .filter_map(|m| ratio_pct_u64(m.pagefile_used_bytes, m.pagefile_total_bytes))
        .reduce(f64::max);
    if let Some(pf_pct) = max_pf_pct {
        ctx.mark_evaluated("memory_pagefile_pressure");
        if let Some((severity, threshold)) = eval_high_threshold(pf_pct, 75.0, 90.0) {
            ctx.push_finding(
                "memory_pagefile_pressure",
                severity,
                DiagnosticCategory::Memory,
                format!(
                    "Peak pagefile utilization in window reached {:.1}%",
                    round1(pf_pct)
                ),
                DiagnosticEvidence {
                    metric: "mem_window.pagefile_pct.max".to_owned(),
                    observed: round1(pf_pct),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: None,
                },
            );
        }
    } else {
        ctx.mark_skipped(
            "memory_pagefile_pressure",
            DiagnosticCategory::Memory,
            "pagefile absent or zero capacity",
        );
    }

    let max_comp_pct = mems
        .iter()
        .filter_map(|m| {
            m.compressed_bytes
                .and_then(|c| ratio_pct_u64(c, m.total_bytes))
        })
        .reduce(f64::max);
    if let Some(comp_pct) = max_comp_pct {
        ctx.mark_evaluated("memory_high_compressed");
        if let Some((severity, threshold)) = eval_high_threshold(comp_pct, 15.0, 25.0) {
            ctx.push_finding(
                "memory_high_compressed",
                severity,
                DiagnosticCategory::Memory,
                format!(
                    "Peak compressed memory in window reached {:.1}% of physical RAM",
                    round1(comp_pct)
                ),
                DiagnosticEvidence {
                    metric: "mem_window.compressed_pct.max".to_owned(),
                    observed: round1(comp_pct),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: None,
                },
            );
        }
    } else {
        ctx.mark_skipped(
            "memory_high_compressed",
            DiagnosticCategory::Memory,
            "compressed memory metric unavailable",
        );
    }
}

fn diagnose_net_window(ctx: &mut EvalContext, snapshots: &[Snapshot], selected: bool) {
    if !selected {
        ctx.mark_skipped(
            "network_health",
            DiagnosticCategory::Network,
            "section not selected",
        );
        return;
    }

    let mut per_iface: HashMap<u64, (String, Vec<f64>)> = HashMap::new();
    let mut saw_any_net = false;
    for snap in snapshots {
        let Some(ifaces) = state_ref(&snap.net) else {
            continue;
        };
        saw_any_net = true;
        for iface in ifaces {
            if !iface.is_up {
                continue;
            }
            let Some(link_bps) = iface.link_speed_bps.filter(|&b| b > 0) else {
                continue;
            };
            let bps = iface
                .rx_bytes_per_sec
                .max(iface.tx_bytes_per_sec)
                .saturating_mul(8);
            if let Some(pct) = ratio_pct_u64(bps, link_bps) {
                let entry = per_iface
                    .entry(iface.luid)
                    .or_insert_with(|| (iface.name.clone(), Vec::new()));
                entry.0.clone_from(&iface.name);
                entry.1.push(pct);
            }
        }
    }

    if !saw_any_net {
        ctx.mark_skipped(
            "network_health",
            DiagnosticCategory::Network,
            "no available network samples in window",
        );
        return;
    }
    if per_iface.is_empty() {
        ctx.mark_skipped(
            "net_high_bandwidth_utilization",
            DiagnosticCategory::Network,
            "no active network interface with known link speed",
        );
        return;
    }

    ctx.mark_evaluated("net_high_bandwidth_utilization");
    for (luid, (name, pcts)) in per_iface {
        if let Some(stats) = ScalarStats::from_f64(&pcts)
            && let Some((severity, threshold)) = eval_high_threshold(stats.p95, 80.0, 95.0)
        {
            ctx.push_finding(
                "net_high_bandwidth_utilization",
                severity,
                DiagnosticCategory::Network,
                format!(
                    "Network interface {name} p95 throughput reached {:.1}% of link speed",
                    round1(stats.p95)
                ),
                DiagnosticEvidence {
                    metric: "net.bandwidth_utilization_pct.p95".to_owned(),
                    observed: round1(stats.p95),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: Some(DiagnosticEntity {
                        interface: Some(name),
                        luid: Some(luid),
                        ..DiagnosticEntity::default()
                    }),
                },
            );
        }
    }
}

fn diagnose_procs_window(
    ctx: &mut EvalContext,
    snapshots: &[Snapshot],
    procs_summary: Option<&crate::summary::ProcsWindowSummary>,
    total_phys_mem: Option<u64>,
    selected: bool,
) {
    if !selected {
        ctx.mark_skipped(
            "process_health",
            DiagnosticCategory::Process,
            "section not selected",
        );
        return;
    }
    let Some(procs) = procs_summary.filter(|p| p.samples_available > 0) else {
        ctx.mark_skipped(
            "process_health",
            DiagnosticCategory::Process,
            "no available process samples in window",
        );
        return;
    };

    diagnose_summary_procs(ctx, &procs.by_cpu, &procs.by_mem, total_phys_mem);

    // Evaluate multi-process subtree CPU dominance across ALL raw window frames so p95 reflects
    // the full window rather than only spiking frames.
    ctx.mark_evaluated("process_subtree_cpu_dominance");
    let mut candidate_roots: HashMap<ProcessIdentity, String> = HashMap::new();
    let proc_frames: Vec<&[ProcessEntry]> = snapshots
        .iter()
        .filter_map(|s| state_ref(&s.processes).map(Vec::as_slice))
        .collect();
    for list in &proc_frames {
        for (idx, _sub_cpu) in find_dominant_subtrees(list) {
            let entry = &list[idx];
            candidate_roots
                .entry(entry.identity)
                .or_insert_with(|| entry.name.clone());
        }
    }

    if candidate_roots.is_empty() {
        return;
    }

    let mut subtree_series: HashMap<ProcessIdentity, Vec<f64>> = candidate_roots
        .keys()
        .map(|&id| (id, Vec::with_capacity(proc_frames.len())))
        .collect();

    for list in &proc_frames {
        let (_parent_idx, children) = build_acyclic_adjacency(list);
        let totals = compute_subtree_totals(list, &children);
        let by_id: HashMap<ProcessIdentity, f64> = list
            .iter()
            .enumerate()
            .map(|(i, p)| (p.identity, f64::from(totals[i].0)))
            .collect();
        for (id, series) in &mut subtree_series {
            series.push(by_id.get(id).copied().unwrap_or(0.0));
        }
    }

    for (identity, values) in subtree_series {
        let name = candidate_roots.get(&identity).cloned().unwrap_or_default();
        if let Some(stats) = ScalarStats::from_f64(&values)
            && let Some((severity, threshold)) = eval_high_threshold(stats.p95, 70.0, 90.0)
        {
            ctx.push_finding(
                "process_subtree_cpu_dominance",
                severity,
                DiagnosticCategory::Process,
                format!(
                    "Process subtree rooted at {name} (PID {}) reached {:.1}% p95 CPU in window",
                    identity.pid,
                    round1(stats.p95)
                ),
                DiagnosticEvidence {
                    metric: "process.subtree_cpu_pct.p95".to_owned(),
                    observed: round1(stats.p95),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: Some(DiagnosticEntity {
                        pid: Some(identity.pid),
                        create_time_100ns: Some(identity.create_time_100ns),
                        name: Some(name),
                        ..DiagnosticEntity::default()
                    }),
                },
            );
        }
    }
}

fn diagnose_summary_procs(
    ctx: &mut EvalContext,
    by_cpu: &[ProcessWindowEntry],
    by_mem: &[ProcessWindowEntry],
    total_phys_mem: Option<u64>,
) {
    ctx.mark_evaluated("process_cpu_dominance");
    for p in by_cpu {
        if p.pid == 0 {
            continue;
        }
        if let Some((severity, threshold)) = eval_high_threshold(p.avg_cpu_pct, 60.0, 85.0) {
            ctx.push_finding(
                "process_cpu_dominance",
                severity,
                DiagnosticCategory::Process,
                format!(
                    "Process {} (PID {}) averaged {:.1}% CPU across the window",
                    p.name,
                    p.pid,
                    round1(p.avg_cpu_pct)
                ),
                DiagnosticEvidence {
                    metric: "process.avg_cpu_pct".to_owned(),
                    observed: round1(p.avg_cpu_pct),
                    threshold,
                    unit: "percent".to_owned(),
                    entity: Some(DiagnosticEntity {
                        pid: Some(p.pid),
                        create_time_100ns: Some(p.create_time_100ns),
                        name: Some(p.name.clone()),
                        ..DiagnosticEntity::default()
                    }),
                },
            );
        }
    }

    if let Some(total_mem) = total_phys_mem.filter(|&b| b > 0) {
        ctx.mark_evaluated("process_memory_dominance");
        for p in by_mem {
            if p.pid == 0 {
                continue;
            }
            if let Some(mem_pct) = ratio_pct_u64(p.peak_working_set_bytes, total_mem)
                && let Some((severity, threshold)) = eval_high_threshold(mem_pct, 25.0, 45.0)
            {
                ctx.push_finding(
                    "process_memory_dominance",
                    severity,
                    DiagnosticCategory::Process,
                    format!(
                        "Process {} (PID {}) peaked at {:.1}% of physical RAM",
                        p.name,
                        p.pid,
                        round1(mem_pct)
                    ),
                    DiagnosticEvidence {
                        metric: "process.peak_working_set_pct".to_owned(),
                        observed: round1(mem_pct),
                        threshold,
                        unit: "percent".to_owned(),
                        entity: Some(DiagnosticEntity {
                            pid: Some(p.pid),
                            create_time_100ns: Some(p.create_time_100ns),
                            name: Some(p.name.clone()),
                            ..DiagnosticEntity::default()
                        }),
                    },
                );
            }
        }
    } else {
        ctx.mark_skipped(
            "process_memory_dominance",
            DiagnosticCategory::Process,
            "total physical memory unavailable",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::win11_16core_dual_gpu_desktop;
    use crate::{CoreKind, CoreSample, LogicalCpuId, ProcessIdentity};

    fn make_proc(
        pid: u32,
        parent_pid: u32,
        create_time: u64,
        name: &str,
        cpu_pct: f32,
        mem_bytes: u64,
    ) -> ProcessEntry {
        ProcessEntry {
            identity: ProcessIdentity {
                pid,
                create_time_100ns: create_time,
            },
            parent_pid,
            name: name.to_owned(),
            image_path: None,
            command_line: None,
            command_line_denied: false,
            user: None,
            user_denied: false,
            threads: 4,
            cpu_pct,
            working_set_private_bytes: mem_bytes,
            io_read_bytes: 0,
            io_write_bytes: 0,
            is_protected: false,
        }
    }

    fn healthy_snapshot() -> Snapshot {
        let cores = (0u8..8)
            .map(|n| CoreSample {
                id: LogicalCpuId {
                    group: 0,
                    number: n,
                },
                busy_pct: 15.0,
                kind: CoreKind::Uniform,
                temp_celsius: None,
            })
            .collect();
        Snapshot {
            timestamp_ms: 1_000,
            hostname: "HOME".to_owned(),
            username: "franz".to_owned(),
            total_phys_mem_bytes: Some(32_000_000_000),
            cpu: DataState::Available(CpuSnapshot {
                model_name: "Ryzen".to_owned(),
                total_busy_pct: 15.0,
                run_queue_ewma: [2.0, 1.5, 1.0],
                cores,
                ..CpuSnapshot::default()
            }),
            mem: DataState::Available(MemSnapshot {
                total_bytes: 32_000_000_000,
                used_bytes: 12_000_000_000,
                available_bytes: 20_000_000_000,
                commit_used_bytes: 14_000_000_000,
                commit_limit_bytes: 40_000_000_000,
                pagefile_used_bytes: 500_000_000,
                pagefile_total_bytes: 8_000_000_000,
                compressed_bytes: Some(500_000_000),
                ..MemSnapshot::default()
            }),
            disks: DataState::Available(vec![DiskSnapshot {
                mount: "C:\\".to_owned(),
                total_bytes: 1_000_000_000_000,
                used_bytes: 400_000_000_000,
                free_bytes: 600_000_000_000,
                busy_pct: Some(5.0),
                ..DiskSnapshot::default()
            }]),
            net: DataState::Available(vec![NetInterfaceSnapshot {
                luid: 1,
                name: "Ethernet".to_owned(),
                is_up: true,
                rx_bytes_per_sec: 1_000_000,
                tx_bytes_per_sec: 500_000,
                link_speed_bps: Some(1_000_000_000),
                ..NetInterfaceSnapshot::default()
            }]),
            processes: DataState::Available(vec![
                // PID 0 idle at 85% must NEVER trigger process_cpu_dominance.
                make_proc(0, 0, 0, "System Idle Process", 85.0, 0),
                make_proc(100, 4, 10, "app.exe", 12.0, 1_000_000_000),
            ]),
            gpus: vec![DataState::Available(GpuSnapshot {
                index: 0,
                name: "RTX 4090".to_owned(),
                vendor: "NVIDIA".to_owned(),
                pci_bdf: None,
                utilization_pct: Some(20.0),
                vram_used_bytes: Some(4_000_000_000),
                vram_total_bytes: Some(24_000_000_000),
                shared_vram_used_bytes: None,
                shared_vram_total_bytes: None,
                luid: None,
                core_clock_mhz: Some(2100),
                mem_clock_mhz: None,
                temp_celsius: Some(45.0),
                temp_max_celsius: Some(88.0),
                power_watts: Some(120.0),
                power_limit_watts: Some(450.0),
                pstate: None,
                pcie_tx_kbs: None,
                pcie_rx_kbs: None,
                encoder_pct: None,
                decoder_pct: None,
            })],
            ..Snapshot::default()
        }
    }

    #[test]
    fn healthy_baseline_reports_zero_findings_and_skips_missing_cpu_temp() {
        let report = diagnose_snapshot(&healthy_snapshot(), JsonSections::all());
        assert_eq!(report.overall_status, HealthStatus::Healthy);
        assert_eq!(report.findings.len(), 0);
        assert!(
            report
                .checks_evaluated
                .iter()
                .any(|c| c == "cpu_saturation")
        );
        assert!(
            report
                .skipped_checks
                .iter()
                .any(|s| s.check == "cpu_thermal_pressure")
        );
    }

    #[test]
    fn threshold_crossings_across_all_categories_produce_sorted_evidence_backed_findings() {
        let mut snap = healthy_snapshot();
        if let DataState::Available(ref mut cpu) = snap.cpu {
            cpu.total_busy_pct = 96.5;
            cpu.run_queue_ewma[0] = 22.0; // > 2.5 * 8 cores = 20.0
            cpu.package_temp_celsius = Some(91.0);
        }
        if let DataState::Available(ref mut mem) = snap.mem {
            mem.available_bytes = 1_500_000_000; // 4.7% -> critical
            mem.commit_used_bytes = 37_500_000_000; // 93.8% -> critical
            mem.pagefile_used_bytes = 6_500_000_000; // 81.3% -> warning
            mem.compressed_bytes = Some(6_400_000_000); // 20.0% -> warning
        }
        if let DataState::Available(ref mut disks) = snap.disks {
            disks[0].free_bytes = 40_000_000_000; // 4.0% -> critical
            disks[0].busy_pct = Some(88.0); // warning
        }
        if let DataState::Available(ref mut net) = snap.net {
            net[0].rx_bytes_per_sec = 110_000_000; // 880 Mbps / 1 Gbps = 88% -> warning
        }
        if let DataState::Available(ref mut gpu) = snap.gpus[0] {
            gpu.vram_used_bytes = Some(23_200_000_000); // 96.7% -> critical
            gpu.temp_celsius = Some(86.0); // warning (88 - 8 = 80)
            gpu.power_watts = Some(430.0); // 95.6% -> warning
            gpu.utilization_pct = Some(99.5); // critical
        }
        snap.processes = DataState::Available(vec![
            make_proc(10, 4, 10, "cargo.exe", 20.0, 2_000_000_000),
            make_proc(11, 10, 20, "rustc.exe", 62.0, 16_000_000_000), // 50% RAM critical, 62% CPU warn
        ]);

        let report = diagnose_snapshot(&snap, JsonSections::all());
        assert_eq!(report.overall_status, HealthStatus::Critical);
        // Critical findings must precede warning findings.
        let first_warn = report
            .findings
            .iter()
            .position(|f| f.severity == DiagnosticSeverity::Warning)
            .expect("has warning");
        assert!(
            report.findings[..first_warn]
                .iter()
                .all(|f| f.severity == DiagnosticSeverity::Critical)
        );
        for expected_code in [
            "cpu_saturation",
            "cpu_run_queue_pressure",
            "cpu_thermal_pressure",
            "memory_low_available",
            "memory_commit_pressure",
            "memory_pagefile_pressure",
            "memory_high_compressed",
            "disk_low_free_space",
            "disk_high_busy",
            "net_high_bandwidth_utilization",
            "gpu_vram_pressure",
            "gpu_thermal_pressure",
            "gpu_power_limit_saturation",
            "gpu_compute_saturation",
            "process_cpu_dominance",
            "process_subtree_cpu_dominance",
            "process_memory_dominance",
        ] {
            assert!(
                report.findings.iter().any(|f| f.code == expected_code),
                "missing expected finding {expected_code}"
            );
        }
    }

    #[test]
    fn warming_up_denied_and_unavailable_states_skip_cleanly_without_false_positives() {
        let snap = Snapshot {
            cpu: DataState::WarmingUp,
            mem: DataState::Unavailable("mem unavailable".to_owned()),
            disks: DataState::Denied,
            net: DataState::WarmingUp,
            processes: DataState::Denied,
            gpus: vec![DataState::Unavailable("no gpu".to_owned())],
            ..Snapshot::default()
        };
        let report = diagnose_snapshot(&snap, JsonSections::all());
        assert_eq!(report.overall_status, HealthStatus::Healthy);
        assert_eq!(report.findings.len(), 0);
        assert_eq!(report.checks_evaluated.len(), 0);
        assert_eq!(report.skipped_checks.len(), 6);
    }

    #[test]
    fn committed_dual_gpu_fixture_diagnoses_deterministically() {
        let fixtures = win11_16core_dual_gpu_desktop();
        let report = diagnose_snapshot(&fixtures[2], JsonSections::all());
        assert_ne!(report.checks_evaluated.len(), 0);
        // Normal desktop fixture must NOT blame Windows kernel System (PID 4) as a critical subtree.
        assert!(
            !report.findings.iter().any(|f| {
                f.code == "process_subtree_cpu_dominance"
                    && f.evidence.entity.as_ref().and_then(|e| e.pid) == Some(4)
            }),
            "PID 4 System must not trigger process_subtree_cpu_dominance"
        );
        // Same input produces identical output.
        assert_eq!(report, diagnose_snapshot(&fixtures[2], JsonSections::all()));
    }

    #[test]
    fn subtree_attribution_blames_deepest_workload_root_and_ignores_solo_leaf_wrappers() {
        // Multi-worker build tree: explorer -> pwsh -> cargo -> {rustc1, rustc2}
        let mut snap = healthy_snapshot();
        snap.processes = DataState::Available(vec![
            make_proc(4, 0, 1, "System", 2.0, 100_000),
            make_proc(100, 4, 10, "explorer.exe", 1.0, 500_000),
            make_proc(200, 100, 20, "pwsh.exe", 1.0, 200_000),
            make_proc(300, 200, 30, "cargo.exe", 10.0, 400_000),
            make_proc(301, 300, 40, "rustc.exe", 35.0, 800_000),
            make_proc(302, 300, 41, "rustc.exe", 35.0, 800_000),
        ]);
        let report = diagnose_snapshot(&snap, JsonSections::all());
        let sub_findings: Vec<&DiagnosticFinding> = report
            .findings
            .iter()
            .filter(|f| f.code == "process_subtree_cpu_dominance")
            .collect();
        assert_eq!(sub_findings.len(), 1);
        assert_eq!(
            sub_findings[0].evidence.entity.as_ref().and_then(|e| e.pid),
            Some(300),
            "must attribute subtree dominance to cargo.exe (PID 300), not explorer/pwsh/System"
        );

        // Single runaway leaf under pwsh must NOT emit a spurious subtree finding on pwsh.
        let mut solo_snap = healthy_snapshot();
        solo_snap.processes = DataState::Available(vec![
            make_proc(100, 4, 10, "explorer.exe", 1.0, 500_000),
            make_proc(200, 100, 20, "pwsh.exe", 1.0, 200_000),
            make_proc(300, 200, 30, "solo_hog.exe", 82.0, 400_000),
        ]);
        let solo_report = diagnose_snapshot(&solo_snap, JsonSections::all());
        assert!(
            solo_report
                .findings
                .iter()
                .any(|f| f.code == "process_cpu_dominance")
        );
        assert!(
            !solo_report
                .findings
                .iter()
                .any(|f| f.code == "process_subtree_cpu_dominance")
        );
    }

    #[test]
    fn window_diagnostics_pair_memory_samples_honor_gpu_temp_max_net_down_and_unfiltered_procs() {
        // Sample 0: commit 30/40 GiB (75%), GPU at 84C with temp_max 84C (critical!), NIC up at 88%.
        let mut s0 = healthy_snapshot();
        s0.timestamp_ms = 1_000;
        if let DataState::Available(ref mut mem) = s0.mem {
            mem.commit_used_bytes = 30_000_000_000;
            mem.commit_limit_bytes = 40_000_000_000;
            mem.pagefile_used_bytes = 4_000_000_000;
            mem.pagefile_total_bytes = 8_000_000_000;
        }
        if let DataState::Available(ref mut gpu) = s0.gpus[0] {
            gpu.temp_celsius = Some(84.0);
            gpu.temp_max_celsius = Some(84.0);
        }
        if let DataState::Available(ref mut net) = s0.net {
            net[0].is_up = true;
            net[0].rx_bytes_per_sec = 110_000_000; // 880 Mbps / 1 Gbps = 88%
        }
        s0.processes = DataState::Available(vec![
            make_proc(500, 4, 50, "hidden_hog.exe", 90.0, 1_000_000_000),
            make_proc(501, 4, 51, "normal.exe", 1.0, 100_000_000),
        ]);

        // Sample 1: Windows expanded pagefile mid-window -> commit 42/56 GiB (still 75%!),
        // NIC disconnects on final tick (`is_up = false`).
        let mut s1 = s0.clone();
        s1.timestamp_ms = 2_000;
        if let DataState::Available(ref mut mem) = s1.mem {
            mem.commit_used_bytes = 42_000_000_000;
            mem.commit_limit_bytes = 56_000_000_000;
            mem.pagefile_used_bytes = 12_000_000_000;
            mem.pagefile_total_bytes = 24_000_000_000;
        }
        if let DataState::Available(ref mut net) = s1.net {
            net[0].is_up = false;
            net[0].rx_bytes_per_sec = 0;
        }

        // Build a heavily filtered & truncated summary (`--filter normal --top 1`), then attach
        // window diagnostics: must still catch `hidden_hog.exe`, GPU 84C critical, NIC 88%, and
        // MUST NOT emit a false commit/pagefile alarm from cross-sample extrema pairing.
        let filter = QueryFilter::compile(Some("normal")).expect("filter");
        let query = HeadlessQuery {
            top: Some(1),
            filter: Some("normal".to_owned()),
            ..HeadlessQuery::default()
        };
        let summary = JsonSummaryEnvelope::from_snapshots(
            &[s0.clone(), s1.clone()],
            "0.1.1",
            None,
            JsonSections::all(),
            &query,
            &filter,
            false,
        )
        .with_window_diagnostics(&[s0, s1], JsonSections::all());

        let diag = summary.diagnostics.expect("window diagnostics");
        assert!(
            !diag
                .findings
                .iter()
                .any(|f| f.code == "memory_commit_pressure"),
            "mid-window pagefile expansion at steady 75% must not trigger commit alarm"
        );
        assert!(
            !diag
                .findings
                .iter()
                .any(|f| f.code == "memory_pagefile_pressure"),
            "mid-window pagefile expansion at steady 50% must not trigger pagefile alarm"
        );
        let gpu_temp = diag
            .findings
            .iter()
            .find(|f| f.code == "gpu_thermal_pressure")
            .expect("gpu temp finding");
        assert_eq!(gpu_temp.severity, DiagnosticSeverity::Critical);
        assert!(
            diag.findings
                .iter()
                .any(|f| f.code == "net_high_bandwidth_utilization"),
            "adapter active at 88% before disconnecting must still trigger finding"
        );
        assert!(
            diag.findings.iter().any(|f| {
                f.code == "process_cpu_dominance"
                    && f.evidence.entity.as_ref().and_then(|e| e.pid) == Some(500)
            }),
            "filtered-out process hidden_hog.exe must still be diagnosed"
        );
    }

    #[test]
    fn window_subtree_p95_uses_all_frames_and_gpu_thresholds_handle_low_target_and_dynamic_boost() {
        // 1. 20 frames: 19 calm frames at 5% subtree CPU + 1 transient spike at 92%.
        // True p95 across 20 frames (index 18) is 5.0%, so NO subtree finding should fire.
        let mut frames = Vec::with_capacity(20);
        for i in 0u64..20 {
            let mut s = healthy_snapshot();
            s.timestamp_ms = 1_000 + i * 1_000;
            let (c0, c1, c2) = if i == 19 {
                (12.0, 40.0, 40.0)
            } else {
                (1.0, 2.0, 2.0)
            };
            s.processes = DataState::Available(vec![
                make_proc(300, 4, 30, "cargo.exe", c0, 400_000),
                make_proc(301, 300, 40, "rustc.exe", c1, 800_000),
                make_proc(302, 300, 41, "rustc.exe", c2, 800_000),
            ]);
            // Dynamic Boost power limit drops mid-window from 300W -> 150W while load stays at 67%.
            if let DataState::Available(ref mut gpu) = s.gpus[0] {
                if i < 10 {
                    gpu.power_watts = Some(200.0);
                    gpu.power_limit_watts = Some(300.0);
                } else {
                    gpu.power_watts = Some(100.0);
                    gpu.power_limit_watts = Some(150.0);
                }
                // Quiet laptop thermal target <= 75C: 66C with max 72C (warn = 64C) must warn!
                gpu.temp_max_celsius = Some(72.0);
                gpu.temp_celsius = Some(66.0);
            }
            frames.push(s);
        }

        let report = diagnose_window(&frames, JsonSections::all());
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.code == "process_subtree_cpu_dominance"),
            "1-of-20 transient subtree spike must not inflate p95"
        );
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.code == "gpu_power_limit_saturation"),
            "mid-window power limit drop at steady 67% must not falsely saturate"
        );
        let temp_finding = report
            .findings
            .iter()
            .find(|f| f.code == "gpu_thermal_pressure")
            .expect("66C against 72C target must emit warning");
        assert_eq!(temp_finding.severity, DiagnosticSeverity::Warning);
    }
}
