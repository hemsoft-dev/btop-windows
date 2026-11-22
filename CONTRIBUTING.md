# Contributing to btop-windows

## Engineering rules

1. **Isolate `unsafe`.** `btop-model`, `btop-config`, and `btop-ui` enforce `#![forbid(unsafe_code)]`. All raw Win32/NT/WDK FFI belongs in `crates/btop-sys` with compile-time `size_of` and `offset_of` assertions for x64 and ARM64.
2. **Never ship or load kernel drivers.** No WinRing0 or unsigned kernel helpers. User-mode Windows APIs and opt-in out-of-process providers only.
3. **Never block the UI loop.** Slow or blocking telemetry (vendor GPU DLLs, `LookupAccountSidW`, remote disk space, SCM polling, external HTTP/shared-memory sensors) must run on dedicated worker threads with timeouts and publish freshness states (`WarmingUp`, `Available`, `Unavailable`, `Denied`, `Stale`).
4. **Restore the console on every exit.** Normal exit, `Ctrl+C`, `CTRL_CLOSE_EVENT`, and panics must restore input/output console modes, leave the alternate screen, and unhide the cursor. Never set `panic = "abort"`.
5. **Keep pure math testable.** Delta calculations, unit formatting, config/theme parsing, and `btop-ui` layout/rendering must be pure functions tested without Windows hardware dependencies.

## Required quality gates

Run these before opening a pull request:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## UI pull request validation

Every pull request that changes visible terminal layout, colors, graphs, menus, or interaction must include a `## Validation` section with:

- exact commands to reproduce the view (preferring `--replay` fixtures when applicable),
- current-head screenshots uploaded as GitHub attachments covering every changed state and target terminal size (`80x24`, `120x40`, `200x60`), and
- an inline screen recording when interaction, scrolling, menu navigation, or resize transitions are part of the change.
