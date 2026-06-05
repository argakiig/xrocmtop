//! Terminal rendering. The UI is a pure function of [`App`] state — it performs no I/O and never
//! mutates the app. Each panel lives in its own submodule; this module arranges the visible panels
//! into a responsive flow grid (see [`layout::flow_grid`]), draws the footer hint, and overlays
//! the help modal on demand.

mod gauges;
mod graphs;
mod layout;
mod metrics;
mod proc_detail;
mod processes;
mod vulkan;

use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::panel::PanelKind;

/// Top-level render entry point, called once per frame.
pub fn render(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let (body, footer) = layout::body_and_footer(frame.area());

    if app.snapshots().is_empty() {
        render_empty(frame, body, theme);
    } else {
        let visible = app.panels().visible();
        for (kind, cell) in visible.iter().zip(layout::flow_grid(body, visible.len())) {
            render_panel(frame, *kind, cell, app);
        }
    }

    frame.render_widget(footer_hint(app), footer);

    // The detail popup sits above the panels; help (if also somehow active) wins on top.
    if app.proc_detail_open() {
        if let Some(proc) = app.selected_proc() {
            proc_detail::render_proc_detail(frame, frame.area(), proc, app.theme());
        }
    }

    if app.show_help() {
        render_help(frame, frame.area(), app.theme());
    }
}

/// Centered modal listing every keybinding. Drawn over the live UI; any key dismisses it.
fn render_help(frame: &mut Frame, area: Rect, theme: &crate::theme::Theme) {
    use ratatui::widgets::Clear;

    let lines = [
        ("Tab", "focus next panel"),
        ("[ ]  ← →", "move focused panel"),
        (
            "↑ ↓ / j k",
            "Processes: select row · Metrics: scroll thermal events",
        ),
        (
            "PgUp PgDn",
            "scroll thermal events a page (Metrics focused)",
        ),
        ("Enter", "process detail (Esc / any key closes)"),
        ("1 2 3 4 5", "toggle Gauges/Graphs/Metrics/Processes/Vulkan"),
        ("t", "cycle theme"),
        ("s", "cycle process sort"),
        ("p", "pause / resume"),
        ("? ", "toggle this help"),
        ("q / Esc", "quit"),
        // Legend for the Metrics panel's throttle line and STAPM limit, whose SMU abbreviations
        // are otherwise opaque. Blank "key" rows render as spacers/headers.
        ("", ""),
        ("Metrics", "throttle & power-limit legend:"),
        ("PROCHOT", "external hot signal asserted"),
        ("SPL/STAPM", "sustained power limit"),
        ("FPPT", "fast package power limit"),
        ("SPPT", "slow package power limit"),
        ("THM_*", "thermal limit: core / gfx / soc"),
    ];
    let width = 60u16.min(area.width.saturating_sub(2));
    let height = (lines.len() as u16 + 4).min(area.height.saturating_sub(2));
    let popup = centered_rect(width, height, area);

    let body: Vec<Line> = lines
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!("  {k:<10}"), Style::default().fg(theme.accent)),
                Span::styled((*d).to_string(), Style::default().fg(theme.text)),
            ])
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.focus))
        .title(" Keys & metrics legend — read-only ")
        .title_style(Style::default().fg(theme.title));
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(body).block(block), popup);
}

/// A `w`×`h` rect centered within `area` (clamped to fit).
fn centered_rect(w: u16, h: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w.min(area.width), h.min(area.height))
}

/// Render one panel into its grid cell. Gauges and Graphs stack all GPUs inside the cell, keeping
/// the panel model independent of GPU count.
fn render_panel(frame: &mut Frame, kind: PanelKind, area: Rect, app: &App) {
    let theme = app.theme();
    let focused = app.panels().is_focused(kind);
    match kind {
        PanelKind::Gauges => {
            let snaps = app.snapshots();
            for (snap, row) in snaps.iter().zip(layout::gpu_rows(area, snaps.len())) {
                gauges::render_gpu(frame, row, snap, theme, focused);
            }
        }
        PanelKind::Graphs => {
            let hist = app.history();
            for (h, row) in hist.iter().zip(layout::gpu_rows(area, hist.len().max(1))) {
                graphs::render_graphs(frame, row, h, theme, focused);
            }
        }
        PanelKind::Metrics => {
            let snaps = app.snapshots();
            // Reserve a strip at the bottom of the cell for the scrollable thermal-events log.
            let (metrics_area, events_area) = layout::metrics_split(area);
            for (snap, row) in snaps
                .iter()
                .zip(layout::gpu_rows(metrics_area, snaps.len()))
            {
                metrics::render_metrics(frame, row, snap, theme, focused);
            }
            if let Some(events_area) = events_area {
                metrics::render_thermal_events(
                    frame,
                    events_area,
                    &app.thermal_events(),
                    app.events_scroll(),
                    snaps.len() > 1,
                    theme,
                    focused,
                );
            }
        }
        PanelKind::Processes => processes::render_processes(
            frame,
            area,
            app.procs(),
            app.procs_hidden(),
            app.proc_selected(),
            theme,
            focused,
        ),
        PanelKind::Vulkan => vulkan::render_vulkan(frame, area, app.vulkan(), theme, focused),
    }
}

/// Build the footer key-hint line, reflecting pause state, sort, and active theme.
fn footer_hint(app: &App) -> Paragraph<'static> {
    let theme = app.theme();
    let mut keys = vec![Span::styled("? help", Style::default().fg(theme.accent))];
    keys.push(sep(theme));
    keys.push(Span::raw("Tab focus"));
    keys.push(sep(theme));
    keys.push(if app.paused() {
        Span::styled("p resume", Style::default().fg(theme.accent))
    } else {
        Span::raw("p pause")
    });
    if app.show_procs() {
        keys.push(sep(theme));
        keys.push(Span::raw(format!("s sort:{}", app.proc_sort().label())));
    }
    keys.push(sep(theme));
    keys.push(Span::raw(format!("t theme:{}", app.theme_name())));
    keys.push(sep(theme));
    keys.push(Span::raw("q quit"));
    keys.push(sep(theme));
    keys.push(Span::styled("read-only", Style::default().fg(theme.dim)));
    if app.paused() {
        keys.insert(
            0,
            Span::styled("[PAUSED] ", Style::default().fg(theme.accent)),
        );
    }
    Paragraph::new(Line::from(keys))
        .style(Style::default().fg(theme.footer))
        .alignment(Alignment::Right)
}

fn sep(theme: &crate::theme::Theme) -> Span<'static> {
    Span::styled(" · ", Style::default().fg(theme.dim))
}

/// Shown when enumeration found no amdgpu cards (or `--gpu` matched none).
fn render_empty(frame: &mut Frame, area: Rect, theme: &crate::theme::Theme) {
    let msg = Paragraph::new("No AMD GPUs detected.\n\nWaiting for the first sample…")
        .alignment(Alignment::Center)
        .style(Style::default().fg(theme.text))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border))
                .title("xrocmtop")
                .title_style(Style::default().fg(theme.title)),
        );
    frame.render_widget(msg, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render_help_text() -> String {
        let mut term = Terminal::new(TestBackend::new(70, 24)).unwrap();
        term.draw(|f| render_help(f, f.area(), &Theme::default()))
            .unwrap();
        let buf = term.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn help_overlay_includes_metrics_throttle_legend() {
        let out = render_help_text();
        // The keybindings are still present...
        assert!(out.contains("focus next panel"));
        // ...and the Metrics throttle/limit legend is now shown alongside them.
        assert!(out.contains("PROCHOT"));
        assert!(out.contains("FPPT"));
        assert!(out.contains("SPPT"));
        assert!(out.contains("sustained power limit")); // SPL/STAPM
        assert!(out.contains("thermal limit")); // THM_*
    }
}
