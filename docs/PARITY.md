# Upstream parity matrix (aristocratos/btop v1.4.8)

This document is the source of truth mapping every upstream `aristocratos/btop` v1.4.8 capability, CLI flag, config key, keybinding, and theme key to its target status in `btop-windows`.

Legend:
- **Parity:** Implements the upstream behavior directly.
- **Mapped:** Adapts the concept to native Windows semantics (documented inline).
- **Windows-only:** Native extension not present in upstream Unix `btop`.
- **N/A:** Unix-specific concept with no Windows equivalent; accepted silently in `btop.conf` so shared configs never error.

## 1. CLI flags

| Flag | Status | Target Milestone | Windows notes |
| --- | --- | --- | --- |
| `-c, --config <file>` | Parity | M1 | Overrides default `%APPDATA%\btop-windows\btop.conf` and portable exe-adjacent `btop.conf`. |
| `-d, --debug` | Parity | M1 | Enables debug log level and frame/collector timing overlay. |
| `-f, --filter <filter>` | Parity | M3 | Sets initial process/service filter (prefix `!` for regex). |
| `--force-utf` | Parity | M1 | Overrides automatic UTF-8/terminal capability detection. |
| `-l, --low-color` | Parity | M5 | Quantizes 24-bit theme colors to 256-color palette. |
| `-p, --preset <0-9>` | Parity | M2 | Starts with the specified layout preset. |
| `-t, --tty` | Parity | M5 | Forces 16-color TTY theme and ASCII/TTY graph symbols. |
| `--no-tty` | Parity | M5 | Disables automatic TTY fallback. |
| `--themes-dir <dir>` | Parity | M5 | Overrides default theme search directories. |
| `-u, --update <ms>` | Parity | M1 | Sets initial refresh interval (clamped to `>= 100 ms`). |
| `--default-config` | Parity | M1 | Prints default `btop.conf` with ANSI highlighting when attached to a TTY. |
| `-h, --help` | Parity | M1 | Prints CLI usage and exits 0. |
| `-v, -V, --version` | Parity | M1 | Prints version (`--version` adds compiler/target build metadata). |
| `--record <file.jsonl>` | Windows-only | M1 | Records timestamped `Snapshot` JSONL stream for debugging and tests. |
| `--replay <file.jsonl>` | Windows-only | M1 | Replays a recorded or synthetic `Snapshot` JSONL stream through the full TUI. |

## 2. Panels and boxes

| Box / Feature | Status | Target Milestone | Windows notes |
| --- | --- | --- | --- |
| `cpu` box | Parity + Mapped | M1, M2 | Group-aware >64-LP support; P/E-core labels; `RunQ` EWMA load average; Task Manager Utility vs Time mode. |
| `mem` box + swap | Parity + Mapped | M2 | Physical Used/Available/Cached(Standby+Modified)/Free + Commit Charge and Pagefile. |
| `disks` + IO mode | Parity + Mapped | M2 | Volume capacity separated from physical disk I/O (`IOCTL_DISK_PERFORMANCE`); BitLocker locked indicator. |
| `net` box | Parity | M2 | 64-bit `GetIfTable2` counters keyed by `NET_LUID`; hardware/Hyper-V interface filtering. |
| `proc` box | Parity + Mapped | M1, M3 | `WorkingSetPrivateSize`, `(PID, CreateTime)` tree safety, PPL handling, Windows actions. |
| `services` view | Windows-only | M3 | Toggleable SCM services view inside the `proc` panel + hosted services in `svchost` detail view. |
| `gpu0..gpu5` boxes | Parity + Mapped | M4 | DXGI + WDDM baseline + NVML/ADLX/IGCL enrichment + hybrid-laptop dGPU wake guard. |
| Battery indicator | Parity | M2 | Multi-battery SetupAPI wattage and ETA; hides cleanly on desktops. |
| Container detection | Mapped | M2 | Detects Windows Server containers / WSL2 host context where applicable. |

## 3. Config keys (`btop.conf`)

| Config key | Status | Target Milestone | Windows behavior |
| --- | --- | --- | --- |
| `color_theme`, `theme_background`, `truecolor`, `force_tty`, `rounded_corners`, `terminal_sync` | Parity | M1, M5 | Full upstream `.theme` and color-depth support; DEC 2026 on by default. |
| `shown_boxes`, `presets`, `disable_presets` | Parity | M2 | Full preset syntax `box:pos:symbol`. |
| `graph_symbol`, `graph_symbol_{cpu,gpu,mem,net,proc}` | Parity | M1, M2 | Defaults to `braille` in Windows Terminal, `block` in ConHost. |
| `update_ms`, `clock_format`, `background_update`, `save_config_on_exit`, `log_level` | Parity | M1, M5 | `clock_format` supports `/host`, `/user`, `/uptime`, `strftime`. |
| `vim_keys`, `disable_mouse` | Parity | M5 | Full hotkey/mouse mapping parity. |
| `cpu_graph_upper`, `cpu_graph_lower`, `cpu_invert_lower`, `cpu_single_graph`, `cpu_bottom`, `show_uptime`, `show_cpu_freq`, `freq_mode`, `custom_cpu_name` | Parity | M1, M2 | Frequency uses nominal x PDH `% Processor Performance`. |
| `check_temp`, `cpu_sensor`, `show_coretemp`, `cpu_core_map`, `temp_scale`, `show_cpu_watts` | Mapped | M4 | Tier 1 (EMI/thermal zones) + opt-in Tier 2 (LHM HTTP / HWiNFO SM2); hides cleanly if unavailable. |
| `cpu_util_mode` | Windows-only | M2 | `"time"` (default) or `"utility"` (Task Manager `% Processor Utility`). |
| `mem_graphs`, `mem_below_net`, `show_swap`, `swap_disk`, `show_disks`, `only_physical`, `disks_filter`, `show_io_stat`, `io_mode`, `io_graph_combined`, `io_graph_speeds`, `base_10_sizes` | Parity + Mapped | M2 | `swap` maps to Commit Charge / Pagefile; `disks_filter` normalizes Windows drive/mount backslashes. |
| `use_fstab`, `zfs_arc_cached`, `zfs_hide_datasets`, `disk_free_priv` | N/A | M1 | Preserved when rewriting `btop.conf`; ignored on Windows. |
| `swap_upload_download`, `net_download`, `net_upload`, `net_auto`, `net_sync`, `net_iface`, `base_10_bitrate` | Parity | M2 | Matches interface friendly name or LUID. |
| `proc_sorting`, `proc_reversed`, `proc_tree`, `proc_colors`, `proc_gradient`, `proc_per_core`, `proc_mem_bytes`, `proc_cpu_graphs`, `proc_left`, `proc_follow_detailed`, `proc_aggregate`, `proc_tree_auto_collapse`, `keep_dead_proc_usage` | Parity | M3 | Full upstream process list behavior. |
| `proc_info_smaps`, `proc_filter_kernel` | Mapped / N/A | M3 | `proc_filter_kernel` filters `Idle`/`System`/`Registry`/`Secure System`; `proc_info_smaps` is N/A (preserved on write). |
| `show_gpu_info`, `shown_gpus`, `gpu_mirror_graph`, `nvml_measure_pcie_speeds`, `custom_gpu_name0..5` | Parity + Mapped | M4 | `shown_gpus` supports `"nvidia amd intel qualcomm"`; `rsmi_measure_pcie_speeds` mapped/preserved for AMD ADLX. |
| `gpu_wake_dgpu_on_battery` | Windows-only | M4 | Default `False`: avoids polling vendor GPU DLLs while on battery when baseline shows dGPU idle. |
| `sensor_provider`, `lhm_http_url` | Windows-only | M4 | Tier-2 external sensor selection (`"none"`, `"lhm_http"`, `"hwinfo_sm2"`). |
| `show_battery`, `selected_battery`, `show_battery_watts` | Parity | M2 | Backed by SetupAPI `IOCTL_BATTERY_QUERY_*`. |

## 4. Keybindings

| Key | Status | Target Milestone | Windows behavior |
| --- | --- | --- | --- |
| `Esc`, `m` | Parity | M5 | Toggle main menu. |
| `F2`, `o` | Parity | M5 | Open Options menu. |
| `F1`, `?`, `h` | Parity | M5 | Open Help menu (`H` when `vim_keys = true`). |
| `q`, `Ctrl+C` | Parity | M1 | Clean exit with full console restoration. |
| `Ctrl+R` | Parity | M1 | Reload `btop.conf` from disk. |
| `Ctrl+Z` | N/A (Mapped) | M1 | Windows has no POSIX `SIGTSTP` job control; intentional no-op documented in Help menu. |
| `p`, `Shift+P` | Parity | M2 | Cycle layout presets forward/backward. |
| `1`, `2`, `3`, `4`, `5`..`0` | Parity | M2, M4 | Toggle CPU, MEM, NET, PROC, and GPU boxes. |
| `+`, `-` | Parity | M1 | Adjust update interval by +/- 100 ms. |
| `d`, `i` | Parity | M2 | Toggle Disks view and Disk I/O graph mode. |
| `b`, `n`, `z`, `a`, `y` | Parity | M2 | Network interface prev/next, reset totals, auto-scale, sync-scale. |
| `Up`, `Down`, `PgUp`, `PgDn`, `Home`, `End`, `Left`, `Right` | Parity | M1, M3 | Navigate process/service list and change sort column. |
| `Enter` | Parity | M3 | Toggle detailed overlay for selected process or service. |
| `f`, `/`, `Delete` | Parity | M3 | Filter processes/services (`!regex`), clear filter. |
| `F`, `u`, `c`, `r`, `e`, `E`, `Space`, `C`, `%` | Parity | M3 | Follow, pause, per-core CPU, reverse sort, tree view, collapse/expand, memory bytes/%. |
| `t`, `k`, `s`, `N` | Mapped | M3 | Graceful close (`WM_CLOSE`), guarded Terminate, Windows Action menu (Kill Tree, Suspend/Resume, Efficiency Mode), Priority Class selector. |
| `S` (or panel tab click) | Windows-only | M3 | Toggle between Processes and Windows Services views. |

## 5. Theme attributes

All 48 upstream `.theme` keys from `src/btop_theme.cpp` are supported directly in `btop-config` and `btop-ui`, including the v1.4 process pause/follow keys (`proc_pause_bg`, `proc_follow_bg`, `proc_banner_bg`, `proc_banner_fg`, `followed_bg`, `followed_fg`). All 49 upstream Apache-2.0 `.theme` files will be bundled in M5 with full attribution in `NOTICE`.
