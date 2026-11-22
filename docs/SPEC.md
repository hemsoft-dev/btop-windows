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

## 3. Windows-first requirements and extensions

`btop-windows` maps every applicable upstream feature to Windows semantics and adds native Windows capabilities where Unix primitives do not apply:

### 3.1 Platform and terminal baseline

- **Operating systems:** Windows 11 (23H2+) and Windows 10 (22H2+).
- **Architectures:** Native `x86_64` (`x86_64-pc-windows-msvc`) and `ARM64` (`aarch64-pc-windows-msvc`). No 32-bit x86 (WOW64 degrades visibility into 64-bit processes).
- **Supported terminal hosts:** Windows Terminal (primary target, default Braille symbols), VT-capable ConHost (Windows 10 1903+/Windows 11, default Block symbols when `WT_SESSION` is absent), VS Code integrated terminal, WezTerm, and Win32-OpenSSH sessions. Legacy non-VT ConHost exits cleanly with a clear remediation message.
- **Privileges:** Runs non-elevated by default with no UAC prompt. Unavailable privileged fields display an explicit unavailable/denied marker instead of failing. Includes an in-app "Restart elevated" action (`ShellExecuteExW` with `runas`) and enables `SeDebugPrivilege` when already elevated.

### 3.2 Windows-specific metric and workflow semantics

- **CPU utilization:** Supports >64 logical processors across multiple Windows processor groups keyed by `(group, number)`. Exposes a configurable metric mode: standard time-based utilization `(DeltaKernel + DeltaUser - DeltaIdle) / (DeltaKernel + DeltaUser)` and Windows Task Manager `% Processor Utility` (frequency-scaled, may exceed 100%). Displays hybrid P-core/E-core classes where reported. Emulates 1/5/15-minute load averages via an EWMA of Ready/Running thread counts, clearly labeled as `RunQ`.
- **Memory accounting:** Reports Physical Total, Used (`Total - Available`), Available, Cached (`Standby + Modified`), Free (`Free + Zero`), Commit Charge / Commit Limit, Pagefile usage, and Compressed Memory without double-counting Standby inside stacked displays.
- **Disks and I/O:** Separates volume capacity (drive letters, mount paths, BitLocker locked state, `only_physical` via `BusType`) from physical disk throughput and busy percentage (`IOCTL_DISK_PERFORMANCE` + `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`), skipping remote SMB shares by default to avoid blocking.
- **Network:** 64-bit `GetIfTable2` counters keyed by `NET_LUID`, filtering out WFP/QoS/loopback pseudo-interfaces by default, auto-selecting active traffic interfaces (including Hyper-V `vEthernet`), and handling adapter resets cleanly.
- **Processes:** Fast single-call `SystemProcessInformation` enumeration with `WorkingSetPrivateSize` (matching Task Manager's Memory column), `(PID, CreateTime)` identity keys, parent-child tree validation (`parent.CreateTime < child.CreateTime`) to prevent PID-reuse orphans, and graceful handling of Protected Process Light (PPL) and special kernel processes (`Idle`, `System`, `Registry`, `Memory Compression`, `Secure System`).
- **Windows process actions (replacing POSIX signals & `renice`):**
  - **Graceful Close (`t`):** Enumerates top-level windows for the target PID and posts `WM_CLOSE` (best-effort, subject to UIPI).
  - **Terminate (`k`):** Opens handle with `PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION`, verifies `CreateTime` on that handle, checks `IsProcessCritical` (refusing or requiring explicit name confirmation to prevent elevated BSOD), then calls `TerminateProcess`.
  - **Action menu (`s`):** Graceful Close, Terminate, Kill Process Tree (verified by create-time ancestry), Suspend / Resume (`NtSuspendProcess`/`NtResumeProcess` with critical-process guard), Toggle Efficiency Mode (`ProcessPowerThrottling` + `IDLE_PRIORITY_CLASS`).
  - **Priority Class (`N`):** `Idle`, `Below Normal`, `Normal`, `Above Normal`, `High`, `Realtime` via `SetPriorityClass`.
- **Windows Services view (toggleable inside the Proc panel, Windows-only extension):**
  - Read-only SCM listing works non-elevated: service name, display name, state, start type (including Delayed Auto-Start), PID, and resource link.
  - Service control actions (Start, Stop, Pause, Continue, Start Type change) detect elevation, handle `START_PENDING`/`STOP_PENDING` transitions cleanly, and grey out when non-elevated.
  - Process detail view for `svchost.exe` lists hosted services matched by `dwProcessId`.
- **Multi-GPU (up to 6 GPUs) without WinRing0:**
  - Universal user-mode baseline via DXGI adapter identity (filtering software renderers) + WDDM utilization and VRAM counters, keyed by PCI bus/device/function (BDF) with LUID fallback.
  - Optional dynamic vendor enrichment via trusted-path loading of `nvml.dll` (NVIDIA), `amdadlx64.dll` via C headers (AMD ADLX), and `ControlLib.dll` (Intel IGCL for Arc).
  - **Hybrid-laptop dGPU wake guard:** Polls vendor DLLs only when the baseline shows the adapter active; defaults to battery-safe behavior when a battery is detected so sleeping discrete GPUs stay in D3cold.
- **Temperature and power sensors (zero-driver tiered model):**
  - **Tier 1 (built-in user-mode):** Battery status/wattage/ETA via SetupAPI `IOCTL_BATTERY_QUERY_*`, Windows Energy Meter Interface (EMI) rails where exposed, and non-admin thermal zones when valid. Missing CPU package temperature hides cleanly instead of displaying bogus constants.
  - **Tier 2 (opt-in external providers):** Connects to user-run LibreHardwareMonitor HTTP (`/data.json`) or HWiNFO shared memory (`Global\HWiNFO_SENS_SM2`, surfacing stale status when HWiNFO's 12-hour free timer expires).
- **Record and replay (Windows-first diagnostic & test tool):**
  - `--record <path.jsonl>` writes timestamped `Snapshot` records per tick.
  - `--replay <path.jsonl>` drives the full TUI deterministically from recorded or synthetic snapshots without live Windows calls.

## 4. Non-functional requirements and budgets

Measured on an 8-core Windows 11 x64 reference machine at `2000 ms` update interval and `200x60` terminal viewport:

| Metric | Target budget |
| --- | --- |
| Self CPU overhead (steady state) | `< 1.0%` of one logical core (`< 0.2%` total system) |
| Resident memory (RSS / Working Set Private) | `< 30 MB` |
| Frame layout + Grid render + VT diff encode | `< 2.0 ms` p95 |
| Binary size (release, x64 `+crt-static`) | `<= 6 MB` (target 2-4 MB) |
| Startup to first painted frame | `< 150 ms` |
| Terminal cleanup on exit/Ctrl+C/panic | 100% restoration of console mode, cursor, and primary screen |
| Zero external runtime dependencies | No `vc_redist`, no `.NET`, no bundled kernel drivers |
