# AGENTS.md

Instructions and engineering principles for contributors and AI agents working on `hemsoft-dev/btop-windows`.

## 1. Release scope and roadmap boundaries

`v0.1.x` and `v0.2.x` established the native English Windows 11 x64 baseline. Following [ADR-0003](docs/adr/0003-cross-platform-local-tui.md) and [ADR-0004](docs/adr/0004-tailscale-fleet-architecture.md), `v0.3+` expands in three bounded milestones (M1 cross-platform local TUI, M2 pull-based Tailscale Fleet Agent and Fleet View, M3 4-target packaging and unprivileged user-login autostart):

- **Supported targets:**
  - Windows 11 (23H2+) x64 (`x86_64-pc-windows-msvc`, English `en-US`, Windows Terminal, binary `btop.exe`, bespoke Win32/NT/DXGI/WDDM/NVML/SCM collectors).
  - Linux x64 and ARM64 static musl (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, covering Ubuntu and Omarchy) and Apple Silicon macOS (`aarch64-apple-darwin`), binary `btop-fleet`, using `sysinfo` for core CPU/RAM/disk/net/process local monitoring.
- **Terminals:** Modern UTF-8, 24-bit TrueColor terminals supporting Unicode Braille/Block glyphs (Windows Terminal on Windows; modern `crossterm`-compatible terminals on Linux and macOS).
- **Domain vocabulary:** Follow [GLOSSARY.md](GLOSSARY.md) (`Fleet`, `Node`, `Agent`, `NodeSample`, `Ring`, `Fleet View`, `Online`/`Stale`/`Offline`, `Cursor`, `Snapshot`).

### Explicitly out of scope (do not build or add workarounds for these)

- Windows 10 or older OS versions, Windows ARM64 (`aarch64-pc-windows-msvc`), and Intel macOS
- Non-English Windows locales and localized PDH counter workarounds
- Legacy `conhost.exe` bugs (`srWindow.Top` mouse offsets, raster fonts, non-VT fallback renderers), 256-color quantization (`--low-color`), and 16-color TTY mode (`--tty`)
- >64 logical processor multi-group Windows topologies (`RelationGroup` / `NtQuerySystemInformationEx` multi-group iteration)
- Remote SMB shares, BitLocker-locked volumes, mounted folder paths, and multi-extent Storage Spaces disk mapping
- Offline domain-joined AD `LookupAccountSidW` async timeout workers
- Multi-battery SetupAPI IOCTLs, USB UPS reporting, and battery wattage
- Bespoke non-Windows GPU (`sysfs`/AMDGPU/`IOReport`), service-manager (`systemd`/`launchd`), or CPU thermal sensor FFI in v1 (render as `DataState::Unavailable` on Linux and macOS)
- AMD ADLX (`amdadlx64.dll`), Intel IGCL (`ControlLib.dll`), hybrid-laptop D3cold dGPU wake guards, and external Tier-2 sensor bridges (LibreHardwareMonitor HTTP, HWiNFO shared memory)
- Process Kill Tree, Suspend/Resume, Efficiency Mode, `svchost` reverse service mapping, and in-app UAC relaunch (`runas`)
- Fleet central hubs, persistent databases or disk history, alerting, web UIs, Prometheus exporters, TLS/token auth layers, Tailscale auto-discovery (`tailscale status --json`), auto-update, privileged system services, and **any remote process kill, service control, or command execution in Fleet View** (permanently out of scope)
- `cargo-fuzz` harnesses, dual-pass `CreateRestrictedToken` CI, Azure Trusted Signing, and WinGet/Scoop publishing

If a task or review suggests adding code for any item above, defer it to a separate post-roadmap issue instead of expanding active milestone scope.

## 2. Principle of simplicity

Every design and implementation choice must optimize for simplicity and directness:

1. **Solve the concrete problem directly.** Write the simplest code that satisfies the issue's acceptance criteria on the supported targets.
2. **Avoid speculative abstractions.** Do not introduce traits, extra worker threads, async runtimes, config knobs, or plugin registries until two real call sites or a measured latency problem demand them.
3. **Keep data flow linear and easy to trace.** Collectors gather data and publish immutable `Snapshot` or `NodeSample` structs; `btop-ui` renders state to `Grid` as a pure function; `btop-term` diffs and writes the frame.
4. **Fail clearly instead of building fallback mazes.** When an optional metric (like CPU temperature, GPU clocks, or disk I/O on a target) is absent, represent it as `None` or `DataState::Unavailable` and hide it cleanly.
5. **Keep files and PRs small.** One collector, widget, or bounded behavior per PR. Delete dead code and unused options immediately.

## 3. Non-negotiable safety rules

Simplicity never overrides these five invariants:

1. **Zero bundled kernel drivers.** Never ship WinRing0 or any kernel driver. Use user-mode OS APIs and optional `nvml.dll` loaded safely from `System32` on Windows.
2. **Always restore the terminal.** Normal exit, `Ctrl+C`, `CTRL_CLOSE_EVENT`, and panics must restore console modes, unhide the cursor, and leave the alternate screen. Keep `panic = "unwind"`.
3. **Never kill the wrong or critical process.** Before terminating a local process, verify process creation/start time on the target handle/PID and reject critical system processes (`IsProcessCritical` on Windows; PID 1 and kernel threads on Unix).
4. **Isolate `unsafe`.** Keep `#![forbid(unsafe_code)]` on `btop-model`, `btop-config`, `btop-ui`, and `btop-fleet`. Put raw Win32/NT FFI structs and compile-time layout assertions in `crates/btop-sys`. Call `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` at startup on Windows.
5. **Fleet mode is permanently read-only and minimal.** Never serialize `Snapshot`, usernames, process lists, or command lines from the Fleet Agent, never bind `0.0.0.0`, and never link process-action or service-control code into the `btop-fleet` library crate (`crates/btop-fleet`) or Fleet View. (Local Unix TUI process signals in `apps/btop-fleet` follow rule 3 and remain isolated to `#[cfg(unix)]` local TUI mode.)

## 4. Quality gates and validation

Before opening or merging a PR:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Every UI pull request must include a `## Validation` section with exact reproduction commands and current-head Windows Terminal screenshots uploaded as GitHub attachments (plus an inline recording when interaction or motion is part of the change).
