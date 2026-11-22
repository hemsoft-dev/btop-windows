# Contributing to btop-windows

Read [AGENTS.md](AGENTS.md) first. Initial release scope is strictly **English Windows 11 x64 in Windows Terminal**, guided by the **Principle of Simplicity**.

## Engineering rules

1. **Keep scope tight and simple.** Do not add Windows 10, non-English locale, legacy ConHost, ARM64, or >64-LP multi-group workarounds to v1.
2. **Isolate `unsafe`.** `btop-model`, `btop-config`, and `btop-ui` enforce `#![forbid(unsafe_code)]`. All raw Win32/NT FFI belongs in `crates/btop-sys` with compile-time `size_of` and `offset_of` assertions on `x86_64`.
3. **Never ship or load kernel drivers.** No WinRing0 or unsigned kernel helpers. User-mode Windows APIs and safe `nvml.dll` loading from `System32` only.
4. **Never block the UI loop.** Slow optional calls (`nvml.dll`, SCM polling) run off the UI thread and publish explicit freshness states (`WarmingUp`, `Available`, `Unavailable`, `Denied`, `Stale`).
5. **Restore the console on every exit.** Normal exit, `Ctrl+C`, `CTRL_CLOSE_EVENT`, and panics must restore console modes, leave the alternate screen, and unhide the cursor. Never set `panic = "abort"`.
6. **Keep pure math testable.** Delta calculations, unit formatting, config/theme parsing, and `btop-ui` layout/rendering must be pure functions tested without Windows hardware dependencies.

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
