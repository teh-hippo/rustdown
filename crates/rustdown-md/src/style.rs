#![forbid(unsafe_code)]
//! Configurable styles for Markdown preview rendering.

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
    /// Heading styles for levels H1–H6 (index 0 = H1).
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
    /// Create a default style derived from egui visuals.
    #[must_use]
    pub fn from_visuals(visuals: &egui::Visuals) -> Self {
        let link = visuals.hyperlink_color;
        let scales = [2.0_f32, 1.5, 1.25, 1.1, 1.0, 0.95];
        let headings = std::array::from_fn(|i| HeadingStyle {
            font_scale: scales[i],
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

    /// Set heading colours from an external palette (e.g., rustdown's
    /// `heading_color()` function).
    pub fn with_heading_colors(&mut self, colors: [egui::Color32; 6]) -> &mut Self {
        for (h, c) in self.headings.iter_mut().zip(colors) {
            h.color = c;
        }
        self
    }

    /// Set heading font scales.
    pub fn with_heading_scales(&mut self, scales: [f32; 6]) -> &mut Self {
        for (h, s) in self.headings.iter_mut().zip(scales) {
            h.font_scale = s;
        }
        self
    }
}
