//! Panel model: the four toggleable, reorderable panels and their layout state.
//!
//! [`PanelLayout`] owns the display order, the hidden set, and which panel is focused. It is built
//! from (and serialized back to) the string identifiers in [`crate::settings::Settings`], so a
//! hand-edited or older config is reconciled rather than trusted: every panel always appears
//! exactly once in the order, unknown ids are dropped, and focus stays on a visible panel.

use std::collections::HashSet;

/// One of the four panels the UI can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelKind {
    Gauges,
    Graphs,
    Processes,
    Vulkan,
}

impl PanelKind {
    /// All panels in their canonical default order.
    pub const ALL: [PanelKind; 4] = [
        PanelKind::Gauges,
        PanelKind::Graphs,
        PanelKind::Processes,
        PanelKind::Vulkan,
    ];

    /// Stable identifier used in the config file.
    pub fn id(self) -> &'static str {
        match self {
            PanelKind::Gauges => "gauges",
            PanelKind::Graphs => "graphs",
            PanelKind::Processes => "processes",
            PanelKind::Vulkan => "vulkan",
        }
    }

    /// Parse a config identifier.
    pub fn from_id(s: &str) -> Option<Self> {
        PanelKind::ALL.into_iter().find(|k| k.id() == s)
    }
}

/// Display order + visibility + focus for the panels.
#[derive(Debug, Clone)]
pub struct PanelLayout {
    /// Every panel exactly once, in display order.
    order: Vec<PanelKind>,
    /// Panels currently hidden.
    hidden: HashSet<PanelKind>,
    /// Index into [`PanelLayout::visible`] of the focused panel.
    focused: usize,
}

impl PanelLayout {
    /// Build from config string identifiers, reconciling to a valid state: parsed order first
    /// (de-duplicated), then any missing panels appended in canonical order; unknown ids ignored.
    pub fn from_settings(order: &[String], hidden: &[String]) -> Self {
        let mut ordered: Vec<PanelKind> = Vec::new();
        for id in order {
            if let Some(k) = PanelKind::from_id(id) {
                if !ordered.contains(&k) {
                    ordered.push(k);
                }
            }
        }
        for k in PanelKind::ALL {
            if !ordered.contains(&k) {
                ordered.push(k);
            }
        }
        let hidden: HashSet<PanelKind> = hidden
            .iter()
            .filter_map(|s| PanelKind::from_id(s))
            .collect();
        Self {
            order: ordered,
            hidden,
            focused: 0,
        }
    }

    /// Visible panels in display order.
    pub fn visible(&self) -> Vec<PanelKind> {
        self.order
            .iter()
            .copied()
            .filter(|k| !self.hidden.contains(k))
            .collect()
    }

    /// The currently focused panel, if any are visible.
    pub fn focused_kind(&self) -> Option<PanelKind> {
        self.visible().get(self.focused).copied()
    }

    /// Whether `kind` is the focused panel.
    pub fn is_focused(&self, kind: PanelKind) -> bool {
        self.focused_kind() == Some(kind)
    }

    /// Hide `kind`. Hiding the focused panel keeps focus in range.
    pub fn hide(&mut self, kind: PanelKind) {
        self.hidden.insert(kind);
        self.clamp_focus();
    }

    /// Show `kind`.
    pub fn show(&mut self, kind: PanelKind) {
        self.hidden.remove(&kind);
        self.clamp_focus();
    }

    /// Toggle `kind`'s visibility.
    pub fn toggle(&mut self, kind: PanelKind) {
        if self.hidden.contains(&kind) {
            self.show(kind);
        } else {
            self.hide(kind);
        }
    }

    /// Move focus to the next visible panel (wrapping).
    pub fn focus_next(&mut self) {
        let n = self.visible().len();
        if n > 0 {
            self.focused = (self.focused + 1) % n;
        }
    }

    /// Move the focused panel one slot earlier (`-1`) or later (`+1`) in the display order,
    /// swapping with its visible neighbor. Focus follows the moved panel.
    pub fn move_focused(&mut self, delta: i32) {
        let vis = self.visible();
        let n = vis.len();
        if n < 2 {
            return;
        }
        let from = self.focused;
        let to = from as i32 + delta;
        if to < 0 || to as usize >= n {
            return; // already at an edge
        }
        let to = to as usize;
        // Translate visible indices to positions in `order`, then swap there. A visible key is
        // always present in `order` today, but treat a missing one as a no-op rather than panicking
        // so a future refactor that diverges `visible`/`order` degrades safely.
        let (Some(a), Some(b)) = (
            self.order.iter().position(|k| *k == vis[from]),
            self.order.iter().position(|k| *k == vis[to]),
        ) else {
            return;
        };
        self.order.swap(a, b);
        self.focused = to;
    }

    /// Serialize back to config identifiers: `(order, hidden)`.
    pub fn to_settings(&self) -> (Vec<String>, Vec<String>) {
        let order = self.order.iter().map(|k| k.id().to_string()).collect();
        let hidden = self
            .order
            .iter()
            .filter(|k| self.hidden.contains(k))
            .map(|k| k.id().to_string())
            .collect();
        (order, hidden)
    }

    /// Keep `focused` pointing at a valid visible index.
    fn clamp_focus(&mut self) {
        let n = self.visible().len();
        if n == 0 {
            self.focused = 0;
        } else if self.focused >= n {
            self.focused = n - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_settings_reconciles_order_and_unknowns() {
        let layout = PanelLayout::from_settings(
            &["vulkan".into(), "bogus".into(), "vulkan".into()],
            &["graphs".into(), "nope".into()],
        );
        // vulkan first (from config), then the rest in canonical order; dupes/unknowns dropped.
        assert_eq!(
            layout.order,
            vec![
                PanelKind::Vulkan,
                PanelKind::Gauges,
                PanelKind::Graphs,
                PanelKind::Processes
            ]
        );
        // graphs hidden; "nope" ignored.
        assert!(!layout.visible().contains(&PanelKind::Graphs));
        assert!(layout.visible().contains(&PanelKind::Vulkan));
    }

    #[test]
    fn default_order_when_empty() {
        let layout = PanelLayout::from_settings(&[], &[]);
        assert_eq!(layout.order, PanelKind::ALL.to_vec());
        assert_eq!(layout.visible().len(), 4);
        assert_eq!(layout.focused_kind(), Some(PanelKind::Gauges));
    }

    #[test]
    fn toggle_hides_and_shows() {
        let mut layout = PanelLayout::from_settings(&[], &[]);
        layout.toggle(PanelKind::Processes);
        assert!(!layout.visible().contains(&PanelKind::Processes));
        layout.toggle(PanelKind::Processes);
        assert!(layout.visible().contains(&PanelKind::Processes));
    }

    #[test]
    fn focus_next_wraps_over_visible() {
        let mut layout = PanelLayout::from_settings(&[], &["graphs".into(), "vulkan".into()]);
        // visible: [Gauges, Processes]
        assert_eq!(layout.focused_kind(), Some(PanelKind::Gauges));
        layout.focus_next();
        assert_eq!(layout.focused_kind(), Some(PanelKind::Processes));
        layout.focus_next();
        assert_eq!(layout.focused_kind(), Some(PanelKind::Gauges));
    }

    #[test]
    fn move_focused_swaps_order() {
        let mut layout = PanelLayout::from_settings(&[], &[]);
        // focus Gauges (idx 0), move later → swaps with Graphs.
        layout.move_focused(1);
        assert_eq!(layout.visible()[0], PanelKind::Graphs);
        assert_eq!(layout.visible()[1], PanelKind::Gauges);
        assert_eq!(layout.focused_kind(), Some(PanelKind::Gauges)); // focus follows
                                                                    // at edge: moving earlier from idx 0 is a no-op when already first
        layout.move_focused(-1); // Gauges back to idx 0
        assert_eq!(layout.visible()[0], PanelKind::Gauges);
    }

    #[test]
    fn move_focused_skips_hidden_panel() {
        // order=[Gauges, Graphs(hidden), Processes, Vulkan]; visible=[Gauges, Processes, Vulkan].
        let mut layout = PanelLayout::from_settings(&[], &["graphs".into()]);
        assert_eq!(layout.focused_kind(), Some(PanelKind::Gauges));
        // Moving Gauges later swaps it with its visible neighbor (Processes), across the hidden gap.
        layout.move_focused(1);
        assert_eq!(
            layout.order,
            vec![
                PanelKind::Processes,
                PanelKind::Graphs,
                PanelKind::Gauges,
                PanelKind::Vulkan
            ]
        );
        assert_eq!(
            layout.visible(),
            vec![PanelKind::Processes, PanelKind::Gauges, PanelKind::Vulkan]
        );
        assert_eq!(layout.focused_kind(), Some(PanelKind::Gauges)); // focus follows the move
    }

    #[test]
    fn move_focused_is_noop_at_edges() {
        let mut layout = PanelLayout::from_settings(&[], &[]);
        // Focus first panel; moving earlier does nothing.
        assert_eq!(layout.focused_kind(), Some(PanelKind::Gauges));
        layout.move_focused(-1);
        assert_eq!(layout.order, PanelKind::ALL.to_vec());
        assert_eq!(layout.focused_kind(), Some(PanelKind::Gauges));
        // Focus the last visible panel; moving later does nothing.
        layout.focused = layout.visible().len() - 1;
        assert_eq!(layout.focused_kind(), Some(PanelKind::Vulkan));
        layout.move_focused(1);
        assert_eq!(layout.order, PanelKind::ALL.to_vec());
        assert_eq!(layout.focused_kind(), Some(PanelKind::Vulkan));
    }

    #[test]
    fn clamp_focus_handles_hiding_last_and_all() {
        let mut layout = PanelLayout::from_settings(&[], &[]);
        // Focus the last visible panel, then hide it → focus clamps to the new last.
        layout.focused = layout.visible().len() - 1;
        assert_eq!(layout.focused_kind(), Some(PanelKind::Vulkan));
        layout.hide(PanelKind::Vulkan);
        assert_eq!(layout.focused_kind(), Some(PanelKind::Processes));
        // Hide everything → no focused panel, and no panic.
        layout.hide(PanelKind::Gauges);
        layout.hide(PanelKind::Graphs);
        layout.hide(PanelKind::Processes);
        assert!(layout.visible().is_empty());
        assert_eq!(layout.focused_kind(), None);
        // Show one back → focus is valid again.
        layout.show(PanelKind::Graphs);
        assert_eq!(layout.focused_kind(), Some(PanelKind::Graphs));
    }

    #[test]
    fn to_settings_roundtrips_through_from_settings() {
        let mut layout = PanelLayout::from_settings(&[], &[]);
        layout.move_focused(1); // Graphs, Gauges, Processes, Vulkan
        layout.hide(PanelKind::Vulkan);
        let (order, hidden) = layout.to_settings();
        let restored = PanelLayout::from_settings(&order, &hidden);
        assert_eq!(restored.order, layout.order);
        assert_eq!(restored.visible(), layout.visible());
    }
}
