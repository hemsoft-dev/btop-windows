# Functional specification: btop-windows

## 1. Purpose and scope

`btop-windows` is a native Windows terminal resource monitor written in Rust. It delivers feature parity with modern [aristocratos/btop](https://github.com/aristocratos/btop) (v1.4.8) plus Windows-first process and Windows Services controls, without bundling kernel drivers or requiring the Visual C++ Redistributable.

### Why a fresh Windows build instead of extending btop4win

Direct inspection of upstream `aristocratos/btop` (v1.4.8, Oct 2026) and `aristocratos/btop4win` (v1.0.5, Oct 2025) shows why a separate modern implementation is needed:

- **Upstream `btop` is Unix-coupled.** POSIX assumptions (`termios`, `unistd.h`, `sigset_t`, `getifaddrs`, `statvfs`, `pid_t`, `uid_t`, `kill()`) appear directly in `src/btop_shared.hpp`, `src/btop_config.cpp`, `src/btop_input.hpp`, `src/btop_menu.cpp`, and `src/btop_cli.cpp`.
- **`btop4win` stalled at the v1.0 feature set.** It lacks v1.3/v1.4 dedicated multi-GPU boxes (`gpu0..gpu5`), process pause/follow banners, tree auto-collapse, and synchronized terminal output (DEC 2026).
- **Defender false positives.** `btop4win-LHM` depends on LibreHardwareMonitor via a C#/.NET wrapper that loads the WinRing0 kernel driver, which Windows Defender flags on Windows 11.
- **Runtime friction.** `btop4win` requires the MSVC runtime redistributable (`vc_redist.x64.exe`) and does not ship native ARM64 builds.

## 2. Deep dive: upstream btop (v1.4.8) capabilities

Every capability below was verified against the upstream `aristocratos/btop` source tree (`src/btop_shared.hpp`, `src/btop_config.cpp`, `src/btop_cli.cpp`, `src/btop_menu.cpp`, `src/btop_theme.cpp`, and `themes/`):

### 2.1 Monitoring panels and metrics

1. **CPU box (`1`)**
   - Dual-series or single-series historical graph (`cpu_graph_upper`, `cpu_graph_lower`, `cpu_invert_lower`, `cpu_single_graph`).
   - Per-core bar meters and percentage readouts, supporting hybrid core labeling and custom core-to-temperature mapping.
   - CPU model name (with `custom_cpu_name` override), clock frequency (`show_cpu_freq`, `freq_mode`: first/range/lowest/highest/average), package and per-core temperatures (`check_temp`, `show_coretemp`, `temp_scale`: celsius/fahrenheit/kelvin/rankine), CPU package wattage (`show_cpu_watts`), 1/5/15-minute load averages, system uptime (`show_uptime`), and container engine indicator.
   - Optional compact GPU summary graph/stats embedded inside the CPU box (`show_gpu_info`: Auto/On/Off).
   - Battery indicator in the top-right corner (`show_battery`, `selected_battery`, `show_battery_watts`) showing charge %, charging state, time remaining, and power draw in watts.
2. **Memory and Disks box (`2`)**
   - Physical memory breakdown: Used, Available, Cached, Free, shown as historical graphs (`mem_graphs = true`) or percentage meters.
   - Swap breakdown (`show_swap`, or displayed as a disk entry with `swap_disk`).
   - Optional split Disks sub-panel (`show_disks`, key `d`): mountpoint/volume label, total/used/free capacity bars, filesystem type, `only_physical` filter, and `disks_filter` include/exclude rules.
   - Disk I/O mode (key `i`): expands disk entries into dedicated read/write throughput graphs (`io_graph_combined`, `io_graph_speeds`) and busy activity percentage (`show_io_stat`).
3. **Network box (`3`)**
   - Dual download/upload historical graphs (`swap_upload_download`) with auto-scaling (`net_auto`, key `a`), synced download/upload scale (`net_sync`, key `y`), or fixed Mebibit ceilings (`net_download`, `net_upload`).
   - Live bitrate (base-10 or binary via `base_10_bitrate`), peak speed, session total with reset toggle (key `z`), IPv4 and IPv6 addresses, and interface cycling (`b`/`n`, `net_iface`).
4. **Process box (`4`)**
   - Sortable by `pid`, `program`, `arguments`, `threads`, `user`, `memory`, `cpu lazy`, and `cpu direct` (Left/Right keys, `r` to reverse).
   - Flat list and collapsible tree view (`e` toggle, `Space`/`+`/`-` expand/collapse node, `C` toggle children, `E` collapse/expand all, `proc_tree_auto_collapse` threshold, `proc_aggregate` child resource rollup).
   - Per-process mini CPU sparkline (`proc_cpu_graphs`), per-core vs total CPU normalization (`proc_per_core`, key `c`), memory bytes vs percent (`proc_mem_bytes`, key `%`), process coloring and vertical darkening gradient (`proc_colors`, `proc_gradient`).
   - Live filtering (`f` or `/`, prefix `!` for regex, `Delete` to clear).
   - Process pause (`u`, banner highlight, `keep_dead_proc_usage`) and process following (`F`, banner + row highlight, `proc_follow_detailed`).
   - Detailed selected-process overlay (`Enter`): CPU and memory history graphs, elapsed time, parent process, status, I/O read/write totals, command line, user, thread count, and action triggers.
   - Process control: Terminate (`t`), Kill (`k`), Signal selector (`s`), Priority/Renice (`N`).
5. **Dedicated GPU boxes (`5`..`0`, `gpu0..gpu5`)**
   - Up to 6 independent GPU boxes (plus optional compact GPU view inside the CPU box).
   - Tracks GPU utilization history, VRAM used/total and memory utilization history, GPU core and memory clock speeds (MHz), power draw and power limit (W) with power history graph, power state (P-state), temperature and max temperature, PCIe Tx/Rx throughput (KB/s), and video encoder/decoder utilization.
   - Vendor filter (`shown_gpus`) and per-GPU custom names (`custom_gpu_name0..5`), plus `gpu_mirror_graph`.

### 2.2 Terminal UI, rendering, menus, and interaction

- **Graph symbol modes:** `braille` (2x4 sub-cell dots per character, highest resolution), `block` (Unicode block elements), and `tty` (3 ASCII/CP437-safe symbols, half horizontal resolution). Selectable globally (`graph_symbol`) and per panel (`graph_symbol_cpu`, `graph_symbol_gpu`, `graph_symbol_mem`, `graph_symbol_net`, `graph_symbol_proc`).
- **Color modes:** 24-bit TrueColor (`truecolor = true`), automatic 24-bit to 256-color quantization (`-l`/`--low-color`), and 16-color TTY mode (`-t`/`--tty`, `--no-tty`, `force_tty`).
- **Theming:** 48 color keys (including 101-step 3-stop gradients for temp, cpu, free, cached, available, used, download, upload, process, plus pause/follow banner colors) and 49 built-in Apache-2.0 `.theme` files. Transparent background toggle (`theme_background`).
- **Layout presets:** Up to 10 presets (`0`..`9`, cycled with `p` and `Shift+P`, configurable via `presets` and `disable_presets`). Preset `0` shows all boxes; custom presets control visible boxes, alternate positions (`cpu_bottom`, `proc_left`, `mem_below_net`), and per-box graph symbols.
- **Game-inspired overlay menus:**
  - Main menu (`Esc`, `m`): ASCII-art Options, Help, Quit buttons.
  - Options menu (`F2`, `o`): Categorized live settings editor with inline text/integer editing, enum cycling, boolean toggles, theme live preview, and validation warnings.
  - Help menu (`F1`, `?`, `h`): Scrollable keybinding reference.
  - Process action dialogs: Signal/Action chooser, confirmation MessageBox (`OK`, `YES_NO`, `NO_YES`), and Priority editor.
- **Input:** Full mouse support (click any highlighted hotkey button, select rows, click menu items, scroll wheel in lists and menus, `disable_mouse` override), optional `vim_keys` (`h,j,k,l,g,G`), `Ctrl+R` config reload, `+/-` 100 ms update interval adjustment, `q`/`Ctrl+C` clean exit.
- **Top header bar:** Embedded clock (`clock_format` supporting `strftime` plus `/host`, `/user`, `/uptime`).
- **CLI flags:** `-c/--config`, `-d/--debug`, `-f/--filter`, `--force-utf`, `-l/--low-color`, `-p/--preset`, `-t/--tty`, `--no-tty`, `--themes-dir`, `-u/--update`, `--default-config`, `-h/--help`, `-V/--version`.

## 3. Windows 11 v1 requirements and scope boundaries

To keep the initial release simple and fast to ship, v1 targets **English Windows 11 x64** in **Windows Terminal** (see [AGENTS.md](../AGENTS.md)). Edge cases such as Windows 10, non-English locales, ConHost quirks, ARM64, >64-LP multi-group CPUs, 256-color/16-color TTY fallback, AMD ADLX / Intel IGCL bindings, and external sensor bridges are explicitly deferred post-v1.

### 3.1 Platform and terminal baseline

- **Operating system:** English Windows 11 (23H2+), `x86_64` (`x86_64-pc-windows-msvc`).
- **Terminal host:** Windows Terminal (ConPTY, UTF-8, 24-bit TrueColor, Braille and Block graphs, DEC 2026 synchronized output).
- **Privileges:** Runs non-elevated by default with no UAC prompt. Unavailable privileged fields display a clean unavailable/denied marker instead of failing.

### 3.2 Windows-specific metric and workflow semantics

- **CPU utilization:** Single-group Windows 11 CPUs (`<= 64` logical processors). Standard time-based utilization `(DeltaKernel + DeltaUser - DeltaIdle) / (DeltaKernel + DeltaUser)` plus optional Task Manager `% Processor Utility` mode, live frequency via English PDH `\Processor Information(*)\% Processor Performance`, P/E-core labels where reported, and 1/5/15-minute `RunQ` EWMA of Ready/Running threads.
- **Memory accounting:** Physical Total, Used (`Total - Available`), Available, Cached (`Standby + Modified`), Free (`Free + Zero`), Commit Charge / Commit Limit, and Pagefile usage without double-counting Standby in stacked displays.
- **Disks and I/O:** Local fixed volumes (`DRIVE_FIXED`) via `GetDiskFreeSpaceExW` paired with physical disk throughput and busy percentage (`IOCTL_DISK_PERFORMANCE`). Remote SMB shares, BitLocker-locked volumes, and spanned Storage Spaces extents are out of scope for v1.
- **Network:** 64-bit `GetIfTable2` counters keyed by `NET_LUID`, filtering hardware interfaces by default and clamping negative deltas on counter resets.
- **Processes:** Single-call `NtQuerySystemInformation(SystemProcessInformation)` with `WorkingSetPrivateSize` (matching Task Manager's Memory column), `(PID, CreateTime)` identity keys, cycle-free tree validation (`parent.CreateTime <= child.CreateTime`), live substring and `!regex` filtering, pause, follow, and `Enter` detail overlay.
- **Windows process actions (v1 simple set):**
  - **Graceful Close (`t`):** Enumerates top-level windows for the target PID and posts `WM_CLOSE`.
  - **Terminate (`k`):** Opens handle with minimum rights, verifies `CreateTime` on that handle, checks `IsProcessCritical` (refusing critical system processes to prevent elevated BSOD), then calls `TerminateProcess`.
  - **Priority Class (`N`):** `Idle`, `Below Normal`, `Normal`, `Above Normal`, `High`, `Realtime` via `SetPriorityClass`.
- **Windows Services view (toggleable inside the Proc panel):**
  - Read-only SCM listing works non-elevated (service name, display name, state, start type, linked PID).
  - Basic Start and Stop actions work when elevated and appear disabled when non-elevated.
- **Multi-GPU (WDDM baseline + NVIDIA NVML):**
  - Universal user-mode baseline via DXGI hardware adapter enumeration (filtering `DXGI_ADAPTER_FLAG_SOFTWARE`) + persistent English WDDM PDH utilization and VRAM (see [ADR-0002](adr/0002-gpu-baseline-api.md)).
  - Optional dynamic `nvml.dll` loading from `System32` on a background worker for NVIDIA temperature, clocks, power, and encoder/decoder stats. AMD ADLX, Intel IGCL, and hybrid-laptop D3cold wake guards are deferred post-v1.
- **Battery:** Simple `GetSystemPowerStatus` charge %, AC/charging status, and remaining time; hides cleanly on desktops without a battery.
- **Record and replay:** `--record <path.jsonl>` and `--replay <path.jsonl>` for deterministic testing and bug reports.

## 4. Non-functional requirements and budgets

Measured on an 8-core English Windows 11 x64 machine at `2000 ms` update interval and `200x60` Windows Terminal viewport:

| Metric | Target budget |
| --- | --- |
| Self CPU overhead (steady state) | `< 1.0%` of one logical core |
| Resident memory (RSS / Working Set Private) | `< 30 MB` |
| Frame layout + Grid render + VT diff encode | `< 2.0 ms` p95 |
| Binary size (release, x64 `+crt-static`) | `<= 6 MB` (target 2-4 MB) |
| Startup to first painted frame | `< 150 ms` |
| Terminal cleanup on exit/Ctrl+C/panic | 100% restoration of console mode, cursor, and primary screen |
| Zero external runtime dependencies | No `vc_redist`, no `.NET`, no bundled kernel drivers |
