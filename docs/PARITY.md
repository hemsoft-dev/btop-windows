# Upstream parity matrix (aristocratos/btop v1.4.8)

This document is the source of truth mapping every upstream `aristocratos/btop` v1.4.8 capability, CLI flag, config key, keybinding, and theme key to its target status in `btop-windows`.

Legend:
- **Parity:** Implements the upstream behavior directly in v1.
- **Mapped:** Adapts the concept to simple English Windows 11 x64 semantics.
- **Windows-only:** Native extension not present in upstream Unix `btop`.
- **Deferred (post-v1):** Intentionally excluded from initial release to minimize scope (see [AGENTS.md](../AGENTS.md)).
- **N/A:** Unix-specific concept with no Windows equivalent; accepted silently in `btop.conf` so shared configs never error.

## 1. CLI flags

| Flag | Status | Target Milestone | Windows 11 v1 notes |
| --- | --- | --- | --- |
| `-c, --config <file>` | Parity | M1 | Overrides default `%APPDATA%\btop\btop.conf` and portable exe-adjacent `btop.conf`. |
| `-d, --debug` | Mapped | M1 | CLI flag accepted; live frame/collector debug overlay deferred post-v1. |
| `-f, --filter <filter>` | Parity | M3 | Sets initial process/service filter (prefix `!` for regex). |
| `--force-utf` | Deferred (post-v1) | Post-v1 | Windows 11 Terminal is UTF-8 by default; flag accepted as no-op. |
| `-l, --low-color` | Deferred (post-v1) | Post-v1 | Windows 11 Terminal supports 24-bit TrueColor natively. |
| `-p, --preset <0-9>` | Parity | M2 | Starts with the specified layout preset. |
| `-t, --tty` | Deferred (post-v1) | Post-v1 | 16-color TTY fallback deferred post-v1. |
| `--no-tty` | Deferred (post-v1) | Post-v1 | Deferred alongside `--tty`. |
| `--themes-dir <dir>` | Parity | M5 | Overrides default theme search directory. |
| `-u, --update <ms>` | Parity | M1 | Sets initial refresh interval (clamped to `>= 100 ms`). |
| `--default-config` | Parity | M1 | Prints default `btop.conf` to stdout. |
| `-h, --help` | Parity | M1 | Prints CLI usage and exits 0. |
| `-v, -V, --version` | Parity | M1 | Prints version. |
| `--record <file.jsonl>` | Windows-only | M1 | Records timestamped `Snapshot` JSONL stream for debugging and tests. |
| `--replay <file.jsonl>` | Windows-only | M1 | Replays a recorded or synthetic `Snapshot` JSONL stream through the full TUI. |

## 2. Panels and boxes

| Box / Feature | Status | Target Milestone | Windows 11 v1 notes |
| --- | --- | --- | --- |
| `cpu` box | Parity + Mapped | M1, M2 | Single-group (`<= 64` LPs); P/E-core labels; PDH frequency; `RunQ` EWMA; optional Task Manager Utility mode. |
| `mem` box + swap | Parity + Mapped | M2 | Physical Used/Available/Cached(Standby+Modified)/Free + Commit Charge and Pagefile. |
| `disks` + IO mode | Parity + Mapped | M2 | Local fixed drives (`DRIVE_FIXED`) + `IOCTL_DISK_PERFORMANCE` throughput and busy %. |
| `net` box | Parity | M2 | 64-bit `GetIfTable2` counters keyed by `NET_LUID`; hardware interface filter. |
| `proc` box | Parity + Mapped | M1, M3 | `WorkingSetPrivateSize`, create-time tree guard, regex filter, pause, follow, detail overlay, `WM_CLOSE`/Terminate/Priority. |
| `services` view | Windows-only | M3 | Toggleable SCM services list inside `proc` panel + Start/Stop when elevated. |
| `gpu0..gpu5` boxes | Parity + Mapped | M4 | DXGI + WDDM baseline + optional NVIDIA `nvml.dll` enrichment. AMD ADLX / Intel IGCL deferred post-v1. |
| Battery indicator | Parity | M2 | Simple `GetSystemPowerStatus` charge %, charging state, and ETA; hides on desktops. |
| Container detection | Deferred (post-v1) | Post-v1 | Not required for initial Windows 11 desktop release. |

## 3. Config keys (`btop.conf`)

| Config key | Status | Target Milestone | Windows 11 v1 behavior |
| --- | --- | --- | --- |
| `color_theme`, `theme_background`, `rounded_corners`, `terminal_sync` | Parity | M1, M5 | 24-bit TrueColor `.theme` support; DEC 2026 on by default. |
| `truecolor`, `force_tty` | Deferred (post-v1) | Post-v1 | Windows 11 Terminal natively supports TrueColor; keys preserved if present. |
| `shown_boxes`, `presets`, `disable_presets` | Parity | M2 | Full preset syntax `box:pos:symbol`. |
| `graph_symbol`, `graph_symbol_{cpu,gpu,mem,net,proc}` | Parity | M1, M2 | `braille` (default) and `block` in v1; `tty` deferred post-v1. |
| `update_ms`, `clock_format`, `background_update`, `save_config_on_exit`, `log_level` | Parity | M1, M5 | `clock_format` supports `/host`, `/user`, `/uptime`, `strftime`. |
| `vim_keys`, `disable_mouse` | Parity | M5 | Full hotkey/mouse mapping parity. |
| `cpu_graph_upper`, `cpu_graph_lower`, `cpu_invert_lower`, `cpu_single_graph`, `cpu_bottom`, `show_uptime`, `show_cpu_freq`, `freq_mode`, `custom_cpu_name` | Parity | M1, M2 | Frequency uses nominal x English PDH `% Processor Performance`. |
| `check_temp`, `cpu_sensor`, `show_coretemp`, `cpu_core_map`, `temp_scale`, `show_cpu_watts` | Deferred (post-v1) | Post-v1 | External CPU sensor bridges deferred post-v1; hidden cleanly when unavailable. |
| `cpu_util_mode` | Windows-only | M2 | `"time"` (default) or `"utility"` (Task Manager `% Processor Utility`). |
| `mem_graphs`, `mem_below_net`, `show_swap`, `show_disks`, `disks_filter`, `show_io_stat`, `io_mode`, `io_graph_combined`, `io_graph_speeds`, `base_10_sizes` | Parity + Mapped | M2 | `swap` maps to Commit Charge / Pagefile; local `DRIVE_FIXED` volumes in v1. |
| `use_fstab`, `zfs_arc_cached`, `zfs_hide_datasets`, `disk_free_priv`, `only_physical`, `swap_disk` | N/A / Deferred | M1 | Preserved when rewriting `btop.conf`; ignored on Windows 11 v1. |
| `swap_upload_download`, `net_download`, `net_upload`, `net_auto`, `net_sync`, `net_iface`, `base_10_bitrate` | Parity | M2 | Matches interface friendly name or LUID. |
| `proc_sorting`, `proc_reversed`, `proc_tree`, `proc_colors`, `proc_gradient`, `proc_per_core`, `proc_mem_bytes`, `proc_cpu_graphs`, `proc_left`, `proc_follow_detailed`, `proc_aggregate` | Parity | M3 | Core upstream process list behavior. |
| `proc_info_smaps`, `proc_filter_kernel`, `proc_tree_auto_collapse`, `keep_dead_proc_usage` | N/A / Deferred | Post-v1 | Preserved when rewriting `btop.conf`; deferred or N/A in v1. |
| `show_gpu_info`, `gpu_mirror_graph`, `nvml_measure_pcie_speeds`, `custom_gpu_name0..5` | Parity + Mapped | M4 | WDDM baseline + optional NVIDIA `nvml.dll`. |
| `show_battery` | Parity | M2 | Simple `GetSystemPowerStatus` charge %, charging state, and ETA (`selected_battery`/`show_battery_watts` deferred post-v1). |

## 4. Keybindings

| Key | Status | Target Milestone | Windows 11 v1 behavior |
| --- | --- | --- | --- |
| `Esc`, `m` | Parity | M5 | Toggle main menu. |
| `F2`, `o` | Parity | M5 | Open Options menu. |
| `F1`, `?`, `h` | Parity | M5 | Open Help menu (`H` when `vim_keys = true`). |
| `q`, `Ctrl+C` | Parity | M1 | Clean exit with full console restoration. |
| `Ctrl+R` | Parity | M1 | Reload `btop.conf` from disk. |
| `Ctrl+L` | Windows extra | v0.1.1 | Force full-grid redraw without resetting filter, sort, or selection (also triggered automatically on terminal focus gain and periodically every 5s for PWA/web terminals). |
| `Ctrl+Z` | N/A | M1 | Windows has no POSIX `SIGTSTP` job control; intentional no-op. |
| `p`, `Shift+P` | Parity | M2 | Cycle layout presets forward/backward. |
| `1`, `2`, `3`, `4`, `5`..`0` | Parity | M2, M4 | Toggle CPU, MEM, NET, PROC, and GPU boxes. |
| `+`, `-` | Parity | M1 | Adjust update interval by +/- 100 ms. |
| `d`, `i` | Parity | M2 | Toggle Disks view and Disk I/O graph mode. |
| `b`, `n`, `z`, `a`, `y` | Parity | M2 | Network interface prev/next, reset totals, auto-scale, sync-scale. |
| `Up`, `Down`, `PgUp`, `PgDn`, `Home`, `End`, `Left`, `Right` | Parity | M1, M3 | Navigate process/service list and change sort column. |
| `Enter` | Parity | M3 | Toggle detailed overlay for selected process. |
| `f`, `/`, `Delete` | Parity | M3 | Filter processes/services (`!regex`), clear filter. |
| `F`, `u`, `c`, `r`, `e`, `E`, `Space`, `%` | Parity | M3 | Follow, pause, per-core CPU, reverse sort, tree view, collapse/expand, memory bytes/%. |
| `t`, `k`, `N` | Mapped | M3 | Graceful close (`WM_CLOSE`), guarded Terminate (critical-process check), Priority Class selector. |
| `S` (or panel tab click) | Windows-only | M3 | Toggle between Processes and Windows Services views. |

## 5. Theme attributes

All 48 upstream `.theme` keys from `src/btop_theme.cpp` are supported in 24-bit TrueColor, including the v1.4 pause/follow banner keys. A curated set of popular upstream Apache-2.0 `.theme` files ships in M5 with attribution in `NOTICE`, and any user-provided `.theme` file loads from disk.

## 6. v1 parity verification sign-off

The implemented v1 behaviors in Sections 1 through 5 are covered by automated tests (`cargo test --workspace` on Linux portable CI and English Windows 11 x64 `x86_64-pc-windows-msvc`) and live Windows Terminal captures:

| Area | Verification evidence |
| --- | --- |
| CLI flags & config (`-c`, `-f`, `-p`, `-u`, `--themes-dir`, `--default-config`, `--record`, `--replay`; `-d` accepted as CLI flag) | `apps/btop-windows/src/main.rs`, `apps/btop-windows/tests/record_replay.rs`, `crates/btop-config/src/lib.rs` (portable exe-adjacent + `%APPDATA%\btop\btop.conf` resolution and roundtrip unknown-key and deferred-key preservation). |
| Panels (`cpu`, `mem` + `disks`/I/O, `net`, `proc` + `services`, `gpu0..gpu5`, battery) | `crates/btop-ui/tests/golden.rs` (53 deterministic golden snapshots across `80x24`, `120x40`, `200x60`), `crates/btop-collect` unit tests, and live Windows Terminal captures across M1-M5. |
| Overlay menus (`Esc`/`m`, `F2`/`o`, `F1`/`?`/`h`/`H`, `MessageBox`) & 48-key `.theme` engine | `crates/btop-ui/src/menu.rs`, `crates/btop-ui/src/menu_render.rs`, `crates/btop-config/src/theme.rs`, 10 bundled Apache-2.0 themes + `NOTICE`, and live theme-preview / transparent-background WT captures. |
| Full mouse `HitMap` clicks, scroll wheel, `disable_mouse`, and `vim_keys` (`h/j/k/l/g/G`, `H`, `K`) | `apps/btop-windows/src/lib.rs` and `apps/btop-windows/src/menu_input.rs` unit tests covering panel hotkeys, sort headers, process/service rows, wheel scroll, and menu actions, plus live Windows Terminal capture. |
| Safety invariants (no kernel drivers, terminal restore on exit/Ctrl+C/panic, `CreateTime` + `IsProcessCritical` kill guard, isolated `unsafe`) | `#![forbid(unsafe_code)]` on `btop-model`, `btop-config`, `btop-ui`; `unsafe` Win32/NT FFI confined to `crates/btop-sys` and `crates/btop-term/src/backend.rs`; `TerminalGuard` RAII restore verified on Windows 11 Terminal. |
