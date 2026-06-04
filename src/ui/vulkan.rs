//! Vulkan device panel: a static read-out of [`VulkanInfo`] (device, driver, API version, and
//! DEVICE_LOCAL memory heap sizes). When Vulkan info is unavailable — the binary was absent, the
//! parse failed, or `--no-vulkan` was passed — it renders a short "unavailable" notice instead.
//!
//! The body is a pure function of `Option<&VulkanInfo>` ([`vulkan_lines`]) so it is tested directly
//! and via ratatui's `TestBackend`, including the `None` case.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::model::{fmt_bytes, VulkanInfo};
use crate::theme::Theme;

/// Render the Vulkan panel into `area`. `info` is `None` when Vulkan data is unavailable.
pub fn render_vulkan(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    info: Option<&VulkanInfo>,
    theme: &Theme,
    focused: bool,
) {
    let border = if focused { theme.focus } else { theme.border };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(" Vulkan ")
        .title_style(
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        );
    let para = Paragraph::new(vulkan_lines(info, theme))
        .style(Style::default().fg(theme.text))
        .block(block);
    frame.render_widget(para, area);
}

/// Build the panel's text lines. Pure and exhaustively testable.
fn vulkan_lines(info: Option<&VulkanInfo>, theme: &Theme) -> Vec<Line<'static>> {
    let Some(info) = info else {
        return vec![Line::from(Span::styled(
            "Vulkan: unavailable",
            Style::default().fg(theme.dim),
        ))];
    };

    vec![
        field("Device", info.device_name.as_deref(), theme),
        field("Driver", driver_label(info).as_deref(), theme),
        field("API", info.api_version.as_deref(), theme),
        Line::from(format!("Heaps   {}", heaps_label(&info.heaps_bytes))),
    ]
}

/// "Driver" combines name and info when both are present: e.g. "radv (Mesa 26.0.3-1ubuntu1)".
fn driver_label(info: &VulkanInfo) -> Option<String> {
    match (info.driver_name.as_deref(), info.driver_info.as_deref()) {
        (Some(name), Some(extra)) => Some(format!("{name} ({extra})")),
        (Some(name), None) => Some(name.to_string()),
        (None, Some(extra)) => Some(extra.to_string()),
        (None, None) => None,
    }
}

/// A "Label   value" line, rendering "n/a" (dimmed) when the value is missing.
fn field(label: &str, value: Option<&str>, theme: &Theme) -> Line<'static> {
    let prefix = format!("{label:<7} ");
    match value {
        Some(v) => Line::from(format!("{prefix}{v}")),
        None => Line::from(vec![
            Span::raw(prefix),
            Span::styled("n/a", Style::default().fg(theme.dim)),
        ]),
    }
}

/// Format DEVICE_LOCAL heap sizes; "n/a" when none were reported.
fn heaps_label(heaps: &[u64]) -> String {
    if heaps.is_empty() {
        return "n/a".to_string();
    }
    heaps
        .iter()
        .map(|&b| fmt_bytes(Some(b)))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn full() -> VulkanInfo {
        VulkanInfo {
            device_name: Some("Radeon 8060S Graphics (RADV STRIX_HALO)".into()),
            driver_name: Some("radv".into()),
            driver_info: Some("Mesa 26.0.3-1ubuntu1".into()),
            api_version: Some("1.4.15".into()),
            heaps_bytes: vec![79_631_667_200],
        }
    }

    fn render(info: Option<&VulkanInfo>) -> String {
        let mut term = Terminal::new(TestBackend::new(80, 6)).unwrap();
        term.draw(|f| render_vulkan(f, f.area(), info, &Theme::default(), false))
            .unwrap();
        let buf = term.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    #[test]
    fn renders_present_info() {
        let info = full();
        let out = render(Some(&info));
        assert!(out.contains("Vulkan"));
        assert!(out.contains("Radeon 8060S Graphics"));
        assert!(out.contains("radv"));
        assert!(out.contains("Mesa 26.0.3-1ubuntu1"));
        assert!(out.contains("1.4.15"));
        assert!(out.contains("74.16 GiB")); // 79_631_667_200 bytes
    }

    #[test]
    fn renders_unavailable_when_none() {
        let out = render(None);
        assert!(out.contains("Vulkan: unavailable"));
    }

    #[test]
    fn partial_info_shows_na_without_panic() {
        let info = VulkanInfo {
            device_name: Some("X".into()),
            ..Default::default()
        };
        let out = render(Some(&info));
        assert!(out.contains("n/a")); // driver/api/heaps missing
    }

    #[test]
    fn driver_label_combines_name_and_info() {
        assert_eq!(
            driver_label(&full()).as_deref(),
            Some("radv (Mesa 26.0.3-1ubuntu1)")
        );
        let name_only = VulkanInfo {
            driver_name: Some("radv".into()),
            ..Default::default()
        };
        assert_eq!(driver_label(&name_only).as_deref(), Some("radv"));
        assert_eq!(driver_label(&VulkanInfo::default()), None);
    }

    #[test]
    fn heaps_label_formats_and_handles_empty() {
        assert_eq!(heaps_label(&[]), "n/a");
        assert_eq!(heaps_label(&[1024]), "1.00 KiB");
        assert_eq!(heaps_label(&[1024, 2048]), "1.00 KiB, 2.00 KiB");
    }
}
