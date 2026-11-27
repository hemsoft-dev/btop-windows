# btop-windows

Domain language for local single-host monitoring and multi-host Tailscale fleet observability in `btop-windows`.

## Fleet monitoring

**Fleet**:
A user-configured set of machines on a private Tailscale tailnet monitored together from a single dashboard.
_Avoid_: Cluster, farm, inventory

**Node**:
One machine participating in a Fleet, identified by its Tailscale MagicDNS name and platform metadata.
_Avoid_: Host, peer, server, box

**Agent**:
The headless, permanently read-only background sampling mode of the binary running on a Node that records `NodeSample` values into a `Ring` and serves them to `Fleet View` callers.
_Avoid_: Daemon, exporter, collector service

**NodeSample**:
A single point-in-time, OS-neutral telemetry reading for one Node covering CPU utilization, memory used and total, local disk fullness and best-effort read/write rates, physical network RX/TX rates, and uptime, excluding usernames, processes, GPUs, and services.
_Avoid_: Snapshot, fleet payload, metric blob

**Ring**:
The bounded, in-memory sliding window of recent `NodeSample` values (default 10 minutes at 5-second intervals) held by an Agent with no disk persistence.
_Avoid_: History database, cache, archive

**Fleet View**:
The permanently read-only multi-node dashboard mode that polls Agents across a Fleet and renders live node health and sparklines.
_Avoid_: Dashboard mode, cluster view, remote console

**Online**:
A Node state in Fleet View where the Agent is reachable and its newest `NodeSample` is no older than three sampling intervals.
_Avoid_: Up, healthy, active

**Stale**:
A Node state in Fleet View where a previous `NodeSample` is retained for display, but its age exceeds three sampling intervals or the latest poll timed out.
_Avoid_: Lagging, degraded, delayed

**Offline**:
A Node state in Fleet View where the Agent cannot be reached (for example while a laptop is asleep), displaying the last seen time from the current session when known.
_Avoid_: Down, dead, disconnected

**Cursor**:
An Agent-clock timestamp passed by Fleet View when polling so each poll fetches only newer `NodeSample` entries from the Ring without depending on synchronized clocks across Nodes.
_Avoid_: Offset, sequence token, watermark

## Local monitoring

**Snapshot**:
A rich single-host local telemetry frame (including per-core CPU, processes, and OS-specific subsystems where available) rendered by the local TUI and headless JSON CLI, never sent over the network by an Agent.
_Avoid_: Sample, fleet reading
