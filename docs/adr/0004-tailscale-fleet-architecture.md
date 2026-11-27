# ADR-0004: Pull-based Tailscale Fleet Agent, `NodeSample` contract, and read-only boundary

- **Status:** Accepted
- **Date:** 2026-11-26
- **Council:** Claude Opus 5.5, GPT 6.2 Sol, Claude Fable 5.2 (missing: none; Gemini 4 Neon excluded as active seat)

## Context

Franz wants a lightweight, zero-ops way to monitor CPU, memory, disk, and network activity across his private Tailscale fleet (`home`, `laptop`, `air`, `mini`, and future Linux/Omarchy/ARM machines) inside `btop` without deploying Prometheus, Grafana, Beszel, a central database, or a web stack. Domain terms follow [GLOSSARY.md](../../GLOSSARY.md).

## Consultation synthesis and disposition

All three Council seats agreed on four core rules: keep the wire contract small and separate from `Snapshot`, use pull-based peer-to-peer polling with an in-memory `Ring` and no central hub, keep the implementation synchronous with no async runtime, and isolate fleet networking in a `#![forbid(unsafe_code)]` crate that cannot link process-kill code.

The Council split on Tailscale listener binding:
- **Opus 5.5 and Fable 5.2** recommended binding directly to the Node's Tailscale IP over plain HTTP (WireGuard already encrypts tailnet traffic, zero Tailscale Serve setup, plus an optional `--bind 127.0.0.1` escape hatch and one documented Windows Firewall rule).
- **GPT 6.2 Sol** recommended binding strictly to `127.0.0.1` behind Tailscale Serve (portable loopback bind, HTTPS, tailnet grants, no Windows Firewall rule).

Franz selected the direct Tailscale IP bind with the `--bind 127.0.0.1` escape hatch so adding a Node needs no per-host Tailscale Serve setup, while still allowing users who prefer Tailscale Serve to use it with zero extra code.

## Decisions

1. **Pull-based peer-to-peer topology, no hub and no disk persistence.** Each `Node` runs `agent` unprivileged at user login (`systemd --user`, `launchd` LaunchAgent, or Windows Scheduled Task), samples on a configurable schedule (default `5s`, valid range `1s..60s`), and stores recent readings in an in-memory `Ring` (default `10m`, valid range `1m..60m`, zero disk writes). Sleeping laptops simply show `Offline` (with session `last seen` when known) rather than forcing an always-on hub or time-series store.
2. **Portable `NodeSample` contract (never transmit `Snapshot`).** Define `NodeSample` in `btop-model` with roughly 15 OS-neutral scalars plus existing `DataState` semantics: agent timestamp, CPU busy percentage, memory used (`total - available`) and total, worst-volume disk fullness percentage plus best-effort aggregate read/write bytes/sec (`Unavailable` when unsupported), physical NIC RX/TX bytes/sec, and uptime. Never serialize `Snapshot`, usernames, process lists, command lines, GPUs, or services over the network. On Windows, the Agent invokes only CPU, memory, disk, and network collectors directly without initializing process, GPU, or SCM polling.
3. **Single versioned read-only HTTP endpoint, synchronous threads.** Expose only `GET /v1/samples?since_ms=<cursor>` (default port `9127`), returning `schema_version`, `agent_version`, `interval_ms`, the Agent's `now_ms`, and all `NodeSample` entries newer than `since_ms` (`since_ms=0` returns the full `Ring` so sparklines fill on open). Use the Agent's own clock for the `Cursor` and sample age so cross-machine clock skew never breaks polling. Reject non-GET methods with strict request size and timeout caps. Use synchronous OS threads with a small sync HTTP server and client (no async runtime).
4. **Sleep-gap and mixed-version safety.** Drop any rate delta spanning more than `3x interval` across host suspend/resume so waking laptops never emit false throughput spikes, and render gaps in graphs instead of interpolating. Tolerate additive JSON fields via `serde(default)` and surface incompatible major schema versions as `DataState::Unavailable("incompatible agent")`.
5. **Tailscale-scoped listener and explicit Node list.** By default, `agent` auto-detects the machine's Tailscale interface address (`100.64.0.0/10` or `fd7a:115c:a1e0::/48`), never binds `0.0.0.0`, retries with backoff at boot until Tailscale is up, and rejects connections whose peer IP is outside Tailscale address ranges. Support `--bind 127.0.0.1:<port>` for Tailscale Serve users and document the single Tailscale-scoped inbound Windows Firewall rule. `Fleet View` polls an explicit list of Tailscale MagicDNS names from `btop.conf` (`fleet_nodes`) or `--nodes`, avoiding `tailscale status --json` auto-discovery so mobile devices and shared nodes never pollute the dashboard.
6. **Permanently read-only boundary.** Place Agent server, Fleet polling client, and `Ring` logic in a new `crates/btop-fleet` crate enforcing `#![forbid(unsafe_code)]` and depending only on `btop-model` (never `btop-sys` or process-action modules). `Fleet View` keeps `allows_live_actions() == false`. Remote process termination, service control, command execution, and remote configuration push are permanently out of scope.
7. **Fleet View UX and headless JSON.** `Fleet View` launches via `fleet` (or toggles from the local TUI via a hotkey when `fleet_nodes` is configured) and renders a top Node summary table (`Online` when age <= `3x interval`, `Stale` when > `3x interval`, `Offline` when unreachable) plus a lower detail strip displaying the selected Node's 10-minute CPU, RAM, Disk, and Network Braille graphs from the already-fetched `Ring` (zero extra RPCs). Also expose `fleet --json` and `fleet --json --diagnose` so scripts and AI agents can inspect the whole fleet in one call.

## Considered options

- **Push to a central hub on `mini` with SQLite retention.** Rejected for v1 because it creates a mandatory always-on server, adds ingestion/retry state and storage lifecycle management, and moves toward Prometheus/Beszel territory when a 10-minute per-node RAM `Ring` meets the live-monitoring goal.
- **Mandatory `127.0.0.1` + Tailscale Serve.** Retained as a zero-code opt-in via `--bind 127.0.0.1`, but not required by default so onboarding a new tailnet machine stays a single binary start.
- **Auto-discover peers via `tailscale status --json`.** Rejected for v1 because the tailnet contains phones (`iphone`, `ipad`) and shared nodes, the macOS GUI app does not place `tailscale` on `PATH`, and explicit `fleet_nodes` config is predictable.
- **Equal-sized multi-node card grid in TUI.** Rejected in favor of a summary table plus a lower detail strip for the selected Node, which scales cleanly from 2 to 20 Nodes inside standard terminal dimensions.

## Consequences

- Zero database, zero disk wear on SBCs/laptops, ~25 KB RAM per Node for the default 10-minute `Ring` (120 samples at 5s), and zero single point of failure.
- Fleet mode cannot accidentally gain remote-execution or process-kill capabilities because `btop-fleet` cannot link `btop-sys`.
- Explicitly cut from v1: central hubs, persistent history, alerting/notifications, web UI, Prometheus exporters, TLS/app-level auth tokens, remote process/GPU/service drill-down, auto-update, and automatic node enrollment.
