//! Cycle-free Windows process tree builder (guarded by `parent.create_time_100ns <= child.create_time_100ns`),
//! child CPU/memory rollup (`proc_aggregate`), and live substring/`!regex` filtering.

use btop_model::{ProcessEntry, ProcessIdentity, build_acyclic_adjacency};
use regex::{Regex, RegexBuilder};

use crate::proc_box::{ProcSort, ProcViewState};

/// Compiled process filter parsed from user input (`f`/`/` or `-f/--filter`).
#[derive(Debug)]
pub enum CompiledFilter {
    /// No active filter; every process matches.
    None,
    /// Case-insensitive substring query matching name, command line, user, or PID.
    Substring(String),
    /// Case-insensitive regular expression compiled from a `!pattern` query.
    Regex(Regex),
    /// Non-fatal regex syntax/size error displayed inline without disrupting the TUI.
    InvalidRegex(String),
}

impl CompiledFilter {
    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::InvalidRegex(msg) => Some(msg.as_str()),
            _ => None,
        }
    }

    pub const fn is_active_matcher(&self) -> bool {
        matches!(self, Self::Substring(_) | Self::Regex(_))
    }
}

/// Compiles a raw filter string (`!` prefix for case-insensitive regex, otherwise substring).
pub fn compile_proc_filter(raw: &str) -> CompiledFilter {
    if raw.is_empty() {
        return CompiledFilter::None;
    }
    if let Some(pattern) = raw.strip_prefix('!') {
        if pattern.is_empty() {
            return CompiledFilter::None;
        }
        return match RegexBuilder::new(pattern)
            .case_insensitive(true)
            .size_limit(1 << 20)
            .build()
        {
            Ok(re) => CompiledFilter::Regex(re),
            Err(err) => {
                let first_line = err
                    .to_string()
                    .lines()
                    .last()
                    .unwrap_or("invalid regex")
                    .trim()
                    .to_owned();
                CompiledFilter::InvalidRegex(first_line)
            }
        };
    }
    CompiledFilter::Substring(raw.to_ascii_lowercase())
}

/// Evaluates whether `entry` matches `filter` across process name, command line, user, and PID.
pub fn matches_proc_filter(entry: &ProcessEntry, filter: &CompiledFilter) -> bool {
    match filter {
        CompiledFilter::None | CompiledFilter::InvalidRegex(_) => true,
        CompiledFilter::Substring(needle) => {
            if entry.name.to_ascii_lowercase().contains(needle) {
                return true;
            }
            if entry
                .command_line
                .as_ref()
                .is_some_and(|cmd| cmd.to_ascii_lowercase().contains(needle))
            {
                return true;
            }
            if entry
                .user
                .as_ref()
                .is_some_and(|u| u.to_ascii_lowercase().contains(needle))
            {
                return true;
            }
            entry.identity.pid.to_string().contains(needle)
        }
        CompiledFilter::Regex(re) => {
            if re.is_match(&entry.name) {
                return true;
            }
            if entry
                .command_line
                .as_ref()
                .is_some_and(|cmd| re.is_match(cmd))
            {
                return true;
            }
            if entry.user.as_ref().is_some_and(|u| re.is_match(u)) {
                return true;
            }
            re.is_match(&entry.identity.pid.to_string())
        }
    }
}

/// One visible row emitted by the flat or tree process view builder.
#[derive(Debug, Clone)]
pub struct DisplayProcRow<'a> {
    pub entry: &'a ProcessEntry,
    pub display_cpu_pct: f32,
    pub display_mem_bytes: u64,
    pub tree_prefix: String,
    pub has_children: bool,
    pub is_collapsed: bool,
}

/// Returns all valid parent identities that have at least one attached child in the tree.
pub fn expandable_parents(procs: &[ProcessEntry]) -> Vec<ProcessIdentity> {
    let (_, children) = build_acyclic_adjacency(procs);
    children
        .iter()
        .enumerate()
        .filter_map(|(idx, kids)| (!kids.is_empty()).then_some(procs[idx].identity))
        .collect()
}

/// Builds the ordered list of visible process rows (flat or cycle-free tree), returning any
/// non-fatal inline regex error alongside the rows.
pub fn build_visible_rows<'a>(
    procs: &'a [ProcessEntry],
    view: &ProcViewState,
) -> (Vec<DisplayProcRow<'a>>, Option<String>) {
    let compiled = compile_proc_filter(&view.filter);
    let filter_error = compiled.error_message().map(str::to_owned);

    if !view.tree_mode {
        let mut rows: Vec<DisplayProcRow<'a>> = procs
            .iter()
            .filter(|entry| matches_proc_filter(entry, &compiled))
            .map(|entry| DisplayProcRow {
                entry,
                display_cpu_pct: entry.cpu_pct,
                display_mem_bytes: entry.working_set_private_bytes,
                tree_prefix: String::new(),
                has_children: false,
                is_collapsed: false,
            })
            .collect();
        sort_display_rows(&mut rows, view.sort, view.reversed);
        return (rows, filter_error);
    }

    let (parent_of, children) = build_acyclic_adjacency(procs);
    let totals = compute_subtree_totals(procs, &children, view.aggregate);

    let keep_mask = if compiled.is_active_matcher() {
        compute_filtered_tree_mask(procs, &parent_of, &compiled)
    } else {
        vec![true; procs.len()]
    };

    let mut roots: Vec<usize> = (0..procs.len())
        .filter(|&i| parent_of[i].is_none() && keep_mask[i])
        .collect();
    sort_indices(&mut roots, procs, &totals, view.sort, view.reversed);

    let ctx = TreeWalkCtx {
        procs,
        children: &children,
        totals: &totals,
        keep_mask: &keep_mask,
        view,
        filter_active: compiled.is_active_matcher(),
    };

    let mut out = Vec::with_capacity(procs.len());
    let mut visited = vec![false; procs.len()];
    let mut ancestors_have_next = Vec::new();
    for (pos, &root_idx) in roots.iter().enumerate() {
        let is_last = pos + 1 == roots.len();
        emit_tree_node(
            &ctx,
            root_idx,
            0,
            is_last,
            &mut ancestors_have_next,
            &mut visited,
            &mut out,
        );
    }

    (out, filter_error)
}

fn compute_subtree_totals(
    procs: &[ProcessEntry],
    children: &[Vec<usize>],
    aggregate: bool,
) -> Vec<(f32, u64)> {
    if !aggregate {
        return procs
            .iter()
            .map(|p| (p.cpu_pct, p.working_set_private_bytes))
            .collect();
    }

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

fn compute_filtered_tree_mask(
    procs: &[ProcessEntry],
    parent_of: &[Option<usize>],
    filter: &CompiledFilter,
) -> Vec<bool> {
    let mut keep = vec![false; procs.len()];
    for (idx, entry) in procs.iter().enumerate() {
        if matches_proc_filter(entry, filter) {
            let mut cur = Some(idx);
            let mut steps = 0usize;
            while let Some(node) = cur {
                if keep[node] || steps > procs.len() {
                    break;
                }
                keep[node] = true;
                steps += 1;
                cur = parent_of[node];
            }
        }
    }
    keep
}

struct TreeWalkCtx<'a, 'b> {
    procs: &'a [ProcessEntry],
    children: &'b [Vec<usize>],
    totals: &'b [(f32, u64)],
    keep_mask: &'b [bool],
    view: &'b ProcViewState,
    filter_active: bool,
}

fn emit_tree_node<'a>(
    ctx: &TreeWalkCtx<'a, '_>,
    idx: usize,
    depth: usize,
    is_last: bool,
    ancestors_have_next: &mut Vec<bool>,
    visited: &mut [bool],
    out: &mut Vec<DisplayProcRow<'a>>,
) {
    if visited[idx] {
        return;
    }
    visited[idx] = true;

    let entry = &ctx.procs[idx];
    let mut visible_kids: Vec<usize> = ctx.children[idx]
        .iter()
        .copied()
        .filter(|&c| ctx.keep_mask[c])
        .collect();
    let has_children = !visible_kids.is_empty();
    let is_collapsed =
        has_children && !ctx.filter_active && ctx.view.collapsed.contains(&entry.identity);

    let mut prefix = String::new();
    if depth > 0 {
        for &has_more in ancestors_have_next.iter().skip(1) {
            prefix.push_str(if has_more { "│ " } else { "  " });
        }
        prefix.push_str(if is_last { "└─" } else { "├─" });
    }
    if has_children {
        prefix.push_str(if is_collapsed { "▸ " } else { "▾ " });
    } else if depth > 0 {
        prefix.push(' ');
    }

    let (display_cpu_pct, display_mem_bytes) = ctx.totals[idx];
    out.push(DisplayProcRow {
        entry,
        display_cpu_pct,
        display_mem_bytes,
        tree_prefix: prefix,
        has_children,
        is_collapsed,
    });

    if is_collapsed || visible_kids.is_empty() {
        return;
    }

    sort_indices(
        &mut visible_kids,
        ctx.procs,
        ctx.totals,
        ctx.view.sort,
        ctx.view.reversed,
    );
    ancestors_have_next.push(!is_last);
    for (pos, &child_idx) in visible_kids.iter().enumerate() {
        let child_last = pos + 1 == visible_kids.len();
        emit_tree_node(
            ctx,
            child_idx,
            depth + 1,
            child_last,
            ancestors_have_next,
            visited,
            out,
        );
    }
    ancestors_have_next.pop();
}

fn compare_proc_metrics(
    a: (&ProcessEntry, f32, u64),
    b: (&ProcessEntry, f32, u64),
    sort: ProcSort,
    reversed: bool,
) -> std::cmp::Ordering {
    let (a, a_cpu, a_mem) = a;
    let (b, b_cpu, b_mem) = b;
    let ord = match sort {
        ProcSort::Pid => a.identity.pid.cmp(&b.identity.pid),
        ProcSort::Program => a
            .name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.identity.pid.cmp(&b.identity.pid)),
        ProcSort::Threads => b
            .threads
            .cmp(&a.threads)
            .then_with(|| a.identity.pid.cmp(&b.identity.pid)),
        ProcSort::Memory => b_mem
            .cmp(&a_mem)
            .then_with(|| a.identity.pid.cmp(&b.identity.pid)),
        ProcSort::CpuDirect => b_cpu
            .total_cmp(&a_cpu)
            .then_with(|| b_mem.cmp(&a_mem))
            .then_with(|| a.identity.pid.cmp(&b.identity.pid)),
        ProcSort::CpuLazy => {
            let a_bucket = cpu_lazy_bucket(a_cpu);
            let b_bucket = cpu_lazy_bucket(b_cpu);
            b_bucket
                .cmp(&a_bucket)
                .then_with(|| b_mem.cmp(&a_mem))
                .then_with(|| a.identity.pid.cmp(&b.identity.pid))
        }
    };
    if reversed { ord.reverse() } else { ord }
}

fn sort_display_rows(rows: &mut [DisplayProcRow<'_>], sort: ProcSort, reversed: bool) {
    rows.sort_by(|a, b| {
        compare_proc_metrics(
            (a.entry, a.display_cpu_pct, a.display_mem_bytes),
            (b.entry, b.display_cpu_pct, b.display_mem_bytes),
            sort,
            reversed,
        )
    });
}

fn sort_indices(
    indices: &mut [usize],
    procs: &[ProcessEntry],
    totals: &[(f32, u64)],
    sort: ProcSort,
    reversed: bool,
) {
    indices.sort_by(|&ai, &bi| {
        compare_proc_metrics(
            (&procs[ai], totals[ai].0, totals[ai].1),
            (&procs[bi], totals[bi].0, totals[bi].1),
            sort,
            reversed,
        )
    });
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn cpu_lazy_bucket(cpu_pct: f32) -> u32 {
    (cpu_pct.clamp(0.0, 10000.0) * 10.0).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(
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
            command_line: Some(format!("C:\\bin\\{name} --arg")),
            command_line_denied: false,
            user: Some("WIN11\\franz".to_owned()),
            user_denied: false,
            threads: 4,
            cpu_pct: cpu,
            working_set_private_bytes: mem,
            io_read_bytes: 0,
            io_write_bytes: 0,
            is_protected: false,
        }
    }

    #[test]
    fn tree_guard_prevents_false_attachment_on_pid_reuse_and_cycles() {
        let procs = vec![
            mk(100, 0, 1000, "explorer.exe", 1.0, 100),
            // Reused PID 4820 started AFTER child 4824 -> must NOT adopt 4824.
            mk(4820, 100, 9000, "reused_worker.exe", 2.0, 200),
            mk(4824, 4820, 5000, "orphaned_child.exe", 0.5, 50),
            // Synthetic mutual cycle with identical create_time -> must stay acyclic.
            mk(700, 701, 2000, "cycle_a.exe", 0.2, 10),
            mk(701, 700, 2000, "cycle_b.exe", 0.3, 20),
        ];

        let view = ProcViewState {
            tree_mode: true,
            sort: ProcSort::Pid,
            ..ProcViewState::default()
        };
        let (rows, err) = build_visible_rows(&procs, &view);
        assert!(err.is_none());
        assert_eq!(
            rows.len(),
            5,
            "all nodes must appear without infinite loops"
        );

        let orphan = rows
            .iter()
            .find(|r| r.entry.identity.pid == 4824)
            .expect("orphan present");
        assert!(
            orphan.tree_prefix.is_empty(),
            "orphaned_child must stay a root, got prefix {:?}",
            orphan.tree_prefix
        );
    }

    #[test]
    fn tree_aggregation_and_collapse_expand_work_accurately() {
        let procs = vec![
            mk(10, 0, 1000, "parent.exe", 1.5, 100),
            mk(20, 10, 2000, "child_a.exe", 4.5, 300),
            mk(30, 20, 3000, "grandchild.exe", 6.0, 600),
        ];

        let mut view = ProcViewState {
            tree_mode: true,
            aggregate: true,
            sort: ProcSort::CpuDirect,
            ..ProcViewState::default()
        };
        let (rows, _) = build_visible_rows(&procs, &view);
        assert_eq!(rows.len(), 3);
        assert!((rows[0].display_cpu_pct - 12.0).abs() < 0.01);
        assert_eq!(rows[0].display_mem_bytes, 1000);
        assert!(rows[0].has_children);

        // Collapsing root hides its subtree while preserving rolled-up metrics.
        view.collapsed.insert(procs[0].identity);
        let (collapsed_rows, _) = build_visible_rows(&procs, &view);
        assert_eq!(collapsed_rows.len(), 1);
        assert!(collapsed_rows[0].is_collapsed);
        assert!((collapsed_rows[0].display_cpu_pct - 12.0).abs() < 0.01);
        assert_eq!(collapsed_rows[0].display_mem_bytes, 1000);
    }

    #[test]
    fn filter_supports_substring_regex_and_non_fatal_regex_errors() {
        let procs = vec![
            mk(10, 0, 1000, "explorer.exe", 1.0, 100),
            mk(20, 10, 2000, "Code.exe", 5.0, 500),
            mk(30, 20, 3000, "rust-analyzer.exe", 12.0, 1200),
        ];

        // Substring match by PID or name.
        let sub_view = ProcViewState {
            filter: "rust".to_owned(),
            ..ProcViewState::default()
        };
        let (sub_rows, err) = build_visible_rows(&procs, &sub_view);
        assert!(err.is_none());
        assert_eq!(sub_rows.len(), 1);
        assert_eq!(sub_rows[0].entry.identity.pid, 30);

        // Regex match in tree mode keeps ancestor path (`explorer -> Code -> rust-analyzer`).
        let re_tree_view = ProcViewState {
            tree_mode: true,
            filter: "!^rust-".to_owned(),
            ..ProcViewState::default()
        };
        let (re_rows, err) = build_visible_rows(&procs, &re_tree_view);
        assert!(err.is_none());
        assert_eq!(
            re_rows
                .iter()
                .map(|r| r.entry.identity.pid)
                .collect::<Vec<_>>(),
            vec![10, 20, 30]
        );

        // Invalid regex returns inline error without panicking.
        let bad_view = ProcViewState {
            filter: "![unclosed".to_owned(),
            ..ProcViewState::default()
        };
        let (bad_rows, err) = build_visible_rows(&procs, &bad_view);
        assert!(err.is_some(), "expected non-fatal regex error message");
        assert_eq!(bad_rows.len(), 3);
    }
}
