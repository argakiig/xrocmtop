# Spec: Expanded NPU (XDNA/IPU) Stats

Enrich the NPU readout in the **SMU Metrics** panel from two fields (activity %, power) to a full
block — adding **NPU clock** and **NPU read/write memory bandwidth** — and add an **NPU activity
sparkline** to the history graphs. **Status: implemented** (working tree, not yet committed). No new
dependencies; all new data already arrives in the `gpu_metrics` blob we parse today and was
previously discarded.

---

## Objective

**What:** Surface the NPU telemetry the SMU already reports but `xrocmtop` throws away. Today the
NPU row shows only `activity %` and `power (W)` (`src/ui/metrics.rs:217`). Add **NPU clock (MHz)**
and **NPU memory bandwidth (read/write, MB/s)**, promote the NPU into its own labeled multi-row
block in the Metrics panel, and feed NPU activity into the rolling history graphs as a sparkline so
its utilization trend is visible over time.

**Who:** Owners of AMD Ryzen AI APUs (XDNA/XDNA2 — e.g. Strix Halo / "RyzenAI-npu5") running local
inference who want to see the NPU actually doing work: is it spun up, how hard, how fast, and how
much memory traffic it is driving — alongside the CPU and GPU it shares the socket with.

**Why:** The data is *free*. `gpu_metrics_v3_0` carries `ipuclk`, `average_ipu_reads`, and
`average_ipu_writes`, all of which the parser already walks past:
`skip_u16(&mut p, 2)` discards the NPU bandwidth pair (`src/collect/gpu_metrics.rs:73`) and `ipuclk`
is the 4th element of the 8-clock skip (`src/collect/gpu_metrics.rs:91`). No new I/O, no new file,
no new crate — only fields read instead of skipped, plus model/UI plumbing.

**Success looks like:** run a local NPU workload (e.g. an ONNX/Ryzen-AI inference job); the Metrics
panel's NPU block shows non-zero activity, a clock in MHz, and read/write bandwidth, and the NPU
sparkline rises with the load and falls when it ends. With the NPU idle, every field degrades to
`n/a`/0 and nothing panics. `--once --json` gains the new fields without breaking existing keys.

### Decisions (confirmed)

| Decision | Choice |
|---|---|
| Scope of new data | **NPU clock + NPU read/write bandwidth** (plus existing activity % and power). *Not* static identity (fw/model), *not* per-column activity breakdown. |
| Data source | **`gpu_metrics` SMU node only** — the fields are already parsed-then-skipped. No new collector, no `amdxdna` sysfs read. |
| Presentation | **All three:** promote the NPU into its own labeled block in the Metrics panel (rows merged), **and** add an NPU activity sparkline to the history graphs — **shown only when an NPU is present**. |
| Idle clock | `ipuclk = 0` renders as **`0 MHz`** (NPU present but idle); only the `0xFFFF` sentinel → `n/a`. |
| Activity semantic | Keep current **peak across `average_ipu_activity[8]`** (the existing `npu_activity_pct`). Per-column detail is explicitly out of scope. |

---

## Tech Stack

No change. Rust 2021, `ratatui` 0.30 + `crossterm` 0.29, no new crates. New numeric fields reuse the
existing `Opt<T>` / sentinel-handling helpers in `src/collect/gpu_metrics.rs` (`present`, `nz`,
`mhz`). The sparkline reuses the existing `History<f64>` buffer and graph widgets.

---

## Commands

```
Build:   cargo build
Run:     cargo run                 # or: cargo run -- --interval 500
Test:    cargo test
Lint:    cargo clippy --all-targets -- -D warnings
Format:  cargo fmt
One-shot:cargo run -- --once --json   # contract surface; see tests/once_contract.rs
```

---

## Project Structure

Files this feature touches (all existing; no new modules):

```
src/model.rs              → add npu_clk_mhz / npu_read_mbps / npu_write_mbps to `Metrics`
src/collect/gpu_metrics.rs→ read ipuclk + average_ipu_reads/writes instead of skipping; map via helpers
src/ui/metrics.rs         → render the NPU as its own multi-row labeled block (clock + R/W rows)
src/history.rs            → add `npu_util: History<f64>` to `GpuHistory`
src/app.rs                → push NPU activity into history each tick
src/ui/graphs.rs          → render the NPU activity sparkline
tests/fixtures/gpu_metrics_v3_0.bin → existing fixture; expected NPU values added to the decode test
specs/npu-stats.md        → this spec
```

---

## Code Style

Match the house style: pure render functions returning `Vec<Line>`, every metric an `Opt<T>` that
renders as `n/a`, doc comments explaining *why*, unit tests via `ratatui::TestBackend`. New fields
follow the existing `Metrics` field doc convention (`src/model.rs:91`):

```rust
// in struct Metrics
/// NPU (XDNA/IPU) clock in MHz. From `ipuclk`; `0` is a valid "idle" reading (shown as
/// `0 MHz`), only the `0xFFFF` sentinel renders as n/a.
pub npu_clk_mhz: Opt<u16>,
/// NPU (XDNA/IPU) memory read bandwidth in MB/s. From `average_ipu_reads`.
pub npu_read_mbps: Opt<u16>,
/// NPU (XDNA/IPU) memory write bandwidth in MB/s. From `average_ipu_writes`.
pub npu_write_mbps: Opt<u16>,
```

Decode by reading the fields the parser already steps over, reusing the existing interpretation
helpers — no new sentinel logic:

```rust
// src/collect/gpu_metrics.rs — was: skip_u16(&mut p, 2);
let ipu_reads  = rd_u16(b, &mut p)?;   // average_ipu_reads
let ipu_writes = rd_u16(b, &mut p)?;   // average_ipu_writes

// ipuclk is element [3] of the 8-entry average-clock block (was a single skip_u16(.., 8)):
// gfxclk, socclk, vpeclk, ipuclk, fclk, vclk, uclk, mpipu
let _gfxclk = rd_u16(b, &mut p)?;
let _socclk = rd_u16(b, &mut p)?;
let _vpeclk = rd_u16(b, &mut p)?;
let ipuclk  = rd_u16(b, &mut p)?;
skip_u16(&mut p, 4); // fclk, vclk, uclk, mpipu

// ... in the returned Metrics:
npu_clk_mhz:   present(ipuclk),   // 0 → "0 MHz" (idle); only 0xFFFF → n/a
npu_read_mbps: present(ipu_reads),
npu_write_mbps:present(ipu_writes),
```

Rendered NPU block (own label, mirroring the existing CPU/Memory blocks at `src/ui/metrics.rs:207`):

```
 NPU    12%   3.2W
        1400MHz
        R 820  W 410 MB/s
```

(idle → `NPU  0%  n/a  0MHz  R 0  W 0 MB/s` — power is n/a when 0, clock shows 0 MHz)

---

## Behavior / Design Detail

**Decode** (`src/collect/gpu_metrics.rs::parse_v3_0`): replace the two skips noted above with real
reads. The struct offsets are unchanged — same total bytes consumed — so the existing fixture and
the truncation/alignment tests still hold. Activity (peak) and power are untouched.

**Model** (`src/model.rs`): three new `Opt<u16>` fields on `Metrics`. As with every other metric,
`0xFFFF` → `n/a` via `present`/`mhz`; bandwidth `0` is a *valid* reading (kept), clock `0` is `n/a`
(an NPU reporting a clock is running).

**Metrics panel** (`src/ui/metrics.rs`): expand the single NPU `labeled("NPU", ...)` row into a
block with activity+power on the first line and clock + R/W bandwidth on following pair lines,
reusing the existing `pair`/`labeled`/`fmt_mhz`/`fmt_bw`/`fmt_watt`/`fmt_pct` helpers. No new
formatter needed (`fmt_bw` already serves the Memory R/W rows at `src/ui/metrics.rs:227`).

**History + sparkline:** add `npu_util: History<f64>` to `GpuHistory` (`src/history.rs:74`),
initialized in `GpuHistory::new` with the same window capacity as `util`/`power`/`temp`. In
`App::tick` (wherever `util`/`power`/`temp` are pushed), push `npu_activity_pct as f64` (skip the
push when `None`, consistent with how other absent samples are handled). In `src/ui/graphs.rs`,
render the NPU series as an additional sparkline **only when the NPU is present** — gated on the GPU
ever having reported a non-`None` `npu_activity_pct` (i.e. `npu_util` is non-empty). On a part with
no NPU the series stays empty and the sparkline is omitted entirely, so the graphs region keeps its
current layout. When shown, the NPU sparkline is fixed to a 0..=100 scale like `util`.

**`--once --json`:** the three new fields serialize automatically (`Metrics` is `Serialize`). This
is an *additive* schema change — existing keys are untouched — but it still changes the contract
surface, so `tests/once_contract.rs` is updated deliberately (see Boundaries).

---

## Testing Strategy

Framework: built-in `cargo test`; UI assertions via `ratatui::backend::TestBackend` + `Buffer`.

- **Unit — decode:** extend `parses_real_v3_0_dump` (`src/collect/gpu_metrics.rs:225`) with the
  expected `npu_clk_mhz`, `npu_read_mbps`, `npu_write_mbps` for the committed Strix Halo fixture.
  Confirm `truncated_blob_is_none_not_panic` and `unsupported_revision_is_none_not_panic` still pass
  (offsets unchanged).
- **Unit — sentinels:** `ipuclk = 0` and `0xFFFF` → `None`; bandwidth `0xFFFF` → `None`, `0` → `Some(0)`.
- **Unit — render:** the NPU block shows all four signals when present; renders `n/a` for each absent
  field; never panics when `metrics` is `None` (existing `gpu_metrics: unavailable` path).
- **Unit — history:** `GpuHistory::new` initializes `npu_util`; pushing samples evicts oldest at
  capacity (reuse the existing `History` test pattern).
- **Unit — graphs:** the NPU sparkline renders on a 0..=100 scale and handles an empty series.
- **Contract:** update `tests/once_contract.rs` to assert the three new keys are present and typed,
  and that all *previously asserted* keys are unchanged (additive-only).

Coverage expectation: every new field has decode + render coverage; no `unwrap`/`panic` on absent
data; `cargo clippy -D warnings` clean.

---

## Boundaries

- **Always:** `cargo fmt` + `cargo clippy -D warnings` + `cargo test` green before commit; keep
  render functions pure and `Opt`-tolerant; reuse existing formatters/helpers; keep the
  `gpu_metrics` parser's byte offsets exact (verify against the fixture).
- **Ask first:** adding any crate; changing the `--once --json` schema *beyond* the three additive
  NPU fields; reading `amdxdna` sysfs (out of scope — would be a new collector); widening scope to
  static identity or per-column activity.
- **Never:** write device state; introduce blocking I/O or a thread in the render path; remove or
  weaken existing decode/panel/contract tests; reorder or drop existing JSON keys.

---

## Success Criteria

1. Under a local NPU workload, the Metrics panel's NPU block shows non-zero activity, a clock in
   MHz, and read/write bandwidth in MB/s; the NPU sparkline rises with load and falls when it ends.
2. With the NPU idle (the committed fixture's state), activity reads `0%`, power/clock read `n/a`,
   bandwidth reads `0`, and nothing panics.
3. The `gpu_metrics` decode test asserts the fixture's exact `ipuclk` / `ipu_reads` / `ipu_writes`
   values, and all pre-existing decode assertions still pass (offsets intact).
4. `--once --json` includes `npu_clk_mhz`, `npu_read_mbps`, `npu_write_mbps`; no existing key
   changed; `tests/once_contract.rs` updated to match.
5. `cargo fmt`, `cargo clippy -D warnings`, and `cargo test` all pass.
6. No new crates; no new files except this spec.

---

## Open Questions

1. ~~**Sparkline placement**~~ — **Resolved:** NPU sparkline shown **only when an NPU is present**
   (gated on `npu_util` being non-empty); omitted entirely on NPU-less parts.
2. **Bandwidth unit at the UI edge** — `fmt_bw` currently formats the DRAM R/W in the Memory row;
   confirm reusing it verbatim for NPU R/W (same MB/s → human units) reads well, or whether NPU
   traffic wants its own scale.
3. **`npu_util` history capacity** — reuse the same window as `util`/`power`/`temp` (proposed), or a
   different retention?
4. ~~**Idle clock semantics**~~ — **Resolved:** `ipuclk = 0` renders as **`0 MHz`**; only `0xFFFF`
   → `n/a` (use `present`, not `mhz`).
5. **Multi-revision** — only `gpu_metrics_v3_0` is decoded today; other APU revisions still render
   the whole Metrics block as `n/a`. Out of scope here; noted for completeness.
