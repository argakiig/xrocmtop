//! Color theming. A [`Theme`] is a flat set of named element colors the UI reads from instead of
//! hardcoding. Three built-in presets ([`Theme::preset`]) cover common needs, and any element can
//! be overridden from the config file ([`Theme::with_overrides`]). Colors are written as named
//! (`green`, `darkgray`) or hex (`#ff8800`) strings.

use std::collections::BTreeMap;

use ratatui::style::Color;

/// Every themeable UI color. Field names double as the config keys used for overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// Panel border (unfocused).
    pub border: Color,
    /// Panel border + title for the focused panel.
    pub focus: Color,
    /// Panel titles.
    pub title: Color,
    /// General body text.
    pub text: Color,
    /// Dimmed text ("n/a", hints).
    pub dim: Color,
    /// Footer key-hint line.
    pub footer: Color,
    /// Attention color (paused, hidden-count, table header).
    pub accent: Color,
    /// Utilization bar fill.
    pub util_bar: Color,
    /// VRAM bar fill.
    pub vram_bar: Color,
    /// GTT bar fill.
    pub gtt_bar: Color,
    /// Utilization history sparkline.
    pub graph_util: Color,
    /// Power history sparkline.
    pub graph_power: Color,
    /// Temperature history sparkline.
    pub graph_temp: Color,
}

impl Theme {
    /// Names of the built-in presets, for help text and validation.
    pub const PRESETS: [&'static str; 3] = ["default", "high-contrast", "mono"];

    /// A preset by name; unknown names fall back to `default`.
    pub fn preset(name: &str) -> Self {
        match name {
            "high-contrast" => Self::high_contrast(),
            "mono" => Self::mono(),
            _ => Self::default_theme(),
        }
    }

    /// The next preset after `name`, for the theme-cycle key.
    pub fn next_preset(name: &str) -> &'static str {
        let i = Self::PRESETS.iter().position(|&p| p == name).unwrap_or(0);
        Self::PRESETS[(i + 1) % Self::PRESETS.len()]
    }

    fn default_theme() -> Self {
        Self {
            border: Color::DarkGray,
            focus: Color::Cyan,
            title: Color::White,
            text: Color::Gray,
            dim: Color::DarkGray,
            footer: Color::Gray,
            accent: Color::Yellow,
            util_bar: Color::Green,
            vram_bar: Color::Cyan,
            gtt_bar: Color::Blue,
            graph_util: Color::Green,
            graph_power: Color::Yellow,
            graph_temp: Color::Red,
        }
    }

    /// Bright, saturated colors on default background for maximum legibility.
    fn high_contrast() -> Self {
        Self {
            border: Color::White,
            focus: Color::LightCyan,
            title: Color::White,
            text: Color::White,
            dim: Color::Gray,
            footer: Color::White,
            accent: Color::LightYellow,
            util_bar: Color::LightGreen,
            vram_bar: Color::LightCyan,
            gtt_bar: Color::LightBlue,
            graph_util: Color::LightGreen,
            graph_power: Color::LightYellow,
            graph_temp: Color::LightRed,
        }
    }

    /// Monochrome — grays only, for low-distraction or limited terminals.
    fn mono() -> Self {
        Self {
            border: Color::DarkGray,
            focus: Color::White,
            title: Color::White,
            text: Color::Gray,
            dim: Color::DarkGray,
            footer: Color::Gray,
            accent: Color::White,
            util_bar: Color::Gray,
            vram_bar: Color::Gray,
            gtt_bar: Color::DarkGray,
            graph_util: Color::Gray,
            graph_power: Color::Gray,
            graph_temp: Color::DarkGray,
        }
    }

    /// Apply per-element overrides parsed from config strings. Unknown keys and unparsable colors
    /// are ignored so a bad config never breaks rendering. Returns the count applied (for tests).
    pub fn with_overrides(mut self, overrides: &BTreeMap<String, String>) -> Self {
        for (key, value) in overrides {
            if let Some(color) = parse_color(value) {
                self.set(key, color);
            }
        }
        self
    }

    /// Set one element by its config key. Unknown keys are a no-op.
    fn set(&mut self, key: &str, color: Color) {
        match key {
            "border" => self.border = color,
            "focus" => self.focus = color,
            "title" => self.title = color,
            "text" => self.text = color,
            "dim" => self.dim = color,
            "footer" => self.footer = color,
            "accent" => self.accent = color,
            "util_bar" => self.util_bar = color,
            "vram_bar" => self.vram_bar = color,
            "gtt_bar" => self.gtt_bar = color,
            "graph_util" => self.graph_util = color,
            "graph_power" => self.graph_power = color,
            "graph_temp" => self.graph_temp = color,
            _ => {}
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::default_theme()
    }
}

/// Parse a color string: a named color (case-insensitive) or `#rrggbb` hex. `None` if invalid.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some(Color::Rgb(r, g, b));
        }
        return None;
    }
    Some(match s.to_ascii_lowercase().as_str() {
        "reset" | "default" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_named_colors() {
        assert_eq!(parse_color("green"), Some(Color::Green));
        assert_eq!(parse_color("  CYAN "), Some(Color::Cyan));
        assert_eq!(parse_color("grey"), Some(Color::Gray));
        assert_eq!(parse_color("lightblue"), Some(Color::LightBlue));
    }

    #[test]
    fn parse_hex_colors() {
        assert_eq!(parse_color("#ff8800"), Some(Color::Rgb(255, 136, 0)));
        assert_eq!(parse_color("#000000"), Some(Color::Rgb(0, 0, 0)));
    }

    #[test]
    fn parse_invalid_is_none() {
        assert_eq!(parse_color("not-a-color"), None);
        assert_eq!(parse_color("#fff"), None); // wrong length
        assert_eq!(parse_color("#gggggg"), None); // non-hex
        assert_eq!(parse_color(""), None);
    }

    #[test]
    fn presets_differ() {
        assert_ne!(Theme::preset("default"), Theme::preset("high-contrast"));
        assert_ne!(Theme::preset("default"), Theme::preset("mono"));
        // Unknown falls back to default.
        assert_eq!(Theme::preset("nonsense"), Theme::preset("default"));
    }

    #[test]
    fn next_preset_cycles() {
        assert_eq!(Theme::next_preset("default"), "high-contrast");
        assert_eq!(Theme::next_preset("high-contrast"), "mono");
        assert_eq!(Theme::next_preset("mono"), "default");
        assert_eq!(Theme::next_preset("bogus"), "high-contrast"); // unknown → index 0 → next
    }

    #[test]
    fn overrides_apply_on_top_of_preset() {
        let mut o = BTreeMap::new();
        o.insert("util_bar".to_string(), "#123456".to_string());
        o.insert("title".to_string(), "magenta".to_string());
        o.insert("bogus_key".to_string(), "red".to_string()); // ignored
        o.insert("border".to_string(), "not-a-color".to_string()); // ignored (unparsable)
        let t = Theme::preset("default").with_overrides(&o);
        assert_eq!(t.util_bar, Color::Rgb(0x12, 0x34, 0x56));
        assert_eq!(t.title, Color::Magenta);
        assert_eq!(t.border, Theme::preset("default").border); // unchanged
    }
}
