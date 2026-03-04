#![forbid(unsafe_code)]
//! Configurable styles for Markdown preview rendering.

/// Default heading font scales (H1-H6).
pub const HEADING_FONT_SCALES: [f32; 6] = [2.0, 1.5, 1.25, 1.1, 1.0, 0.95];

/// Dracula-inspired heading colours for dark themes.
pub const DARK_HEADING_COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(0xFF, 0xB8, 0x6C), // orange
    egui::Color32::from_rgb(0x8B, 0xE9, 0xFD), // cyan
    egui::Color32::from_rgb(0x50, 0xFA, 0x7B), // green
    egui::Color32::from_rgb(0xBD, 0x93, 0xF9), // purple
    egui::Color32::from_rgb(0xFF, 0x79, 0xC6), // pink
    egui::Color32::from_rgb(0xF1, 0xFA, 0x8C), // yellow
];

/// Heading colours for light themes.
pub const LIGHT_HEADING_COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(0x9C, 0x3D, 0x00),
    egui::Color32::from_rgb(0x00, 0x5F, 0x9A),
    egui::Color32::from_rgb(0x2E, 0x7D, 0x32),
    egui::Color32::from_rgb(0x6A, 0x1B, 0x9A),
    egui::Color32::from_rgb(0xAD, 0x14, 0x57),
    egui::Color32::from_rgb(0x5D, 0x40, 0x37),
];

/// Per-heading-level style: font scale relative to body and colour.
#[derive(Clone, Debug)]
pub struct HeadingStyle {
    /// Multiplier applied to body font size.
    pub font_scale: f32,
    /// Text colour for this heading level.
    pub color: egui::Color32,
}

/// Full style configuration for the Markdown renderer.
#[derive(Clone, Debug)]
pub struct MarkdownStyle {
    /// Heading styles for levels H1-H6 (index 0 = H1).
    pub headings: [HeadingStyle; 6],
    /// Body text colour (falls back to `visuals.text_color()` if `None`).
    pub body_color: Option<egui::Color32>,
    /// Code background tint.
    pub code_bg: Option<egui::Color32>,
    /// Blockquote left-border colour.
    pub blockquote_bar: Option<egui::Color32>,
    /// Link colour.
    pub link_color: Option<egui::Color32>,
    /// Horizontal rule colour.
    pub hr_color: Option<egui::Color32>,
}

impl MarkdownStyle {
    /// Create a default style derived from egui visuals (no heading colours).
    #[must_use]
    pub fn from_visuals(visuals: &egui::Visuals) -> Self {
        let link = visuals.hyperlink_color;
        let headings = std::array::from_fn(|i| HeadingStyle {
            font_scale: HEADING_FONT_SCALES[i],
            color: link,
        });
        Self {
            headings,
            body_color: None,
            code_bg: Some(visuals.faint_bg_color),
            blockquote_bar: Some(visuals.weak_text_color()),
            link_color: Some(link),
            hr_color: Some(visuals.weak_text_color()),
        }
    }

    /// Create a style with coloured headings, auto-selecting palette by theme.
    #[must_use]
    pub fn colored(visuals: &egui::Visuals) -> Self {
        let mut s = Self::from_visuals(visuals);
        let colors = if visuals.dark_mode {
            DARK_HEADING_COLORS
        } else {
            LIGHT_HEADING_COLORS
        };
        let _ = s.with_heading_colors(colors);
        s
    }

    /// Set heading colours from an external palette.
    #[must_use]
    pub fn with_heading_colors(&mut self, colors: [egui::Color32; 6]) -> &mut Self {
        for (h, c) in self.headings.iter_mut().zip(colors) {
            h.color = c;
        }
        self
    }

    /// Set heading font scales.
    #[must_use]
    pub fn with_heading_scales(&mut self, scales: [f32; 6]) -> &mut Self {
        for (h, s) in self.headings.iter_mut().zip(scales) {
            h.font_scale = s;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colored_dark_uses_dark_palette() {
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        assert_eq!(style.headings[0].color, DARK_HEADING_COLORS[0]);
    }

    #[test]
    fn colored_light_uses_light_palette() {
        let style = MarkdownStyle::colored(&egui::Visuals::light());
        assert_eq!(style.headings[0].color, LIGHT_HEADING_COLORS[0]);
    }

    #[test]
    fn default_scales_match_constant() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        for (i, h) in style.headings.iter().enumerate() {
            assert!(
                (h.font_scale - HEADING_FONT_SCALES[i]).abs() < f32::EPSILON,
                "heading {i} scale mismatch"
            );
        }
    }
}
