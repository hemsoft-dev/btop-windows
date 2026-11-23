//! Live Windows 11 x64 telemetry invariant checks and steady-state self-overhead
//! (private working set RSS `< 30 MiB` and single-core CPU overhead reporting at `2000 ms`).

#![cfg(windows)]

use std::hint::black_box;
use std::time::{Duration, Instant};

use btop_collect::{LiveCollector, Sampler, SnapshotSource};
use btop_config::Config;
use btop_model::DataState;
use btop_term::{DiffEncoder, Grid, Size};
use btop_ui::{UiHistory, UiState, render_frame};

const RSS_BUDGET_BYTES: u64 = 30 * 1024 * 1024;
const STEADY_STATE_TICKS: u64 = 3;
const UPDATE_MS: u32 = 2000;

#[test]
#[allow(clippy::too_many_lines)]
fn windows_live_telemetry_invariants() {
    let mut collector = LiveCollector::new().expect("initialize LiveCollector");
    collector.set_services_active(true);

    // Warm-up tick followed by a live delta tick so rate-based collectors populate cleanly.
    let _first = collector.next_snapshot().expect("first live snapshot");
    std::thread::sleep(Duration::from_millis(250));
    let snap = collector.next_snapshot().expect("second live snapshot");

    // 1. CPU invariants.
    let cpu = match &snap.cpu {
        DataState::Available(c) | DataState::Stale { value: c, .. } => c,
        other => panic!("expected live CPU snapshot, got {other:?}"),
    };
    assert!(!cpu.cores.is_empty(), "CPU must report >= 1 logical core");
    assert!(
        (0.0..=100.0).contains(&cpu.total_busy_pct),
        "total CPU busy {} out of 0..=100",
        cpu.total_busy_pct
    );
    assert!(
        cpu.cores
            .iter()
            .all(|c| (0.0..=100.0).contains(&c.busy_pct)),
        "all per-core busy percentages must be in 0..=100"
    );
    assert!(cpu.uptime_secs > 0, "system uptime must be positive");

    // 2. Memory invariants.
    let mem = match &snap.mem {
        DataState::Available(m) | DataState::Stale { value: m, .. } => m,
        other => panic!("expected live Memory snapshot, got {other:?}"),
    };
    assert!(mem.total_bytes > 0, "total physical RAM must be positive");
    assert_eq!(
        mem.used_bytes.saturating_add(mem.available_bytes),
        mem.total_bytes,
        "used + available must equal total physical RAM"
    );
    assert!(
        mem.free_bytes <= mem.available_bytes,
        "free RAM must not exceed available RAM"
    );
    assert!(
        mem.commit_limit_bytes >= mem.total_bytes,
        "commit limit should be at least physical RAM"
    );

    // 3. Disk invariants.
    let disks = match &snap.disks {
        DataState::Available(d) | DataState::Stale { value: d, .. } => d,
        other => panic!("expected live Disk snapshot, got {other:?}"),
    };
    assert!(
        !disks.is_empty(),
        "at least one fixed drive (e.g. C:\\) expected"
    );
    for disk in disks {
        assert!(!disk.mount.is_empty(), "disk mount label must be non-empty");
        assert!(
            disk.used_bytes <= disk.total_bytes,
            "disk {} used ({}) exceeded total ({})",
            disk.mount,
            disk.used_bytes,
            disk.total_bytes
        );
    }

    // 4. Network invariants.
    let ifaces = match &snap.net {
        DataState::Available(n) | DataState::Stale { value: n, .. } => n,
        other => panic!("expected live Network snapshot, got {other:?}"),
    };
    for iface in ifaces {
        assert!(!iface.name.is_empty(), "interface name must be non-empty");
        assert_ne!(iface.luid, 0, "interface NET_LUID must be non-zero");
    }

    // 5. Process invariants.
    let procs = match &snap.processes {
        DataState::Available(p) | DataState::Stale { value: p, .. } => p,
        other => panic!("expected live Processes snapshot, got {other:?}"),
    };
    assert!(!procs.is_empty(), "process list must be non-empty");
    let my_pid = std::process::id();
    let self_proc = procs
        .iter()
        .find(|p| p.identity.pid == my_pid)
        .expect("current test process must appear in SystemProcessInformation");
    assert!(
        self_proc.identity.create_time_100ns > 0,
        "self process create_time_100ns must be non-zero"
    );
    assert!(
        self_proc.working_set_private_bytes > 0,
        "self process private working set must be positive"
    );

    // 6. Services invariants.
    let services = match &snap.services {
        DataState::Available(s) | DataState::Stale { value: s, .. } => s,
        other => panic!("expected live Services snapshot, got {other:?}"),
    };
    assert!(!services.is_empty(), "Windows SCM must report services");
    assert!(
        services
            .iter()
            .all(|svc| !svc.name.is_empty() && !svc.state.is_empty()),
        "every service must report non-empty name and state"
    );

    // 7. Render the live snapshot cleanly at standard and wide viewport sizes.
    let cfg = Config::default();
    let history = UiHistory::from_snapshot_with_config(&snap, &cfg);
    let state = UiState::from_config(&cfg);
    for (w, h) in [(120u16, 40u16), (200u16, 60u16)] {
        let grid = render_frame(
            &snap,
            &history,
            &state,
            &cfg,
            Size {
                width: w,
                height: h,
            },
        );
        let plain = grid.to_plain_text();
        assert!(plain.contains("cpu"), "expected cpu panel in {w}x{h} frame");
        assert!(
            plain.contains("proc"),
            "expected proc panel in {w}x{h} frame"
        );
    }
}

#[test]
#[ignore = "run with `cargo test --release -p btop-windows --test live_budgets -- --ignored --nocapture`"]
fn windows_live_steady_state_rss_and_cpu_budget() {
    let collector = LiveCollector::new().expect("initialize LiveCollector");
    let sampler = Sampler::spawn(Box::new(collector), UPDATE_MS, None).expect("spawn Sampler");

    let cfg = Config {
        update_ms: UPDATE_MS,
        ..Config::default()
    };
    let size = Size {
        width: 120,
        height: 40,
    };
    let diff = DiffEncoder { sync_output: true };
    let mut history = UiHistory::default();
    let mut state = UiState::from_config(&cfg);

    // Warm up through the initial live snapshot before measuring steady-state overhead.
    let mut latest_snap = sampler.latest();
    history.push(&latest_snap);
    state.net.observe_snapshot(&latest_snap);
    let warmup_grid = render_frame(&latest_snap, &history, &state, &cfg, size);
    black_box(diff.encode(None, &warmup_grid));
    let mut prev_grid: Option<Grid> = Some(warmup_grid);

    let start_gen = sampler.telemetry_generation();
    let cpu_start_100ns = btop_sys::query_self_cpu_time_100ns().expect("query initial CPU time");
    let wall_start = Instant::now();
    let mut last_gen = start_gen;
    let timeout = Duration::from_secs(u64::from(UPDATE_MS) * (STEADY_STATE_TICKS + 3) / 1000);

    while last_gen.saturating_sub(start_gen) < STEADY_STATE_TICKS && wall_start.elapsed() < timeout
    {
        let gen_now = sampler.telemetry_generation();
        if gen_now == last_gen {
            std::thread::sleep(Duration::from_millis(50));
        } else {
            last_gen = gen_now;
            latest_snap = sampler.latest();
            history.push(&latest_snap);
            state.net.observe_snapshot(&latest_snap);
            let next_grid = render_frame(&latest_snap, &history, &state, &cfg, size);
            let vt_frame = diff.encode(prev_grid.as_ref(), &next_grid);
            black_box(&vt_frame);
            prev_grid = Some(next_grid);
        }
    }

    let wall_secs = wall_start.elapsed().as_secs_f64().max(0.001);
    let cpu_end_100ns = btop_sys::query_self_cpu_time_100ns().expect("query final CPU time");
    let rss_bytes = btop_sys::query_self_private_working_set_bytes()
        .expect("query steady-state private working set");
    let observed_ticks = last_gen.saturating_sub(start_gen);
    let _ = sampler.stop();

    assert!(
        observed_ticks >= STEADY_STATE_TICKS,
        "expected >= {STEADY_STATE_TICKS} live 2000ms ticks, observed {observed_ticks}"
    );
    assert!(
        rss_bytes > 0,
        "steady-state private working set must be positive"
    );

    #[allow(clippy::cast_precision_loss)]
    let rss_mib = rss_bytes as f64 / (1024.0 * 1024.0);
    #[allow(clippy::cast_precision_loss)]
    let cpu_secs = cpu_end_100ns.saturating_sub(cpu_start_100ns) as f64 / 10_000_000.0;
    let single_core_cpu_pct = (cpu_secs / wall_secs) * 100.0;

    println!(
        "[self-overhead] steady-state ({UPDATE_MS}ms cadence, {observed_ticks} ticks, {wall_secs:.2}s): private_working_set={rss_mib:.2} MiB (budget < 30.00 MiB), single_core_cpu={single_core_cpu_pct:.2}% (target < 1.00%)"
    );

    assert!(
        rss_bytes < RSS_BUDGET_BYTES,
        "steady-state private working set RSS ({rss_mib:.2} MiB) exceeded 30.00 MiB budget"
    );
}
