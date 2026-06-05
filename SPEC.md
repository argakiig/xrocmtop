# Spec: xrocmtop

A [`btop`](https://github.com/aristocratos/btop)-style terminal UI for monitoring AMD ROCm /
Vulkan GPUs, run entirely from the CLI. This is the single source of truth for what v1 is, how it
is built, and how we know it is done. It describes the **current, shipped** product.

---

## Objective

**What:** A single-binary, keyboard-driven terminal dashboard showing live AMD GPU state —
utilization, memory, temperature, power, and clocks — as gauges and scrolling history graphs,
alongside a per-process GPU table, a Vulkan device panel, and an SMU metrics panel (CPU / NPU /
unified-memory bandwidth, hotspot temperatures, and live throttle reasons).

**Who:** Developers and operators on AMD ROCm/Vulkan machines (workstations, APU laptops/mini-PCs,
compute boxes) who want an always-on monitor richer and friendlier than `watch rocm-smi`, and
hackable enough to bend toward the metrics they actually care about.

**Why:** AMD TUI tooling is thin — `rocm-smi` is a one-shot text dump and `nvtop` is NVIDIA-only.
xrocmtop fills the gap with a focused, dependency-light, read-only TUI that treats unified-memory
APUs (where "VRAM" is carved from system RAM, plus a separate GTT pool) as first-class.

**Success looks like:** launch `xrocmtop`, immediately see every local AMD GPU with live gauges and
graphs updating smoothly at ~1 Hz (configurable), a process table, and Vulkan info — no crashes
when a metric isn't supported, and negligible idle CPU overhead.

---

## Tech Stack

- **Language:** Rust (stable, edition 2021).
- **TUI:** `ratatui` + `crossterm` backend.
- **CLI args:** `clap` (derive).
- **Serialization:** `serde` + `serde_json` (parsing `rocm-smi --json` and `vulkaninfo --json`,
  and emitting `--once --json`).
- **Config:** `toml` (XDG-aware `config.toml`).
- **Errors:** `anyhow` (app), `thiserror` (collector error types).

**Data sources**, in priority order:
1. **amdgpu sysfs** — `/sys/class/drm/cardN/device/...` + its `hwmon` node. Primary,
   high-frequency, cheap (plain file reads, no fork).
2. **`gpu_metrics`** — the binary SMU telemetry node under the same `device/` dir. A versioned C
   struct (header-dispatched; v3_0 decoded for SMU13 APUs) carrying the APU-side signals hwmon does
   not expose: CPU power/clock/per-core residency, the NPU (XDNA/IPU), unified-memory bandwidth,
   hotspot temperatures, the STAPM limit, and throttle-residency counters. Read every tick.
3. **`rocm-smi --json`** — static identity (device name, VBIOS, IDs) and any metric not in sysfs.
   Polled at low frequency since forking is expensive. Used only if present.
4. **`vulkaninfo --json`** — Vulkan device panel (driver/API version, memory heaps). One-shot at
   startup. Used only if present.
5. **`/proc/<pid>/fdinfo`** — per-process amdgpu DRM accounting (memory pools + engine counters).

`ash`/live Vulkan bindings remain deliberately out of scope; v1 parses `vulkaninfo --json` to
avoid a heavy build/runtime dependency.

---

## Commands

```
Build (debug):    cargo build
Build (release):  cargo build --release
Run:              cargo run --                 # or ./target/release/xrocmtop
Test:             cargo test
Lint:             cargo clippy --all-targets --all-features -- -D warnings
Format:           cargo fmt --all
Format check:     cargo fmt --all --check
```

Runtime flags:

```
xrocmtop [OPTIONS]
  -i, --interval <MS>     Refresh interval in ms        [default: 1000]
      --gpu <INDEX>       Restrict to one GPU index
      --no-vulkan         Skip the Vulkan panel
      --no-procs          Skip per-process accounting
      --once              Print a single snapshot and exit (no TUI; scriptable)
      --json              With --once, emit JSON instead of text
  -h, --help / -V, --version
```

---

## Project Structure

```
Cargo.toml
SPEC.md                  → This document (single source of truth)
README.md                → User-facing usage
src/
  main.rs                → Entry: arg parse, terminal setup/teardown, run loop, panic-safe restore
  app.rs                 → App state: tick(), input handling, per-process engine sampler
  config.rs              → CLI args (clap) + resolved runtime config
  model.rs               → Data types: GpuSnapshot, MemInfo, Clocks, ProcInfo, ProcClient, etc.
  history.rs             → Fixed-capacity ring buffers for the time-series graphs
  settings.rs            → Persisted user settings (theme, color overrides, panel layout) as TOML
  theme.rs               → Theme presets + color resolution/overrides
  panel.rs               → Panel order / visibility / focus model
  report.rs              → --once snapshot, text and JSON (the public --json contract)
  collect/
    mod.rs               → Collector module wiring
    sysfs.rs             → amdgpu sysfs + hwmon reader (primary, high-frequency)
    gpu_metrics.rs       → Binary gpu_metrics node decoder (SMU telemetry; versioned C struct)
    smi.rs               → rocm-smi --json wrapper + parser (low-frequency/static identity)
    vulkan.rs            → vulkaninfo --json wrapper + parser (one-shot)
    process.rs           → /proc/<pid>/fdinfo amdgpu DRM accounting (per-process usage)
  ui/
    mod.rs               → Top-level render(): flow-grid layout, footer, help + detail overlays
    layout.rs            → Responsive layout (adapts to terminal size)
    gauges.rs            → Util / mem / temp / power gauges + bars
    graphs.rs            → Sparkline history graphs
    metrics.rs           → SMU metrics panel (power split, hotspot temps, clocks, throttle reasons)
    processes.rs         → Per-process GPU table (width-aware columns, row selection)
    proc_detail.rs       → Per-process detail popup (full cmdline, all pools + engines, clients)
    vulkan.rs            → Vulkan device panel
tests/
  once_contract.rs       → End-to-end --once / --json contract test (runs the real binary)
  fixtures/              → Captured real outputs from the probe machine
    sysfs/{drm,degraded} →   gpu_busy_percent, mem_info_*, hwmon/*, pp_dpm_* (+ a degraded tree)
    rocm-smi.json        →   real rocm-smi --json capture
    vulkaninfo.json      →   trimmed vulkaninfo --json capture
    fdinfo/              →   sample /proc/<pid>/fdinfo entries (incl. multi-engine, degraded)
    once.json            →   committed --json contract fixture
```

---

## Code Style

Idiomatic Rust, `rustfmt` defaults, `clippy -D warnings` clean. Collectors are **fallible and
total**: a missing sysfs field yields `None`, never a panic. Prefer small pure parser functions
over `&str`/`&Path` so they are trivially fixture-testable. Each UI panel is a **pure function of
its inputs** that never performs I/O and never panics — missing values render as `n/a`.

```rust
/// A metric that may be unsupported on a given device. Renders as "n/a" when None.
pub type Opt<T> = Option<T>;

/// Read a single integer-valued sysfs file, returning None if absent/unreadable/unparsable.
fn read_u64(path: &Path) -> Opt<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Memory is modeled as distinct pools — unified-memory APUs are first-class.
#[derive(Debug, Clone, Default)]
pub struct MemInfo {
    pub vram_total: Opt<u64>, // bytes; on APUs this is carved from system RAM
    pub vram_used:  Opt<u64>,
    pub gtt_total:  Opt<u64>,
    pub gtt_used:   Opt<u64>,
}
```

Conventions: snake_case fns/fields, PascalCase types; bytes stored as `u64` and formatted at the
UI edge (GiB) — never pre-divided in the model; no `unwrap()`/`expect()` in collector or UI paths
(reserve those for `main` setup and tests).

---

## Features

### GPU gauges & history
Per GPU: utilization, VRAM and GTT (used / total / %), edge temperature, package power, and
sclk/mclk/fclk/socclk clocks as live gauges; rolling sparkline history for utilization, power, and
temperature. Each metric is a text column beside a pure-fill bar so labels never overlap the fill.
Multi-GPU systems stack all cards inside the single Gauges/Graphs panel cell, keeping the panel
model independent of GPU count.

### SMU metrics panel
Per GPU, telemetry decoded from the binary `gpu_metrics` node. Deliberately scoped to what the
GPU-centric Gauges/Graphs panels *structurally cannot* show — the rest of the APU sharing the
socket — rather than re-printing GPU util/clocks/total-power: **CPU** power, peak core clock, and a
busy-core count derived from per-core C0 residency; the **NPU** (XDNA/IPU) activity and power;
**unified-memory bandwidth** (DRAM read/write); GFX/SoC **hotspot temperatures** (the
throttle-relevant sensors hwmon omits, distinct from the edge temp in Gauges); the **STAPM**
sustained-power limit; and **which limits are actively throttling** — derived in-`App` by diffing
each tick's throttle-residency counters (PROCHOT / SPL / FPPT / SPPT / per-domain thermal), so a
source reads as active only while its counter advances. The decoder is header-dispatched on the
struct's revision (v3_0 today) and validates layout against a committed binary fixture; unsupported
revisions and absent fields render `n/a`. Decoded values also appear in the `--once --json` output.

Below the per-GPU metrics the panel carries a scrollable **Thermal events** log: each time a
throttle source starts, an episode opens with a plain-English reason ("GPU too hot", "Power limit
(sustained)", …) and a relative start time; when it stops, the row stamps a duration ("lasted 6s")
— still-active episodes read "ongoing". The log is session-only (an in-memory ring buffer, never
written to disk and absent from `--once --json`) and scrolls with `↑`/`↓`/`j`/`k` and `PgUp`/`PgDn`
while the Metrics panel is focused.

### Per-process table & detail
One row per process holding an amdgpu DRM handle, de-duplicated per `(pid, drm-client-id)`.
Columns are width-aware (`PID · Process · VRAM · GTT · GFX · COM`, dropping GTT → compute →
graphics as the panel narrows). Sortable by memory / pid / name. Per-engine utilization is derived
by an in-`App` sampler that diffs the cumulative `drm-engine-*` counters between process walks over
the wall-clock interval; a process reads `n/a` until it has been seen twice. Select a row (`↑`/`↓`)
and press `Enter` for a detail popup with the full command line, every memory pool and all four
engines (graphics/compute/encode/decode), and a per-`drm-client-id` breakdown. Processes that
can't be inspected without elevation are summarized as "+N hidden".

### Vulkan panel
Device name, driver, API version, and device-local memory heaps from `vulkaninfo --json`.

### Customization & persistence
Built-in themes (`default`, `high-contrast`, `mono`) cycled at runtime, plus a
`$XDG_CONFIG_HOME/xrocmtop/config.toml` (falling back to `~/.config/xrocmtop/`) that can override
any element color (named like `green` or hex like `#ff8800`). The five panels can be toggled and
reordered at runtime; `Tab` cycles focus, move keys reposition the focused panel in the flow grid,
number keys toggle visibility, and the focused panel is highlighted. Theme choice, color
overrides, and panel order/visibility auto-save on exit and reload at startup; `--no-procs` /
`--no-vulkan` seed initial visibility.

### Scriptable snapshot
`xrocmtop --once` prints a single text snapshot; `--once --json` emits a parseable JSON document
whose field names are the public contract (asserted by a committed fixture + the integration test).

### Refresh model
Single-threaded loop with staggered per-source cadence: sysfs every tick (cheap, authoritative);
`rocm-smi` every 5 ticks (forking is expensive — fills static identity); the `/proc` walk every 3
ticks (O(pids×fds), with engine % averaged over that window); `vulkaninfo` once at startup. The
terminal is restored via a panic hook + RAII guard so a crash never leaves a broken terminal.

---

## Testing Strategy

- **Framework:** built-in `cargo test`; no extra runner.
- **Where:** unit tests inline (`#[cfg(test)]`) next to the code they cover (parsers, sampler, UI
  panels). The `--once`/`--json` contract is an integration test in `tests/once_contract.rs` that
  runs the real binary. Real captures from the probe machine live in `tests/fixtures/`.
- **What gets tested:**
  - Parsers are the core risk → heavy fixture coverage on `collect/*` and `model.rs`, including
    *degraded* inputs (empty `mem_busy_percent`, "Not supported", missing files, missing engine
    lines).
  - The engine-utilization sampler: synthetic two-sample diffs with injected wall-clock deltas
    (ratio, clamp to 0..=100, first-sight/counter-reset → `None`).
  - UI panels via ratatui's `TestBackend`: layout-doesn't-panic, key cells render, width-aware
    column drop, selected-row highlight, detail-popup contents.
  - `--once --json` asserted against `tests/fixtures/once.json` for a stable, scriptable contract.

---

## Boundaries

**Always:**
- Run `cargo fmt --all` + `cargo clippy --all-targets -- -D warnings` + `cargo test` before any commit.
- Treat every metric as optional; render `n/a` and keep going when a source is absent/unsupported.
- Read-only access to `/sys` and `/proc`. Capture a real fixture when adding any new parser.

**Ask first:**
- Adding a dependency beyond the listed set (esp. `ash` / Vulkan loader, async runtimes).
- Changing the refresh architecture (threads vs. single-loop, per-source cadence).
- Adding a non-AMD vendor backend or any networked/remote feature.
- Changing the `--json` output beyond additive fields.

**Never:**
- Write to sysfs or otherwise *control* the GPU (no clock/power/fan changes, no signalling/killing
  processes). This tool only reads.
- Require root for core operation, or panic on a missing/unsupported metric.
- Remove or skip failing tests to go green; commit secrets/credentials.

---

## Success Criteria

1. `cargo build --release` produces a single self-contained binary; `cargo clippy -D warnings`
   and `cargo test` pass clean.
2. On an AMD box, every GPU shows live, smoothly-updating gauges for **utilization, VRAM, GTT,
   temperature, power, and clocks** at the configured interval, with scrolling **history graphs**
   for utilization, power, and temperature.
3. The **process table** lists amdgpu-DRM processes with split **VRAM/GTT** memory and live
   **GFX/compute** utilization, sortable and selectable; `Enter` opens a **detail popup** with the
   full command line, all memory pools, all four engines, and a per-client breakdown.
4. The **Vulkan panel** shows device name, driver, API version, and memory heaps.
5. The **SMU metrics panel** decodes the `gpu_metrics` node where present: CPU power/clock/busy
   cores, the NPU, unified-memory bandwidth, hotspot temperatures, the STAPM limit, and live
   throttle reasons (covered by a committed binary fixture).
6. **Graceful degradation:** unsupported metrics render as `n/a` and never panic (covered by a
   degraded-input fixture test).
7. **Customization persists:** theme, color overrides, and panel layout survive a restart via the
   XDG config file.
8. `xrocmtop --once --json` prints a parseable snapshot and exits 0 (contract test).
9. `q` / `Ctrl-C` exit cleanly and fully restore the terminal; a panic also restores it.
10. Idle CPU overhead is low (well under one core's worth at 1 Hz).

---

## Risks & Mitigations

| Risk | Mitigation |
|---|---|
| sysfs field names/units vary across amdgpu versions & cards | Every field is `Opt`; capture real fixtures; unit-test degraded inputs. Never assume a field exists. |
| `fdinfo` accounting format differs / lacks engine time on some kernels | Parse defensively; show whatever is available (memory at least); engine % is `n/a` rather than blocking the row. |
| Forking `rocm-smi` / `vulkaninfo` adds latency or they aren't installed | Run off the hot path (low cadence / once); feature-detect and degrade gracefully when the binary is missing. |
| Terminal left broken on panic | Panic hook tears down raw mode + alt-screen; RAII guard around terminal setup. |
| Unified-memory APU semantics confuse "VRAM%" | Model VRAM and GTT as distinct pools throughout; label clearly. |
| Engine % skewed by the 3-tick averaging window or a process's client set changing mid-window | Documented as an average; counter resets / first sight yield `n/a`; self-corrects on the next walk. |

---

## Out of Scope

Remote/fleet monitoring, non-AMD vendors, any GPU control/writes, killing or signalling processes,
OS packaging, and `ash`/live-Vulkan. Each is a possible future direction, not part of v1.
