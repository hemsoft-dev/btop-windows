# ADR-0002: Windows 11 x64 GPU baseline API (DXGI + WDDM PDH)

- **Status:** Accepted
- **Date:** 2026-11-21
- **Issue:** [#2](https://github.com/hemsoft-dev/btop-windows/issues/2)

## Context

`btop-windows` needs a user-mode, zero-driver baseline for system-wide GPU utilization and dedicated/shared VRAM on English Windows 11 x64 (Milestone M4, [#18](https://github.com/hemsoft-dev/btop-windows/issues/18)). `IDXGIAdapter3::QueryVideoMemoryInfo` cannot provide system-wide VRAM because it reports only the calling process's usage and budget.

Two user-mode candidates were evaluated:

1. **D3DKMT (`D3DKMTQueryStatistics` in `gdi32.dll`):** Queries adapter segment and node statistics per adapter LUID using the 808-byte (`0x328`) `D3DKMT_QUERYSTATISTICS` structure from `d3dkmthk.h` (marked reserved for system use by Microsoft).
2. **English WDDM PDH (`PdhAddEnglishCounterW`):** Queries `\GPU Engine(*)\Utilization Percentage`, `\GPU Adapter Memory(*)\Dedicated Usage`, and `\GPU Adapter Memory(*)\Shared Usage` via a persistent `PDH_HQUERY`, joined to DXGI hardware adapters by LUID.

## Test environment

- **OS:** Microsoft Windows 11 Pro `10.0.26200` (Build `26200`, English `en-US` / Locale `1033`)
- **Privilege level:** Standard non-elevated user (`Medium Mandatory Level`, `BUILTIN\Administrators` deny-only)
- **Hardware adapters:**
  - `NVIDIA GeForce RTX 5090` (Vendor `0x10DE`, Device `0x2B85`, Driver `32.0.16.1088`, LUID `0x00000000_000189EF`, 32,187 MiB dedicated VRAM, 3 segments, 16 nodes; active ~90-96% workload during measurement)
  - `AMD Radeon(TM) Graphics` (Vendor `0x1002`, Device `0x13C0`, Driver `32.0.21042.62`, LUID `0x00000000_0001E8E2`, 2,021 MiB dedicated VRAM, 5 segments, 11 nodes; idle during measurement)
- **Software adapter filtered:** `Microsoft Basic Render Driver` (`Vendor 0x1414`, `Device 0x008C`, `DXGI_ADAPTER_FLAG_SOFTWARE = true`)
- **Methodology:** Standalone throwaway Rust release binary built outside the workspace runtime crates; 60 samples per API spaced at 50 ms intervals, plus 10 full PDH open/add/collect/read rebuild samples.

## Measured results

### Latency (60 samples each, standard non-elevated user)

| Approach | Pass 1 p50 | Pass 1 p95 | Pass 1 max | Pass 2 p50 | Pass 2 p95 | Pass 2 max | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **D3DKMT** (all hardware adapters) | `8.613 ms` | `24.862 ms` | `31.601 ms` | `10.569 ms` | `21.945 ms` | `25.753 ms` | 37 kernel calls/tick across both adapters |
| D3DKMT: RTX 5090 only | - | - | - | `4.504 ms` | `16.268 ms` | `19.930 ms` | 1 adapter + 3 segments + 16 nodes |
| D3DKMT: AMD iGPU only | - | - | - | `5.703 ms` | `6.720 ms` | `8.916 ms` | 1 adapter + 5 segments + 11 nodes |
| **WDDM PDH** (persistent query) | **`3.036 ms`** | **`3.787 ms`** | **`4.017 ms`** | **`3.049 ms`** | **`3.595 ms`** | **`3.967 ms`** | Single collect across 1,728 engine instances |
| **WDDM PDH** (open + add + collect + read, n=10) | `1.724 ms` | `1.967 ms` | `1.967 ms` | `1.850 ms` | `2.595 ms` | `2.595 ms` | Includes full query rebuild and array read |

### Accuracy comparison (standard user)

| Adapter | Metric | D3DKMT (Pass 1 / Pass 2) | WDDM PDH (Pass 1 / Pass 2) | Reference (`nvidia-smi` after Pass 1) |
| --- | --- | --- | --- | --- |
| **NVIDIA GeForce RTX 5090** | Busiest engine util | `90.1%` / `73.5%` | `93.2%` / `90.7%` | `96%` |
| **NVIDIA GeForce RTX 5090** | Dedicated VRAM | `12,583 MiB` / `12,683 MiB` | `12,616 MiB` / `12,696 MiB` | `12,551 MiB` |
| **NVIDIA GeForce RTX 5090** | Shared GPU memory | `590 MiB` / `600 MiB` | `590 MiB` / `591 MiB` | N/A |
| **AMD Radeon(TM) Graphics** | Busiest engine util | `0.0%` / `0.0%` | `0.0%` / `0.0%` | N/A |
| **AMD Radeon(TM) Graphics** | Dedicated VRAM | `58 MiB` / `58 MiB` | `58 MiB` / `58 MiB` | N/A |
| **AMD Radeon(TM) Graphics** | Shared GPU memory | `93 MiB` / `93 MiB` | `93 MiB` / `93 MiB` | N/A |

### Key findings

1. **WDDM PDH is 2.8x-3.5x faster at p50 and 6.1x-6.6x faster at p95 than D3DKMT.** D3DKMT issues one `D3DKMTQueryStatistics` transition per adapter, segment, and node (37 calls per tick on this two-GPU host) and spiked to `24.86 ms` p95 under active GPU load, while persistent WDDM PDH stayed at `3.05 ms` p50 and `3.60 ms` p95 even with 1,728 active engine instances.
2. **Both APIs work non-elevated and match Task Manager semantics.** Dedicated and shared memory matched to the MiB on the idle AMD iGPU and within ~0.3% on the actively shifting RTX 5090 workload, tracking `nvidia-smi` closely.
3. **DXGI adapter LUID correlation works cleanly.** `IDXGIFactory6::EnumAdapters1` with `DXGI_ADAPTER_FLAG_SOFTWARE` filtering cleanly excluded `Microsoft Basic Render Driver`, and `AdapterLuid` (`HighPart`, `LowPart`) matched PDH's `luid_0x{High:08X}_0x{Low:08X}` instance tokens on both NVIDIA and AMD adapters.
4. **CPU overhead stays well inside the v1 budget.** At the default `2000 ms` polling interval, a `3.05 ms` PDH collect consumes ~`0.15%` of one logical core.

## Decision

Use **DXGI (`IDXGIFactory6::EnumAdapters1`, excluding `DXGI_ADAPTER_FLAG_SOFTWARE`) + English WDDM PDH (`PdhAddEnglishCounterW`) with a persistent `PDH_HQUERY`** as the M4 GPU baseline collector:

1. Enumerate hardware adapters via DXGI at startup (and on adapter topology changes) to obtain adapter name, vendor/device ID, total dedicated/shared memory, and `AdapterLuid`.
2. Open one persistent `PDH_HQUERY` with three English wildcard counters:
   - `\GPU Engine(*)\Utilization Percentage`
   - `\GPU Adapter Memory(*)\Dedicated Usage`
   - `\GPU Adapter Memory(*)\Shared Usage`
3. Parse `luid_0x{High}_0x{Low}` from each counter instance name and join to DXGI adapters by LUID.
4. Compute adapter headline utilization using Task Manager's formula: sum utilization across processes for each `(luid, eng_N_engtype_X)` engine, clamp to `0..=100%`, and take the maximum across engines for each adapter.
5. Publish `DataState::WarmingUp` on the initial baseline `PdhCollectQueryData` call before reporting the first utilization delta.
6. Do not add `D3DKMTQueryStatistics` or hand-written 808-byte `d3dkmthk.h` unions to `btop-sys` for v1.

## Consequences

- **Simpler codebase:** Follows [AGENTS.md](../../AGENTS.md) Principle of Simplicity by relying solely on documented public Win32 APIs (`windows` / `windows-sys`) with zero reserved WDK union maintenance across future Windows 11 builds.
- **Predictable latency:** Sub-4 ms p95 collection across multi-GPU hosts under active load, well below D3DKMT's 22-25 ms p95 tail spikes.
- **No per-process GPU breakdown in baseline:** WDDM PDH instance names already contain `pid_<N>`, so per-process GPU attribution remains possible post-v1 without switching APIs if desired later.
