//! Frame layout helpers. Splitting logic lives here so it can be unit-tested independently of
//! the widgets that fill the resulting rects.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Split the full frame into a body area and a single-line footer.
pub fn body_and_footer(area: Rect) -> (Rect, Rect) {
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    (parts[0], parts[1])
}

/// At/above this width the panel grid uses two columns; below it, a single column.
const WIDE_COLS: u16 = 100;

/// Lay out `n` panels into a responsive flow grid filling `area` row-major. Two columns when the
/// area is wide, one when narrow. A final row with a single panel spans the full width. Returns
/// exactly `n` rects in panel order; `n == 0` yields an empty vec.
pub fn flow_grid(area: Rect, n: usize) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let cols = if area.width >= WIDE_COLS { 2 } else { 1 };
    let rows = n.div_ceil(cols);
    let row_rects = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Ratio(1, rows as u32); rows])
        .split(area);

    let mut cells = Vec::with_capacity(n);
    for (r, row) in row_rects.iter().enumerate() {
        let in_row = (n - r * cols).min(cols);
        let col_rects = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Ratio(1, in_row as u32); in_row])
            .split(*row);
        cells.extend(col_rects.iter().copied());
    }
    cells
}

/// Divide a panel cell into `n` equal horizontal rows, one per GPU. `n` is assumed ≥ 1.
pub fn gpu_rows(area: Rect, n: usize) -> Vec<Rect> {
    let n = n.max(1);
    let constraints = vec![Constraint::Ratio(1, n as u32); n];
    Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area)
        .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0, 0, 80, 24)
    }

    #[test]
    fn footer_is_one_line_below_body() {
        let (body, footer) = body_and_footer(area());
        assert_eq!(footer.height, 1);
        assert_eq!(body.height, 23);
        assert_eq!(body.y, 0);
        assert_eq!(footer.y, 23);
    }

    #[test]
    fn gpu_rows_partition_height() {
        let rows = gpu_rows(Rect::new(0, 0, 80, 24), 3);
        assert_eq!(rows.len(), 3);
        let total: u16 = rows.iter().map(|r| r.height).sum();
        assert_eq!(total, 24); // no height lost
    }

    #[test]
    fn gpu_rows_handles_single() {
        let rows = gpu_rows(area(), 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].height, 24);
    }

    #[test]
    fn flow_grid_empty_is_empty() {
        assert!(flow_grid(area(), 0).is_empty());
    }

    #[test]
    fn flow_grid_wide_uses_two_columns() {
        // 4 panels, wide area → 2x2; every cell non-empty, two distinct columns and rows.
        let cells = flow_grid(Rect::new(0, 0, 120, 40), 4);
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[0].y, cells[1].y); // first row shares y
        assert_ne!(cells[0].x, cells[1].x); // ...different columns
        assert!(cells[2].y > cells[0].y); // second row below
    }

    #[test]
    fn flow_grid_narrow_is_single_column() {
        let cells = flow_grid(Rect::new(0, 0, 60, 40), 3);
        assert_eq!(cells.len(), 3);
        // All share the same x and full width (single column).
        assert!(cells.iter().all(|c| c.x == cells[0].x && c.width == 60));
    }

    #[test]
    fn flow_grid_odd_last_row_spans_full_width() {
        // 3 panels, wide → row0 has 2 cols, row1 has a single full-width cell.
        let cells = flow_grid(Rect::new(0, 0, 120, 40), 3);
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[2].width, 120); // lone last panel spans the row
    }

    #[test]
    fn flow_grid_width_threshold_picks_columns() {
        // Exactly at WIDE_COLS (100) → two columns: first two cells share a row, differ in x.
        let wide = flow_grid(Rect::new(0, 0, 100, 40), 4);
        assert_eq!(wide.len(), 4);
        assert_eq!(wide[0].y, wide[1].y);
        assert_ne!(wide[0].x, wide[1].x);
        // One below the threshold (99) → single column: all share x and span the full width.
        let narrow = flow_grid(Rect::new(0, 0, 99, 40), 4);
        assert_eq!(narrow.len(), 4);
        assert!(narrow.iter().all(|c| c.x == narrow[0].x && c.width == 99));
    }

    #[test]
    fn flow_grid_tiny_area_does_not_panic() {
        // A 1x1 area still yields exactly n rects; cells may be zero-sized, which is fine.
        let cells = flow_grid(Rect::new(0, 0, 1, 1), 4);
        assert_eq!(cells.len(), 4);
    }

    #[test]
    fn flow_grid_single_panel_spans_full_width() {
        let cells = flow_grid(Rect::new(0, 0, 120, 40), 1);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].width, 120);
    }
}
