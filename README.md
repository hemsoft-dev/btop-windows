# btop-windows

A fast, zero-driver, native Windows 11 terminal resource monitor written in Rust. It ships as a single static `btop.exe` binary (~3 MiB) bringing modern [btop++](https://github.com/aristocratos/btop) (v1.4.8) panels, themes, and multi-GPU telemetry together with Windows-first process and Windows Services controls.

![btop-windows running in Windows Terminal showing hybrid 8P+8E CPU, dual NVIDIA RTX 5090 and AMD Radeon GPU telemetry, memory, disks, network, and processes](docs/assets/btop-windows-hero.png)

## Quick install

Install `btop.exe` into `%LOCALAPPDATA%\Programs\btop`, verify its SHA-256 checksum, and add `btop` to your User `PATH` (no Administrator prompt or Visual C++ Redistributable required):

```powershell
irm https://raw.githubusercontent.com/hemsoft-dev/btop-windows/main/install.ps1 | iex
```

Then open **Windows Terminal** and run:

```powershell
btop
```

To uninstall later:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/hemsoft-dev/btop-windows/main/install.ps1))) -Uninstall
```

### Manual portable install

1. Download `btop-windows-<tag>-x86_64-windows.zip` and `SHA256SUMS.txt` from [GitHub Releases](https://github.com/hemsoft-dev/btop-windows/releases).
2. Verify the SHA-256 hash in PowerShell:

   ```powershell
   Get-FileHash .\btop-windows-<tag>-x86_64-windows.zip -Algorithm SHA256
   Get-Content .\SHA256SUMS.txt
   ```

3. Extract anywhere (for example `C:\Tools\btop`) and run `C:\Tools\btop\btop.exe` in Windows Terminal, or add the folder to your User `PATH`.

## Platform baseline

| Area | Baseline |
| --- | --- |
| Operating system | English Windows 11 (23H2+) x64 (`x86_64-pc-windows-msvc`) |
| Terminal | Windows Terminal (24-bit TrueColor, UTF-8, Cascadia Mono or Nerd Font for Braille/Block graphs) |
| Runtime dependencies | Zero external runtimes (`+crt-static`, audited to 11 inbox Windows system DLLs only) |
| Kernel drivers | Zero (user-mode Win32/NT APIs only; no WinRing0 or Defender friction) |
| Privileges | Non-elevated by default; Administrator required only to Start/Stop Windows Services |
| GPU telemetry | All hardware GPUs via DXGI + WDDM PDH, plus automatic `System32\nvml.dll` enrichment on NVIDIA GPUs |

For contributor scope boundaries and planned post-v1 items, see [AGENTS.md](AGENTS.md).

## Why btop-windows

If you already know [btop4win](https://github.com/aristocratos/btop4win) (frozen at v1.0.5), here is an honest comparison of where `btop-windows` improves on it and where `btop4win` still has an edge.

### Where btop-windows is better

- **Multi-GPU telemetry without a kernel driver.** Standard `btop4win` has no GPU panels, and its LHM build relies on LibreHardwareMonitor (`WinRing0`), which requires Administrator privileges and triggers Windows Defender and game anti-cheat (Vanguard, EAC). `btop-windows` renders dedicated multi-GPU boxes (`5`..`0`) across Intel, AMD, and NVIDIA using user-mode DXGI + WDDM PDH, plus automatic `System32\nvml.dll` enrichment on NVIDIA (temperature, power, clocks, fan), with zero kernel drivers and no Admin prompt.
- **Modern `btop++` v1.4.8 UX on Windows 11.** Brings newer upstream features (`btop++` v1.4.8 layout presets, GPU boxes, live theme preview in Options, regex process filter, follow/pause) natively to Windows 11.
- **Windows-first workflows.** Adds a toggleable Windows Services (`S`) view with Start/Stop/Restart controls, hybrid P/E-core CPU labeling (`GetSystemCpuSetInformation`), Task Manager utility mode, guarded process termination (`CreateTime` PID-reuse verification and `IsProcessCritical` protection), and deterministic `--record` / `--replay` JSONL traces.
- **Minimal, audited runtime footprint.** Written in Rust (`#![forbid(unsafe_code)]` on model, config, and UI), statically linked against the MSVC CRT (`+crt-static`, audited in CI to 11 inbox Windows DLLs), hardened with `LOAD_LIBRARY_SEARCH_SYSTEM32`, and distributed with a SHA-256-verified installer (`~3.0 MiB` binary, `~1.3 MiB` zip).

### Where `btop4win` still has an edge

- **CPU die temperatures and motherboard fan RPMs.** Windows does not expose reliable Intel/AMD CPU die sensors or motherboard fan RPMs in user mode. Because `btop-windows` refuses to bundle a kernel driver (`WinRing0`), CPU temperature and motherboard fan readings are unavailable on most desktops until an optional external sensor bridge is added post-v1.
- **Legacy OS and terminal coverage.** `btop4win` supports Windows 10, legacy `conhost.exe`, non-English locales, and >64 logical cores. `btop-windows` v1 intentionally focuses on English Windows 11 x64 in Windows Terminal.
- **Longer field history.** `btop4win` has years of community mileage across edge-case hardware. `btop-windows` is new and actively developed, so real-world feedback on diverse setups is welcome.

## Highlights

- **Zero kernel drivers and zero runtime installers.** Every core metric uses user-mode Windows APIs plus optional safe loading of `System32\nvml.dll`. No WinRing0, no `vc_redist.x64.exe`, no Defender false positives.
- **Modern btop UX plus Windows workflows.** CPU with hybrid P/E-core labels, frequency, and Task Manager utility mode; physical/commit/pagefile/cache memory accounting; local disk capacity and I/O mode; 64-bit auto-scaling network graphs; dedicated multi-GPU boxes (`5`..`0`); process tree, regex filter, pause, follow, detail overlay, and guarded process controls; plus a toggleable Windows Services (`S`) view.
- **TrueColor themes and mouse/Vim controls.** 10 Apache-2.0 themes embedded directly in `btop.exe`, live preview in Options, full mouse click and scroll hit-testing, and optional `vim_keys`.
- **Deterministic record and replay.** Capture live sessions with `btop --record <file>` and inspect them anywhere with `btop --replay <file>`.
- **Headless JSON telemetry for scripts and AI agents.** Emit single snapshots (`--json`), nested process trees (`--tree --top`), NDJSON streams (`--count --interval-ms`), time-window summaries (`--summary --window-ms`), deterministic bottleneck diagnostics (`--diagnose`), and Draft 2020-12 JSON Schema (`--schema`) without opening a TTY.

## Headless JSON CLI (scripts and AI agents)

Run `btop --json` in any non-interactive shell, pipe, or agent tool call to query live Windows 11 telemetry or replay recorded `.jsonl` traces without entering the TUI:

```powershell
# Fast single snapshot of CPU, memory, and GPUs (default 500ms live delta warmup)
btop --json --sections cpu,mem,gpus

# Nested process tree with rolled-up subtree CPU/memory, top 5 roots sorted by CPU
btop --json --sections procs --tree --sort cpu --top 5

# Inspect one PID with its verified ancestors and descendants, or filter by !regex
btop --json --sections procs --tree --pid 4216
btop --json --sections procs --filter '!^(cargo|rustc)' --sort mem --top 10

# Filter running Windows Services and include linked live process metadata
btop --json --sections services --services-state running --sample-ms 0

# Stream 5 live NDJSON snapshots (0 = stream until Ctrl+C or broken pipe)
btop --json --sections cpu,mem,gpus --count 5 --interval-ms 500

# Capture a 3-second spike summary (min/avg/p95/max + compact series + diagnostics)
btop --json --summary --window-ms 3000 --interval-ms 500 --include-series --diagnose

# Print the Draft 2020-12 JSON Schema (all, snapshot, summary, diagnose, error)
btop --schema
btop --schema snapshot
```

Every subsystem uses an explicit tagged `DataState` union (`available`, `stale`, `warming_up`, `denied`, `unavailable`) so missing sensors or access-denied PPL processes stay visible rather than turning into fabricated zeros.

Exit codes in `--json` mode:

- `0`: Completed cleanly (including `Ctrl+C` or broken pipe during streaming).
- `1`: Runtime error (config, replay file, collector, or stdout I/O), written as `{"schema_version":1,"error":{"code":...,"message":...}}` to stderr.
- `2`: CLI usage or validation error (`code: "cli_usage"` on stderr; plain stderr when `--json` is not active).

## Everyday keys and configuration

- **Panels and presets:** `1`..`4` toggle CPU/MEM/NET/PROC, `5`..`0` toggle GPU boxes, `p`/`P` cycle layout presets, `d` toggles disks, `i` toggles Disk I/O mode.
- **Processes and Services:** `e` tree view, `f` filter (`!` prefix for regex), `Enter` detail overlay, `u` pause, `F` follow, `t` graceful close, `k` guarded terminate, `N` priority class, `S` toggle Windows Services view.
- **Menus and themes:** `Esc` / `m` Main menu, `o` / `F2` Options (live `color_theme` switcher), `h` / `F1` Help, `Ctrl+L` force full redraw, `Ctrl+R` reload config, `q` quit.
- **Config location:** Place `btop.conf` next to `btop.exe` for portable mode, or let `btop` load and save `%APPDATA%\btop\btop.conf` automatically. Print a starter config with `btop --default-config`.
- **Custom themes:** Searched in `--themes-dir`, exe-adjacent `themes\`, `%APPDATA%\btop\themes`, then the embedded bundle.

## Local development

```powershell
# Build (when needed) and launch btop.exe from the repo root
.\run.ps1

# Helpful run.ps1 options
.\run.ps1 -Build                  # Force a fresh release build
.\run.ps1 -DebugBuild             # Build and run the debug binary
.\run.ps1 -Replay                 # Run against the dual-GPU replay fixture
.\run.ps1 --update 1000           # Forward CLI flags to btop.exe

# Quality gates
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Workspace layout

```text
crates/
  btop-sys/       # Hand-written NT/WDK FFI structs, compile-time layout assertions
  btop-model/     # Platform-independent Snapshot, DataState, serde (no unsafe)
  btop-config/    # btop.conf and .theme parsers, validation, defaults (no unsafe)
  btop-collect/   # Collector trait, pure delta math, Windows collectors
  btop-term/      # Grid, HitMap, VT diff encoder, InputSource, Windows VT console
  btop-ui/        # Pure render(&Snapshot, &History, &UiState, &Config, Size) -> Grid (no unsafe)
apps/
  btop-windows/   # btop.exe CLI (clap), thread wiring, startup hardening, panic-safe restore
docs/
  SPEC.md         # Full functional specification
  ARCHITECTURE.md # System architecture, telemetry matrix, test and CI plan
  PARITY.md       # Upstream btop v1.4.8 config, CLI, keybinding, and theme parity map
  adr/            # Architecture Decision Records
```

## Documentation and contributing

- [Contributing Guide](CONTRIBUTING.md)
- [Agent Instructions (AGENTS.md)](AGENTS.md) and [llms.txt](llms.txt)
- [Functional Specification](docs/SPEC.md)
- [System Architecture](docs/ARCHITECTURE.md)
- [Upstream Parity Matrix](docs/PARITY.md)
- [ADR-0001: Tech Stack, Renderer, and Telemetry Architecture](docs/adr/0001-tech-stack-and-renderer.md)
- [ADR-0002: Windows 11 x64 GPU Baseline API (DXGI + WDDM PDH)](docs/adr/0002-gpu-baseline-api.md)

## License

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE) for upstream `aristocratos/btop` attribution.
