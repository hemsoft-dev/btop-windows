# ADR-0003: Cross-platform local TUI and `sysinfo` capability boundary

- **Status:** Accepted
- **Date:** 2026-11-26
- **Council:** Claude Opus 5.5, GPT 6.2 Sol, Claude Fable 5.2 (missing: none; Gemini 4 Neon excluded as active seat)

## Context

`btop-windows` (`v0.2.0`) already keeps `btop-model`, `btop-config`, and `btop-ui` free of Windows dependencies and `unsafe` code, while `btop-term` uses `crossterm` with Windows APIs gated behind `cfg(windows)`. Franz wants to run the full local single-host TUI, the headless fleet Agent, and the multi-node Fleet View across Windows 11, Linux (including Ubuntu and Omarchy on x64 and ARM64), and macOS (Apple Silicon), without drowning in three parallel native OS collector stacks or colliding with upstream `btop++` on Unix hosts.

## Consultation synthesis and disposition

All three Council seats warned that chasing full OS-native parity for GPU, services, and thermal sensors across Windows, Linux, and macOS triples unsafe FFI maintenance. Opus 5.5 and Fable 5.2 recommended limiting non-Windows binaries to the fleet Agent and Fleet View while leaving the full local TUI Windows-only.

Franz chose to include the full local single-host TUI on Linux, Omarchy, and macOS as well, while directly adopting the Council's maintenance guardrail: use `sysinfo` on non-Windows for core metrics and processes, and hide OS-specific GPU, service-manager, and thermal subsystems cleanly behind `DataState::Unavailable` in v1 rather than writing bespoke Linux/macOS FFI.

## Decisions

1. **Day-1 build matrix (4 release targets).** Ship static or minimal-dependency binaries for:
   - `x86_64-pc-windows-msvc` (Windows 11 x64, `+crt-static`)
   - `x86_64-unknown-linux-musl` (static Linux x64 covering Ubuntu and Omarchy)
   - `aarch64-unknown-linux-musl` (static Linux ARM64)
   - `aarch64-apple-darwin` (Apple Silicon macOS)
   Windows ARM64 (`aarch64-pc-windows-msvc`) and Intel macOS remain deferred until needed.
2. **Binary naming and repository.** Stay in `hemsoft-dev/btop-windows` with one binary per target exposing the local TUI (default), `agent`, `fleet`, and `--json` modes. Keep `btop.exe` on Windows and name the Linux and macOS binary `btop-fleet` so it never collides with upstream `btop++` (which Omarchy ships by default). Defer any repository rename until multi-node telemetry works end to end.
3. **Collector boundary.**
   - **Windows:** Keep the bespoke Win32, NT, DXGI, WDDM PDH, SCM, and optional `System32\nvml.dll` collectors unchanged.
   - **Linux and macOS:** Add a target-gated `sysinfo` collector delivering total and per-core CPU busy percentage, RAM and swap accounting (`used = total - available`), local disk fullness plus best-effort read/write throughput, physical NIC RX/TX rates, and the process list and tree with guarded POSIX signals (`SIGTERM`/`SIGKILL` verified against process start time and rejecting PID 1 and kernel threads). Represent GPU telemetry, OS service managers (`systemd`, `launchd`), and CPU die temperatures as `DataState::Unavailable` so the UI hides them cleanly in v1.
4. **Filter noisy Unix mounts and interfaces.** Exclude virtual filesystems (`tmpfs`, `squashfs`, `overlay`, snap mounts), deduplicate APFS container volumes, and exclude loopback, Tailscale (`utun`/`tailscale0`), and Docker `veth` interfaces from physical NIC totals.
5. **Phased rollout.**
   - **M1:** Gate Windows-only dependencies behind `cfg(windows)`, add the `sysinfo` collector and POSIX `btop-term` backend, and validate the local TUI on Ubuntu (`mini`), Omarchy, and Apple Silicon macOS (`air`).
   - **M2:** Add the `#![forbid(unsafe_code)]` `btop-fleet` crate (`agent` ring server, `fleet` TUI, and `fleet --json` / `--diagnose`; see [ADR-0004](0004-tailscale-fleet-architecture.md)).
   - **M3:** Add 4-target CI release builds, installers, and unprivileged user-login autostart scripts (`systemd --user`, `launchd` LaunchAgent, Windows Scheduled Task).

## Considered options

- **Keep local TUI Windows-only and ship only Agent + Fleet View on Linux/macOS.** Rejected because Franz wants one consistent terminal monitor, theme set, and `--json`/`--diagnose` CLI across every machine in his fleet.
- **Write hand-rolled `/proc`, `sysfs`, Mach, and `IOReport` collectors upfront.** Rejected because maintaining three native FFI implementations before shipping fleet telemetry invites severe scope creep.
- **Ship two separate binaries (`btop` and `btop-agent`) per OS.** Rejected to avoid an agent/viewer version pairing matrix and double the release artifacts.

## Consequences

- `btop-collect` and `apps/btop-windows` gain clean `cfg(windows)` and `cfg(unix)` module boundaries; `btop-model`, `btop-config`, and `btop-ui` remain `#![forbid(unsafe_code)]` and OS-independent.
- Non-Windows local TUI users get immediate CPU, memory, disk, network, process tree, themes, and `--json` support without waiting for bespoke Linux/macOS GPU or service collectors.
- Linux and macOS terminals are supported via `crossterm` modern TrueColor UTF-8 terminals; legacy `conhost.exe` and 16-color/256-color fallback modes remain out of scope.
