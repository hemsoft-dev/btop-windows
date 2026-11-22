# btop-windows

A fast, zero-driver, native Windows terminal resource monitor written in Rust, bringing modern [btop++](https://github.com/aristocratos/btop) (v1.4.8) capabilities and Windows-first process and service workflows to Windows 11 and Windows 10 (x64 and ARM64).

## Why btop-windows

Upstream `aristocratos/btop` targets Linux, macOS, and BSDs. The separate `aristocratos/btop4win` port remains on the v1.0 codebase: it lacks modern multi-GPU boxes, requires the Visual C++ Redistributable, and depends on LibreHardwareMonitor's WinRing0 kernel driver, which triggers Windows Defender detections on Windows 11.

`btop-windows` is a clean-room Rust implementation built around four principles:

1. **Zero bundled kernel drivers.** Every core metric uses documented user-mode Windows APIs and safe dynamic loading of vendor GPU libraries (`nvml.dll`, AMD ADLX, Intel IGCL). No WinRing0, no Defender false positives.
2. **Modern btop parity plus Windows workflows.** Full CPU (including >64 logical processors and hybrid P/E-core topology), memory and commit accounting, disk I/O, 64-bit network counters, multi-GPU boxes (up to 6 GPUs with hybrid-laptop dGPU wake protection), process tree/filter/follow/pause, plus a native Windows Services (SCM) view, Priority Class, and Efficiency Mode controls.
3. **Single static binary.** Compiled in Rust with `+crt-static` on x64 and ARM64 with no runtime installer dependencies.
4. **Deterministic testing from day one.** Platform-independent model, config, renderer, and UI crates build and run unit and golden snapshot tests on Linux and Windows, backed by `--record` and `--replay` snapshot streams.

## Tech stack

- **Language:** Rust (Edition 2024, MSRV 1.95), static CRT (`+crt-static`) on x64 and ARM64
- **Windows bindings:** `windows-sys` for raw Win32/NT FFI, `windows` minimally for COM/DXGI, hand-audited NT/WDK structs with compile-time layout assertions in `crates/btop-sys`
- **Terminal rendering:** Custom cell `Grid`, per-frame `HitMap`, and DEC 2026 synchronized VT diff encoder in `crates/btop-term`, with `crossterm` behind an `InputSource` trait
- **State & concurrency:** Immutable timestamped `Snapshot` publication via `arc-swap`; blocking collectors and optional vendor libraries run on isolated worker threads with timeouts so UI input never stalls

## Workspace layout

```text
crates/
  btop-sys/       # Hand-written NT/WDK FFI structs, compile-time layout assertions
  btop-model/     # Platform-independent Snapshot, DataState, serde (no unsafe)
  btop-config/    # btop.conf and .theme parsers, validation, defaults (no unsafe)
  btop-collect/   # Collector trait, pure delta math, Windows collectors
  btop-term/      # Grid, HitMap, VT diff encoder, InputSource, Windows VT console
  btop-ui/        # Pure render(&Snapshot, &Config, &Theme, Size) -> Grid (no unsafe)
apps/
  btop-windows/   # CLI (clap), thread wiring, startup hardening, panic-safe restore
docs/
  SPEC.md         # Full functional specification and upstream btop deep dive
  ARCHITECTURE.md # System architecture, Windows telemetry matrix, test & CI plan
  PARITY.md       # Upstream btop v1.4.8 config, CLI, keybinding, and theme parity map
  adr/            # Architecture Decision Records
```

## Documentation

- [Functional Specification](docs/SPEC.md)
- [System Architecture](docs/ARCHITECTURE.md)
- [Upstream Parity Matrix](docs/PARITY.md)
- [ADR-0001: Tech Stack, Renderer, and Telemetry Architecture](docs/adr/0001-tech-stack-and-renderer.md)

## Local development

```powershell
# Format check
cargo fmt --all -- --check

# Lint (strict)
cargo clippy --workspace --all-targets -- -D warnings

# Run tests
cargo test --workspace

# Run CLI help / default config
cargo run -p btop-windows -- --help
cargo run -p btop-windows -- --default-config
```

## Milestones

Track progress on [GitHub Milestones](https://github.com/hemsoft-dev/btop-windows/milestones) and [GitHub Issues](https://github.com/hemsoft-dev/btop-windows/issues):

- **M0:** Technical Spikes & ADRs
- **M1:** Walking Skeleton & Vertical Slice
- **M2:** Core System Boxes & Layout Engine
- **M3:** Process Manager & Windows Services
- **M4:** Multi-GPU & Hardware Sensors
- **M5:** Menus, Themes, Mouse & Upstream Parity
- **M6:** Release, Signing & Performance Gates

## License

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE) for upstream `aristocratos/btop` attribution.
