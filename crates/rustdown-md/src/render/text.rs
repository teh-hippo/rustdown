//! Inline text rendering — `SpanFormat`, styled text, layout jobs.

use crate::parse::{SpanStyle, StyledText};
use crate::style::MarkdownStyle;

/// Snap `pos` to a valid UTF-8 char boundary in `text`, rounding down.
/// If `pos` is already on a boundary (or at 0 / `text.len()`), returns it unchanged.
#[inline]
const fn snap_to_char_boundary(text: &str, pos: usize) -> usize {
    if pos >= text.len() {
        return text.len();
    }
    text.floor_char_boundary(pos)
}

/// Inline formatting properties resolved from a composite `SpanStyle`.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct SpanFormat {
    font_family: egui::FontFamily,
    color: egui::Color32,
    background: egui::Color32,
    underline: bool,
    strikethrough: bool,
    italics: bool,
    strong: bool,
}

impl SpanFormat {
    #[inline]
    fn resolve(
        ss: SpanStyle,
        md_style: &MarkdownStyle,
        base_color: egui::Color32,
        ui: &egui::Ui,
    ) -> Self {
        Self {
            font_family: if ss.code() {
                egui::FontFamily::Monospace
            } else {
                egui::FontFamily::Proportional
            },
            color: if ss.has_link() {
                md_style
                    .link_color
                    .unwrap_or_else(|| ui.visuals().hyperlink_color)
            } else {
                base_color
            },
            background: if ss.code() {
                md_style
                    .code_bg
                    .unwrap_or_else(|| ui.visuals().faint_bg_color)
            } else {
                egui::Color32::TRANSPARENT
            },
            underline: ss.has_link(),
            strikethrough: ss.strikethrough(),
            italics: ss.emphasis(),
            strong: ss.strong(),
        }
    }
}

pub(super) fn render_styled_text(ui: &mut egui::Ui, st: &StyledText, style: &MarkdownStyle) {
    render_styled_text_ex(ui, st, style, None, None);
}

pub(super) fn render_styled_text_ex(
    ui: &mut egui::Ui,
    st: &StyledText,
    style: &MarkdownStyle,
    font_size: Option<f32>,
    color_override: Option<egui::Color32>,
) {
    if st.text.is_empty() {
        return;
    }

    let has_links = st.has_links;

    // If there are links, render in a horizontal wrap so we can make links clickable.
    if has_links {
        render_text_with_links(ui, st, style, font_size, color_override);
        return;
    }

    let body_size = ui.text_style_height(&egui::TextStyle::Body);
    let size = font_size.unwrap_or(body_size);
    let base_color = color_override
        .or(style.body_color)
        .unwrap_or_else(|| ui.visuals().text_color());
    let wrap_width = ui.available_width();

    let job = if st.spans.is_empty() {
        egui::text::LayoutJob::simple(
            st.text.clone(),
            egui::FontId::new(size, egui::FontFamily::Proportional),
            base_color,
            wrap_width,
        )
    } else {
        build_layout_job(st, &st.spans, style, base_color, size, wrap_width, ui)
    };

    let galley = ui.fonts_mut(|f| f.layout_job(job));
    ui.label(galley);
}

/// Render text that contains links: non-link spans as labels, link spans as hyperlinks.
pub(super) fn render_text_with_links(
    ui: &mut egui::Ui,
    st: &StyledText,
    style: &MarkdownStyle,
    font_size: Option<f32>,
    color_override: Option<egui::Color32>,
) {
    let body_size = ui.text_style_height(&egui::TextStyle::Body);
    let size = font_size.unwrap_or(body_size);
    let base_color = color_override
        .or(style.body_color)
        .unwrap_or_else(|| ui.visuals().text_color());

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for span in &st.spans {
            let start = snap_to_char_boundary(&st.text, (span.start as usize).min(st.text.len()));
            let end = snap_to_char_boundary(&st.text, (span.end as usize).min(st.text.len()));
            if start >= end {
                continue;
            }
            let text = &st.text[start..end];
            let is_code = span.style.code();
            let span_size = if is_code { size * 0.9 } else { size };
            let font_family = if is_code {
                egui::FontFamily::Monospace
            } else {
                egui::FontFamily::Proportional
            };
            let mut rt = egui::RichText::new(text).font(egui::FontId::new(span_size, font_family));

            if span.style.emphasis() {
                rt = rt.italics();
            }
            if span.style.strikethrough() {
                rt = rt.strikethrough();
            }
            if is_code {
                rt = rt
                    .background_color(style.code_bg.unwrap_or_else(|| ui.visuals().faint_bg_color));
            }

            if let Some(url) = st.link_url(span.style.link_idx) {
                if span.style.strong() {
                    rt = rt.strong();
                }
                ui.hyperlink_to(rt, url.as_ref());
            } else {
                rt = rt.color(if span.style.strong() {
                    strengthen_color(base_color)
                } else {
                    base_color
                });
                ui.label(rt);
            }
        }
    });
}

/// Build a `LayoutJob` for non-link text spans.
pub(super) fn build_layout_job(
    st: &StyledText,
    spans: &[crate::parse::Span],
    style: &MarkdownStyle,
    base_color: egui::Color32,
    size: f32,
    wrap_width: f32,
    ui: &egui::Ui,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob {
        text: st.text.clone(),
        sections: Vec::with_capacity(spans.len()),
        wrap: egui::text::TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        ..Default::default()
    };

    for span in spans {
        let start = snap_to_char_boundary(&st.text, (span.start as usize).min(st.text.len()));
        let end = snap_to_char_boundary(&st.text, (span.end as usize).min(st.text.len()));
        if start >= end {
            continue;
        }
        let sf = SpanFormat::resolve(span.style, style, base_color, ui);
        let span_size = if span.style.code() { size * 0.9 } else { size };
        let color = if sf.strong {
            strengthen_color(sf.color)
        } else {
            sf.color
        };
        let stroke = |active: bool| {
            if active {
                egui::Stroke::new(1.0, color)
            } else {
                egui::Stroke::NONE
            }
        };
        job.sections.push(egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: start..end,
            format: egui::TextFormat {
                font_id: egui::FontId::new(span_size, sf.font_family),
                color,
                background: sf.background,
                italics: sf.italics,
                underline: stroke(sf.underline),
                strikethrough: stroke(sf.strikethrough),
                ..Default::default()
            },
        });
    }
    job
}

/// Approximate "bold" by increasing contrast of a colour.
/// egui has no bold font weight, so we visually distinguish strong text.
/// On dark backgrounds (bright text) the colour is brightened;
/// on light backgrounds (dark text) it is darkened.
#[inline]
pub(super) fn strengthen_color(color: egui::Color32) -> egui::Color32 {
    // Work in unmultiplied space so luma is correct for semi-transparent
    // colours and the output never violates the premultiplied invariant.
    let [red, green, blue, alpha] = color.to_srgba_unmultiplied();
    // Perceptual luminance (ITU-R BT.601).
    let luma = 0.114f32.mul_add(
        f32::from(blue),
        0.299f32.mul_add(f32::from(red), 0.587 * f32::from(green)),
    );
    if luma > 127.0 {
        // Bright text (dark background) → brighten toward white.
        let boost = |val: u8| {
            let delta = (u16::from(255_u8.saturating_sub(val))) / 3;
            val.saturating_add(delta.min(255) as u8)
        };
        egui::Color32::from_rgba_unmultiplied(boost(red), boost(green), boost(blue), alpha)
    } else {
        // Dark text (light background) → darken toward black.
        let darken = |val: u8| {
            let delta = u16::from(val) / 3;
            val.saturating_sub(delta.min(255) as u8)
        };
        egui::Color32::from_rgba_unmultiplied(darken(red), darken(green), darken(blue), alpha)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_to_char_boundary_ascii() {
        let text = "hello";
        for i in 0..=text.len() {
            assert_eq!(snap_to_char_boundary(text, i), i);
        }
    }

    #[test]
    fn snap_to_char_boundary_multibyte() {
        // 'é' = 2 bytes, '日' = 3 bytes, '🦀' = 4 bytes
        let text = "aé日🦀z";
        // Valid boundaries: 0, 1, 3, 6, 10, 11
        assert_eq!(snap_to_char_boundary(text, 0), 0); // 'a'
        assert_eq!(snap_to_char_boundary(text, 1), 1); // start of 'é'
        assert_eq!(snap_to_char_boundary(text, 2), 1); // mid 'é' → snaps to 1
        assert_eq!(snap_to_char_boundary(text, 3), 3); // start of '日'
        assert_eq!(snap_to_char_boundary(text, 4), 3); // mid '日' → snaps to 3
        assert_eq!(snap_to_char_boundary(text, 5), 3); // mid '日' → snaps to 3
        assert_eq!(snap_to_char_boundary(text, 6), 6); // start of '🦀'
        assert_eq!(snap_to_char_boundary(text, 7), 6); // mid '🦀' → snaps to 6
        assert_eq!(snap_to_char_boundary(text, 8), 6); // mid '🦀' → snaps to 6
        assert_eq!(snap_to_char_boundary(text, 9), 6); // mid '🦀' → snaps to 6
        assert_eq!(snap_to_char_boundary(text, 10), 10); // start of 'z'
        assert_eq!(snap_to_char_boundary(text, 11), 11); // end of string
    }

    #[test]
    fn snap_to_char_boundary_beyond_length() {
        let text = "abc";
        assert_eq!(snap_to_char_boundary(text, 100), 3);
        assert_eq!(snap_to_char_boundary(text, usize::MAX), 3);
    }

    #[test]
    fn snap_to_char_boundary_empty_string() {
        assert_eq!(snap_to_char_boundary("", 0), 0);
        assert_eq!(snap_to_char_boundary("", 5), 0);
    }

    #[test]
    fn snap_to_char_boundary_only_multibyte() {
        // String of only 4-byte chars
        let text = "🦀🦀🦀";
        for pos in 0..text.len() {
            let snapped = snap_to_char_boundary(text, pos);
            assert!(
                text.is_char_boundary(snapped),
                "pos {pos} → {snapped} is not a boundary"
            );
            assert!(snapped <= pos, "snapped {snapped} > original {pos}");
        }
    }
}
