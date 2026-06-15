//! Application state and the input/tick update logic.
//!
//! [`App`] is the single source of truth the UI renders from. It enumerates amdgpu cards once at
//! startup and reads Vulkan info once, then on every tick re-reads each card's sysfs snapshot,
//! merges low-cadence rocm-smi identity/extra fields, samples per-process GPU usage, and appends
//! to the history buffers.
//!
//! Data-source cadence (single-threaded, staggered — see SPEC Open Question 1):
//! - sysfs: every tick (cheap, authoritative for live metrics)
//! - rocm-smi: every [`SMI_EVERY_TICKS`] ticks (forking is expensive; fills static identity)
//! - process accounting: every [`PROC_EVERY_TICKS`] ticks (an O(pids×fds) /proc walk; unless
//!   `--no-procs`). In-between ticks carry the prior rows forward.
//! - vulkan: once at startup (static; unless `--no-vulkan`)

use std::collections::BTreeMap;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::collect::smi::{self, SmiData};
use crate::collect::sysfs::{self, SysfsGpu};
use crate::collect::{process, vulkan};
use crate::config::Config;
use crate::history::GpuHistory;
use crate::model::{EngineNs, GpuSnapshot, Opt, ProcInfo, ThermalEvent, Throttle, VulkanInfo};
use crate::panel::{PanelKind, PanelLayout};
use crate::settings::Settings;
use crate::theme::Theme;
use crate::thermal::ThermalLog;

/// Re-run `rocm-smi` every N ticks. At the default 1 s interval this is ~every 5 s.
const SMI_EVERY_TICKS: u64 = 5;

/// Re-run the per-process `/proc` walk every N ticks. The walk is O(pids×fds), so at the default
/// 1 s interval refreshing the process table ~every 3 s keeps the hot path cheap. In-between ticks
/// carry the prior rows forward (re-sorting only).
const PROC_EVERY_TICKS: u64 = 3;

/// Rows the thermal-events list jumps per PageUp/PageDown (the viewport height isn't known in the
/// key handler, so a fixed page keeps scrolling predictable).
const EVENTS_PAGE: isize = 10;

/// Sort order for the process table, cycled with the `s` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcSort {
    /// GPU memory, descending (default).
    Mem,
    /// PID, ascending.
    Pid,
    /// Process name, ascending.
    Name,
}

impl ProcSort {
    fn next(self) -> Self {
        match self {
            ProcSort::Mem => ProcSort::Pid,
            ProcSort::Pid => ProcSort::Name,
            ProcSort::Name => ProcSort::Mem,
        }
    }

    /// Short label for the footer hint.
    pub fn label(self) -> &'static str {
        match self {
            ProcSort::Mem => "mem",
            ProcSort::Pid => "pid",
            ProcSort::Name => "name",
        }
    }
}

/// Whole-program state. Rendered by `ui::render`; mutated by `tick` and `on_key`.
#[derive(Debug)]
pub struct App {
    pub config: Config,
    should_quit: bool,
    gpus: Vec<SysfsGpu>,
    snapshots: Vec<GpuSnapshot>,
    history: Vec<GpuHistory>,
    /// Cached rocm-smi data keyed by card index, refreshed every [`SMI_EVERY_TICKS`] ticks.
    smi_cache: BTreeMap<usize, SmiData>,
    /// Per-process GPU rows (memory desc). Empty when `--no-procs` is set.
    procs: Vec<ProcInfo>,
    /// Processes holding an amdgpu handle whose fdinfo was unreadable (needs elevation).
    procs_hidden: usize,
    /// Derives per-process engine utilization from the delta between two process walks.
    engine_sampler: EngineSampler,
    /// Previous throttle-residency counters per card index, for deriving which sources are
    /// actively throttling (a counter that advanced between two ticks).
    prev_throttle: BTreeMap<usize, Throttle>,
    /// Session-long history of throttling episodes, shown in the Metrics panel.
    thermal: ThermalLog,
    /// Scroll offset (rows from the top, newest-first) into the thermal-events list.
    events_scroll: usize,
    /// Wall-clock instant of the previous process walk, for the engine-utilization denominator.
    last_proc_walk: Option<Instant>,
    /// Index of the highlighted process row (clamped to the current list).
    proc_selected: usize,
    /// Whether the per-process detail popup is open (for the selected row).
    proc_detail_open: bool,
    /// Static Vulkan device info, read once. `None` when `--no-vulkan` or unavailable.
    vulkan: Option<VulkanInfo>,
    ticks: u64,
    /// When true, ticking is suspended and the last sample is frozen on screen.
    paused: bool,
    /// Current process-table sort order.
    proc_sort: ProcSort,
    /// Persisted user settings (theme, color overrides, panel layout).
    settings: Settings,
    /// Active theme, resolved from `settings` (preset + overrides).
    theme: Theme,
    /// Panel order / visibility / focus.
    panels: PanelLayout,
    /// Whether the help overlay is shown.
    show_help: bool,
    /// Test-only override for the `/proc` root. `None` in production (real `/proc` via
    /// [`process::collect`]); `Some` in tests (a fixture tree via [`process::collect_in`]).
    proc_root_override: Option<PathBuf>,
}

impl App {
    /// Enumerate cards (honoring `--gpu`) and read static Vulkan info once. Per-GPU snapshots are
    /// deferred to the first [`App::tick`], keeping construction light.
    pub fn new(config: Config) -> Self {
        let gpus = discover(&config);
        let history = (0..gpus.len())
            .map(|_| GpuHistory::new(config.history))
            .collect();
        let vulkan = if config.no_vulkan {
            None
        } else {
            vulkan::collect()
        };
        let settings = Settings::load();
        let theme = settings.resolve_theme();
        let mut panels = PanelLayout::from_settings(&settings.order, &settings.hidden);
        // CLI flags seed initial visibility (the collectors also skip these when disabled).
        if config.no_procs {
            panels.hide(PanelKind::Processes);
        }
        if config.no_vulkan {
            panels.hide(PanelKind::Vulkan);
        }
        Self {
            config,
            should_quit: false,
            gpus,
            snapshots: Vec::new(),
            history,
            smi_cache: BTreeMap::new(),
            procs: Vec::new(),
            procs_hidden: 0,
            engine_sampler: EngineSampler::new(),
            prev_throttle: BTreeMap::new(),
            thermal: ThermalLog::new(),
            events_scroll: 0,
            last_proc_walk: None,
            proc_selected: 0,
            proc_detail_open: false,
            vulkan,
            ticks: 0,
            paused: false,
            proc_sort: ProcSort::Mem,
            settings,
            theme,
            panels,
            show_help: false,
            proc_root_override: None,
        }
    }

    /// Construct an [`App`] from fixtures with **no live I/O**, for deterministic unit tests.
    ///
    /// Unlike [`App::new`] this does not read the real `/sys`, fork `vulkaninfo`/`rocm-smi`, or load
    /// the on-disk config: GPUs come from `drm_root` via [`sysfs::enumerate_in`], `vulkan` is `None`,
    /// the rocm-smi cache starts empty, theme/panels are resolved from the passed `settings`, and
    /// `tick()` reads processes from `proc_root` via [`process::collect_in`] instead of real `/proc`.
    #[cfg(test)]
    pub(crate) fn for_test(
        config: Config,
        drm_root: &Path,
        proc_root: &Path,
        settings: Settings,
    ) -> Self {
        let gpus = sysfs::enumerate_in(drm_root);
        let history = (0..gpus.len())
            .map(|_| GpuHistory::new(config.history))
            .collect();
        let theme = settings.resolve_theme();
        let mut panels = PanelLayout::from_settings(&settings.order, &settings.hidden);
        if config.no_procs {
            panels.hide(PanelKind::Processes);
        }
        if config.no_vulkan {
            panels.hide(PanelKind::Vulkan);
        }
        Self {
            config,
            should_quit: false,
            gpus,
            snapshots: Vec::new(),
            history,
            smi_cache: BTreeMap::new(),
            procs: Vec::new(),
            procs_hidden: 0,
            engine_sampler: EngineSampler::new(),
            prev_throttle: BTreeMap::new(),
            thermal: ThermalLog::new(),
            events_scroll: 0,
            last_proc_walk: None,
            proc_selected: 0,
            proc_detail_open: false,
            vulkan: None,
            ticks: 0,
            paused: false,
            proc_sort: ProcSort::Mem,
            settings,
            theme,
            panels,
            show_help: false,
            proc_root_override: Some(proc_root.to_path_buf()),
        }
    }

    /// The active theme the UI renders with.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// The panel layout (order / visibility / focus).
    pub fn panels(&self) -> &PanelLayout {
        &self.panels
    }

    /// True once the user has asked to exit; the run loop breaks on this.
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Advance one refresh interval: re-read sysfs, merge cadenced rocm-smi, sample processes,
    /// and append to history.
    pub fn tick(&mut self) {
        // Refresh the rocm-smi cache on the first tick and every SMI_EVERY_TICKS afterward.
        if self.ticks.is_multiple_of(SMI_EVERY_TICKS) {
            self.smi_cache = smi::collect();
        }

        self.snapshots = self
            .gpus
            .iter()
            .map(|g| {
                let mut snap = g.read();
                if let Some(data) = self.smi_cache.get(&snap.index) {
                    smi::merge(&mut snap, data); // sysfs is authoritative; only fills None/identity
                }
                snap
            })
            .collect();

        self.derive_throttle();

        for (hist, snap) in self.history.iter_mut().zip(self.snapshots.iter()) {
            // Missing metrics carry the last known value (0.0 before any sample) so the three
            // series stay continuous and time-aligned.
            hist.util
                .push(carry(hist.util.latest(), snap.busy_pct.map(|p| p as f64)));
            hist.power.push(carry(hist.power.latest(), snap.power_w));
            hist.temp.push(carry(hist.temp.latest(), snap.temp_c));
            // NPU activity is only tracked once the part has reported it at least once, so a
            // part without an NPU keeps an empty series and the graphs panel omits its sparkline.
            let npu = snap
                .metrics
                .as_ref()
                .and_then(|m| m.npu_activity_pct)
                .map(|p| p as f64);
            if npu.is_some() || !hist.npu_util.is_empty() {
                hist.npu_util.push(carry(hist.npu_util.latest(), npu));
            }
        }

        if self.config.no_procs {
            self.procs.clear();
            self.procs_hidden = 0;
        } else if self.ticks.is_multiple_of(PROC_EVERY_TICKS) {
            // Re-walk /proc on sampled ticks only — the walk is O(pids×fds).
            let (procs, hidden) = match &self.proc_root_override {
                Some(root) => process::collect_in(root),
                None => process::collect(),
            };
            self.procs = procs;
            self.procs_hidden = hidden;
            // Turn the cumulative engine counters into utilization percentages using the wall-clock
            // interval since the previous walk. The first walk has no prior sample → all `n/a`.
            let now = Instant::now();
            let wall_ns = self
                .last_proc_walk
                .map_or(0, |prev| now.duration_since(prev).as_nanos());
            self.engine_sampler.update(&mut self.procs, wall_ns);
            self.last_proc_walk = Some(now);
            self.sort_procs();
        } else {
            // In-between ticks: carry the prior rows forward; re-sorting is cheap and harmless.
            self.sort_procs();
        }
        // Keep the highlight valid after the list changed (grew, shrank, or emptied).
        self.clamp_selection();

        self.ticks = self.ticks.wrapping_add(1);
    }

    /// Fill each snapshot's `throttle_active` set by diffing its throttle-residency counters against
    /// the previous tick's, then record the current counters for next time. The first sample for a
    /// card has no baseline, so it reports nothing until the following tick.
    fn derive_throttle(&mut self) {
        let now = Instant::now();
        for snap in self.snapshots.iter_mut() {
            let index = snap.index;
            // Sources active this tick: empty when the card reports no metrics or has no prior
            // sample. Feeding the empty set to the log still matters — it closes any episode that
            // was open for this card when its metrics vanished.
            let active = match (snap.metrics.as_ref(), self.prev_throttle.get(&index)) {
                (Some(m), Some(prev)) => m.throttle.active_sources_since(prev),
                _ => Vec::new(),
            };
            if let Some(m) = snap.metrics.as_mut() {
                m.throttle_active = active.iter().map(|s| s.code().to_string()).collect();
                self.prev_throttle.insert(index, m.throttle.clone());
            }
            self.thermal.observe(index, &active, now);
        }
    }

    /// Re-order the process list per the current [`ProcSort`]. The collector pre-sorts by memory;
    /// this re-applies whenever the user changes the order, even while paused.
    fn sort_procs(&mut self) {
        match self.proc_sort {
            ProcSort::Mem => self
                .procs
                .sort_by(|a, b| b.mem_bytes.cmp(&a.mem_bytes).then(a.pid.cmp(&b.pid))),
            ProcSort::Pid => self.procs.sort_by_key(|p| p.pid),
            ProcSort::Name => self
                .procs
                .sort_by(|a, b| a.name.cmp(&b.name).then(a.pid.cmp(&b.pid))),
        }
    }

    /// The latest per-GPU snapshots, in stable index order.
    pub fn snapshots(&self) -> &[GpuSnapshot] {
        &self.snapshots
    }

    /// Per-GPU history buffers, parallel to [`App::snapshots`].
    pub fn history(&self) -> &[GpuHistory] {
        &self.history
    }

    /// Per-process GPU rows (memory desc), and how many were hidden behind permissions.
    pub fn procs(&self) -> &[ProcInfo] {
        &self.procs
    }

    pub fn procs_hidden(&self) -> usize {
        self.procs_hidden
    }

    /// Whether per-process collection runs (false under `--no-procs`). Drives the footer sort hint.
    pub fn show_procs(&self) -> bool {
        !self.config.no_procs
    }

    /// Static Vulkan device info, or `None` when unavailable / disabled.
    pub fn vulkan(&self) -> Option<&VulkanInfo> {
        self.vulkan.as_ref()
    }

    /// Whether refresh is currently suspended (toggled with `p`).
    pub fn paused(&self) -> bool {
        self.paused
    }

    /// The current process-table sort order.
    pub fn proc_sort(&self) -> ProcSort {
        self.proc_sort
    }

    /// Whether the Processes panel currently holds focus (gates row navigation keys).
    fn proc_panel_focused(&self) -> bool {
        self.panels.is_focused(PanelKind::Processes)
    }

    /// Whether the Metrics panel currently holds focus (gates thermal-events scrolling).
    fn metrics_panel_focused(&self) -> bool {
        self.panels.is_focused(PanelKind::Metrics)
    }

    /// Scroll the thermal-events list by `delta` rows (newest-first), clamped so at least one
    /// event stays in view. The viewport height isn't known here, so the render side trims the
    /// window; this only bounds the offset to the event count.
    fn scroll_events(&mut self, delta: isize) {
        let max = self.thermal.len().saturating_sub(1) as isize;
        self.events_scroll = (self.events_scroll as isize)
            .saturating_add(delta)
            .clamp(0, max) as usize;
    }

    /// Move the highlight up one row (saturating at the top).
    fn select_prev(&mut self) {
        self.proc_selected = self.proc_selected.saturating_sub(1);
    }

    /// Move the highlight down one row (clamped to the last row).
    fn select_next(&mut self) {
        if !self.procs.is_empty() {
            self.proc_selected = (self.proc_selected + 1).min(self.procs.len() - 1);
        }
    }

    /// Keep the selection in range after the list grows, shrinks, or empties between walks.
    fn clamp_selection(&mut self) {
        let max = self.procs.len().saturating_sub(1);
        if self.proc_selected > max {
            self.proc_selected = max;
        }
    }

    /// Index of the highlighted process row (clamped to the current list).
    pub fn proc_selected(&self) -> usize {
        self.proc_selected
    }

    /// Whether the per-process detail popup is open.
    pub fn proc_detail_open(&self) -> bool {
        self.proc_detail_open
    }

    /// The process row the detail popup should render, if any.
    pub fn selected_proc(&self) -> Option<&ProcInfo> {
        self.procs.get(self.proc_selected)
    }

    /// Throttling episodes for the Metrics panel, newest-first (ongoing episodes lead).
    pub fn thermal_events(&self) -> Vec<&ThermalEvent> {
        self.thermal.newest_first()
    }

    /// Scroll offset (newest-first rows from the top) into the thermal-events list.
    pub fn events_scroll(&self) -> usize {
        self.events_scroll
    }

    /// Whether the help overlay is shown.
    pub fn show_help(&self) -> bool {
        self.show_help
    }

    /// The active theme preset name (for the footer/help).
    pub fn theme_name(&self) -> &str {
        &self.settings.theme
    }

    /// Handle a key press. Returns nothing; mutates app state.
    pub fn on_key(&mut self, key: KeyEvent) {
        // The help overlay swallows everything except its own dismissal.
        if self.show_help {
            self.show_help = false;
            return;
        }
        // The process-detail popup swallows keys too: any key closes it, but `q` / Ctrl-C still
        // quit so the user is never trapped in the overlay.
        if self.proc_detail_open {
            match (key.code, key.modifiers) {
                (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    self.should_quit = true
                }
                _ => self.proc_detail_open = false,
            }
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => self.should_quit = true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,
            (KeyCode::Char('?'), _) => self.show_help = true,
            // Process-row navigation — only while the Processes panel is focused, so it doesn't
            // collide with panel-move (`[ ] ← →`) or other panels.
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) if self.proc_panel_focused() => {
                self.select_prev()
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) if self.proc_panel_focused() => {
                self.select_next()
            }
            (KeyCode::Enter, _) if self.proc_panel_focused() && !self.procs.is_empty() => {
                self.proc_detail_open = true
            }
            // Thermal-events scrolling — only while the Metrics panel is focused, mirroring the
            // Processes navigation so the same keys never collide across panels.
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) if self.metrics_panel_focused() => {
                self.scroll_events(-1)
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) if self.metrics_panel_focused() => {
                self.scroll_events(1)
            }
            (KeyCode::PageUp, _) if self.metrics_panel_focused() => {
                self.scroll_events(-EVENTS_PAGE)
            }
            (KeyCode::PageDown, _) if self.metrics_panel_focused() => {
                self.scroll_events(EVENTS_PAGE)
            }
            (KeyCode::Char('p'), _) => self.paused = !self.paused,
            (KeyCode::Char('s'), _) => {
                self.proc_sort = self.proc_sort.next();
                self.sort_procs(); // re-order immediately, even when paused
            }
            (KeyCode::Char('t'), _) => self.cycle_theme(),
            (KeyCode::Tab, _) => self.panels.focus_next(),
            // Reorder the focused panel earlier/later in the flow.
            (KeyCode::Char('['), _) | (KeyCode::Left, _) => self.panels.move_focused(-1),
            (KeyCode::Char(']'), _) | (KeyCode::Right, _) => self.panels.move_focused(1),
            // Toggle a specific panel by number (matches the canonical panel order).
            (KeyCode::Char('1'), _) => self.panels.toggle(PanelKind::Gauges),
            (KeyCode::Char('2'), _) => self.panels.toggle(PanelKind::Graphs),
            (KeyCode::Char('3'), _) => self.panels.toggle(PanelKind::Metrics),
            (KeyCode::Char('4'), _) => self.panels.toggle(PanelKind::Processes),
            (KeyCode::Char('5'), _) => self.panels.toggle(PanelKind::Vulkan),
            _ => {}
        }
    }

    /// Advance to the next theme preset and re-resolve colors (keeps user overrides).
    fn cycle_theme(&mut self) {
        self.settings.theme = Theme::next_preset(&self.settings.theme).to_string();
        self.theme = self.settings.resolve_theme();
    }

    /// Persist the current layout + theme choice to the config file. Best-effort: a failed write
    /// is ignored (customization is a convenience, not a requirement).
    pub fn save_settings(&mut self) {
        self.sync_settings();
        let _ = self.settings.save();
    }

    /// Like [`App::save_settings`] but to an explicit path (testable round-trip).
    #[cfg(test)]
    fn save_settings_to(&mut self, path: &Path) -> std::io::Result<()> {
        self.sync_settings();
        self.settings.save_to(path)
    }

    /// Fold the current panel layout back into `settings` ahead of a save.
    fn sync_settings(&mut self) {
        let (order, hidden) = self.panels.to_settings();
        self.settings.order = order;
        self.settings.hidden = hidden;
    }
}

/// Pick the next history sample: the fresh value when present, else carry forward the previous
/// sample, else `0.0`. Keeps the three graphs continuous when a metric reads "n/a".
fn carry(prev: Option<f64>, fresh: Option<f64>) -> f64 {
    fresh.or(prev).unwrap_or(0.0)
}

/// Derives per-process engine utilization from two consecutive process walks. The collector emits
/// monotonic cumulative engine-busy nanosecond counters; this turns each one into a percentage of
/// the wall-clock interval between walks, retaining the previous walk's counters keyed by pid.
///
/// Keyed by pid (not `(pid, drm-client-id)`): a pid's clients are summed by the collector, so a
/// client appearing/disappearing between walks can briefly skew one sample — self-correcting on the
/// next walk, and far simpler than tracking every client's lifetime.
#[derive(Debug, Default)]
struct EngineSampler {
    /// Cumulative engine counters from the previous walk, per pid.
    prev: BTreeMap<u32, EngineNs>,
}

impl EngineSampler {
    fn new() -> Self {
        Self::default()
    }

    /// Fill each row's `*_pct` fields from the delta against the previous walk over `wall_ns`
    /// nanoseconds, then record the current counters for next time. A pid not seen last walk, a
    /// zero interval, or a counter that went backwards yields `None` (the table's "n/a").
    fn update(&mut self, rows: &mut [ProcInfo], wall_ns: u128) {
        for row in rows.iter_mut() {
            let prev = self.prev.get(&row.pid);
            let pct = |sel: fn(&EngineNs) -> Opt<u64>| {
                engine_pct(prev.and_then(sel), sel(&row.engine_ns), wall_ns)
            };
            row.gfx_pct = pct(|e| e.gfx);
            row.compute_pct = pct(|e| e.compute);
            row.enc_pct = pct(|e| e.enc);
            row.dec_pct = pct(|e| e.dec);
        }
        self.prev = rows.iter().map(|r| (r.pid, r.engine_ns.clone())).collect();
    }
}

/// Convert a pair of cumulative engine-busy nanosecond readings into a 0..=100 utilization
/// percentage over `wall_ns` nanoseconds of wall-clock. `None` when the engine is absent in either
/// sample, the interval is zero, or the counter went backwards (process restart / pid reuse).
fn engine_pct(prev: Opt<u64>, cur: Opt<u64>, wall_ns: u128) -> Opt<u8> {
    let (prev, cur) = (prev?, cur?);
    if wall_ns == 0 || cur < prev {
        return None;
    }
    let busy = u128::from(cur - prev);
    Some((busy.saturating_mul(100) / wall_ns).min(100) as u8)
}

/// Enumerate amdgpu cards, restricted to a single index when `--gpu` is given.
fn discover(config: &Config) -> Vec<SysfsGpu> {
    let mut gpus = sysfs::enumerate();
    if let Some(idx) = config.gpu {
        gpus.retain(|g| g.index == idx);
    }
    gpus
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::path::PathBuf;

    fn app() -> App {
        App::new(Config::parse_from(["xrocmtop"]))
    }

    /// The committed sysfs fixture DRM root (one amdgpu card).
    fn drm_fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sysfs/drm")
    }

    /// A unique, empty temp dir to stand in for `/proc` (procs collect to empty, deterministically).
    fn empty_proc(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "xrocmtop_app_proc_{tag}_{}_{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A unique temp config path for save round-trips.
    fn temp_cfg(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("xrocmtop_app_cfg_{tag}_{}_{n}", std::process::id()))
            .join("config.toml")
    }

    /// Build a deterministic, fixture-backed App: one GPU from the sysfs fixture, an empty `/proc`.
    fn fixture_app(tag: &str) -> App {
        App::for_test(
            Config::parse_from(["xrocmtop"]),
            &drm_fixture(),
            &empty_proc(tag),
            Settings::default(),
        )
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn proc_row(pid: u32) -> ProcInfo {
        ProcInfo {
            pid,
            mem_bytes: Some(u64::from(pid)),
            ..Default::default()
        }
    }

    /// Cycle panel focus until the Processes panel is focused (default layout shows all four).
    fn focus_processes(a: &mut App) {
        for _ in 0..8 {
            if a.proc_panel_focused() {
                return;
            }
            a.panels.focus_next();
        }
        panic!("could not focus Processes panel");
    }

    #[test]
    fn process_selection_moves_and_clamps_when_focused() {
        let mut a = app();
        a.procs = vec![proc_row(1), proc_row(2), proc_row(3)];
        focus_processes(&mut a);
        assert_eq!(a.proc_selected(), 0);
        a.on_key(key(KeyCode::Down, KeyModifiers::NONE));
        a.on_key(key(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(a.proc_selected(), 2);
        a.on_key(key(KeyCode::Down, KeyModifiers::NONE)); // clamps at the last row
        assert_eq!(a.proc_selected(), 2);
        a.on_key(key(KeyCode::Char('k'), KeyModifiers::NONE));
        a.on_key(key(KeyCode::Up, KeyModifiers::NONE));
        a.on_key(key(KeyCode::Up, KeyModifiers::NONE)); // saturates at the top
        assert_eq!(a.proc_selected(), 0);
    }

    #[test]
    fn selection_keys_ignored_when_processes_not_focused() {
        let mut a = app();
        a.procs = vec![proc_row(1), proc_row(2)];
        while a.proc_panel_focused() {
            a.panels.focus_next();
        }
        a.on_key(key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(a.proc_selected(), 0, "navigation gated on panel focus");
    }

    /// Cycle panel focus until the Metrics panel is focused.
    fn focus_metrics(a: &mut App) {
        for _ in 0..8 {
            if a.metrics_panel_focused() {
                return;
            }
            a.panels.focus_next();
        }
        panic!("could not focus Metrics panel");
    }

    /// Add `n` distinct closed throttle episodes to the thermal log.
    fn add_events(a: &mut App, n: usize) {
        use crate::model::ThrottleSource;
        use std::time::Duration;
        let t0 = Instant::now();
        for i in 0..n as u64 {
            a.thermal
                .observe(0, &[ThrottleSource::Spl], t0 + Duration::from_secs(i * 2));
            a.thermal
                .observe(0, &[], t0 + Duration::from_secs(i * 2 + 1));
        }
    }

    #[test]
    fn events_scroll_moves_and_clamps_when_metrics_focused() {
        let mut a = app();
        add_events(&mut a, 5);
        focus_metrics(&mut a);
        assert_eq!(a.events_scroll(), 0);
        a.on_key(key(KeyCode::Down, KeyModifiers::NONE));
        a.on_key(key(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(a.events_scroll(), 2);
        // PageDown jumps a page but clamps to the last event (len - 1 = 4).
        a.on_key(key(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(a.events_scroll(), 4);
        // Up / k step back; PageUp saturates at the top.
        a.on_key(key(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(a.events_scroll(), 3);
        a.on_key(key(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(a.events_scroll(), 0);
    }

    #[test]
    fn events_scroll_ignored_when_metrics_not_focused() {
        let mut a = app();
        add_events(&mut a, 5);
        // Ensure some other panel holds focus.
        while a.metrics_panel_focused() {
            a.panels.focus_next();
        }
        a.on_key(key(KeyCode::Down, KeyModifiers::NONE));
        a.on_key(key(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(a.events_scroll(), 0, "scrolling gated on Metrics focus");
    }

    #[test]
    fn enter_opens_detail_and_any_key_closes() {
        let mut a = app();
        a.procs = vec![proc_row(1)];
        focus_processes(&mut a);
        a.on_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(a.proc_detail_open());
        a.on_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!a.proc_detail_open());
    }

    #[test]
    fn enter_on_empty_list_does_not_open_detail() {
        let mut a = app();
        a.procs.clear();
        focus_processes(&mut a);
        a.on_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!a.proc_detail_open());
    }

    #[test]
    fn detail_popup_swallows_keys_but_q_quits() {
        let mut a = app();
        a.procs = vec![proc_row(1)];
        focus_processes(&mut a);
        a.on_key(key(KeyCode::Enter, KeyModifiers::NONE));
        // A key that normally cycles sort is swallowed (closes the popup, sort unchanged).
        let sort_before = a.proc_sort();
        a.on_key(key(KeyCode::Char('s'), KeyModifiers::NONE));
        assert!(!a.proc_detail_open());
        assert_eq!(a.proc_sort(), sort_before);
        // Reopen, then `q` still quits rather than trapping the user.
        a.on_key(key(KeyCode::Enter, KeyModifiers::NONE));
        a.on_key(key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(a.should_quit());
    }

    #[test]
    fn selection_clamps_when_list_empties_on_tick() {
        let mut a = fixture_app("clampsel"); // empty /proc → zero procs after a walk
        a.procs = vec![proc_row(1), proc_row(2), proc_row(3)];
        a.proc_selected = 2;
        a.tick(); // tick 0 is a walk tick → procs becomes empty
        assert!(a.procs.is_empty());
        assert_eq!(a.proc_selected(), 0);
    }

    #[test]
    fn starts_running_with_no_snapshots() {
        let a = app();
        assert!(!a.should_quit());
        assert!(a.snapshots().is_empty()); // first read happens on tick
    }

    #[test]
    fn quits_on_q_esc_and_ctrl_c() {
        for (code, mods) in [
            (KeyCode::Char('q'), KeyModifiers::NONE),
            (KeyCode::Esc, KeyModifiers::NONE),
            (KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            let mut a = app();
            a.on_key(key(code, mods));
            assert!(a.should_quit(), "expected quit for {code:?}/{mods:?}");
        }
    }

    #[test]
    fn ignores_unrelated_keys() {
        let mut a = app();
        a.on_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
        a.on_key(key(KeyCode::Char('c'), KeyModifiers::NONE)); // 'c' without Ctrl
        assert!(!a.should_quit());
    }

    #[test]
    fn tick_produces_one_snapshot_and_history_per_gpu() {
        let mut a = app();
        let n = a.gpus.len();
        a.tick();
        assert_eq!(a.snapshots().len(), n);
        assert_eq!(a.history().len(), n);
        for h in a.history() {
            assert_eq!(h.util.len(), 1); // one sample appended
        }
    }

    #[test]
    fn no_procs_flag_keeps_process_list_empty() {
        let mut a = App::new(Config::parse_from(["xrocmtop", "--no-procs"]));
        a.tick();
        assert!(!a.show_procs());
        assert!(a.procs().is_empty());
    }

    #[test]
    fn carry_prefers_fresh_then_prev_then_zero() {
        assert_eq!(carry(Some(10.0), Some(42.0)), 42.0);
        assert_eq!(carry(Some(10.0), None), 10.0);
        assert_eq!(carry(None, None), 0.0);
    }

    #[test]
    fn p_toggles_pause() {
        let mut a = app();
        assert!(!a.paused());
        a.on_key(key(KeyCode::Char('p'), KeyModifiers::NONE));
        assert!(a.paused());
        a.on_key(key(KeyCode::Char('p'), KeyModifiers::NONE));
        assert!(!a.paused());
    }

    #[test]
    fn engine_pct_basic_ratio_and_guards() {
        // 0.5 s busy over a 1 s wall window → 50%.
        assert_eq!(
            engine_pct(Some(1_000_000_000), Some(1_500_000_000), 1_000_000_000),
            Some(50)
        );
        // Fully busy → 100%.
        assert_eq!(
            engine_pct(Some(0), Some(2_000_000_000), 2_000_000_000),
            Some(100)
        );
        // Over-busy (multi-queue can exceed wall time) clamps to 100, never wraps.
        assert_eq!(
            engine_pct(Some(0), Some(5_000_000_000), 1_000_000_000),
            Some(100)
        );
        // Missing in either sample, zero interval, or a backwards counter → None.
        assert_eq!(engine_pct(None, Some(10), 1_000), None);
        assert_eq!(engine_pct(Some(10), None, 1_000), None);
        assert_eq!(engine_pct(Some(10), Some(20), 0), None);
        assert_eq!(engine_pct(Some(20), Some(10), 1_000), None);
    }

    #[test]
    fn sampler_first_walk_is_na_then_derives_pct() {
        use crate::model::EngineNs;
        let mut s = EngineSampler::new();
        let row = |pid: u32, gfx: u64, compute: u64| ProcInfo {
            pid,
            engine_ns: EngineNs {
                gfx: Some(gfx),
                compute: Some(compute),
                ..Default::default()
            },
            ..Default::default()
        };

        // First walk: no prior counters → all None even though wall_ns is given.
        let mut rows = vec![row(1, 1_000_000_000, 0)];
        s.update(&mut rows, 1_000_000_000);
        assert_eq!(rows[0].gfx_pct, None);
        assert_eq!(rows[0].compute_pct, None);

        // Second walk: gfx advanced 0.25 s over a 1 s window → 25%; compute idle → 0%.
        let mut rows = vec![row(1, 1_250_000_000, 0)];
        s.update(&mut rows, 1_000_000_000);
        assert_eq!(rows[0].gfx_pct, Some(25));
        assert_eq!(rows[0].compute_pct, Some(0));

        // A brand-new pid on the next walk is still n/a until it has a prior sample.
        let mut rows = vec![row(2, 500_000_000, 0)];
        s.update(&mut rows, 1_000_000_000);
        assert_eq!(rows[0].gfx_pct, None);
    }

    #[test]
    fn derive_throttle_flags_advanced_sources_after_first_sample() {
        use crate::model::{Metrics, Throttle};
        let mut a = fixture_app("throttle");
        let snap = |spl: u32| GpuSnapshot {
            index: 0,
            metrics: Some(Metrics {
                throttle: Throttle {
                    spl: Some(spl),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        let active = |a: &App| {
            a.snapshots[0]
                .metrics
                .as_ref()
                .unwrap()
                .throttle_active
                .clone()
        };

        // First sample: no baseline yet → nothing reported active.
        a.snapshots = vec![snap(100)];
        a.derive_throttle();
        assert!(active(&a).is_empty());

        // SPL residency advanced since the baseline → SPL is actively throttling.
        a.snapshots = vec![snap(150)];
        a.derive_throttle();
        assert_eq!(active(&a), vec!["SPL".to_string()]);

        // Unchanged residency → nothing active again.
        a.snapshots = vec![snap(150)];
        a.derive_throttle();
        assert!(active(&a).is_empty());
    }

    #[test]
    fn tick_populates_metrics_from_fixture() {
        // The committed sysfs fixture includes a gpu_metrics node, so a ticked snapshot carries
        // decoded SMU telemetry; the first tick has no throttle baseline so nothing is "active".
        let mut a = fixture_app("metrics");
        a.tick();
        let m = a.snapshots()[0].metrics.as_ref().expect("metrics decoded");
        assert_eq!(m.temp_gfx_c, Some(70.38));
        assert!(m.throttle_active.is_empty());
    }

    #[test]
    fn s_cycles_sort_and_reorders() {
        use crate::model::ProcInfo;
        let mut a = app();
        a.procs = vec![
            ProcInfo {
                pid: 200,
                name: "alpha".into(),
                mem_bytes: Some(10),
                ..Default::default()
            },
            ProcInfo {
                pid: 100,
                name: "beta".into(),
                mem_bytes: Some(99),
                ..Default::default()
            },
        ];
        a.sort_procs(); // Mem desc → beta(99) first
        assert_eq!(a.procs[0].pid, 100);
        a.on_key(key(KeyCode::Char('s'), KeyModifiers::NONE)); // → Pid asc
        assert_eq!(a.proc_sort(), ProcSort::Pid);
        assert_eq!(a.procs[0].pid, 100);
        a.on_key(key(KeyCode::Char('s'), KeyModifiers::NONE)); // → Name asc
        assert_eq!(a.proc_sort(), ProcSort::Name);
        assert_eq!(a.procs[0].name, "alpha");
        a.on_key(key(KeyCode::Char('s'), KeyModifiers::NONE)); // → back to Mem
        assert_eq!(a.proc_sort(), ProcSort::Mem);
    }

    #[test]
    fn for_test_tick_yields_exactly_one_snapshot_and_history() {
        // Deterministic via the committed sysfs fixture (one card), not the host's GPU count.
        let mut a = fixture_app("tick");
        assert_eq!(a.gpus.len(), 1);
        a.tick();
        assert_eq!(a.snapshots().len(), 1);
        assert_eq!(a.history().len(), 1);
        assert_eq!(a.history()[0].util.len(), 1); // one sample appended
        assert!(a.procs().is_empty()); // empty /proc fixture
    }

    #[test]
    fn save_settings_round_trips_layout() {
        let mut a = fixture_app("save");
        // Mutate the layout: hide Processes and move the focused panel later.
        a.on_key(key(KeyCode::Char('4'), KeyModifiers::NONE)); // hide Processes
        a.on_key(key(KeyCode::Char(']'), KeyModifiers::NONE)); // move focused panel later

        let expected = a.panels.to_settings();
        // Guard against clobbering a user's config: order must never be empty.
        assert!(!expected.0.is_empty(), "saved order must not be empty");

        let path = temp_cfg("save");
        a.save_settings_to(&path).unwrap();

        let loaded = Settings::load_from(&path);
        assert!(
            !loaded.order.is_empty(),
            "persisted order must not be empty"
        );
        let restored = PanelLayout::from_settings(&loaded.order, &loaded.hidden);
        let (rorder, rhidden) = restored.to_settings();
        assert_eq!(rorder, expected.0, "order preserved");
        assert_eq!(rhidden, expected.1, "hidden preserved");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn t_cycles_theme_from_default_to_high_contrast() {
        let mut a = fixture_app("theme");
        assert_eq!(a.theme_name(), "default");
        a.on_key(key(KeyCode::Char('t'), KeyModifiers::NONE));
        assert_eq!(a.theme_name(), "high-contrast");
    }

    #[test]
    fn help_swallows_the_next_key_without_acting() {
        let mut a = fixture_app("help");
        a.on_key(key(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(a.show_help());
        // The dismissing key must NOT perform its own action (here: 'q' does not quit).
        a.on_key(key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(!a.show_help(), "help dismissed");
        assert!(!a.should_quit(), "'q' was swallowed, not acted upon");
    }

    #[test]
    fn tab_changes_focused_panel() {
        let mut a = fixture_app("tab");
        let before = a.panels().focused_kind();
        a.on_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_ne!(a.panels().focused_kind(), before);
    }

    #[test]
    fn four_toggles_processes_panel_hidden() {
        let mut a = fixture_app("toggle4");
        assert!(a.panels().visible().contains(&PanelKind::Processes));
        a.on_key(key(KeyCode::Char('4'), KeyModifiers::NONE));
        assert!(!a.panels().visible().contains(&PanelKind::Processes));
        a.on_key(key(KeyCode::Char('4'), KeyModifiers::NONE));
        assert!(a.panels().visible().contains(&PanelKind::Processes));
    }

    #[test]
    fn three_toggles_metrics_panel_hidden() {
        let mut a = fixture_app("toggle3");
        assert!(a.panels().visible().contains(&PanelKind::Metrics));
        a.on_key(key(KeyCode::Char('3'), KeyModifiers::NONE));
        assert!(!a.panels().visible().contains(&PanelKind::Metrics));
        a.on_key(key(KeyCode::Char('3'), KeyModifiers::NONE));
        assert!(a.panels().visible().contains(&PanelKind::Metrics));
    }
}
