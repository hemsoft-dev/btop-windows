//! Shared cycle-free Windows process tree builder (`create_time_100ns`-guarded),
//! process/service filtering, sorting, PID subtree slicing, and JSON graph projection.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::{ProcessEntry, ServiceEntry};

/// Sort field for headless `--json` process queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcSortField {
    #[default]
    Cpu,
    Mem,
    Pid,
    Name,
    Threads,
    Io,
}

impl ProcSortField {
    pub const VALID_NAMES: &[&str] = &["cpu", "mem", "pid", "name", "threads", "io"];

    pub fn parse_str(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "cpu" => Ok(Self::Cpu),
            "mem" | "memory" => Ok(Self::Mem),
            "pid" => Ok(Self::Pid),
            "name" => Ok(Self::Name),
            "threads" => Ok(Self::Threads),
            "io" => Ok(Self::Io),
            other => Err(format!(
                "unknown --sort value '{other}': expected one of [{}]",
                Self::VALID_NAMES.join(", ")
            )),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Mem => "mem",
            Self::Pid => "pid",
            Self::Name => "name",
            Self::Threads => "threads",
            Self::Io => "io",
        }
    }
}

/// State filter for headless `--json` Windows Services queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStateFilter {
    #[default]
    All,
    Running,
    Stopped,
}

impl ServiceStateFilter {
    pub const VALID_NAMES: &[&str] = &["all", "running", "stopped"];

    pub fn parse_str(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "running" => Ok(Self::Running),
            "stopped" => Ok(Self::Stopped),
            other => Err(format!(
                "unknown --services-state value '{other}': expected one of [{}]",
                Self::VALID_NAMES.join(", ")
            )),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Running => "running",
            Self::Stopped => "stopped",
        }
    }

    pub fn matches(self, state: &str) -> bool {
        match self {
            Self::All => true,
            Self::Running => state.eq_ignore_ascii_case("running"),
            Self::Stopped => state.eq_ignore_ascii_case("stopped"),
        }
    }
}

/// Compiled case-insensitive substring or `!regex` query filter for headless JSON mode.
#[derive(Debug, Clone)]
pub enum QueryFilter {
    None,
    Substring(String),
    Regex(Regex),
}

impl QueryFilter {
    /// Compiles a raw filter string (`!` prefix compiles case-insensitive regex; otherwise substring).
    /// Returns an error when a `!regex` pattern is invalid or exceeds the 1 MiB size limit.
    pub fn compile(raw: Option<&str>) -> Result<Self, String> {
        let Some(raw) = raw else {
            return Ok(Self::None);
        };
        if raw.is_empty() {
            return Ok(Self::None);
        }
        if let Some(pattern) = raw.strip_prefix('!') {
            if pattern.is_empty() {
                return Ok(Self::None);
            }
            return RegexBuilder::new(pattern)
                .case_insensitive(true)
                .size_limit(1 << 20)
                .build()
                .map(Self::Regex)
                .map_err(|err| format!("invalid regex in --filter '{raw}': {err}"));
        }
        Ok(Self::Substring(raw.to_ascii_lowercase()))
    }

    pub const fn is_active(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn matches_process(&self, entry: &ProcessEntry) -> bool {
        match self {
            Self::None => true,
            Self::Substring(needle) => {
                entry.name.to_ascii_lowercase().contains(needle)
                    || entry
                        .command_line
                        .as_ref()
                        .is_some_and(|cmd| cmd.to_ascii_lowercase().contains(needle))
                    || entry
                        .user
                        .as_ref()
                        .is_some_and(|u| u.to_ascii_lowercase().contains(needle))
                    || entry.identity.pid.to_string().contains(needle)
            }
            Self::Regex(re) => {
                re.is_match(&entry.name)
                    || entry
                        .command_line
                        .as_ref()
                        .is_some_and(|cmd| re.is_match(cmd))
                    || entry.user.as_ref().is_some_and(|u| re.is_match(u))
                    || re.is_match(&entry.identity.pid.to_string())
            }
        }
    }

    pub fn matches_service(&self, entry: &ServiceEntry) -> bool {
        match self {
            Self::None => true,
            Self::Substring(needle) => {
                entry.name.to_ascii_lowercase().contains(needle)
                    || entry.display_name.to_ascii_lowercase().contains(needle)
                    || entry.state.to_ascii_lowercase().contains(needle)
                    || entry.start_type.to_ascii_lowercase().contains(needle)
                    || entry.pid.is_some_and(|p| p.to_string().contains(needle))
            }
            Self::Regex(re) => {
                re.is_match(&entry.name)
                    || re.is_match(&entry.display_name)
                    || re.is_match(&entry.state)
                    || re.is_match(&entry.start_type)
                    || entry.pid.is_some_and(|p| re.is_match(&p.to_string()))
            }
        }
    }
}

/// Query configuration applied to `procs` and `services` when building a headless JSON envelope.
#[derive(Debug, Clone, Default)]
pub struct HeadlessQuery {
    pub top: Option<usize>,
    pub sort: ProcSortField,
    pub reversed: bool,
    pub filter: Option<String>,
    pub pid: Option<u32>,
    pub tree: bool,
    pub services_state: ServiceStateFilter,
}

/// Nested process tree node emitted when `--tree` is enabled in headless `--json` mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessTreeNode {
    #[serde(flatten)]
    pub process: ProcessEntry,
    pub depth: usize,
    pub subtree_cpu_pct: f32,
    pub subtree_working_set_bytes: u64,
    pub children: Vec<Self>,
}

/// Query metadata accompanying `procs` or `proc_tree` in headless JSON output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcsQueryMeta {
    pub total_processes: usize,
    pub total_matched: usize,
    pub returned: usize,
    pub truncated: bool,
    pub sort: ProcSortField,
    pub reversed: bool,
    pub tree: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// Live process metrics joined onto a running Windows Service entry when its PID is present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceLinkedProcess {
    pub pid: u32,
    pub name: String,
    pub cpu_pct: f32,
    pub working_set_private_bytes: u64,
    pub threads: u32,
}

/// Service entry enriched with optional live process linkage for headless JSON output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceJsonEntry {
    #[serde(flatten)]
    pub service: ServiceEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ServiceLinkedProcess>,
}

/// Query metadata accompanying `services` in headless JSON output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServicesQueryMeta {
    pub total_services: usize,
    pub total_matched: usize,
    pub returned: usize,
    pub state_filter: ServiceStateFilter,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

/// Validates parent links using `parent.create_time_100ns <= child.create_time_100ns` plus
/// ancestor cycle detection so reused Windows PIDs or equal-timestamp synthetic inputs can
/// never form cycles.
pub fn build_acyclic_adjacency(procs: &[ProcessEntry]) -> (Vec<Option<usize>>, Vec<Vec<usize>>) {
    let mut pid_to_idx: BTreeMap<u32, usize> = BTreeMap::new();
    for (idx, proc_entry) in procs.iter().enumerate() {
        pid_to_idx.entry(proc_entry.identity.pid).or_insert(idx);
    }

    let mut parent_of = vec![None; procs.len()];
    for (idx, child) in procs.iter().enumerate() {
        if child.parent_pid == child.identity.pid {
            continue;
        }
        let Some(&p_idx) = pid_to_idx.get(&child.parent_pid) else {
            continue;
        };
        if p_idx == idx {
            continue;
        }
        let parent = &procs[p_idx];
        if parent.identity.create_time_100ns == 0
            || parent.identity.create_time_100ns > child.identity.create_time_100ns
        {
            continue;
        }
        if !would_create_cycle(idx, p_idx, &parent_of) {
            parent_of[idx] = Some(p_idx);
        }
    }

    let mut children = vec![Vec::new(); procs.len()];
    for (idx, parent) in parent_of.iter().enumerate() {
        if let Some(p_idx) = *parent {
            children[p_idx].push(idx);
        }
    }
    (parent_of, children)
}

fn would_create_cycle(
    child_idx: usize,
    candidate_parent: usize,
    parent_of: &[Option<usize>],
) -> bool {
    let mut cur = Some(candidate_parent);
    let mut steps = 0usize;
    while let Some(node) = cur {
        if node == child_idx {
            return true;
        }
        steps += 1;
        if steps > parent_of.len() {
            return true;
        }
        cur = parent_of[node];
    }
    false
}

/// Computes rolled-up `(subtree_cpu_pct, subtree_working_set_bytes)` for every process index.
pub fn compute_subtree_totals(procs: &[ProcessEntry], children: &[Vec<usize>]) -> Vec<(f32, u64)> {
    let mut memo: Vec<Option<(f32, u64)>> = vec![None; procs.len()];
    let mut visiting = vec![false; procs.len()];
    for idx in 0..procs.len() {
        let _ = rollup_node(idx, procs, children, &mut memo, &mut visiting);
    }
    memo.into_iter()
        .enumerate()
        .map(|(i, val)| val.unwrap_or((procs[i].cpu_pct, procs[i].working_set_private_bytes)))
        .collect()
}

fn rollup_node(
    idx: usize,
    procs: &[ProcessEntry],
    children: &[Vec<usize>],
    memo: &mut [Option<(f32, u64)>],
    visiting: &mut [bool],
) -> (f32, u64) {
    if let Some(cached) = memo[idx] {
        return cached;
    }
    if visiting[idx] {
        return (procs[idx].cpu_pct, procs[idx].working_set_private_bytes);
    }
    visiting[idx] = true;
    let mut cpu = procs[idx].cpu_pct;
    let mut mem = procs[idx].working_set_private_bytes;
    for &child in &children[idx] {
        let (c_cpu, c_mem) = rollup_node(child, procs, children, memo, visiting);
        cpu += c_cpu;
        mem = mem.saturating_add(c_mem);
    }
    visiting[idx] = false;
    let res = (cpu, mem);
    memo[idx] = Some(res);
    res
}

/// Builds a mask containing `target_pid`, its verified ancestor chain, and all verified descendants.
fn compute_pid_lineage_mask(
    procs: &[ProcessEntry],
    parent_of: &[Option<usize>],
    children: &[Vec<usize>],
    target_pid: u32,
) -> Vec<bool> {
    let mut keep = vec![false; procs.len()];
    let Some(target_idx) = procs.iter().position(|p| p.identity.pid == target_pid) else {
        return keep;
    };

    // Mark target and ancestors.
    let mut cur = Some(target_idx);
    let mut steps = 0usize;
    while let Some(node) = cur {
        if keep[node] || steps > procs.len() {
            break;
        }
        keep[node] = true;
        cur = parent_of[node];
        steps += 1;
    }

    // Mark all descendants via explicit stack (avoids deep recursion).
    let mut stack = vec![target_idx];
    while let Some(node) = stack.pop() {
        for &kid in &children[node] {
            if !keep[kid] {
                keep[kid] = true;
                stack.push(kid);
            }
        }
    }

    keep
}

#[derive(Clone, Copy)]
struct ProcSortKey<'a> {
    entry: &'a ProcessEntry,
    cpu: f32,
    mem: u64,
}

fn compare_proc_entries(
    a: ProcSortKey<'_>,
    b: ProcSortKey<'_>,
    sort: ProcSortField,
    reversed: bool,
) -> Ordering {
    let ord = match sort {
        ProcSortField::Cpu => b.cpu.total_cmp(&a.cpu),
        ProcSortField::Mem => b.mem.cmp(&a.mem),
        ProcSortField::Pid => a.entry.identity.pid.cmp(&b.entry.identity.pid),
        ProcSortField::Name => a
            .entry
            .name
            .to_ascii_lowercase()
            .cmp(&b.entry.name.to_ascii_lowercase()),
        ProcSortField::Threads => b.entry.threads.cmp(&a.entry.threads),
        ProcSortField::Io => b
            .entry
            .io_read_bytes
            .saturating_add(b.entry.io_write_bytes)
            .cmp(&a.entry.io_read_bytes.saturating_add(a.entry.io_write_bytes)),
    };
    let ord = if reversed { ord.reverse() } else { ord };
    ord.then_with(|| a.entry.identity.pid.cmp(&b.entry.identity.pid))
}

/// Queries flat processes with optional `--pid` lineage scoping, `--filter`, `--sort`,
/// `--reverse`, and `--top`.
pub fn query_flat_procs(
    procs: &[ProcessEntry],
    query: &HeadlessQuery,
    compiled_filter: &QueryFilter,
) -> (Vec<ProcessEntry>, ProcsQueryMeta) {
    let total_processes = procs.len();
    let pid_mask = query.pid.map(|pid| {
        let (parent_of, children) = build_acyclic_adjacency(procs);
        compute_pid_lineage_mask(procs, &parent_of, &children, pid)
    });

    let mut indices: Vec<usize> = (0..procs.len())
        .filter(|&idx| {
            pid_mask.as_ref().is_none_or(|mask| mask[idx])
                && compiled_filter.matches_process(&procs[idx])
        })
        .collect();

    indices.sort_by(|&ia, &ib| {
        let a = &procs[ia];
        let b = &procs[ib];
        compare_proc_entries(
            ProcSortKey {
                entry: a,
                cpu: a.cpu_pct,
                mem: a.working_set_private_bytes,
            },
            ProcSortKey {
                entry: b,
                cpu: b.cpu_pct,
                mem: b.working_set_private_bytes,
            },
            query.sort,
            query.reversed,
        )
    });

    let total_matched = indices.len();
    let limit = query.top.filter(|&n| n > 0);
    let truncated = limit.is_some_and(|n| total_matched > n);
    if let Some(n) = limit {
        indices.truncate(n);
    }

    let result: Vec<ProcessEntry> = indices.into_iter().map(|i| procs[i].clone()).collect();
    let returned = result.len();
    let meta = ProcsQueryMeta {
        total_processes,
        total_matched,
        returned,
        truncated,
        sort: query.sort,
        reversed: query.reversed,
        tree: false,
        top: limit,
        filter: query.filter.clone(),
        pid: query.pid,
    };
    (result, meta)
}

/// Queries a nested parent-child process tree with `create_time_100ns` validation,
/// rolled-up subtree totals, optional `--pid` lineage scoping, `--filter`, `--sort`,
/// `--reverse`, and `--top` limiting top-level root trees.
pub fn query_proc_tree(
    procs: &[ProcessEntry],
    query: &HeadlessQuery,
    compiled_filter: &QueryFilter,
) -> (Vec<ProcessTreeNode>, ProcsQueryMeta) {
    let total_processes = procs.len();
    let (parent_of, children) = build_acyclic_adjacency(procs);
    let totals = compute_subtree_totals(procs, &children);

    let pid_mask = query
        .pid
        .map(|pid| compute_pid_lineage_mask(procs, &parent_of, &children, pid));

    // Match mask starts from pid_mask (or all true), then applies filter while preserving
    // ancestors of matched processes so the tree stays connected.
    let mut keep = vec![false; procs.len()];
    let mut matched_count = 0usize;
    for (idx, entry) in procs.iter().enumerate() {
        let in_pid = pid_mask.as_ref().is_none_or(|m| m[idx]);
        if !in_pid {
            continue;
        }
        if compiled_filter.matches_process(entry) {
            matched_count += 1;
            let mut cur = Some(idx);
            let mut steps = 0usize;
            while let Some(node) = cur {
                if keep[node] || steps > procs.len() {
                    break;
                }
                if pid_mask.as_ref().is_some_and(|m| !m[node]) {
                    break;
                }
                keep[node] = true;
                cur = parent_of[node];
                steps += 1;
            }
        }
    }

    let mut roots: Vec<usize> = (0..procs.len())
        .filter(|&i| keep[i] && parent_of[i].is_none_or(|p| !keep[p]))
        .collect();

    roots.sort_by(|&ia, &ib| {
        compare_proc_entries(
            ProcSortKey {
                entry: &procs[ia],
                cpu: totals[ia].0,
                mem: totals[ia].1,
            },
            ProcSortKey {
                entry: &procs[ib],
                cpu: totals[ib].0,
                mem: totals[ib].1,
            },
            query.sort,
            query.reversed,
        )
    });

    let total_roots = roots.len();
    let limit = query.top.filter(|&n| n > 0);
    let truncated = limit.is_some_and(|n| total_roots > n);
    if let Some(n) = limit {
        roots.truncate(n);
    }

    let mut visited = vec![false; procs.len()];
    let tree_nodes: Vec<ProcessTreeNode> = roots
        .into_iter()
        .map(|root_idx| {
            build_tree_node(
                root_idx,
                0,
                procs,
                &children,
                &totals,
                &keep,
                query.sort,
                query.reversed,
                &mut visited,
            )
        })
        .collect();

    let returned = tree_nodes.len();
    let meta = ProcsQueryMeta {
        total_processes,
        total_matched: matched_count,
        returned,
        truncated,
        sort: query.sort,
        reversed: query.reversed,
        tree: true,
        top: limit,
        filter: query.filter.clone(),
        pid: query.pid,
    };
    (tree_nodes, meta)
}

#[allow(clippy::too_many_arguments)]
fn build_tree_node(
    idx: usize,
    depth: usize,
    procs: &[ProcessEntry],
    children: &[Vec<usize>],
    totals: &[(f32, u64)],
    keep: &[bool],
    sort: ProcSortField,
    reversed: bool,
    visited: &mut [bool],
) -> ProcessTreeNode {
    visited[idx] = true;
    let mut visible_kids: Vec<usize> = children[idx]
        .iter()
        .copied()
        .filter(|&k| keep[k] && !visited[k])
        .collect();
    visible_kids.sort_by(|&ia, &ib| {
        compare_proc_entries(
            ProcSortKey {
                entry: &procs[ia],
                cpu: totals[ia].0,
                mem: totals[ia].1,
            },
            ProcSortKey {
                entry: &procs[ib],
                cpu: totals[ib].0,
                mem: totals[ib].1,
            },
            sort,
            reversed,
        )
    });

    let child_nodes = visible_kids
        .into_iter()
        .map(|kid| {
            build_tree_node(
                kid,
                depth + 1,
                procs,
                children,
                totals,
                keep,
                sort,
                reversed,
                visited,
            )
        })
        .collect();

    ProcessTreeNode {
        process: procs[idx].clone(),
        depth,
        subtree_cpu_pct: totals[idx].0,
        subtree_working_set_bytes: totals[idx].1,
        children: child_nodes,
    }
}

/// Filters Windows Services by state and `--filter`, joining matching live process metadata
/// whenever a service reports a live `pid`.
pub fn query_services(
    services: &[ServiceEntry],
    procs: Option<&[ProcessEntry]>,
    query: &HeadlessQuery,
    compiled_filter: &QueryFilter,
) -> (Vec<ServiceJsonEntry>, ServicesQueryMeta) {
    let proc_by_pid: BTreeMap<u32, &ProcessEntry> = procs
        .into_iter()
        .flatten()
        .map(|p| (p.identity.pid, p))
        .collect();

    let total_services = services.len();
    let mut entries: Vec<ServiceJsonEntry> = services
        .iter()
        .filter(|svc| {
            query.services_state.matches(&svc.state) && compiled_filter.matches_service(svc)
        })
        .map(|svc| {
            let linked =
                svc.pid
                    .and_then(|pid| proc_by_pid.get(&pid))
                    .map(|p| ServiceLinkedProcess {
                        pid: p.identity.pid,
                        name: p.name.clone(),
                        cpu_pct: p.cpu_pct,
                        working_set_private_bytes: p.working_set_private_bytes,
                        threads: p.threads,
                    });
            ServiceJsonEntry {
                service: svc.clone(),
                process: linked,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        a.service
            .name
            .to_ascii_lowercase()
            .cmp(&b.service.name.to_ascii_lowercase())
    });

    let total_matched = entries.len();
    let returned = entries.len();
    let meta = ServicesQueryMeta {
        total_services,
        total_matched,
        returned,
        state_filter: query.services_state,
        filter: query.filter.clone(),
    };
    (entries, meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProcessIdentity;

    fn proc_entry(
        pid: u32,
        parent_pid: u32,
        create_time: u64,
        name: &str,
        cpu: f32,
        mem: u64,
    ) -> ProcessEntry {
        ProcessEntry {
            identity: ProcessIdentity {
                pid,
                create_time_100ns: create_time,
            },
            parent_pid,
            name: name.to_owned(),
            image_path: None,
            command_line: Some(format!("{name} --run")),
            command_line_denied: false,
            user: Some("franz".to_owned()),
            user_denied: false,
            threads: 4,
            cpu_pct: cpu,
            working_set_private_bytes: mem,
            io_read_bytes: 100,
            io_write_bytes: 50,
            is_protected: false,
        }
    }

    fn count_nodes(nodes: &[ProcessTreeNode]) -> usize {
        nodes.iter().map(|n| 1 + count_nodes(&n.children)).sum()
    }

    #[test]
    fn tree_guards_against_pid_reuse_and_synthetic_cycles_and_rolls_up_subtrees() {
        let procs = vec![
            // Root parent created at t=100
            proc_entry(10, 0, 100, "parent.exe", 5.0, 1_000),
            // Valid child created at t=200
            proc_entry(20, 10, 200, "child.exe", 12.5, 2_000),
            // Reused PID 30 created at t=500 claims to be parent of PID 40 created earlier at t=300
            proc_entry(30, 0, 500, "reused.exe", 1.0, 500),
            proc_entry(40, 30, 300, "orphan.exe", 2.0, 600),
            // Equal-timestamp synthetic 2-cycle: 50 <-> 60
            proc_entry(50, 60, 400, "cyc_a.exe", 3.0, 300),
            proc_entry(60, 50, 400, "cyc_b.exe", 4.0, 400),
        ];

        let query = HeadlessQuery {
            tree: true,
            sort: ProcSortField::Cpu,
            ..HeadlessQuery::default()
        };
        let (tree, meta) = query_proc_tree(&procs, &query, &QueryFilter::None);
        assert_eq!(meta.total_processes, 6);
        assert_eq!(meta.total_matched, 6);
        assert!(!meta.truncated);

        // PID 10 must be ranked first by rolled-up subtree CPU (5.0 + 12.5 = 17.5).
        assert_eq!(tree[0].process.identity.pid, 10);
        assert!((tree[0].subtree_cpu_pct - 17.5).abs() < 0.01);
        assert_eq!(tree[0].subtree_working_set_bytes, 3_000);
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].process.identity.pid, 20);
        assert_eq!(tree[0].children[0].depth, 1);

        // PID 40 must NOT be attached under younger reused PID 30.
        let reused_node = tree
            .iter()
            .find(|n| n.process.identity.pid == 30)
            .expect("reused root");
        assert_eq!(reused_node.children.len(), 0);
        assert!(tree.iter().any(|n| n.process.identity.pid == 40));

        // Cycle 50 <-> 60 must be broken cleanly (all 6 processes accounted for, no hang).
        assert_eq!(count_nodes(&tree), 6);
    }

    #[test]
    fn pid_slicing_keeps_ancestors_and_descendants_only() {
        let procs = vec![
            proc_entry(1, 0, 10, "init.exe", 1.0, 100),
            proc_entry(2, 1, 20, "shell.exe", 2.0, 200),
            proc_entry(3, 2, 30, "cargo.exe", 15.0, 300),
            proc_entry(4, 3, 40, "rustc.exe", 50.0, 400),
            proc_entry(99, 1, 50, "unrelated.exe", 80.0, 900),
        ];

        let query = HeadlessQuery {
            pid: Some(3),
            tree: true,
            ..HeadlessQuery::default()
        };
        let (tree, meta) = query_proc_tree(&procs, &query, &QueryFilter::None);
        assert_eq!(meta.total_processes, 5);
        assert_eq!(meta.total_matched, 4);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].process.identity.pid, 1);
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].process.identity.pid, 2);
        assert_eq!(tree[0].children[0].children[0].process.identity.pid, 3);
        assert_eq!(
            tree[0].children[0].children[0].children[0]
                .process
                .identity
                .pid,
            4
        );

        // Missing PID returns empty match cleanly.
        let missing = HeadlessQuery {
            pid: Some(9999),
            tree: true,
            ..HeadlessQuery::default()
        };
        let (empty_tree, empty_meta) = query_proc_tree(&procs, &missing, &QueryFilter::None);
        assert_eq!(empty_tree.len(), 0);
        assert_eq!(empty_meta.total_matched, 0);
    }

    #[test]
    fn flat_query_sorts_filters_and_applies_top_metadata() {
        let procs = vec![
            proc_entry(10, 0, 10, "alpha.exe", 10.0, 5_000),
            proc_entry(20, 0, 20, "beta.exe", 30.0, 1_000),
            proc_entry(30, 0, 30, "gamma.exe", 20.0, 9_000),
        ];

        let filter = QueryFilter::compile(Some("!^(alpha|gamma)")).expect("regex");
        let query = HeadlessQuery {
            top: Some(1),
            sort: ProcSortField::Mem,
            reversed: false,
            filter: Some("!^(alpha|gamma)".to_owned()),
            ..HeadlessQuery::default()
        };
        let (rows, meta) = query_flat_procs(&procs, &query, &filter);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "gamma.exe");
        assert_eq!(meta.total_processes, 3);
        assert_eq!(meta.total_matched, 2);
        assert_eq!(meta.returned, 1);
        assert!(meta.truncated);
    }

    #[test]
    fn services_query_filters_state_and_links_live_process() {
        let svcs = vec![
            ServiceEntry {
                name: "Spooler".to_owned(),
                display_name: "Print Spooler".to_owned(),
                state: "Running".to_owned(),
                start_type: "Auto".to_owned(),
                pid: Some(500),
            },
            ServiceEntry {
                name: "WSearch".to_owned(),
                display_name: "Windows Search".to_owned(),
                state: "Stopped".to_owned(),
                start_type: "Manual".to_owned(),
                pid: None,
            },
        ];
        let procs = vec![proc_entry(500, 1, 100, "spoolsv.exe", 1.5, 16_000)];
        let query = HeadlessQuery {
            services_state: ServiceStateFilter::Running,
            ..HeadlessQuery::default()
        };
        let (rows, meta) = query_services(&svcs, Some(&procs), &query, &QueryFilter::None);
        assert_eq!(meta.total_services, 2);
        assert_eq!(meta.total_matched, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].service.name, "Spooler");
        let linked = rows[0].process.as_ref().expect("linked process");
        assert_eq!(linked.pid, 500);
        assert_eq!(linked.name, "spoolsv.exe");
    }

    #[test]
    fn invalid_regex_filter_returns_error() {
        assert!(QueryFilter::compile(Some("![unclosed")).is_err());
    }
}
