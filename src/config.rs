//! Command-line configuration for xrocmtop.
//!
//! Parsing lives here so the rest of the app depends on a resolved [`Config`] rather than on
//! `clap` directly. All flags are read-only knobs — nothing here can change GPU state.

use clap::Parser;

/// A btop-style terminal UI for monitoring AMD ROCm / Vulkan GPUs.
#[derive(Debug, Clone, Parser)]
#[command(name = "xrocmtop", version, about)]
pub struct Config {
    /// Refresh interval in milliseconds.
    #[arg(short, long, default_value_t = 1000, value_name = "MS")]
    pub interval: u64,

    /// Restrict the view to a single GPU index.
    #[arg(long, value_name = "INDEX")]
    pub gpu: Option<usize>,

    /// Skip the Vulkan device panel.
    #[arg(long)]
    pub no_vulkan: bool,

    /// Skip per-process GPU accounting.
    #[arg(long)]
    pub no_procs: bool,

    /// Print a single snapshot and exit (no TUI; scriptable).
    #[arg(long)]
    pub once: bool,

    /// With --once, emit JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,

    /// Number of samples retained for history graphs.
    #[arg(long, default_value_t = 240, value_name = "N")]
    pub history: usize,
}

impl Config {
    /// Refresh interval as a [`std::time::Duration`].
    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.interval.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn defaults_are_sane() {
        let c = Config::parse_from(["xrocmtop"]);
        assert_eq!(c.interval, 1000);
        assert_eq!(c.history, 240);
        assert!(!c.once && !c.json && !c.no_vulkan && !c.no_procs);
        assert_eq!(c.gpu, None);
    }

    #[test]
    fn flags_parse() {
        let c = Config::parse_from([
            "xrocmtop",
            "--interval",
            "500",
            "--gpu",
            "1",
            "--no-vulkan",
            "--once",
            "--json",
        ]);
        assert_eq!(c.interval, 500);
        assert_eq!(c.gpu, Some(1));
        assert!(c.no_vulkan && c.once && c.json);
        assert_eq!(c.interval(), std::time::Duration::from_millis(500));
    }

    #[test]
    fn interval_never_zero() {
        let c = Config::parse_from(["xrocmtop", "--interval", "0"]);
        assert_eq!(c.interval(), std::time::Duration::from_millis(1));
    }
}
