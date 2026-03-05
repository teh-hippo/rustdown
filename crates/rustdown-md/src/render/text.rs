//! Inline text rendering — `SpanFormat`, styled text, layout jobs.

use crate::parse::{SpanStyle, StyledText};
use crate::style::MarkdownStyle;

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
    fn resolve(
        span_style: &SpanStyle,
        md_style: &MarkdownStyle,
        base_color: egui::Color32,
        ui: &egui::Ui,
    ) -> Self {
        let mut sf = Self {
            font_family: egui::FontFamily::Proportional,
            color: base_color,
            background: egui::Color32::TRANSPARENT,
            underline: false,
            strikethrough: false,
            italics: false,
            strong: false,
        };

        if span_style.strong() {
            sf.strong = true;
        }
        if span_style.emphasis() {
            sf.italics = true;
        }
        if span_style.strikethrough() {
            sf.strikethrough = true;
        }
        if span_style.code() {
            sf.font_family = egui::FontFamily::Monospace;
            sf.background = md_style
                .code_bg
                .unwrap_or_else(|| ui.visuals().faint_bg_color);
        }
        if span_style.link.is_some() {
            sf.color = md_style
                .link_color
                .unwrap_or_else(|| ui.visuals().hyperlink_color);
            sf.underline = true;
        }

        sf
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
        // Use zero horizontal spacing between inline spans so link and text
        // widgets flow together without extra gaps.
        ui.spacing_mut().item_spacing.x = 0.0;
        for span in &st.spans {
            let start = (span.start as usize).min(st.text.len());
            let end = (span.end as usize).min(st.text.len());
            if start >= end {
                continue;
            }
            let text = &st.text[start..end];
            let font_family = if span.style.code() {
                egui::FontFamily::Monospace
            } else {
                egui::FontFamily::Proportional
            };
            let span_size = if span.style.code() { size * 0.9 } else { size };
            let mut rt = egui::RichText::new(text).font(egui::FontId::new(span_size, font_family));

            if span.style.emphasis() {
                rt = rt.italics();
            }
            if span.style.strikethrough() {
                rt = rt.strikethrough();
            }

            if let Some(ref url) = span.style.link {
                if span.style.strong() {
                    rt = rt.strong();
                }
                ui.hyperlink_to(rt, url.as_ref());
            } else {
                let color = if span.style.strong() {
                    strengthen_color(base_color)
                } else {
                    base_color
                };
                rt = rt.color(color);

                if span.style.code() {
                    rt = rt.background_color(
                        style.code_bg.unwrap_or_else(|| ui.visuals().faint_bg_color),
                    );
                }
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
        let start = (span.start as usize).min(st.text.len());
        let end = (span.end as usize).min(st.text.len());
        if start >= end {
            continue;
        }
        let sf = SpanFormat::resolve(&span.style, style, base_color, ui);
        let span_size = if span.style.code() { size * 0.9 } else { size };
        let mut format = egui::TextFormat {
            font_id: egui::FontId::new(span_size, sf.font_family),
            color: sf.color,
            background: sf.background,
            italics: sf.italics,
            ..Default::default()
        };
        if sf.underline {
            format.underline = egui::Stroke::new(1.0, sf.color);
        }
        if sf.strikethrough {
            format.strikethrough = egui::Stroke::new(1.0, sf.color);
        }
        if sf.strong {
            format.color = strengthen_color(sf.color);
        }
        job.sections.push(egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: start..end,
            format,
        });
    }
    job
}

/// Approximate "bold" by increasing contrast of a colour.
/// egui has no bold font weight, so we visually distinguish strong text.
/// On dark backgrounds (bright text) the colour is brightened;
/// on light backgrounds (dark text) it is darkened.
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
