# Contributing to btop-windows

Thanks for contributing to `btop-windows` (shipping `btop.exe`). Both human contributors and AI coding agents should read [AGENTS.md](AGENTS.md) alongside this guide before opening a pull request.

## Prerequisites and quick start

- **Toolchain:** Rust stable (`rust-version = "1.95"`, Edition 2024) targeting `x86_64-pc-windows-msvc` on Windows 11 x64.
- **Portable development on Linux/macOS:** `btop-model`, `btop-config`, `btop-collect`, `btop-term`, and `btop-ui` build and pass unit, golden, and render-budget tests without Windows APIs.
- **Run locally on Windows 11:** Use `.\run.ps1` from the repository root to build (when sources changed) and launch `btop.exe` in Windows Terminal. Use `.\run.ps1 -Replay` to run deterministically against `fixtures/win11-16core-dual-gpu-desktop.jsonl`.

## Target platform and scope

v1 optimizes for **English Windows 11 (23H2+) x64** in **Windows Terminal**. Keeping that baseline focused lets `btop.exe` stay fast, small (~3 MiB), and maintainable. Before implementing features outside the v1 baseline (such as Windows 10 fallbacks, non-English PDH workarounds, legacy ConHost quirks, ARM64, or vendor sensor SDKs), open an issue first to align on post-v1 design. See [AGENTS.md](AGENTS.md) for the full list of v1 boundaries.

## Architecture at a glance

Data flow is strictly linear so every layer stays easy to reason about and test:

1. `crates/btop-sys`: Raw Win32/NT FFI structs with compile-time `size_of` and `offset_of` assertions on `x86_64`, DLL startup hardening, and optional `System32\nvml.dll` loading.
2. `crates/btop-collect`: Windows telemetry collectors and pure delta math publishing immutable `Snapshot` values via `arc-swap`. Slow optional calls (NVML, SCM) never block the UI loop.
3. `crates/btop-model` and `crates/btop-config`: Platform-independent data models, JSONL record/replay, `btop.conf`, and `.theme` parsing (`#![forbid(unsafe_code)]`).
4. `crates/btop-ui`: Pure layout and widget rendering `(&Snapshot, &UiHistory, &UiState, &Config, Size) -> Grid` (`#![forbid(unsafe_code)]`).
5. `crates/btop-term`: Cell `Grid`, mouse `HitMap`, DEC 2026 synchronized VT diff encoder, and panic-safe console restore guard.
6. `apps/btop-windows`: `btop.exe` CLI (`clap`), input handling, event loop, and config persistence.

## Non-negotiable safety invariants

1. **Zero bundled kernel drivers.** Never add WinRing0 or any kernel helper. Use user-mode Windows APIs and safe `LoadLibraryExW(..., LOAD_LIBRARY_SEARCH_SYSTEM32)` for optional `nvml.dll`.
2. **Always restore the console.** Normal exit, `Ctrl+C`, `CTRL_CLOSE_EVENT`, and panics must restore console modes, leave the alternate screen, and unhide the cursor. Keep `panic = "unwind"`.
3. **Never terminate the wrong or critical process.** Process termination must verify `CreateTime` on the opened handle and refuse critical system processes (`IsProcessCritical`) and self-PID.
4. **Isolate `unsafe`.** Keep `#![forbid(unsafe_code)]` on `btop-model`, `btop-config`, and `btop-ui`. Confine raw FFI to `crates/btop-sys` and VT console setup to `crates/btop-term/src/backend.rs`.

## Required quality gates

Run these checks before opening a pull request:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

For performance-sensitive or release packaging changes, also run:

```powershell
# 200x60 render + VT diff latency budget (< 2.0 ms p95)
cargo test --release -p btop-ui --test perf_budget -- --ignored --nocapture

# Live Windows 11 steady-state private working set RSS (< 30 MiB) and CPU overhead
cargo test --release -p btop-windows --test live_budgets -- --ignored --nocapture

# Static-CRT PE import audit (confirm zero vc_redist or non-system DLLs)
cargo build --release --locked -p btop-windows --target x86_64-pc-windows-msvc
pwsh -NoProfile -File scripts/check-pe-imports.ps1 -BinaryPath target/x86_64-pc-windows-msvc/release/btop.exe
```

## Pull request guidelines

- Keep pull requests small and focused on one collector, widget, or bounded behavior.
- Add or update unit tests (and `insta` golden snapshots in `crates/btop-ui/tests/` for layout/rendering changes).
- Delete dead code and unused options instead of keeping unused scaffolding.
- **UI pull requests** must include a `## Validation` section with:
  - exact reproduction commands (preferring `--replay` fixtures when applicable),
  - current-head Windows Terminal screenshots uploaded as GitHub attachments covering every changed state and target viewport (`80x24`, `120x40`, `200x60`), and
  - an inline screen recording when interaction, scrolling, menu navigation, or resize behavior changed.

## Reporting issues

Open an issue on [GitHub Issues](https://github.com/hemsoft-dev/btop-windows/issues) with your Windows 11 build (`winver`), Windows Terminal version, exact `btop --version`, reproduction steps, and a `--record` JSONL capture when reporting a telemetry or layout bug.
