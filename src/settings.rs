//! Persisted user settings: theme choice, per-element color overrides, and panel layout. Stored
//! as TOML at `$XDG_CONFIG_HOME/xrocmtop/config.toml` (falling back to `~/.config/...`).
//!
//! Loading and saving are total and best-effort: a missing/invalid file yields defaults, and a
//! failed write is silently ignored — customization is a convenience, never a hard dependency.
//! Panel identifiers are kept as strings here (the on-disk schema); the UI maps them to its panel
//! enum so an unknown panel name in a hand-edited config can't crash anything.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::theme::Theme;

/// The full persisted configuration. `#[serde(default)]` makes every field optional on disk, so a
/// partial or older config still loads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Preset name: one of [`Theme::PRESETS`]. Unknown → `default`.
    pub theme: String,
    /// Per-element color overrides (element key → color string). Applied atop the preset.
    pub colors: BTreeMap<String, String>,
    /// Panel display order, by identifier. Unknown/missing entries are reconciled by the UI.
    pub order: Vec<String>,
    /// Panels hidden from the layout, by identifier.
    pub hidden: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "default".to_string(),
            colors: BTreeMap::new(),
            order: Vec::new(),
            hidden: Vec::new(),
        }
    }
}

impl Settings {
    /// Load settings from the default config path, or return defaults if absent/unreadable/invalid.
    pub fn load() -> Self {
        match config_path() {
            Some(path) => Self::load_from(&path),
            None => Self::default(),
        }
    }

    /// Load from an explicit path; defaults on any error (testable).
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Save to the default config path, creating the directory if needed. Errors are returned for
    /// tests but callers may ignore them — failing to persist is non-fatal.
    pub fn save(&self) -> std::io::Result<()> {
        match config_path() {
            Some(path) => self.save_to(&path),
            None => Ok(()),
        }
    }

    /// Save to an explicit path, creating parent directories (testable). The write is atomic: the
    /// serialized config goes to a sibling temp file which is then renamed over `path`, so a crash
    /// mid-write leaves the existing config intact rather than truncated (rename is atomic on the
    /// same filesystem, which the sibling temp guarantees).
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Sibling temp keyed by pid so concurrent saves don't clobber each other's temp file.
        let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    /// Resolve the active [`Theme`] from the preset name plus color overrides.
    pub fn resolve_theme(&self) -> Theme {
        Theme::preset(&self.theme).with_overrides(&self.colors)
    }
}

/// `$XDG_CONFIG_HOME/xrocmtop/config.toml`, or `$HOME/.config/xrocmtop/config.toml`.
/// `None` only if neither environment variable is set.
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("xrocmtop").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    /// A unique temp path for a round-trip test (no rand/time available).
    fn temp_cfg(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!(
                "xrocmtop_settings_{tag}_{}_{n}",
                std::process::id()
            ))
            .join("config.toml")
    }

    #[test]
    fn defaults_are_sane() {
        let s = Settings::default();
        assert_eq!(s.theme, "default");
        assert!(s.colors.is_empty() && s.order.is_empty() && s.hidden.is_empty());
    }

    #[test]
    fn missing_file_loads_defaults() {
        let s = Settings::load_from(Path::new("/nonexistent/xrocmtop/config.toml"));
        assert_eq!(s.theme, "default");
    }

    #[test]
    fn save_then_load_roundtrips() {
        let path = temp_cfg("roundtrip");
        let mut colors = BTreeMap::new();
        colors.insert("util_bar".to_string(), "#ff8800".to_string());
        let s = Settings {
            theme: "mono".to_string(),
            colors,
            order: vec!["gauges".into(), "vulkan".into()],
            hidden: vec!["processes".into()],
        };
        s.save_to(&path).unwrap();

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.theme, "mono");
        assert_eq!(
            loaded.colors.get("util_bar").map(String::as_str),
            Some("#ff8800")
        );
        assert_eq!(loaded.order, vec!["gauges", "vulkan"]);
        assert_eq!(loaded.hidden, vec!["processes"]);

        // The atomic write must not leave its sibling temp file behind.
        let dir = path.parent().unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_toml_loads_defaults() {
        let path = temp_cfg("invalid");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is = not valid toml [[[").unwrap();
        assert_eq!(Settings::load_from(&path).theme, "default");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn resolve_theme_applies_preset_and_overrides() {
        let mut colors = BTreeMap::new();
        colors.insert("util_bar".to_string(), "red".to_string());
        let s = Settings {
            theme: "mono".to_string(),
            colors,
            ..Default::default()
        };
        let t = s.resolve_theme();
        assert_eq!(t.util_bar, Color::Red); // override wins
        assert_eq!(t.border, Theme::preset("mono").border); // rest from preset
    }
}
