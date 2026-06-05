//! SMU metrics panel: the deep, APU-first signal decoded from the binary `gpu_metrics` node —
//! GFX/SoC hotspot temperatures, the socket TDP split across GPU and CPU, average clocks, engine
//! activity, and which power/thermal limits are actively throttling.
//!
//! The body is a pure function of `&GpuSnapshot` ([`metrics_lines`]) so it is tested directly and
//! through ratatui's `TestBackend`, including the "unavailable" case (no node / unsupported
//! revision). Every value falls back to a dimmed "n/a" rather than panicking.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::model::{GpuSnapshot, Metrics};
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
}
