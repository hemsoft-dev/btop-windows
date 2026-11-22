# System architecture: btop-windows

This document defines the crate architecture, threading model, rendering pipeline, Windows telemetry implementation details, and verification strategy for `btop-windows`.

## 1. Crate layout and unsafe boundaries

The repository uses a 6-crate Cargo workspace plus a thin binary crate:

```text
crates/
  btop-sys/       # Hand-written NT/WDK FFI structs, compile-time layout assertions (unsafe FFI)
  btop-model/     # Snapshot, DataState, Metric types, serde (#![forbid(unsafe_code)])
  btop-config/    # btop.conf & .theme parsers, validation, defaults (#![forbid(unsafe_code)])
  btop-collect/   # Collector traits, pure delta math, Windows collectors
  btop-term/      # Grid, HitMap, VT diff encoder, InputSource trait, Win32 console backend
  btop-ui/        # Pure render(&Snapshot, &Config, &Theme, Size) -> Grid (#![forbid(unsafe_code)])
apps/
  btop-windows/   # CLI (clap), thread orchestration, startup hardening, panic-safe restore
```

### Boundary rules

- `btop-model`, `btop-config`, and `btop-ui` enforce `#![forbid(unsafe_code)]` and have **zero Windows crate dependencies**. They build and pass unit and golden tests natively on Linux, macOS, and Windows.
- `btop-sys` isolates all hand-written NT/WDK structures (`SYSTEM_PROCESS_INFORMATION`, `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION`, D3DKMT structs) ported from `phnt`/WDK headers, each guarded by compile-time `const _: () = assert!(size_of::<T>() == ... && offset_of!(T, field) == ...);` checks for both `x86_64` and `aarch64`.
- `windows-sys` is used for plain C Win32 FFI to keep compile times fast; `windows` is added only where COM interfaces (DXGI) are required, with minimal feature flags.

## 2. Concurrency and data freshness model

A system monitor must remain responsive at 60 FPS input latency even if a GPU driver call, network share, or domain SID lookup stalls.

### Thread topology

1. **Main UI & Input Thread:** Owns the terminal handle, reads events from `InputSource`, updates UI selection/menu/filter state, reads the latest `Arc<Snapshot>` via `arc_swap::ArcSwap`, renders `Grid`, and emits a single `WriteConsoleW` per frame wrapped in DEC 2026 synchronized output (`CSI ?2026h ... CSI ?2026l`). Never blocks on any OS telemetry call.
2. **Core Sampler Thread:** Wakes on a Windows high-resolution waitable timer (`CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`) at `update_ms` (default 2000 ms, minimum 100 ms). Collects fast local telemetry (CPU, memory, local disk I/O, selected NIC, process table) and swaps an immutable `Snapshot` into `ArcSwap`.
3. **Isolated Slow/Blocking Workers (bounded timeouts, independent cadences):**
   - **Vendor GPU worker:** Runs NVML / ADLX / IGCL queries with a strict per-call timeout and hybrid-laptop dGPU wake guard.
   - **SID resolution worker:** Resolves process owner SIDs (`LookupAccountSidW`) asynchronously and caches `SID -> AccountName` so offline domain-joined laptops never stall process refresh.
   - **Disk free-space worker:** Queries volume capacity asynchronously (skipping `DRIVE_REMOTE` by default) so hung mounts cannot stall the core sampler.
   - **Services worker:** Polls SCM (`EnumServicesStatusExW`) every 2-5 seconds only when the Services view or `svchost` detail view is active.
   - **Tier-2 sensor worker:** Polls opt-in LibreHardwareMonitor HTTP (`/data.json`) or HWiNFO shared memory (`Global\HWiNFO_SENS_SM2`) on its own timer.

### Explicit data states

Every optional or asynchronous subsystem publishes its state explicitly in `btop-model`:

```rust
pub enum DataState<T> {
    WarmingUp,              // Waiting for second sample to compute delta/rate
    Available(T),           // Fresh data within expected cadence
    Stale { value: T, age_ms: u64 }, // Worker timed out or provider paused
    Denied,                 // Requires higher privilege (e.g., PPL or non-elevated SCM action)
    Unavailable(String),    // Hardware/sensor absent or unsupported on this host
}
```

The UI renders `WarmingUp`, `Stale`, `Denied`, and `Unavailable` distinctly so missing metrics look intentional rather than showing misleading zeros.

## 3. Terminal rendering engine (`btop-term` + `btop-ui`)

We use a custom retained cell `Grid` and incremental VT diff encoder rather than Ratatui (see [ADR-0001](adr/0001-tech-stack-and-renderer.md)):

- **`Grid`:** Flat row-major `Vec<Cell>` where each `Cell` holds `{ ch: char, fg: Color, bg: Color, attrs: CellAttrs }` plus wide-character continuation flags (`unicode-width` ensures CJK/emoji process names never corrupt column alignment).
- **`HitMap`:** Built during the exact same pass that paints boxes, hotkeys (`┐¹cpu┌`), process rows, and menu buttons. Maps `Rect -> HitAction` in cell coordinates so mouse clicks cannot drift from rendered visuals and host DPI/font scaling works automatically.
- **`DiffEncoder`:** Compares `prev: &Grid` and `next: &Grid`, emits minimal cursor moves and SGR transitions, quantizes 24-bit RGB to 256-color or 16-color TTY when configured, wraps the frame in `CSI ?2026h ... CSI ?2026l`, and flushes in one write. Verified by a property test that feeds incremental diff output into a `vt100` terminal emulator and asserts exact cell equality against a full repaint.
- **`InputSource` trait:** Initially backed by `crossterm::event` behind a trait boundary, with explicit Windows adjustments:
  - ConHost mouse coordinates are buffer-relative and must subtract `srWindow.Top`.
  - Window size is read from `GetConsoleScreenBufferInfo().srWindow` on resize events and tick backstops rather than trusting `WINDOW_BUFFER_SIZE_EVENT` buffer dimensions.
  - `ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN` on output; `ENABLE_PROCESSED_INPUT` and `ENABLE_QUICK_EDIT_MODE` off, `ENABLE_EXTENDED_FLAGS | ENABLE_MOUSE_INPUT | ENABLE_WINDOW_INPUT` on for input.
- **Guaranteed console restoration:** `TerminalGuard` restores saved input/output console modes, disables mouse capture, unhides the cursor, and exits the alternate screen (`?1049l`) across normal exit, `SetConsoleCtrlHandler` (`CTRL_CLOSE_EVENT`, `CTRL_LOGOFF_EVENT`, `CTRL_SHUTDOWN_EVENT`), and a registered panic hook (`panic = "unwind"`).

## 4. Windows telemetry architecture and API contracts

### 4.1 Startup hardening

Before loading any dynamic library or querying storage:

1. Call `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` to prevent DLL hijacking when probing optional vendor libraries.
2. Call `SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX)` so empty removable drives or card readers never pop modal Windows error dialogs.
3. Call `SetConsoleOutputCP(CP_UTF8)` and enable VT mode; if VT mode fails (legacy console), print an actionable message and exit non-zero.

### 4.2 Collector API matrix

| Domain | Primary Windows APIs | Key pitfalls mitigated |
| --- | --- | --- |
| **CPU topology & usage** | `GetLogicalProcessorInformationEx` (`RelationGroup`, `RelationProcessorCore`), `NtQuerySystemInformationEx` per group, PDH English counters (`PdhAddEnglishCounterW`: `\Processor Information(*)\% Processor Performance`, `% Processor Utility`), Registry `ProcessorNameString` (works on ARM64 without CPUID) | `KernelTime` includes `IdleTime`; `GetSystemTimes` misses >64-LP processor groups; `CurrentMhz` misses turbo; `EfficiencyClass` higher = P-core; localized PDH names break non-English Windows |
| **Memory & Commit** | `GlobalMemoryStatusEx`, `GetPerformanceInfo`, `EnumPageFilesW` (pages x page size), non-elevated PDH `\Memory\Standby Cache *`, `Modified Page List Bytes`, `Free & Zero Page List Bytes`, `GetPhysicallyInstalledSystemMemory` | `SystemMemoryListInformation` needs `SeProfileSingleProcessPrivilege` (admin); Standby is part of Available so never double-count; `ullTotalPhys` omits hardware-reserved RAM |
| **Disks & I/O** | `FindFirstVolumeW`, `GetVolumePathNamesForVolumeNameW`, `GetDiskFreeSpaceExW`, `IOCTL_STORAGE_QUERY_PROPERTY` (`BusType`), `IOCTL_DISK_PERFORMANCE` (access `0`, non-elevated), `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` | Separate volume capacity from physical disk I/O; handle spanned/Storage Spaces extents; skip `DRIVE_REMOTE` by default; handle BitLocker-locked volumes without crashing |
| **Network** | `GetIfTable2` (topology scan), `GetIfEntry2` (per-tick selected NIC), `GetAdaptersAddresses` (IPv4/IPv6), keyed by `NET_LUID` | Filter `HardwareInterface && !FilterInterface && ConnectorPresent`; auto-select by active traffic (Hyper-V `vEthernet`); clamp negative deltas on adapter reset |
| **Processes** | `NtQuerySystemInformation(SystemProcessInformation)` with `WorkingSetPrivateSize`, `SystemProcessIdInformation` (class 88, works on PPL), `NtQueryInformationProcess(ProcessCommandLineInformation)`, cached `LookupAccountSidW` on worker | Reallocate with 25% headroom on `STATUS_INFO_LENGTH_MISMATCH`; key cache by `(PID, CreateTime)`; verify `parent.CreateTime < child.CreateTime` for tree; handle PPL `ACCESS_DENIED` cleanly |
| **Process actions** | `OpenProcess` (minimal rights) + `GetProcessTimes` verification on the same handle, `IsProcessCritical`, `EnumWindows` + `WM_CLOSE`, `TerminateProcess`, `NtSuspendProcess`/`NtResumeProcess`, `SetPriorityClass`, `SetProcessInformation(ProcessPowerThrottling)` | Eliminates TOCTOU PID-reuse kills; prevents elevated BSOD on critical processes; Efficiency Mode sets both `EXECUTION_SPEED` throttling and `IDLE_PRIORITY_CLASS` |
| **Windows Services** | `OpenSCManagerW(SC_MANAGER_ENUMERATE_SERVICE)`, `EnumServicesStatusExW`, `QueryServiceConfigW`, `QueryServiceConfig2W`, `StartServiceW`, `ControlService`, `ChangeServiceConfigW`, `ChangeServiceConfig2W` (delayed auto-start) | Non-elevated read-only works out of the box; poll on 2-5 s cadence; handle `*_PENDING` transitions; group per-user `_xxxxx` service instances |
| **Multi-GPU (up to 6)** | DXGI (`IDXGIFactory6`, filter `DXGI_ADAPTER_FLAG_SOFTWARE`) + WDDM baseline (D3DKMT vs PDH decided in M0 Spike ADR-002), keyed by PCI BDF; optional trusted-path dynamic load of `nvml.dll`, `amdadlx64.dll` (ADLX C headers), `ControlLib.dll` (IGCL) | `IDXGIAdapter3::QueryVideoMemoryInfo` is per-process only (do not use for system VRAM); hybrid-laptop dGPU wake guard prevents battery drain; release vendor interfaces before DLL unload; ARM64 GPUs use baseline only |
| **Battery & Sensors** | `GetSystemPowerStatus`, SetupAPI `IOCTL_BATTERY_QUERY_TAG` / `QUERY_INFORMATION` / `QUERY_STATUS`, EMI (`GUID_DEVICE_ENERGY_METER`), opt-in LHM HTTP (`/data.json`) and HWiNFO shared memory (`Global\HWiNFO_SENS_SM2`) | Check `BATTERY_CAPACITY_RELATIVE` and `BATTERY_UNKNOWN_RATE`; label EMI by rail; detect stale HWiNFO shared memory after 12 h free-tier cutoff; zero WinRing0 |

## 5. Testing and CI strategy

Seven verification layers enforce correctness from M1 onward:

1. **Pure math unit & property tests:** CPU busy %, processor-group aggregation, counter wrap/reset clamping, unit formatting (base-10 and binary), EWMA load average, PID-reuse tree validation.
2. **Parser fuzzing & roundtrip tests:** `btop.conf` (preserving comments and unknown keys, Windows backslash escaping), `.theme` (all 48 keys + hex/greyscale/256-color quantization), and NT variable-length record parsers.
3. **UI golden snapshots (`insta`):** Pure `render(&Snapshot, &Config, &Theme, Size) -> Grid` snapshots (character + style layers) across `80x24`, `120x40`, `200x60`, and degenerate sizes, fed by synthetic fixtures (2-group 128-LP CPU, 6 mixed-vendor GPUs, 300 processes with PID reuse and CJK names, missing battery/temp, 0 B and 100 TB disks).
4. **VT diff encoder property test:** Apply incremental diff sequences to a `vt100` emulator and assert exact screen equivalence to a full redraw.
5. **Input decoder table tests:** Recorded Win32 `INPUT_RECORD` and VT sequences, including ConHost `srWindow.Top` mouse offset correction.
6. **Live Windows CI smoke tests (elevated AND non-elevated):** Run on `windows-latest` (x64) and `windows-11-arm` (ARM64) checking system invariants (`used <= total`, `0 <= pct <= 100`, self PID present with matching create time, >= 1 interface). Because GitHub Windows runners run elevated, CI also runs a restricted-token pass (`CreateRestrictedToken` dropping Administrators) to catch standard-user privilege regressions.
7. **Release hardware acceptance matrix:** Windows Terminal, ConHost (Win10 + Win11), VS Code terminal, WezTerm, Win32-OpenSSH, >64-LP host, hybrid-GPU laptop on battery, and clean-machine x64/ARM64 import check (`dumpbin /dependents` verifying zero `vcruntime140.dll` / `msvcp140.dll` dependencies).
