//! Per-GPU history graphs panel: rolling sparklines for utilization, power, and temperature.
//!
//! Three labeled [`Sparkline`]s are stacked vertically, each titled with its metric and current
//! value (e.g. "Util 42%", "Power 23 W", "Temp 35°C"). The series come from the App's per-GPU
//! [`GpuHistory`] ring buffers. An empty history renders an empty graph area — never a panic.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Sparkline};
use ratatui::Frame;

use crate::history::{GpuHistory, History};
use crate::theme::Theme;

/// Render one GPU's history graphs into `area`. `focused` highlights the border.
pub fn render_graphs(
    frame: &mut Frame,
    area: Rect,
    hist: &GpuHistory,
    theme: &Theme,
    focused: bool,
) {
    let border = if focused { theme.focus } else { theme.border };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(" History ")
        .title_style(
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Three equal stacked rows, one sparkline each.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
        ])
        .split(inner);

    // Utilization is a percentage, so its scale is fixed at 0..=100 for a stable baseline; power
    // and temperature auto-scale to their own observed maxima. Each series is fitted to the row
    // width so the graph is always full and scrolls in from the right (see `window`).
    frame.render_widget(
        sparkline(
            util_title(&hist.util),
            window(to_u64(&hist.util), rows[0].width),
            theme.graph_util,
            theme.title,
            Some(100),
        ),
        rows[0],
    );
    frame.render_widget(
        sparkline(
            power_title(&hist.power),
            window(to_u64(&hist.power), rows[1].width),
            theme.graph_power,
            theme.title,
            None,
        ),
        rows[1],
    );
    frame.render_widget(
        sparkline(
            temp_title(&hist.temp),
            window(to_u64(&hist.temp), rows[2].width),
            theme.graph_temp,
            theme.title,
            None,
        ),
        rows[2],
    );
}

/// Fit a series to exactly `width` samples for a stable, right-anchored graph.
///
/// ratatui's [`Sparkline`] draws the *leading* `width` values of its data left-to-right and ignores
/// the rest. On its own that pins the graph to its oldest samples and only "fills up" as history
/// accumulates. To match the scrolling behavior of `top`/`htop`/`btop`, we keep the newest `width`
/// samples and left-pad with zeros when history is shorter than the graph — so the latest sample is
/// always at the right edge and older data scrolls off the left.
fn window(data: Vec<u64>, width: u16) -> Vec<u64> {
    let width = width as usize;
    if data.len() >= width {
        // Trim to the trailing `width` samples (handles width == 0 -> empty).
        data[data.len() - width..].to_vec()
    } else {
        // Left-pad with zeros so the newest sample stays anchored to the right edge.
        let mut padded = vec![0; width - data.len()];
        padded.extend(data);
        padded
    }
}

/// Build one titled sparkline. An empty `data` slice renders as a blank bar — no panic.
fn sparkline<'a>(
    title: String,
    data: Vec<u64>,
    color: Color,
    title_color: Color,
    max: Option<u64>,
) -> Sparkline<'a> {
    let mut s = Sparkline::default()
        .block(
            Block::default()
                .title(title)
                .title_style(Style::default().fg(title_color)),
        )
        .style(Style::default().fg(color))
        .data(data);
    if let Some(m) = max {
        s = s.max(m);
    }
    s
}

/// Convert a `History<f64>` into the `Vec<u64>` a [`Sparkline`] consumes.
///
/// Negative samples clamp to 0 (a sparkline value can't be negative); fractional values round to
/// the nearest whole unit, matching the panel's intent (whole %, whole watts, whole °C).
fn to_u64(h: &History<f64>) -> Vec<u64> {
    h.iter().map(f64_to_u64).collect()
}

/// Round one sample to a non-negative `u64`, saturating non-finite/huge values.
fn f64_to_u64(v: f64) -> u64 {
    if !v.is_finite() || v <= 0.0 {
        0
    } else {
        v.round() as u64
    }
}

fn util_title(h: &History<f64>) -> String {
    match h.latest() {
        Some(v) => format!("Util {}%", f64_to_u64(v)),
        None => "Util n/a".to_string(),
    }
}

fn power_title(h: &History<f64>) -> String {
    match h.latest() {
        Some(v) => format!("Power {} W", f64_to_u64(v)),
        None => "Power n/a".to_string(),
    }
}

fn temp_title(h: &History<f64>) -> String {
    match h.latest() {
        Some(v) => format!("Temp {}°C", f64_to_u64(v)),
        None => "Temp n/a".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn filled() -> GpuHistory {
        let mut h = GpuHistory::new(8);
        for (u, p, t) in [(0.0, 5.0, 30.0), (42.4, 16.6, 35.2), (100.0, 23.9, 40.7)] {
            h.util.push(u);
            h.power.push(p);
            h.temp.push(t);
        }
        h
    }

    #[test]
    fn f64_to_u64_rounds_and_clamps() {
        assert_eq!(f64_to_u64(0.0), 0);
        assert_eq!(f64_to_u64(42.4), 42);
        assert_eq!(f64_to_u64(42.6), 43);
        assert_eq!(f64_to_u64(-5.0), 0); // negatives clamp to 0
        assert_eq!(f64_to_u64(f64::NAN), 0); // non-finite clamps to 0
        assert_eq!(f64_to_u64(f64::INFINITY), 0);
    }

    #[test]
    fn window_anchors_newest_to_the_right() {
        // Shorter than width: left-padded with zeros, newest stays at the right edge.
        assert_eq!(window(vec![1, 2, 3], 8), vec![0, 0, 0, 0, 0, 1, 2, 3]);
        // Longer than width: drops the oldest, keeps the newest `width`.
        assert_eq!(window(vec![1, 2, 3, 4, 5], 3), vec![3, 4, 5]);
        // Exactly width: untouched.
        assert_eq!(window(vec![1, 2, 3], 3), vec![1, 2, 3]);
        // Empty history: a full window of zeros (flat baseline).
        assert_eq!(window(vec![], 4), vec![0, 0, 0, 0]);
        // Zero width: nothing to show.
        assert_eq!(window(vec![1, 2, 3], 0), Vec::<u64>::new());
    }

    #[test]
    fn to_u64_preserves_order_oldest_first() {
        let h = filled();
        assert_eq!(to_u64(&h.util), vec![0, 42, 100]);
        assert_eq!(to_u64(&h.power), vec![5, 17, 24]);
        assert_eq!(to_u64(&h.temp), vec![30, 35, 41]);
    }

    #[test]
    fn titles_show_latest_value_or_na() {
        let h = filled();
        assert_eq!(util_title(&h.util), "Util 100%");
        assert_eq!(power_title(&h.power), "Power 24 W");
        assert_eq!(temp_title(&h.temp), "Temp 41°C");

        let empty = GpuHistory::new(8);
        assert_eq!(util_title(&empty.util), "Util n/a");
        assert_eq!(power_title(&empty.power), "Power n/a");
        assert_eq!(temp_title(&empty.temp), "Temp n/a");
    }

    fn render(hist: &GpuHistory) -> String {
        let mut term = Terminal::new(TestBackend::new(40, 9)).unwrap();
        term.draw(|f| render_graphs(f, f.area(), hist, &Theme::default(), false))
            .unwrap();
        let buf = term.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    #[test]
    fn renders_filled_history_with_titles() {
        let out = render(&filled());
        assert!(out.contains("History"));
        assert!(out.contains("Util"));
        assert!(out.contains("Power"));
        assert!(out.contains("Temp"));
        assert!(out.contains("100%"));
    }

    #[test]
    fn empty_history_renders_without_panic() {
        let out = render(&GpuHistory::new(8));
        assert!(out.contains("History"));
        assert!(out.contains("n/a")); // no samples yet -> titles read n/a, no panic
    }
}
