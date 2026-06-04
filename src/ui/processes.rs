//! Per-process GPU table: one row per process holding an amdgpu handle, sorted by attributed GPU
//! memory descending. A trailing note reports how many processes were hidden behind permissions.
//!
//! Like the other panels this is a pure function of its inputs (`&[ProcInfo]` + hidden count) and
//! never panics: missing memory or gfx values render as "n/a".

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Row, Table};
use ratatui::Frame;

use crate::model::{fmt_bytes, ProcInfo};
use crate::theme::Theme;

/// Render the process table into `area`.
///
/// `procs` is assumed pre-sorted by the caller; this function does not re-sort, so the on-screen
/// order matches whatever ordering policy was applied upstream.
#[allow(clippy::too_many_arguments)]
pub fn render_processes(
    frame: &mut Frame,
    area: Rect,
    procs: &[ProcInfo],
    hidden: usize,
    selected: usize,
    theme: &Theme,
    focused: bool,
) {
    let border = if focused { theme.focus } else { theme.border };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(title(procs.len(), hidden))
        .title_style(
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        );

    if procs.is_empty() {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let msg = ratatui::widgets::Paragraph::new(empty_message(hidden, theme))
            .style(Style::default().fg(theme.dim));
        frame.render_widget(msg, inner);
        return;
    }

    // Pick the columns that fit the inner width (borders take one cell per side), dropping the
    // optional ones in priority order so the essentials (pid, name, VRAM) always survive.
    let cols = columns_for(area.width.saturating_sub(2));

    let header = Row::new(cols.iter().map(|c| c.header())).style(
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    );
    let rows = procs
        .iter()
        .enumerate()
        .map(|(i, p)| process_row(p, &cols, i == selected, theme));

    let constraints: Vec<Constraint> = cols.iter().map(Col::constraint).collect();
    let table = Table::new(rows, constraints)
        .header(header)
        .block(block)
        .column_spacing(1);
    frame.render_widget(table, area);
}

/// A process-table column. Ordering in [`ALL_COLS`] is the on-screen left-to-right order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Pid,
    Name,
    Vram,
    Gtt,
    Gfx,
    Com,
}

/// Full set of columns, left to right.
const ALL_COLS: [Col; 6] = [Col::Pid, Col::Name, Col::Vram, Col::Gtt, Col::Gfx, Col::Com];

/// Optional columns in the order they are dropped as the panel narrows: GTT first (VRAM is the
/// headline pool), then compute, then graphics.
const DROP_ORDER: [Col; 3] = [Col::Gtt, Col::Com, Col::Gfx];

impl Col {
    fn header(self) -> &'static str {
        match self {
            Col::Pid => "PID",
            Col::Name => "Process",
            Col::Vram => "VRAM",
            Col::Gtt => "GTT",
            Col::Gfx => "GFX",
            Col::Com => "COM",
        }
    }

    /// Minimum width this column needs. The name column flexes (`Min`); the rest are fixed.
    fn width(self) -> u16 {
        match self {
            Col::Pid => 7,
            Col::Name => 10,
            Col::Vram | Col::Gtt => 10,
            Col::Gfx | Col::Com => 4,
        }
    }

    fn constraint(&self) -> Constraint {
        match self {
            Col::Name => Constraint::Min(self.width()),
            other => Constraint::Length(other.width()),
        }
    }

    fn cell(self, p: &ProcInfo) -> String {
        match self {
            Col::Pid => p.pid.to_string(),
            Col::Name => p.name.clone(),
            Col::Vram => fmt_bytes(p.vram_bytes),
            Col::Gtt => fmt_bytes(p.gtt_bytes),
            Col::Gfx => pct_label(p.gfx_pct),
            Col::Com => pct_label(p.compute_pct),
        }
    }
}

/// Total width a column set needs: each column's min width plus one-cell gaps between them.
fn required_width(cols: &[Col]) -> u16 {
    let sum: u16 = cols.iter().map(|c| c.width()).sum();
    let gaps = cols.len().saturating_sub(1) as u16;
    sum + gaps
}

/// Choose which columns fit `inner_width`, dropping optional ones in [`DROP_ORDER`] until the set
/// fits (or only the essentials remain).
fn columns_for(inner_width: u16) -> Vec<Col> {
    let mut cols: Vec<Col> = ALL_COLS.to_vec();
    for drop in DROP_ORDER {
        if required_width(&cols) <= inner_width {
            break;
        }
        cols.retain(|c| *c != drop);
    }
    cols
}

fn title(shown: usize, hidden: usize) -> String {
    if hidden > 0 {
        format!(" GPU Processes ({shown}, +{hidden} hidden) ")
    } else {
        format!(" GPU Processes ({shown}) ")
    }
}

/// The "+N hidden (needs elevation)" note, shown as a hint line when the list is empty and also
/// embedded in the title otherwise.
fn empty_message(hidden: usize, theme: &Theme) -> Line<'static> {
    if hidden > 0 {
        Line::from(vec![
            Span::raw("No readable GPU processes. "),
            Span::styled(
                format!("+{hidden} hidden (needs elevation)"),
                Style::default().fg(theme.accent),
            ),
        ])
    } else {
        Line::from("No GPU processes.")
    }
}

fn process_row(p: &ProcInfo, cols: &[Col], selected: bool, theme: &Theme) -> Row<'static> {
    let mut style = Style::default().fg(theme.text);
    if selected {
        // Reverse video reads as a highlight under any theme without needing a `bg` color.
        style = style.add_modifier(Modifier::REVERSED);
    }
    Row::new(
        cols.iter()
            .map(|c| Cell::from(c.cell(p)))
            .collect::<Vec<_>>(),
    )
    .style(style)
}

fn pct_label(pct: Option<u8>) -> String {
    match pct {
        Some(p) => format!("{p}%"),
        None => "n/a".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use ratatui::buffer::Buffer;

    /// Build a process row. `vram`/`gtt` feed the split memory columns; `gfx`/`com` the engine
    /// columns.
    fn proc(
        pid: u32,
        name: &str,
        vram: Option<u64>,
        gtt: Option<u64>,
        gfx: Option<u8>,
        com: Option<u8>,
    ) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.to_string(),
            vram_bytes: vram,
            gtt_bytes: gtt,
            gfx_pct: gfx,
            compute_pct: com,
            ..Default::default()
        }
    }

    fn draw(procs: &[ProcInfo], hidden: usize, selected: usize, w: u16) -> Buffer {
        let mut term = Terminal::new(TestBackend::new(w, 10)).unwrap();
        term.draw(|f| {
            render_processes(
                f,
                f.area(),
                procs,
                hidden,
                selected,
                &Theme::default(),
                true,
            )
        })
        .unwrap();
        term.backend().buffer().clone()
    }

    fn text(buf: &Buffer) -> String {
        buf.content().iter().map(|c| c.symbol()).collect()
    }

    /// True if the buffer row containing `needle` carries the REVERSED highlight modifier.
    fn row_with_is_reversed(buf: &Buffer, needle: &str) -> bool {
        let w = buf.area().width;
        let h = buf.area().height;
        for y in 0..h {
            let line: String = (0..w)
                .map(|x| buf.content()[(y * w + x) as usize].symbol())
                .collect();
            if line.contains(needle) {
                return (0..w).any(|x| {
                    buf.content()[(y * w + x) as usize]
                        .modifier
                        .contains(Modifier::REVERSED)
                });
            }
        }
        false
    }

    fn render(procs: &[ProcInfo], hidden: usize) -> String {
        text(&draw(procs, hidden, 0, 80))
    }

    #[test]
    fn wide_table_shows_all_columns_and_split_memory() {
        let procs = [
            proc(
                645226,
                "llama-server",
                Some(31_086_206_976),
                Some(395_148 * 1024),
                None,
                Some(91),
            ),
            proc(
                1248555,
                "sd-server",
                Some(7_086_080_000),
                None,
                Some(42),
                None,
            ),
        ];
        let out = render(&procs, 0);
        assert!(out.contains("GPU Processes"));
        // All six column headers present at full width.
        for h in ["PID", "Process", "VRAM", "GTT", "GFX", "COM"] {
            assert!(out.contains(h), "missing header {h}");
        }
        assert!(out.contains("645226"));
        assert!(out.contains("llama-server"));
        assert!(out.contains("GiB")); // memory humanized in the VRAM/GTT columns
        assert!(out.contains("42%")); // gfx for the second row
        assert!(out.contains("91%")); // compute for the first row
        assert!(out.contains("n/a")); // absent engine/memory renders as n/a
    }

    #[test]
    fn narrow_table_drops_optional_columns_without_panic() {
        let procs = [proc(1, "p", Some(1024), Some(1024), Some(10), Some(20))];
        // Wide: GTT and COM visible.
        let wide = text(&draw(&procs, 0, 0, 80));
        assert!(wide.contains("GTT"));
        assert!(wide.contains("COM"));
        // Narrow: optional columns drop in order (GTT first, then COM), essentials remain.
        let narrow = text(&draw(&procs, 0, 0, 34));
        assert!(narrow.contains("PID"));
        assert!(narrow.contains("Process"));
        assert!(narrow.contains("VRAM"));
        assert!(!narrow.contains("GTT"), "GTT should drop first when narrow");
    }

    #[test]
    fn columns_for_drops_in_priority_order() {
        // Full set fits a wide panel.
        assert_eq!(columns_for(80), ALL_COLS.to_vec());
        // As width shrinks, GTT goes first, then COM, then GFX — VRAM/PID/Name always survive.
        assert!(!columns_for(48).contains(&Col::Gtt));
        let very_narrow = columns_for(20);
        assert!(very_narrow.contains(&Col::Pid));
        assert!(very_narrow.contains(&Col::Name));
        assert!(very_narrow.contains(&Col::Vram));
        assert!(!very_narrow.contains(&Col::Gtt));
        assert!(!very_narrow.contains(&Col::Com));
        assert!(!very_narrow.contains(&Col::Gfx));
    }

    #[test]
    fn selected_row_is_highlighted() {
        let procs = [
            proc(645226, "llama-server", Some(1024), None, None, None),
            proc(1248555, "sd-server", Some(1024), None, None, None),
        ];
        let buf = draw(&procs, 0, 1, 80); // select the second row
        assert!(
            row_with_is_reversed(&buf, "1248555"),
            "selected row highlighted"
        );
        assert!(
            !row_with_is_reversed(&buf, "645226"),
            "unselected row not highlighted"
        );
    }

    #[test]
    fn empty_list_with_hidden_shows_elevation_note() {
        let out = render(&[], 5);
        assert!(out.contains("No readable GPU processes"));
        assert!(out.contains("+5 hidden (needs elevation)"));
    }

    #[test]
    fn empty_list_no_hidden_renders_without_panic() {
        let out = render(&[], 0);
        assert!(out.contains("GPU Processes (0)"));
        assert!(out.contains("No GPU processes."));
        assert!(!out.contains("hidden"));
    }

    #[test]
    fn hidden_count_appears_in_title() {
        let procs = [proc(1, "x", Some(1024), None, None, None)];
        let out = render(&procs, 3);
        assert!(out.contains("+3 hidden"));
    }

    #[test]
    fn missing_memory_renders_na_not_panic() {
        let procs = [proc(7, "ghost", None, None, None, None)];
        let out = render(&procs, 0);
        assert!(out.contains("ghost"));
        assert!(out.contains("n/a"));
    }
}
