# AGENTS.md

Instructions and engineering principles for contributors and AI agents working on `hemsoft-dev/btop-windows`.

## 1. Initial release scope: English Windows 11 x64 only

To ship a clean, fast v1 without drowning in platform edge cases, initial release scope is strictly bounded:

- **OS:** English Windows 11 (23H2+), `x86_64` (`x86_64-pc-windows-msvc`).
- **Terminal:** Windows Terminal (ConPTY, UTF-8, 24-bit TrueColor, Unicode Braille and Block glyphs, DEC 2026 synchronized output).
- **Locale:** English (`en-US`) Windows counter and system strings.

### Explicitly out of scope for v1 (do not build or add workarounds for these)

- Windows 10 or older OS versions
- Non-English Windows locales and localized PDH counter workarounds
- Legacy `conhost.exe` bugs (`srWindow.Top` mouse offsets, raster fonts, non-VT fallback renderers) or multi-terminal compatibility matrices (VS Code, WezTerm, Win32-OpenSSH)
- ARM64 (`aarch64-pc-windows-msvc`) builds and Qualcomm GPU support
- >64 logical processor multi-group topologies (`RelationGroup` / `NtQuerySystemInformationEx` multi-group iteration)
- 256-color quantization (`--low-color`) and 16-color TTY mode (`--tty`)
- Remote SMB shares, BitLocker-locked volumes, mounted folder paths, and multi-extent Storage Spaces disk mapping
- Offline domain-joined AD `LookupAccountSidW` async timeout workers
- Multi-battery SetupAPI IOCTLs, USB UPS reporting, and battery wattage
- AMD ADLX (`amdadlx64.dll`), Intel IGCL (`ControlLib.dll`), and hybrid-laptop D3cold dGPU wake guards
- External Tier-2 sensor bridges (LibreHardwareMonitor HTTP, HWiNFO shared memory)
- Process Kill Tree, Suspend/Resume, Efficiency Mode, `svchost` reverse service mapping, and in-app UAC relaunch (`runas`)
- `cargo-fuzz` harnesses, dual-pass `CreateRestrictedToken` CI, Azure Trusted Signing, and WinGet/Scoop publishing

If a task or review suggests adding code for any item above, defer it to a post-v1 issue instead of expanding v1.

## 2. Principle of simplicity

Every design and implementation choice must optimize for simplicity and directness:

1. **Solve the concrete problem directly.** Write the simplest code that satisfies the issue's acceptance criteria on English Windows 11 x64.
2. **Avoid speculative abstractions.** Do not introduce traits, extra worker threads, config knobs, or plugin registries until two real call sites or a measured latency problem demand them.
3. **Keep data flow linear and easy to trace.** Collectors gather data and publish immutable `Snapshot` structs; `btop-ui` renders `(&Snapshot, &Config, Size) -> Grid` as a pure function; `btop-term` diffs and writes the frame.
4. **Fail clearly instead of building fallback mazes.** When an optional metric (like CPU temperature or GPU clocks) is absent, represent it as `None` or `DataState::Unavailable` and hide it cleanly.
5. **Keep files and PRs small.** One collector, widget, or bounded behavior per PR. Delete dead code and unused options immediately.

## 3. Non-negotiable safety rules

Simplicity never overrides these four invariants:

1. **Zero bundled kernel drivers.** Never ship WinRing0 or any kernel driver. Use user-mode Windows APIs and optional `nvml.dll` loaded safely from `System32`.
2. **Always restore the terminal.** Normal exit, `Ctrl+C`, `CTRL_CLOSE_EVENT`, and panics must restore console modes, unhide the cursor, and leave the alternate screen. Keep `panic = "unwind"`.
3. **Never kill the wrong or critical process.** Before calling `TerminateProcess`, verify `CreateTime` on the opened process handle and reject critical system processes (`IsProcessCritical`).
4. **Isolate `unsafe`.** Keep `#![forbid(unsafe_code)]` on `btop-model`, `btop-config`, and `btop-ui`. Put raw Win32/NT FFI structs and compile-time layout assertions in `crates/btop-sys`. Call `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` at startup.

## 4. Quality gates and validation

Before opening or merging a PR:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Every UI pull request must include a `## Validation` section with exact reproduction commands and current-head Windows Terminal screenshots uploaded as GitHub attachments (plus an inline recording when interaction or motion is part of the change).
