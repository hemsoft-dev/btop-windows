//! Pure, deterministic system health and bottleneck diagnostics for headless `--json --diagnose`.

use serde::{Deserialize, Serialize};

use crate::json_output::JsonSections;
use crate::proc_query::{build_acyclic_adjacency, compute_subtree_totals};
use crate::summary::{JsonSummaryEnvelope, ProcessWindowEntry};
use crate::{
    CpuSnapshot, DataState, DiskSnapshot, GpuSnapshot, MemSnapshot, NetInterfaceSnapshot,
    ProcessEntry, Snapshot,
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
        let warn = (inp.temp_crit_c - 8.0).max(75.0);
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

fn diagnose_proc_subtrees(ctx: &mut EvalContext, procs: &[ProcessEntry]) {
    ctx.mark_evaluated("process_subtree_cpu_dominance");
    let (parent_idx, children) = build_acyclic_adjacency(procs);
    let subtree_totals = compute_subtree_totals(procs, &children);
    for (idx, proc_entry) in procs.iter().enumerate() {
        if proc_entry.identity.pid == 0 || children[idx].is_empty() {
            continue;
        }
        if let Some(p) = parent_idx[idx]
            && procs[p].identity.pid != 0
            && f64::from(subtree_totals[p].0) >= 70.0
        {
            continue;
        }
        let sub_cpu = f64::from(subtree_totals[idx].0);
        let self_cpu = f64::from(proc_entry.cpu_pct.max(0.0));
        if sub_cpu <= self_cpu + 5.0 {
            continue;
        }
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

/// Evaluates pure deterministic health and bottleneck diagnostics on an aggregated [`JsonSummaryEnvelope`].
pub fn diagnose_summary(
    summary: &JsonSummaryEnvelope,
    sections: JsonSections,
) -> DiagnosticsReport {
    let mut ctx = EvalContext::default();

    diagnose_cpu_summary(&mut ctx, summary.cpu.as_ref(), sections.cpu);

    let total_phys_mem = summary
        .mem
        .as_ref()
        .map(|m| m.total_bytes)
        .filter(|&b| b > 0);
    if !sections.mem {
        ctx.mark_skipped(
            "memory_health",
            DiagnosticCategory::Memory,
            "section not selected",
        );
    } else if let Some(ref mem) = summary.mem {
        diagnose_mem_values(
            &mut ctx,
            mem.total_bytes,
            mem.available_bytes.min,
            mem.commit_used_bytes.max,
            mem.commit_limit_bytes.min,
            mem.pagefile_used_bytes.max,
            mem.pagefile_total_bytes,
            mem.compressed_bytes.map(|s| s.max),
            "mem_window",
        );
    } else {
        ctx.mark_skipped(
            "memory_health",
            DiagnosticCategory::Memory,
            "no available memory samples in window",
        );
    }

    diagnose_disks_summary(&mut ctx, summary.disks.as_deref(), sections.disks);
    diagnose_net_summary(&mut ctx, summary.net.as_deref(), sections.net);
    diagnose_gpus_summary(&mut ctx, summary.gpus.as_deref(), sections.gpus);

    if !sections.procs {
        ctx.mark_skipped(
            "process_health",
            DiagnosticCategory::Process,
            "section not selected",
        );
    } else if let Some(ref procs) = summary.procs
        && procs.samples_available > 0
    {
        diagnose_summary_procs(&mut ctx, &procs.by_cpu, &procs.by_mem, total_phys_mem);
    } else {
        ctx.mark_skipped(
            "process_health",
            DiagnosticCategory::Process,
            "no available process samples in window",
        );
    }

    ctx.finish()
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

fn diagnose_net_summary(
    ctx: &mut EvalContext,
    net: Option<&[crate::summary::NetSummary]>,
    selected: bool,
) {
    if !selected {
        ctx.mark_skipped(
            "network_health",
            DiagnosticCategory::Network,
            "section not selected",
        );
        return;
    }
    let Some(net) = net else {
        ctx.mark_skipped(
            "network_health",
            DiagnosticCategory::Network,
            "no available network samples in window",
        );
        return;
    };

    let mut eval_bw = false;
    for iface in net {
        if !iface.is_up {
            continue;
        }
        let Some(link_bps) = iface.link_speed_bps.filter(|&b| b > 0) else {
            continue;
        };
        eval_bw = true;
        let p95_bps = iface
            .rx_bytes_per_sec
            .p95
            .max(iface.tx_bytes_per_sec.p95)
            .saturating_mul(8);
        if let Some(util_pct) = ratio_pct_u64(p95_bps, link_bps)
            && let Some((severity, threshold)) = eval_high_threshold(util_pct, 80.0, 95.0)
        {
            ctx.push_finding(
                "net_high_bandwidth_utilization",
                severity,
                DiagnosticCategory::Network,
                format!(
                    "Network interface {} p95 throughput reached {:.1}% of link speed",
                    iface.name,
                    round1(util_pct)
                ),
                DiagnosticEvidence {
                    metric: "net.bandwidth_utilization_pct.p95".to_owned(),
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
    if eval_bw {
        ctx.mark_evaluated("net_high_bandwidth_utilization");
    } else {
        ctx.mark_skipped(
            "net_high_bandwidth_utilization",
            DiagnosticCategory::Network,
            "no active network interface with known link speed",
        );
    }
}

fn diagnose_gpus_summary(
    ctx: &mut EvalContext,
    gpus: Option<&[crate::summary::GpuSummary]>,
    selected: bool,
) {
    if !selected {
        ctx.mark_skipped(
            "gpu_health",
            DiagnosticCategory::Gpu,
            "section not selected",
        );
        return;
    }
    let Some(gpus) = gpus.filter(|g| !g.is_empty()) else {
        ctx.mark_skipped(
            "gpu_health",
            DiagnosticCategory::Gpu,
            "no available GPU samples in window",
        );
        return;
    };

    let mut cov = GpuCheckCoverage::default();
    for gpu in gpus {
        let inp = GpuEvalInputs {
            index: gpu.index,
            name: &gpu.name,
            vram_used: gpu.vram_used_bytes.map(|s| s.max),
            vram_total: gpu.vram_total_bytes,
            util_pct: gpu.utilization_pct.map(|s| s.p95),
            temp_c: gpu.temp_celsius.map(|s| s.max),
            temp_crit_c: 90.0,
            power_w: gpu.power_watts.map(|s| s.p95),
            power_limit_w: gpu.power_limit_watts.map(f64::from),
            metric_suffix: ".window",
        };
        evaluate_single_gpu(ctx, &mut cov, &inp);
    }
    record_optional_gpu_checks(ctx, &cov);
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
        // Same input produces identical output.
        assert_eq!(report, diagnose_snapshot(&fixtures[2], JsonSections::all()));
    }
}
