# System architecture: btop-windows

This document defines the crate layout, threading model, rendering pipeline, English Windows 11 x64 telemetry stack, and verification strategy for `btop-windows`. All scope boundaries follow [AGENTS.md](../AGENTS.md).

## 1. Crate layout and unsafe boundaries

The repository uses a 6-crate Cargo workspace plus a thin binary crate:

```text
crates/
  btop-sys/       # Hand-written NT FFI structs, compile-time layout assertions (unsafe FFI)
  btop-model/     # Snapshot, DataState, metric types, serde (#![forbid(unsafe_code)])
  btop-config/    # btop.conf & .theme parsers, validation, defaults (#![forbid(unsafe_code)])
  btop-collect/   # SnapshotSource trait, pure delta math, Windows 11 collectors
  btop-term/      # Grid, HitMap, VT diff encoder, InputSource trait, Windows Terminal backend
  btop-ui/        # Pure render(&Snapshot, &Config, Size) -> Grid (#![forbid(unsafe_code)])
apps/
  btop-windows/   # CLI (clap), thread wiring, startup hardening, panic-safe restore
```

### Boundary rules

- `btop-model`, `btop-config`, and `btop-ui` enforce `#![forbid(unsafe_code)]` and have zero Windows crate dependencies. They build and pass unit and golden tests on Linux and Windows.
- `btop-sys` isolates hand-written NT structures (`SYSTEM_PROCESS_INFORMATION`, `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION`) guarded by compile-time `size_of` and `offset_of` assertions on `x86_64`.
- `windows-sys` is used for plain Win32 FFI; `windows` is added only where COM interfaces (DXGI) are required, with minimal features.

## 2. Concurrency and data freshness model

Keep threading minimal and predictable:

1. **Main UI & Input Thread:** Owns the Windows Terminal handle, reads events from `InputSource`, updates selection/menu/filter state, reads the latest `Arc<Snapshot>` via `arc_swap::ArcSwap`, renders `Grid`, and emits one frame wrapped in DEC 2026 synchronized output (`CSI ?2026h ... CSI ?2026l`). Never blocks on telemetry.
2. **Core Sampler Thread:** Wakes on a waitable timer at `update_ms` (default 2000 ms, minimum 100 ms), collects local Windows 11 metrics (CPU, memory, local disk I/O, active NIC, processes, battery), and swaps an immutable `Snapshot` into `ArcSwap`.
3. **Optional Background Worker:** Runs optional NVIDIA `nvml.dll` queries (and SCM polling while the Services view is open) off the UI and core sampler threads so vendor calls cannot delay UI input.

### Explicit data states

Every optional or asynchronous subsystem publishes its state explicitly in `btop-model`:

```rust
pub enum DataState<T> {
    WarmingUp,              // Waiting for second sample to compute delta/rate
    Available(T),           // Fresh data within expected cadence
    Stale { value: T, age_ms: u64 }, // Optional worker timed out
    Denied,                 // Requires higher privilege (e.g., PPL or non-elevated SCM action)
    Unavailable(String),    // Hardware/sensor absent or unsupported on this host
}
```

The UI renders `WarmingUp`, `Stale`, `Denied`, and `Unavailable` distinctly so missing metrics look intentional rather than showing misleading zeros.

## 3. Terminal rendering engine (`btop-term` + `btop-ui`)

We use a custom retained cell `Grid` and incremental VT diff encoder rather than Ratatui (see [ADR-0001](adr/0001-tech-stack-and-renderer.md)):

- **`Grid`:** Flat row-major `Vec<Cell>` storing `{ ch, fg, bg, bold, continuation }`.
- **`HitMap`:** Built during the same pass that paints boxes, hotkeys, process rows, and menu buttons. Maps `Rect -> HitAction` in cell coordinates so mouse clicks cannot drift from rendered visuals.
- **`DiffEncoder`:** Compares `prev: &Grid` and `next: &Grid`, emits minimal cursor moves and 24-bit TrueColor SGR transitions, wraps the frame in `CSI ?2026h ... CSI ?2026l`, and flushes in one write.
- **`InputSource` trait:** Backed by `crossterm::event` on Windows Terminal (ConPTY), handling keyboard, mouse click/wheel, and resize events cleanly without legacy ConHost workarounds.
- **Guaranteed console restoration:** `TerminalGuard` restores saved console modes, disables mouse capture, unhides the cursor, and exits the alternate screen (`?1049l`) across normal exit, `SetConsoleCtrlHandler` (`CTRL_CLOSE_EVENT`), and a panic hook (`panic = "unwind"`).

## 4. Windows 11 telemetry matrix (v1 simplified)

### 4.1 Startup hardening

1. Call `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` to prevent DLL hijacking when loading optional `nvml.dll`.
2. Call `SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX)`.
3. Enable `CP_UTF8` and `ENABLE_VIRTUAL_TERMINAL_PROCESSING`; exit cleanly with an actionable message if VT mode is unavailable.

### 4.2 Collector API matrix

| Domain | Primary English Windows 11 x64 APIs | Key notes |
| --- | --- | --- |
| **CPU** | `NtQuerySystemInformation(SystemProcessorPerformanceInformation)`, `GetLogicalProcessorInformationEx(RelationProcessorCore)`, English PDH `\Processor Information(*)\% Processor Performance` & `% Processor Utility`, Registry `ProcessorNameString`, `GetTickCount64` | `KernelTime` includes `IdleTime`; `EfficiencyClass` higher = P-core; single-group (`<= 64` LPs) for v1 |
| **Memory & Commit** | `GlobalMemoryStatusEx`, `GetPerformanceInfo`, `EnumPageFilesW` (pages x page size), English PDH `\Memory\Standby Cache *`, `Modified Page List Bytes`, `Free & Zero Page List Bytes` | Non-elevated baseline; never double-count Standby inside stacked bars |
| **Disks & I/O** | `GetLogicalDriveStringsW` / `GetDriveTypeW` (`DRIVE_FIXED`), `GetDiskFreeSpaceExW`, `IOCTL_DISK_PERFORMANCE` (access `0`, non-elevated) | Local fixed drives only in v1; no SMB/BitLocker/Storage Spaces complexity |
| **Network** | `GetIfTable2`, `GetIfEntry2`, `GetAdaptersAddresses`, keyed by `NET_LUID` | Filter `HardwareInterface && !FilterInterface && ConnectorPresent`; clamp negative deltas on reset |
| **Processes** | `NtQuerySystemInformation(SystemProcessInformation)` (`WorkingSetPrivateSize`), `SystemProcessIdInformation` (class 88), `ProcessCommandLineInformation`, local SID cache | Key by `(PID, CreateTime)`; validate `parent.CreateTime <= child.CreateTime` for tree; handle PPL `ACCESS_DENIED` cleanly |
| **Process actions** | `EnumWindows` + `WM_CLOSE`, handle-verified `TerminateProcess` + `IsProcessCritical`, `SetPriorityClass` | Prevents TOCTOU PID-reuse kills and elevated BSOD on critical processes |
| **Windows Services** | `OpenSCManagerW(SC_MANAGER_ENUMERATE_SERVICE)`, `EnumServicesStatusExW`, `QueryServiceConfigW`, `StartServiceW`, `ControlService` (Stop) | Non-elevated read-only list; Start/Stop enabled when elevated |
| **GPU** | DXGI (`IDXGIFactory6`, exclude `DXGI_ADAPTER_FLAG_SOFTWARE`) + WDDM baseline (decided in M0 Spike `#2`) + optional `nvml.dll` from `System32` | Never use `IDXGIAdapter3::QueryVideoMemoryInfo` for system VRAM; NVML runs off the UI thread |
| **Battery** | `GetSystemPowerStatus` | Charge %, AC/charging state, remaining time; hides when no battery is present |

## 5. Testing and CI strategy

Keep verification fast, deterministic, and high-signal:

1. **Pure math unit tests:** CPU busy %, counter rollback clamping, unit formatting, RunQ EWMA, and PID-reuse tree validation.
2. **Parser tests:** `btop.conf` and `.theme` parsing roundtrips plus bounds-checked NT record parsing.
3. **UI golden snapshots (`insta`):** Pure `render(&Snapshot, &Config, Size) -> Grid` tests at `80x24`, `120x40`, and `200x60` driven by representative `--replay` JSONL fixtures.
4. **VT diff encoder tests:** Verify incremental diff output matches full redraw state and wraps frames in DEC 2026.
5. **CI (`ubuntu-latest` + `windows-latest`):** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, live Windows 11 x64 invariant tests, and M6 render/RSS/CPU budget checks.
