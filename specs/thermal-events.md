# Spec: Thermal / Throttle Events Log

A scrollable, plain-English history of throttling episodes, shown as a section inside the existing
**SMU Metrics** panel. **Status: shipped** (branch `feat/thermal-events`). Timestamps are relative
(`2m ago`, `lasted 6s`) — no new dependencies; absolute clock remains Open Question #1.

---

## Objective

**What:** Record every throttling episode the hardware reports and show it as a human-readable,
scrollable list inside the Metrics panel. Each row answers two questions at a glance: *when* it
happened and *what was throttled*, in everyday English (not raw counter names).

**Who:** APU/GPU users watching `xrocmtop` who notice a performance dip and want to know "did it
throttle, and why?" without decoding `THM_GFX` vs `SPPT`.

**Why:** The hardware already exposes per-source throttle residency counters, and the app already
diffs them each tick into `throttle_active` (`src/app.rs:295`). But that signal is **recomputed and
discarded every ~1 s** — there is no history. The moment a throttle ends, the evidence is gone. This
feature persists those transitions into a browsable log for the session.

**Success looks like:** put load on the machine until it throttles; a new row appears in the Metrics
panel's "Thermal events" section reading e.g. `2m ago  GPU too hot  (lasted 6s)`. The list scrolls
with `j`/`k` when the Metrics panel is focused, never floods, never panics when metrics are absent,
and adds zero new runtime dependencies.

### Decisions (confirmed)

| Decision | Choice |
|---|---|
| Sources counted | **All** throttle sources: thermal (CPU/GFX/SoC), power limits (SPL/FPPT/SPPT), and PROCHOT |
| Granularity | **One row per episode** — open on start, stamp a duration on end |
| Persistence | **In-memory only** — fixed-size ring buffer, cleared on exit |
| Placement | **Section inside the Metrics panel** (not a standalone panel) |
| Timestamps | **Relative** (`2m ago`, `lasted 6s`) — zero new deps. Absolute clock is an open question. |

---

## Tech Stack

No change. Rust 2021, `ratatui` 0.29 + `crossterm`, no new crates. Time math uses
`std::time::Instant` deltas only (the app already holds `Instant` values, e.g.
`last_proc_walk` in `src/app.rs`). Wall-clock formatting is deliberately avoided — see Open
Questions.

---

## Commands

```
Build:   cargo build
Run:     cargo run                 # or: cargo run -- --interval 500
Test:    cargo test
Lint:    cargo clippy --all-targets -- -D warnings
Format:  cargo fmt
One-shot:cargo run -- --once --json   # contract test surface; see tests/once_contract.rs
```

---

## Project Structure

Files this feature touches (all existing except where noted):

```
src/model.rs            → add ThrottleSource enum + ThermalEvent struct + label() mapping
src/app.rs              → own the event log (ring buffer); detect episodes in tick(); scroll state + keys
src/ui/metrics.rs       → render the "Thermal events" section + scroll inside the Metrics panel
src/ui/mod.rs           → pass event log + scroll offset into the metrics renderer
specs/thermal-events.md → this spec (new dir: specs/)
```

No new module is required, consistent with the "section in Metrics panel" decision.

---

## Code Style

Match the existing house style: pure render functions returning `Vec<Line>`, `Option`-tolerant
("n/a" / graceful empty), doc comments explaining *why*, unit tests via `ratatui::TestBackend`.

Source→English mapping lives next to the data model as a method on a small enum, mirroring how
`Throttle::active_since` already returns the `&'static str` source names (`src/model.rs:143`):

```rust
/// One hardware throttle source, with a plain-English label for the events log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleSource {
    Prochot, Spl, Fppt, Sppt, ThmCore, ThmGfx, ThmSoc,
}

impl ThrottleSource {
    /// Short, everyday-English description of what was being limited.
    pub fn label(self) -> &'static str {
        match self {
            ThrottleSource::Prochot => "External overheat signal",
            ThrottleSource::Spl     => "Socket power limit",
            ThrottleSource::Fppt    => "Power limit (fast burst)",
            ThrottleSource::Sppt    => "Power limit (sustained)",
            ThrottleSource::ThmCore => "CPU cores too hot",
            ThrottleSource::ThmGfx  => "GPU too hot",
            ThrottleSource::ThmSoc  => "SoC too hot",
        }
    }

    /// The raw counter name as reported by `gpu_metrics` (matches `Throttle::active_since`).
    pub fn code(self) -> &'static str { /* "PROCHOT", "SPL", ... */ }
}
```

```rust
/// One throttling episode for a single source on a single GPU. An episode opens when the source
/// starts advancing and closes when it stops; `ended` is None while still active.
#[derive(Debug, Clone)]
pub struct ThermalEvent {
    pub gpu_index: usize,
    pub source: ThrottleSource,
    pub started: Instant,
    pub ended: Option<Instant>,
}
```

Example rendered rows (relative time; "GPU 0" prefix only shown when >1 GPU present):

```
 Thermal events ─────────────────────────────
 just now   GPU too hot              ongoing
 2m ago     Power limit (sustained)  lasted 6s
 5m ago     CPU cores too hot        lasted 1s
 ↓ 12 older
```

---

## Behavior / Design Detail

**Episode detection** (extends `App::derive_throttle`, `src/app.rs:295`). Per GPU, keep the set of
currently-open sources. Each tick, after computing the active set (the existing
`Throttle::active_since` diff):

- source **newly active** (active now, no open episode) → push a new `ThermalEvent { started: now, ended: None }` to the ring buffer and mark it open.
- source **no longer active** (was open, not active now) → set that episode's `ended = Some(now)`.

Granularity is tick-bounded (~1 s); a one-tick blip renders as `lasted <1s`.

**Storage:** a single `VecDeque<ThermalEvent>` ring buffer on `App`, capacity `THERMAL_EVENT_CAP`
(propose **200**). When full, drop the oldest. Events are stored append-order; displayed
**newest-first**, merged across GPUs into one chronological list.

**Rendering** (`src/ui/metrics.rs`): split the Metrics panel's inner area — existing per-GPU metric
lines on top, a "Thermal events" list region at the bottom (propose a `Constraint::Min`/`Length`
split leaving the metrics their current space and giving events the remainder, min ~4 rows). Render
newest-first, applying `events_scroll` offset. Show a `↑ N newer` / `↓ N older` hint (or a ratatui
`Scrollbar`) when the list overflows. Empty state: `No throttling recorded this session.`

**Scrolling / keys** (`src/app.rs` `on_key`): when the Metrics panel is focused, `j`/`Down` and
`k`/`Up` adjust `events_scroll`; `PageDown`/`PageUp` jump by the visible height; clamp to
`[0, len - visible]`. This reuses the focus-gating pattern already used for the Processes panel
(`proc_panel_focused()`, `src/app.rs:439`). Reserve a `metrics_panel_focused()` helper to gate it.
Tradeoff (accepted): because events live in the Metrics panel rather than their own, scrolling is
available only while Metrics is focused.

---

## Testing Strategy

Framework: built-in `cargo test`; UI assertions via `ratatui::backend::TestBackend` + `Buffer`
(same approach as `src/ui/processes.rs` tests).

- **Unit — model:** `ThrottleSource::label()`/`code()` cover all 7 variants; `ThermalEvent`
  duration formatting (`<1s`, `6s`, `2m ago`, `ongoing`).
- **Unit — episode logic:** drive a synthetic sequence of `Throttle` samples through the detection
  helper and assert: episode opens on first advance, stays open while advancing, closes when it
  stops; multiple concurrent sources tracked independently; ring buffer drops oldest past cap.
- **Unit — render:** empty state renders the placeholder; a list longer than the area shows the
  overflow hint and the correct slice at a given scroll offset; `>1 GPU` adds the `GPU n` prefix;
  missing metrics never panic.
- **Unit — keys:** `j`/`k` move `events_scroll` only when Metrics focused and clamp at both ends;
  ignored when another panel is focused.
- **Contract:** confirm `tests/once_contract.rs` / `--once --json` output is unchanged (events are
  session-only and not part of the snapshot contract) — or, if we choose to expose them, add an
  explicit field and update the contract test deliberately.

Coverage expectation: every new public function and the episode state machine have a test;
no `unwrap`/`panic` on absent data.

---

## Boundaries

- **Always:** `cargo fmt` + `cargo clippy -D warnings` + `cargo test` green before commit; keep
  render functions pure and `Option`-tolerant; preserve existing `--once --json` contract.
- **Ask first:** adding any crate (e.g. `chrono`/`time` for wall-clock timestamps); changing the
  `--once --json` schema; changing default keybindings beyond the Metrics-focused scroll keys;
  raising `THERMAL_EVENT_CAP` enough to matter for memory.
- **Never:** write event data to disk (in-memory-only is a confirmed decision); introduce a
  background thread or blocking I/O in the render path; remove or weaken existing panel tests.

---

## Success Criteria

1. Under sustained load that triggers throttling, a new row appears in the Metrics panel's
   "Thermal events" section within ~2 ticks, with a plain-English label and a relative time.
2. An ongoing throttle shows `ongoing`; once it stops, the same row shows `lasted Ns`.
3. All 7 throttle sources map to distinct, non-cryptic English labels.
4. With more events than fit, `j`/`k` (Metrics focused) scroll the list and an overflow hint shows
   the count above/below; scroll clamps without panic at both ends.
5. On hardware that reports no `gpu_metrics` throttle data, the section shows the empty-state
   message and nothing panics.
6. Ring buffer never grows beyond `THERMAL_EVENT_CAP`; no disk writes; no new crates.
7. `cargo fmt`, `cargo clippy -D warnings`, and `cargo test` all pass; `--once --json` contract
   unchanged (unless deliberately extended with a test update).

---

## Open Questions

1. **Absolute timestamps?** Relative time (`2m ago`) needs zero deps and is the default. Showing
   real clock time (`14:02:31`) for correlating with logs/workloads needs a `time`/`chrono`
   dependency — an *ask-first* boundary. Want absolute time, and if so, accept the dependency?
2. **`THERMAL_EVENT_CAP` value** — 200 proposed. Higher keeps more history at trivial memory cost.
3. **Episode close on shrink** — if the metrics blob disappears mid-episode (source becomes `None`),
   should we close open episodes as `ended` at that tick, or leave them `ongoing`? Proposed: close.
4. **Expose in `--once --json`?** Default no (session-only). Confirm we don't want events in the
   machine-readable snapshot.
```

