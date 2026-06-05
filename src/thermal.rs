//! Session-long log of throttling episodes, the data behind the Metrics panel's "Thermal events"
//! section.
//!
//! The hardware reports cumulative per-source throttle-residency counters; [`crate::app::App`]
//! diffs consecutive samples into the *set of sources active this tick* and feeds that set here via
//! [`ThermalLog::observe`]. This type turns that per-tick signal into discrete **episodes**: a
//! source that becomes active opens an episode; when it stops being active the episode closes and
//! gets a duration. Open episodes render as "ongoing".
//!
//! Storage is in-memory only (never written to disk) and bounded: closed episodes live in a
//! fixed-capacity ring buffer that drops the oldest past [`THERMAL_EVENT_CAP`]. Open episodes are
//! held separately, keyed by `(gpu_index, source)`, and are naturally bounded by the small number
//! of sources per card.

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use crate::model::{ThermalEvent, ThrottleSource};

/// Maximum number of *closed* episodes retained. Open episodes are bounded separately by the
/// source count, so the total never exceeds this by more than a handful.
pub const THERMAL_EVENT_CAP: usize = 200;

/// Per-session throttle-episode history. See the module docs for the open/closed split.
#[derive(Debug, Default)]
pub struct ThermalLog {
    /// Finished episodes, oldest at the front; capped at [`THERMAL_EVENT_CAP`].
    closed: VecDeque<ThermalEvent>,
    /// Currently-active episodes, one per `(gpu_index, source)` still throttling.
    open: BTreeMap<(usize, ThrottleSource), ThermalEvent>,
}

impl ThermalLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one tick's observation for a single GPU into the log. `active` is the set of sources
    /// whose residency advanced this tick (empty when nothing throttled, or when the card reports
    /// no metrics at all — which correctly closes any episodes still open for it).
    ///
    /// `now` is injected rather than read from the clock so the episode logic is deterministically
    /// testable.
    pub fn observe(&mut self, gpu_index: usize, active: &[ThrottleSource], now: Instant) {
        // Open an episode for each newly-active source; an already-open one keeps its start time.
        for &src in active {
            self.open
                .entry((gpu_index, src))
                .or_insert_with(|| ThermalEvent {
                    gpu_index,
                    source: src,
                    started: now,
                    ended: None,
                });
        }
        // Close episodes for this GPU whose source is no longer active.
        let stale: Vec<(usize, ThrottleSource)> = self
            .open
            .keys()
            .copied()
            .filter(|(g, s)| *g == gpu_index && !active.contains(s))
            .collect();
        for key in stale {
            if let Some(mut ev) = self.open.remove(&key) {
                ev.ended = Some(now);
                self.closed.push_back(ev);
                while self.closed.len() > THERMAL_EVENT_CAP {
                    self.closed.pop_front();
                }
            }
        }
    }

    /// All episodes newest-first (by start time), merging still-open and closed. Ties (same-tick
    /// starts) break by source order for a stable display.
    pub fn newest_first(&self) -> Vec<&ThermalEvent> {
        let mut all: Vec<&ThermalEvent> = self.open.values().chain(self.closed.iter()).collect();
        all.sort_by(|a, b| {
            b.started
                .cmp(&a.started)
                .then(a.gpu_index.cmp(&b.gpu_index))
                .then(a.source.cmp(&b.source))
        });
        all
    }

    /// Total episodes currently held (open + closed). Part of the type's natural interface and
    /// exercised by the unit tests; the UI counts the slice from [`ThermalLog::newest_first`].
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.open.len() + self.closed.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn opens_stays_open_then_closes_with_duration() {
        let t0 = Instant::now();
        let mut log = ThermalLog::new();

        // Tick 1: GFX starts throttling → one ongoing episode.
        log.observe(0, &[ThrottleSource::ThmGfx], at(t0, 0));
        assert_eq!(log.len(), 1);
        assert_eq!(log.newest_first()[0].duration(), None, "ongoing");

        // Tick 2: still active → same episode, no duplicate.
        log.observe(0, &[ThrottleSource::ThmGfx], at(t0, 1));
        assert_eq!(log.len(), 1);

        // Tick 3: no longer active → closes with a 2s duration.
        log.observe(0, &[], at(t0, 2));
        assert_eq!(log.len(), 1);
        assert_eq!(
            log.newest_first()[0].duration(),
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn tracks_concurrent_sources_independently() {
        let t0 = Instant::now();
        let mut log = ThermalLog::new();
        log.observe(
            0,
            &[ThrottleSource::ThmGfx, ThrottleSource::Sppt],
            at(t0, 0),
        );
        assert_eq!(log.len(), 2);
        // SPPT stops but GFX continues: GFX stays open, SPPT closes.
        log.observe(0, &[ThrottleSource::ThmGfx], at(t0, 1));
        assert_eq!(log.len(), 2);
        let open: Vec<_> = log
            .newest_first()
            .into_iter()
            .filter(|e| e.ended.is_none())
            .map(|e| e.source)
            .collect();
        assert_eq!(open, vec![ThrottleSource::ThmGfx]);
    }

    #[test]
    fn missing_metrics_close_open_episodes() {
        let t0 = Instant::now();
        let mut log = ThermalLog::new();
        log.observe(0, &[ThrottleSource::ThmCore], at(t0, 0));
        // A tick with no active sources (e.g. the metrics blob vanished) closes the episode.
        log.observe(0, &[], at(t0, 1));
        assert!(log.newest_first()[0].ended.is_some());
    }

    #[test]
    fn separate_gpus_do_not_interfere() {
        let t0 = Instant::now();
        let mut log = ThermalLog::new();
        log.observe(0, &[ThrottleSource::ThmGfx], at(t0, 0));
        log.observe(1, &[ThrottleSource::ThmGfx], at(t0, 0));
        // Closing GPU 0's episode must not touch GPU 1's.
        log.observe(0, &[], at(t0, 1));
        let still_open: Vec<_> = log
            .newest_first()
            .into_iter()
            .filter(|e| e.ended.is_none())
            .map(|e| e.gpu_index)
            .collect();
        assert_eq!(still_open, vec![1]);
    }

    #[test]
    fn ring_buffer_caps_closed_episodes() {
        let t0 = Instant::now();
        let mut log = ThermalLog::new();
        // Open then immediately close CAP + 50 distinct episodes (one source, toggled).
        for i in 0..(THERMAL_EVENT_CAP as u64 + 50) {
            log.observe(0, &[ThrottleSource::Spl], at(t0, i * 2));
            log.observe(0, &[], at(t0, i * 2 + 1));
        }
        assert_eq!(log.len(), THERMAL_EVENT_CAP, "oldest dropped past the cap");
    }

    #[test]
    fn newest_first_orders_by_start_descending() {
        let t0 = Instant::now();
        let mut log = ThermalLog::new();
        log.observe(0, &[ThrottleSource::Spl], at(t0, 0));
        log.observe(0, &[], at(t0, 1));
        log.observe(0, &[ThrottleSource::ThmGfx], at(t0, 5));
        let order: Vec<_> = log.newest_first().into_iter().map(|e| e.source).collect();
        // ThmGfx started later (t+5) → first; Spl (t+0) → second.
        assert_eq!(order, vec![ThrottleSource::ThmGfx, ThrottleSource::Spl]);
    }
}
