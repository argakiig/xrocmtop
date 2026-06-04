//! Per-GPU gauge panel. Each metric is a three-column row — `name · bar · value` — so the value
//! text sits to the *right* of the bar and is never drawn over the colored fill (the old layout
//! centered the label on the bar, which became unreadable as it filled). Colors come from the
//! active [`Theme`]; missing metrics render as "n/a" with an empty bar, never a panic.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

use crate::model::{fmt_bytes, GpuSnapshot, Opt};
use crate::theme::Theme;

/// Width of the left name column ("Util", "VRAM", "GTT").
const NAME_W: u16 = 5;
/// Width of the right value column.
const VALUE_W: u16 = 26;

/// Render one GPU's gauges into `area`. `focused` highlights the border with the theme accent.
pub fn render_gpu(frame: &mut Frame, area: Rect, snap: &GpuSnapshot, theme: &Theme, focused: bool) {
    let title = match &snap.name {
        Some(name) => format!(" GPU {} — {name} ", snap.index),
        None => format!(" GPU {} ", snap.index),
    };
    let border = if focused { theme.focus } else { theme.border };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(title)
        .title_style(
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);

    metric_row(
        frame,
        rows[0],
        "Util",
        snap.busy_pct.map(|p| p as f64 / 100.0),
        util_value(snap.busy_pct),
        theme.util_bar,
        theme,
    );
    metric_row(
        frame,
        rows[1],
        "VRAM",
        snap.mem.vram_frac(),
        mem_value(snap.mem.vram_used, snap.mem.vram_total),
        theme.vram_bar,
        theme,
    );
    metric_row(
        frame,
        rows[2],
        "GTT",
        snap.mem.gtt_frac(),
        mem_value(snap.mem.gtt_used, snap.mem.gtt_total),
        theme.gtt_bar,
        theme,
    );
    frame.render_widget(stats_line(snap, theme), rows[3]);
}

/// One `name · bar · value` row. `frac` of `None` renders an empty bar.
#[allow(clippy::too_many_arguments)]
fn metric_row(
    frame: &mut Frame,
    area: Rect,
    name: &str,
    frac: Opt<f64>,
    value: String,
    bar_color: ratatui::style::Color,
    theme: &Theme,
) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(NAME_W),
            Constraint::Min(4),
            Constraint::Length(VALUE_W),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(name).style(Style::default().fg(theme.text)),
        cols[0],
    );
    let ratio = frac.unwrap_or(0.0).clamp(0.0, 1.0);
    frame.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(bar_color))
            .ratio(ratio)
            .label(""), // pure fill — the value lives in its own column to the right
        cols[1],
    );
    frame.render_widget(
        Paragraph::new(format!(" {value}")).style(Style::default().fg(theme.text)),
        cols[2],
    );
}

fn util_value(busy: Opt<u8>) -> String {
    busy.map_or("n/a".to_string(), |p| format!("{p}%"))
}

fn mem_value(used: Opt<u64>, total: Opt<u64>) -> String {
    let pct = match (used, total) {
        (Some(u), Some(t)) if t > 0 => format!(" {}%", (u as u128 * 100 / t as u128)),
        _ => String::new(),
    };
    format!("{} / {}{}", fmt_bytes(used), fmt_bytes(total), pct)
}

fn stats_line<'a>(snap: &GpuSnapshot, theme: &Theme) -> Paragraph<'a> {
    let temp = snap
        .temp_c
        .map_or("n/a".to_string(), |t| format!("{t:.0}°C"));
    let power = snap
        .power_w
        .map_or("n/a".to_string(), |w| format!("{w:.1} W"));
    let c = &snap.clocks;
    let text = format!(
        "Temp {temp}  Power {power}  sclk {}  mclk {}  fclk {}  socclk {}",
        mhz(c.sclk_mhz),
        mhz(c.mclk_mhz),
        mhz(c.fclk_mhz),
        mhz(c.socclk_mhz),
    );
    Paragraph::new(text).style(Style::default().fg(theme.text))
}

fn mhz(v: Opt<u32>) -> String {
    v.map_or("n/a".to_string(), |m| format!("{m}MHz"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Clocks, MemInfo};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn full_snapshot() -> GpuSnapshot {
        GpuSnapshot {
            index: 0,
            name: Some("Radeon 8060S Graphics".into()),
            busy_pct: Some(42),
            mem: MemInfo {
                vram_total: Some(103_079_215_104),
                vram_used: Some(20_902_731_776),
                gtt_total: Some(16_368_283_648),
                gtt_used: Some(1_877_319_680),
            },
            temp_c: Some(35.0),
            power_w: Some(16.0),
            clocks: Clocks {
                sclk_mhz: Some(2900),
                mclk_mhz: Some(937),
                fclk_mhz: Some(2000),
                socclk_mhz: Some(1472),
            },
            ..Default::default()
        }
    }

    fn render(snap: &GpuSnapshot) -> String {
        let mut term = Terminal::new(TestBackend::new(80, 8)).unwrap();
        term.draw(|f| render_gpu(f, f.area(), snap, &Theme::default(), false))
            .unwrap();
        let buf = term.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    #[test]
    fn renders_full_snapshot_with_name_and_values() {
        let out = render(&full_snapshot());
        assert!(out.contains("GPU 0"));
        assert!(out.contains("Util"));
        assert!(out.contains("42%")); // value is in its own column, not over the bar
        assert!(out.contains("VRAM"));
        assert!(out.contains("GTT"));
        assert!(out.contains("Temp"));
    }

    #[test]
    fn bar_fills_proportionally() {
        // A full 100% util row should paint at least one filled gauge cell ('█').
        let mut snap = full_snapshot();
        snap.busy_pct = Some(100);
        let out = render(&snap);
        assert!(out.contains('█'), "expected a filled bar cell");
    }

    #[test]
    fn all_none_snapshot_renders_na_without_panic() {
        let out = render(&GpuSnapshot::default());
        assert!(out.contains("n/a"));
    }

    #[test]
    fn mem_value_handles_missing() {
        assert_eq!(mem_value(None, None), "n/a / n/a");
        assert_eq!(mem_value(Some(50), Some(200)), "50 B / 200 B 25%");
    }

    #[test]
    fn mem_value_large_values_do_not_overflow() {
        // used * 100 overflows u64 (2e17 * 100 = 2e19 > u64::MAX ~1.84e19);
        // the u128 math keeps the percentage correct.
        let out = mem_value(Some(200_000_000_000_000_000), Some(400_000_000_000_000_000));
        assert!(out.ends_with(" 50%"), "got {out}");
    }
}
