//! SMU metrics panel: the deep, APU-first signal decoded from the binary `gpu_metrics` node —
//! GFX/SoC hotspot temperatures, the socket TDP split across GPU and CPU, average clocks, engine
//! activity, and which power/thermal limits are actively throttling.
//!
//! The body is a pure function of `&GpuSnapshot` ([`metrics_lines`]) so it is tested directly and
//! through ratatui's `TestBackend`, including the "unavailable" case (no node / unsupported
//! revision). Every value falls back to a dimmed "n/a" rather than panicking.

use std::time::Instant;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::model::{fmt_duration, GpuSnapshot, Metrics, ThermalEvent};
use crate::theme::Theme;

/// Width of the left label column.
const LABEL_W: usize = 8;

/// Render one GPU's SMU metrics into `area`.
pub fn render_metrics(
    frame: &mut Frame,
    area: Rect,
    snap: &GpuSnapshot,
    theme: &Theme,
    focused: bool,
) {
    let border = if focused { theme.focus } else { theme.border };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(format!(" GPU {} metrics ", snap.index))
        .title_style(
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        );
    let para = Paragraph::new(metrics_lines(snap, theme))
        .style(Style::default().fg(theme.text))
        .block(block);
    frame.render_widget(para, area);
}

/// Width of the relative-time column in the events list ("just now", "12m ago", …).
const AGE_W: usize = 10;
/// Width of the plain-English reason column.
const REASON_W: usize = 26;

/// Render the scrollable "Thermal events" list — the human-readable history of throttling
/// episodes. `events` is newest-first; `scroll` is the row offset from the top; `show_gpu` adds a
/// "GPU n" tag when more than one card is present. Pure layout lives in [`thermal_event_lines`].
pub fn render_thermal_events(
    frame: &mut Frame,
    area: Rect,
    events: &[&ThermalEvent],
    scroll: usize,
    show_gpu: bool,
    theme: &Theme,
    focused: bool,
) {
    let border = if focused { theme.focus } else { theme.border };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(format!(" Thermal events ({}) ", events.len()))
        .title_style(
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = thermal_event_lines(
        events,
        scroll,
        inner.height as usize,
        show_gpu,
        Instant::now(),
        theme,
    );
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(theme.text)),
        inner,
    );
}

/// Build the events-list lines for a viewport of `rows` height, applying `scroll` and adding
/// overflow hints ("↑ N newer" / "↓ N older") when episodes fall outside the window. Pure and
/// fully testable; `now` is injected so relative times are deterministic.
fn thermal_event_lines(
    events: &[&ThermalEvent],
    scroll: usize,
    rows: usize,
    show_gpu: bool,
    now: Instant,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if events.is_empty() {
        return vec![Line::from(Span::styled(
            "No throttling recorded this session.",
            Style::default().fg(theme.dim),
        ))];
    }
    if rows == 0 {
        return Vec::new();
    }

    let total = events.len();
    let scroll = scroll.min(total - 1);
    // Reserve a row for the top hint when episodes are hidden above the window, and one for the
    // bottom hint when more remain below than fit in what's left.
    let top_hint = scroll > 0;
    let avail = rows.saturating_sub(top_hint as usize);
    let remaining = total - scroll;
    let bottom_hint = remaining > avail;
    let shown = if bottom_hint {
        avail.saturating_sub(1)
    } else {
        remaining.min(avail)
    };

    let mut lines = Vec::with_capacity(rows);
    if top_hint {
        lines.push(hint_line(format!("↑ {scroll} newer"), theme));
    }
    for ev in &events[scroll..scroll + shown] {
        lines.push(event_line(ev, show_gpu, now, theme));
    }
    if bottom_hint {
        let older = total - scroll - shown;
        lines.push(hint_line(format!("↓ {older} older"), theme));
    }
    lines
}

/// One episode row: "`<age> ago   <reason>   ongoing|lasted <dur>`". Ongoing episodes are
/// highlighted in the accent color so an active throttle stands out.
fn event_line(ev: &ThermalEvent, show_gpu: bool, now: Instant, theme: &Theme) -> Line<'static> {
    let age = ev.age(now);
    let when = if age.as_secs() == 0 {
        "just now".to_string()
    } else {
        format!("{} ago", fmt_duration(age))
    };

    let reason = if show_gpu {
        format!("GPU {} {}", ev.gpu_index, ev.source.label())
    } else {
        ev.source.label().to_string()
    };

    let (status, status_style) = match ev.duration() {
        None => (
            "ongoing".to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Some(d) => (
            format!("lasted {}", fmt_duration(d)),
            Style::default().fg(theme.dim),
        ),
    };

    Line::from(vec![
        Span::styled(format!("{when:<AGE_W$}"), Style::default().fg(theme.text)),
        Span::styled(
            format!("{reason:<REASON_W$}"),
            Style::default().fg(theme.text),
        ),
        Span::styled(status, status_style),
    ])
}

fn hint_line(text: String, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(text, Style::default().fg(theme.dim)))
}

/// Build the panel's lines. Pure and exhaustively testable.
fn metrics_lines(snap: &GpuSnapshot, theme: &Theme) -> Vec<Line<'static>> {
    let Some(m) = snap.metrics.as_ref() else {
        return vec![Line::from(Span::styled(
            "gpu_metrics: unavailable",
            Style::default().fg(theme.dim),
        ))];
    };

    // Every row is something the GPU-centric Gauges panel can't show: the CPU/NPU sharing the
    // socket, unified-memory bandwidth, hotspot temps, the sustained limit, and throttle reasons.
    vec![
        labeled(
            "Temp",
            vec![
                pair(
                    "Hotspot",
                    m.temp_gfx_c.map(fmt_temp),
                    theme.graph_temp,
                    theme,
                ),
                pair("SoC", m.temp_soc_c.map(fmt_temp), theme.graph_temp, theme),
            ],
            theme,
        ),
        labeled(
            "CPU",
            vec![
                pair("", m.cpu_power_w.map(fmt_watt), theme.graph_power, theme),
                pair("", m.cpu_clk_max_mhz.map(fmt_mhz), theme.text, theme),
                pair("", cpu_busy_label(&m.cpu_core_c0), theme.text, theme),
            ],
            theme,
        ),
        labeled(
            "NPU",
            vec![
                pair("", m.npu_activity_pct.map(fmt_pct), theme.text, theme),
                pair("", m.npu_power_w.map(fmt_watt), theme.graph_power, theme),
            ],
            theme,
        ),
        labeled(
            "Memory",
            vec![
                pair("R", m.dram_read_mbps.map(fmt_bw), theme.vram_bar, theme),
                pair("W", m.dram_write_mbps.map(fmt_bw), theme.gtt_bar, theme),
            ],
            theme,
        ),
        labeled(
            "Limit",
            vec![pair(
                "STAPM",
                m.stapm_limit_w.map(fmt_watt),
                theme.text,
                theme,
            )],
            theme,
        ),
        throttle_line(m, theme),
    ]
}

/// "N/M busy" summary of per-core C0 residency — a core counts as busy at ≥5% C0. `None` (→ n/a)
/// when the per-core data is absent.
fn cpu_busy_label(c0: &[u8]) -> Option<String> {
    if c0.is_empty() {
        return None;
    }
    let busy = c0.iter().filter(|&&r| r >= 5).count();
    Some(format!("{busy}/{} busy", c0.len()))
}

/// "Throttle" line: list the active sources in the accent color, or a dimmed "none". Before a
/// second sample exists the active set is empty, so it reads "none" until throttling is observed.
fn throttle_line(m: &Metrics, theme: &Theme) -> Line<'static> {
    let mut spans = vec![label_span("Throttle", theme)];
    if m.throttle_active.is_empty() {
        spans.push(Span::styled("none", Style::default().fg(theme.dim)));
    } else {
        for (i, src) in m.throttle_active.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                src.clone(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    Line::from(spans)
}

/// A line of `Label   k=v  k=v …` spans.
fn labeled(label: &str, mut values: Vec<Span<'static>>, theme: &Theme) -> Line<'static> {
    let mut spans = vec![label_span(label, theme)];
    spans.append(&mut values);
    Line::from(spans)
}

/// The left-column label span, padded and dimmed.
fn label_span(label: &str, theme: &Theme) -> Span<'static> {
    Span::styled(format!("{label:<LABEL_W$}"), Style::default().fg(theme.dim))
}

/// A `"key value  "` span pair: dimmed key, colored value (or dimmed "n/a"), trailing gap.
fn pair(
    key: &str,
    value: Option<String>,
    color: ratatui::style::Color,
    theme: &Theme,
) -> Span<'static> {
    // An empty key renders just the value (used where the label column already names the group).
    let prefix = if key.is_empty() {
        String::new()
    } else {
        format!("{key} ")
    };
    match value {
        Some(v) => Span::styled(format!("{prefix}{v}  "), Style::default().fg(color)),
        None => Span::styled(format!("{prefix}n/a  "), Style::default().fg(theme.dim)),
    }
}

fn fmt_temp(c: f64) -> String {
    format!("{c:.0}°C")
}

/// MB/s → "NN.N GB/s" for readability.
fn fmt_bw(mbps: u16) -> String {
    format!("{:.1} GB/s", f64::from(mbps) / 1000.0)
}

fn fmt_watt(w: f64) -> String {
    format!("{w:.1}W")
}

fn fmt_mhz(m: u16) -> String {
    format!("{m}MHz")
}

fn fmt_pct(p: u16) -> String {
    format!("{p}%")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Metrics, Throttle};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn full_metrics() -> Metrics {
        Metrics {
            temp_gfx_c: Some(70.38),
            temp_soc_c: Some(62.62),
            cpu_power_w: Some(22.77),
            cpu_clk_max_mhz: Some(5040),
            cpu_core_c0: vec![7, 21, 2, 1, 2, 3, 32, 5, 3, 3, 2, 2, 0, 1, 1, 5],
            npu_activity_pct: Some(0),
            npu_power_w: None,
            dram_read_mbps: Some(47791),
            dram_write_mbps: Some(1463),
            stapm_limit_w: None,
            throttle: Throttle::default(),
            throttle_active: vec!["FPPT".into(), "SPPT".into()],
        }
    }

    fn snap_with(metrics: Option<Metrics>) -> GpuSnapshot {
        GpuSnapshot {
            index: 0,
            metrics,
            ..Default::default()
        }
    }

    fn render(snap: &GpuSnapshot) -> String {
        let mut term = Terminal::new(TestBackend::new(90, 9)).unwrap();
        term.draw(|f| render_metrics(f, f.area(), snap, &Theme::default(), false))
            .unwrap();
        let buf = term.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    #[test]
    fn renders_full_metrics() {
        let out = render(&snap_with(Some(full_metrics())));
        assert!(out.contains("GPU 0 metrics"));
        assert!(out.contains("70°C")); // GFX hotspot temp
        assert!(out.contains("22.8W")); // CPU power
        assert!(out.contains("5040MHz")); // peak CPU clock
        assert!(out.contains("5/16 busy")); // CPU cores with C0 >= 5%
        assert!(out.contains("47.8 GB/s")); // DRAM read bandwidth
        assert!(out.contains("FPPT"));
        assert!(out.contains("SPPT"));
        // GPU-side duplicates are intentionally NOT here (those live in the Gauges panel).
        assert!(!out.contains("Socket"));
    }

    #[test]
    fn renders_unavailable_when_no_metrics() {
        let out = render(&snap_with(None));
        assert!(out.contains("gpu_metrics: unavailable"));
    }

    #[test]
    fn missing_fields_show_na_and_no_throttle_reads_none() {
        // A metrics struct with everything absent and no active throttlers.
        let out = render(&snap_with(Some(Metrics::default())));
        assert!(out.contains("n/a"));
        assert!(out.contains("none")); // throttle line with empty active set
    }

    #[test]
    fn stapm_sentinel_renders_na_not_a_value() {
        let lines = metrics_lines(&snap_with(Some(full_metrics())), &Theme::default());
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("STAPM n/a"));
    }

    // --- Thermal events section ---

    use crate::model::{ThermalEvent, ThrottleSource};
    use std::time::Duration;

    fn lines_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn ev(gpu: usize, src: ThrottleSource, start: Instant, end: Option<Instant>) -> ThermalEvent {
        ThermalEvent {
            gpu_index: gpu,
            source: src,
            started: start,
            ended: end,
        }
    }

    #[test]
    fn empty_log_shows_placeholder() {
        let lines = thermal_event_lines(&[], 0, 5, false, Instant::now(), &Theme::default());
        assert!(lines_text(&lines).contains("No throttling recorded this session."));
    }

    #[test]
    fn ongoing_and_closed_render_plain_english() {
        let now = Instant::now();
        let start = now - Duration::from_secs(120);
        let ongoing = ev(0, ThrottleSource::ThmGfx, start, None);
        let closed = ev(
            0,
            ThrottleSource::Sppt,
            now - Duration::from_secs(300),
            Some(now - Duration::from_secs(294)),
        );
        let refs = [&ongoing, &closed];
        let lines = thermal_event_lines(&refs, 0, 10, false, now, &Theme::default());
        let text = lines_text(&lines);
        assert!(text.contains("GPU too hot"), "plain-English label");
        assert!(text.contains("Power limit (sustained)"));
        assert!(text.contains("2m ago"), "relative start time");
        assert!(text.contains("ongoing"), "active episode flagged");
        assert!(text.contains("lasted 6s"), "closed episode shows duration");
    }

    #[test]
    fn multi_gpu_prefixes_gpu_index() {
        let now = Instant::now();
        let e = ev(
            1,
            ThrottleSource::ThmGfx,
            now - Duration::from_secs(5),
            None,
        );
        let refs = [&e];
        let with = thermal_event_lines(&refs, 0, 5, true, now, &Theme::default());
        let without = thermal_event_lines(&refs, 0, 5, false, now, &Theme::default());
        assert!(lines_text(&with).contains("GPU 1 GPU too hot"));
        assert!(!lines_text(&without).contains("GPU 1"));
    }

    #[test]
    fn overflow_shows_hints_and_scrolls() {
        let now = Instant::now();
        // 10 closed episodes, newest-first.
        let owned: Vec<ThermalEvent> = (0..10)
            .map(|i| {
                let s = now - Duration::from_secs((100 - i) as u64);
                ev(0, ThrottleSource::Spl, s, Some(s + Duration::from_secs(1)))
            })
            .collect();
        let refs: Vec<&ThermalEvent> = owned.iter().collect();
        // Window of 4 rows, scrolled down by 3 → both hints present.
        let lines = thermal_event_lines(&refs, 3, 4, false, now, &Theme::default());
        let text = lines_text(&lines);
        assert!(text.contains("↑ 3 newer"), "rows hidden above");
        assert!(text.contains("older"), "rows hidden below");
        assert_eq!(lines.len(), 4, "fills exactly the viewport");
    }

    #[test]
    fn scroll_past_end_is_clamped_without_panic() {
        let now = Instant::now();
        let e = ev(0, ThrottleSource::Spl, now, Some(now));
        let refs = [&e];
        // Absurd scroll offset must not panic and still renders the single event.
        let lines = thermal_event_lines(&refs, 999, 5, false, now, &Theme::default());
        assert!(!lines.is_empty());
    }

    #[test]
    fn renders_through_backend_without_panic() {
        let now = Instant::now();
        let e = ev(
            0,
            ThrottleSource::ThmGfx,
            now - Duration::from_secs(3),
            None,
        );
        let refs = [&e];
        let mut term = Terminal::new(TestBackend::new(60, 8)).unwrap();
        term.draw(|f| render_thermal_events(f, f.area(), &refs, 0, false, &Theme::default(), true))
            .unwrap();
        let buf = term.backend().buffer().clone();
        let out: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(out.contains("Thermal events"));
        assert!(out.contains("GPU too hot"));
    }
}
