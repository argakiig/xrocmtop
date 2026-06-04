//! Per-process detail popup. Opened from the Processes panel (`Enter` on the selected row) and
//! dismissed by any key. Like the other panels it is a pure function of its input [`ProcInfo`] and
//! never panics: every absent field renders as "n/a".

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::model::{fmt_bytes, ProcInfo};
use crate::theme::Theme;

/// Draw the detail popup for `proc`, centered over `area`.
pub fn render_proc_detail(frame: &mut Frame, area: Rect, proc: &ProcInfo, theme: &Theme) {
    let lines = detail_lines(proc, theme);
    // Tall enough for the content (clamped to the screen), wide enough to be readable.
    let width = 72u16.min(area.width.saturating_sub(2));
    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let popup = centered_rect(width, height, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.focus))
        .title(format!(" Process {} — {} ", proc.pid, proc.name))
        .title_style(
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(ratatui::widgets::Wrap { trim: true }),
        popup,
    );
}

/// Build the popup body: command line, memory pools, engine utilization, and a per-DRM-client
/// memory breakdown. Pure so it can be unit-tested without a terminal.
fn detail_lines(proc: &ProcInfo, theme: &Theme) -> Vec<Line<'static>> {
    let label = |s: &str| Span::styled(format!("{s:<9}"), Style::default().fg(theme.accent));
    let value = |s: String| Span::styled(s, Style::default().fg(theme.text));
    let mut lines = vec![
        Line::from(vec![
            label("Command"),
            value(proc.cmdline.clone().unwrap_or_else(|| "n/a".to_string())),
        ]),
        Line::from(""),
        Line::from(vec![
            label("Memory"),
            value(format!(
                "{} total · VRAM {} · GTT {}",
                fmt_bytes(proc.mem_bytes),
                fmt_bytes(proc.vram_bytes),
                fmt_bytes(proc.gtt_bytes),
            )),
        ]),
        Line::from(vec![
            label("Engines"),
            value(format!(
                "GFX {} · Compute {} · Encode {} · Decode {}",
                pct(proc.gfx_pct),
                pct(proc.compute_pct),
                pct(proc.enc_pct),
                pct(proc.dec_pct),
            )),
        ]),
    ];

    if !proc.clients.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(label(&format!(
            "Clients ({})",
            proc.clients.len()
        ))));
        for c in &proc.clients {
            lines.push(Line::from(value(format!(
                "  id {} — VRAM {} · GTT {}",
                c.client_id,
                fmt_bytes(c.vram_bytes),
                fmt_bytes(c.gtt_bytes),
            ))));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "any key to close",
        Style::default().fg(theme.dim),
    )));
    lines
}

fn pct(p: Option<u8>) -> String {
    match p {
        Some(v) => format!("{v}%"),
        None => "n/a".to_string(),
    }
}

/// A `w`×`h` rect centered within `area` (clamped to fit). Mirrors the help modal's geometry.
fn centered_rect(w: u16, h: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w.min(area.width), h.min(area.height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProcClient;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(proc: &ProcInfo) -> String {
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| render_proc_detail(f, f.area(), proc, &Theme::default()))
            .unwrap();
        let buf = term.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect()
    }

    fn sample() -> ProcInfo {
        ProcInfo {
            pid: 693842,
            name: "llama-server".into(),
            cmdline: Some("/usr/bin/llama-server --model big.gguf --ctx 8192".into()),
            mem_bytes: Some(31_086_206_976),
            vram_bytes: Some(30_000_000_000),
            gtt_bytes: Some(1_086_206_976),
            gfx_pct: Some(73),
            compute_pct: Some(12),
            enc_pct: None,
            dec_pct: Some(5),
            clients: vec![
                ProcClient {
                    client_id: 196416,
                    vram_bytes: Some(20_000_000_000),
                    gtt_bytes: Some(500_000_000),
                },
                ProcClient {
                    client_id: 196417,
                    vram_bytes: Some(10_000_000_000),
                    gtt_bytes: None,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn shows_cmdline_memory_and_all_engines() {
        let out = render(&sample());
        assert!(out.contains("Process 693842"));
        assert!(out.contains("llama-server"));
        assert!(out.contains("big.gguf")); // full command line
        assert!(out.contains("VRAM"));
        assert!(out.contains("GiB"));
        // All four engines labeled; present ones show %, absent show n/a.
        assert!(out.contains("73%")); // gfx
        assert!(out.contains("12%")); // compute
        assert!(out.contains("5%")); // decode
        assert!(out.contains("Encode n/a")); // enc absent
    }

    #[test]
    fn lists_per_client_breakdown() {
        let out = render(&sample());
        assert!(out.contains("Clients (2)"));
        assert!(out.contains("id 196416"));
        assert!(out.contains("id 196417"));
    }

    #[test]
    fn absent_cmdline_renders_na_not_panic() {
        let mut p = sample();
        p.cmdline = None;
        p.clients.clear();
        let out = render(&p);
        assert!(out.contains("Command"));
        assert!(out.contains("n/a"));
        // No clients section when there is no breakdown.
        assert!(!out.contains("Clients ("));
    }
}
