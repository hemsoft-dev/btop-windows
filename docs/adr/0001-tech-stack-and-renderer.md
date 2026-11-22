# ADR-0001: Tech stack, terminal renderer, and Windows telemetry strategy

- **Status:** Accepted
- **Date:** 2026-11-21
- **Reviewers consulted:** Claude Opus 5.5 (high), GPT 6.2 Sol (high)

## Context

We are building `btop-windows` (`hemsoft-dev/btop-windows`), a native Windows resource monitor targeting feature parity with `aristocratos/btop` v1.4.8 plus Windows-native workflows (Windows Services, Priority Class, Efficiency Mode). We needed to settle four foundational decisions before writing collectors or UI:

1. Implementation language (Rust vs C++23 vs Zig/Go)
2. Terminal rendering stack (`ratatui` + `crossterm` vs custom cell `Grid` + VT diff encoder)
3. Workspace structure (single crate vs multi-crate workspace)
4. Zero-WinRing0 Windows telemetry strategy

## Consultation synthesis

Both reviewers converged strongly on core platform choices and surfaced critical Windows API traps:

- **Agreed:** Rust is the right language for a clean-room Windows-first implementation; x64 and ARM64 only (no 32-bit WOW64); static CRT (`+crt-static`) for a zero-dependency executable; zero bundled kernel drivers (no WinRing0); `PdhAddEnglishCounterW` everywhere; `(group, number)` keys for >64-LP CPUs; `KernelTime` includes `IdleTime`; `IDXGIAdapter3::QueryVideoMemoryInfo` is per-process only and cannot measure system VRAM; `SystemMemoryListInformation` requires admin privilege (`SeProfileSingleProcessPrivilege`) so non-elevated PDH memory counters are the baseline; process actions must verify `CreateTime` on the opened handle and guard `IsProcessCritical`; slow/vendor queries must run on isolated worker threads with explicit freshness states; CI must run both elevated and non-elevated (restricted token) smoke tests plus x64/ARM64 builds from M1.
- **Divergence 1 (Renderer):** GPT 6.2 Sol favored `ratatui` + `crossterm` with custom widgets writing to `ratatui::buffer::Buffer`. Opus 5.5 favored a custom cell `Grid` + `HitMap` + VT diff encoder (keeping `crossterm::event` behind an `InputSource` trait initially), noting that btop's dual-series Braille graphs, embedded hotkey border titles (`┐¹cpu┌`), preset layout geometry, and exact click hit-testing replace every Ratatui widget and layout primitive anyway.
- **Divergence 2 (Crate layout):** GPT 6.2 Sol favored starting as a single package with clean modules. Opus 5.5 favored a 6-crate workspace (`sys`, `model`, `collect`, `config`, `term`, `ui` + binary) so platform-independent crates compile and run golden tests natively on Linux/macOS and `unsafe` FFI stays strictly isolated.
- **Divergence 3 (GPU baseline):** GPT 6.2 Sol noted Microsoft marks `D3DKMTQueryStatistics` reserved for system use and preferred WDDM PDH as the documented baseline. Opus 5.5 noted PDH `GPU Engine` wildcard re-expansion costs tens of milliseconds per tick and highlighted the hybrid-laptop dGPU wake battery drain trap when polling NVML/ADLX.

## Decisions

1. **Language & v1 target:** Rust (Edition 2024, MSRV 1.95), targeting **English Windows 11 x64** (`x86_64-pc-windows-msvc`) in Windows Terminal with `+crt-static`, `panic = "unwind"` (so console-restoring `Drop` guards and panic hooks always execute), `windows-sys` for raw FFI, and `windows` minimally for COM/DXGI. Windows 10, non-English locales, ConHost, ARM64, and >64-LP multi-group CPUs are deferred post-v1 (see [AGENTS.md](../../AGENTS.md)).
2. **Renderer (`btop-term`):** Custom `Grid`, per-frame `HitMap`, and DEC 2026 VT `DiffEncoder` targeting 24-bit TrueColor Windows Terminal, with `crossterm::event` behind an `InputSource` trait.
3. **Workspace layout:** 6 workspace crates (`btop-sys`, `btop-model`, `btop-config`, `btop-collect`, `btop-term`, `btop-ui`) plus `apps/btop-windows`. `btop-model`, `btop-config`, and `btop-ui` enforce `#![forbid(unsafe_code)]` and run fast cross-platform CI on Linux.
4. **Focused M0 spike:** Run one focused spike (`#2`) comparing D3DKMT vs WDDM PDH for Windows 11 x64 system GPU utilization and VRAM before implementing M4.
5. **M1 vertical slice:** Ship a working CPU graph + read-only process list + `--record`/`--replay` + clean terminal restoration in M1 so real Windows 11 telemetry and rendering validate the architecture immediately.

## Consequences

- Zero Ratatui version churn or widget impedance mismatch; full control over btop's visual fidelity and cell hit-testing.
- Strict compile-time separation between unsafe Win32/NT FFI (`btop-sys`) and pure UI/config/model logic.
- We maintain ~300-500 lines of custom `Grid` and VT diff encoder code, mitigated by `vt100` roundtrip property tests and `insta` golden snapshots.
