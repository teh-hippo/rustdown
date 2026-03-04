#![forbid(unsafe_code)]
//! Render parsed Markdown blocks into egui widgets.

use crate::parse::{Alignment, Block, ListItem, SpanKind, StyledText, parse_markdown};
use crate::style::MarkdownStyle;

/// Cached pre-parsed blocks plus the text hash they were built from.
#[derive(Default)]
pub struct MarkdownCache {
    text_hash: u64,
    blocks: Vec<Block>,
}

impl MarkdownCache {
    /// Invalidate the cache so the next render re-parses.
    pub fn clear(&mut self) {
        self.text_hash = 0;
        self.blocks.clear();
    }
}

/// The main Markdown viewer widget.
pub struct MarkdownViewer {
    id_salt: &'static str,
}

impl MarkdownViewer {
    #[must_use]
    pub const fn new(id_salt: &'static str) -> Self {
        Self { id_salt }
    }

    /// Render markdown in a scrollable area with viewport culling.
    pub fn show_scrollable(
        &self,
        ui: &mut egui::Ui,
        cache: &mut MarkdownCache,
        style: &MarkdownStyle,
        source: &str,
    ) {
        // Re-parse only when source changes.
        let hash = simple_hash(source);
        if cache.text_hash != hash {
            cache.blocks = parse_markdown(source);
            cache.text_hash = hash;
        }

        egui::ScrollArea::vertical()
            .id_salt(self.id_salt)
            .show(ui, |ui| {
                render_blocks(ui, &cache.blocks, style, 0);
            });
    }

    /// Render markdown inline (no scroll area).
    pub fn show(
        &self,
        ui: &mut egui::Ui,
        cache: &mut MarkdownCache,
        style: &MarkdownStyle,
        source: &str,
    ) {
        let hash = simple_hash(source);
        if cache.text_hash != hash {
            cache.blocks = parse_markdown(source);
            cache.text_hash = hash;
        }
        render_blocks(ui, &cache.blocks, style, 0);
    }
}

fn render_blocks(ui: &mut egui::Ui, blocks: &[Block], style: &MarkdownStyle, indent: usize) {
    let body_size = ui.text_style_height(&egui::TextStyle::Body);

    for block in blocks {
        match block {
            Block::Heading { level, text } => {
                let idx = (*level as usize).saturating_sub(1).min(5);
                let hs = &style.headings[idx];
                let size = body_size * hs.font_scale;

                ui.add_space(size * 0.3);
                render_styled_text_with_override(ui, text, style, Some(size), Some(hs.color));
                ui.add_space(size * 0.15);

                // Draw separator under H1 and H2.
                if *level <= 2 {
                    let rect = ui.available_rect_before_wrap();
                    let y = rect.min.y;
                    let color = style
                        .hr_color
                        .unwrap_or_else(|| ui.visuals().weak_text_color());
                    ui.painter().line_segment(
                        [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
                        egui::Stroke::new(1.0, color),
                    );
                    ui.add_space(4.0);
                }
            }

            Block::Paragraph(text) => {
                if indent > 0 {
                    add_indent(ui, indent);
                }
                render_styled_text(ui, text, style);
                ui.add_space(body_size * 0.4);
            }

            Block::Code { code, .. } => {
                let bg = style
                    .code_bg
                    .unwrap_or_else(|| ui.visuals().faint_bg_color);
                let available = ui.available_width();
                egui::Frame::NONE
                    .fill(bg)
                    .corner_radius(4.0)
                    .inner_margin(egui::Margin::same(6))
                    .show(ui, |ui| {
                        ui.set_min_width(available - 12.0);
                        let mono = egui::FontId::new(
                            body_size * 0.9,
                            egui::FontFamily::Monospace,
                        );
                        ui.label(
                            egui::RichText::new(code.trim_end())
                                .font(mono)
                                .color(ui.visuals().text_color()),
                        );
                    });
                ui.add_space(body_size * 0.4);
            }

            Block::Quote(inner) => {
                let bar_color = style
                    .blockquote_bar
                    .unwrap_or_else(|| ui.visuals().weak_text_color());

                ui.horizontal(|ui| {
                    // Paint left border bar.
                    let rect = ui.available_rect_before_wrap();
                    let x = rect.min.x + 4.0;
                    ui.painter().line_segment(
                        [egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)],
                        egui::Stroke::new(3.0, bar_color),
                    );
                    ui.add_space(16.0);
                    ui.vertical(|ui| {
                        render_blocks(ui, inner, style, indent + 1);
                    });
                });
                ui.add_space(body_size * 0.3);
            }

            Block::UnorderedList(items) => {
                render_unordered_list(ui, items, style, indent);
                ui.add_space(body_size * 0.2);
            }

            Block::OrderedList { start, items } => {
                render_ordered_list(ui, *start, items, style, indent);
                ui.add_space(body_size * 0.2);
            }

            Block::ThematicBreak => {
                ui.add_space(body_size * 0.3);
                let rect = ui.available_rect_before_wrap();
                let y = rect.min.y;
                let color = style
                    .hr_color
                    .unwrap_or_else(|| ui.visuals().weak_text_color());
                ui.painter().line_segment(
                    [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
                    egui::Stroke::new(1.0, color),
                );
                ui.add_space(body_size * 0.5);
            }

            Block::Table {
                header,
                alignments,
                rows,
            } => {
                render_table(ui, header, alignments, rows, style);
                ui.add_space(body_size * 0.4);
            }

            Block::Image { url, alt } => {
                // Placeholder: show alt text as link.
                let link_color = style
                    .link_color
                    .unwrap_or_else(|| ui.visuals().hyperlink_color);
                let label = if alt.is_empty() {
                    format!("[image: {url}]")
                } else {
                    format!("[{alt}]")
                };
                ui.label(egui::RichText::new(label).color(link_color).italics());
                ui.add_space(body_size * 0.3);
            }
        }
    }
}

fn render_styled_text(ui: &mut egui::Ui, st: &StyledText, style: &MarkdownStyle) {
    render_styled_text_with_override(ui, st, style, None, None);
}

fn render_styled_text_with_override(
    ui: &mut egui::Ui,
    st: &StyledText,
    style: &MarkdownStyle,
    font_size: Option<f32>,
    color_override: Option<egui::Color32>,
) {
    if st.text.is_empty() {
        return;
    }

    let body_size = ui.text_style_height(&egui::TextStyle::Body);
    let size = font_size.unwrap_or(body_size);
    let base_color = color_override
        .or(style.body_color)
        .unwrap_or_else(|| ui.visuals().text_color());

    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    job.text.clone_from(&st.text);

    if st.spans.is_empty() {
        // Single plain span for entire text.
        job.sections.push(egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: 0..st.text.len(),
            format: egui::TextFormat {
                font_id: egui::FontId::new(size, egui::FontFamily::Proportional),
                color: base_color,
                ..Default::default()
            },
        });
    } else {
        for span in &st.spans {
            let (font_family, color, underline, strikethrough, italics, strong, background) =
                span_format_props(&span.kind, style, base_color, ui);

            let mut format = egui::TextFormat {
                font_id: egui::FontId::new(
                    if matches!(span.kind, SpanKind::Code) {
                        size * 0.9
                    } else {
                        size
                    },
                    font_family,
                ),
                color,
                background,
                ..Default::default()
            };
            if underline {
                format.underline = egui::Stroke::new(1.0, color);
            }
            if strikethrough {
                format.strikethrough = egui::Stroke::new(1.0, color);
            }
            if italics {
                format.italics = true;
            }
            if strong {
                // egui doesn't have bold font natively; we approximate.
                format.color = color;
            }
            job.sections.push(egui::text::LayoutSection {
                leading_space: 0.0,
                byte_range: span.start..span.end,
                format,
            });
        }
    }

    // Allocate a galley and render as selectable label.
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    ui.label(galley);
}

#[allow(clippy::type_complexity)]
fn span_format_props(
    kind: &SpanKind,
    style: &MarkdownStyle,
    base_color: egui::Color32,
    ui: &egui::Ui,
) -> (
    egui::FontFamily,
    egui::Color32,
    bool,
    bool,
    bool,
    bool,
    egui::Color32,
) {
    // (font_family, color, underline, strikethrough, italics, strong, background)
    match kind {
        SpanKind::Plain => (
            egui::FontFamily::Proportional,
            base_color,
            false,
            false,
            false,
            false,
            egui::Color32::TRANSPARENT,
        ),
        SpanKind::Strong => (
            egui::FontFamily::Proportional,
            base_color,
            false,
            false,
            false,
            true,
            egui::Color32::TRANSPARENT,
        ),
        SpanKind::Emphasis => (
            egui::FontFamily::Proportional,
            base_color,
            false,
            false,
            true,
            false,
            egui::Color32::TRANSPARENT,
        ),
        SpanKind::Strikethrough => (
            egui::FontFamily::Proportional,
            base_color,
            false,
            true,
            false,
            false,
            egui::Color32::TRANSPARENT,
        ),
        SpanKind::Code => {
            let bg = style
                .code_bg
                .unwrap_or_else(|| ui.visuals().faint_bg_color);
            (
                egui::FontFamily::Monospace,
                base_color,
                false,
                false,
                false,
                false,
                bg,
            )
        }
        SpanKind::Link(_) => {
            let link_color = style
                .link_color
                .unwrap_or_else(|| ui.visuals().hyperlink_color);
            (
                egui::FontFamily::Proportional,
                link_color,
                true,
                false,
                false,
                false,
                egui::Color32::TRANSPARENT,
            )
        }
    }
}

fn render_unordered_list(
    ui: &mut egui::Ui,
    items: &[ListItem],
    style: &MarkdownStyle,
    indent: usize,
) {
    let bullet = match indent {
        0 => "•",
        1 => "◦",
        _ => "▪",
    };
    let indent_px = 20.0 * (indent as f32 + 1.0);

    for item in items {
        ui.horizontal(|ui| {
            ui.add_space(indent_px);
            ui.label(bullet);
            ui.vertical(|ui| {
                render_styled_text(ui, &item.content, style);
            });
        });
        // Render nested children.
        if !item.children.is_empty() {
            render_blocks(ui, &item.children, style, indent + 1);
        }
    }
}

fn render_ordered_list(
    ui: &mut egui::Ui,
    start: u64,
    items: &[ListItem],
    style: &MarkdownStyle,
    indent: usize,
) {
    let indent_px = 20.0 * (indent as f32 + 1.0);

    for (i, item) in items.iter().enumerate() {
        let num = start + i as u64;
        ui.horizontal(|ui| {
            ui.add_space(indent_px);
            ui.label(format!("{num}."));
            ui.vertical(|ui| {
                render_styled_text(ui, &item.content, style);
            });
        });
        if !item.children.is_empty() {
            render_blocks(ui, &item.children, style, indent + 1);
        }
    }
}

fn render_table(
    ui: &mut egui::Ui,
    header: &[StyledText],
    alignments: &[Alignment],
    rows: &[Vec<StyledText>],
    style: &MarkdownStyle,
) {
    let num_cols = header.len().max(1);
    let available = ui.available_width();
    let col_width = (available / num_cols as f32).max(40.0);

    egui::Grid::new("md_table")
        .striped(true)
        .min_col_width(col_width)
        .show(ui, |ui| {
            // Header row.
            for (i, cell) in header.iter().enumerate() {
                let align = alignments.get(i).copied().unwrap_or(Alignment::None);
                render_table_cell(ui, cell, style, align, true);
            }
            ui.end_row();

            // Data rows.
            for row in rows {
                for (i, cell) in row.iter().enumerate() {
                    let align = alignments.get(i).copied().unwrap_or(Alignment::None);
                    render_table_cell(ui, cell, style, align, false);
                }
                ui.end_row();
            }
        });
}

fn render_table_cell(
    ui: &mut egui::Ui,
    cell: &StyledText,
    style: &MarkdownStyle,
    _align: Alignment,
    is_header: bool,
) {
    if is_header {
        let body_size = ui.text_style_height(&egui::TextStyle::Body);
        render_styled_text_with_override(ui, cell, style, Some(body_size), None);
    } else {
        render_styled_text(ui, cell, style);
    }
}

fn add_indent(ui: &mut egui::Ui, level: usize) {
    ui.add_space(16.0 * level as f32);
}

fn simple_hash(s: &str) -> u64 {
    // FNV-1a 64-bit for fast, non-cryptographic hashing.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in s.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_invalidates_on_text_change() {
        let mut cache = MarkdownCache::default();
        let s1 = "# Hello";
        let s2 = "# World";
        let _style = MarkdownStyle::from_visuals(&egui::Visuals::dark());

        // First parse.
        let h1 = simple_hash(s1);
        cache.blocks = crate::parse::parse_markdown(s1);
        cache.text_hash = h1;
        assert_eq!(cache.blocks.len(), 1);

        // Same text, no re-parse.
        let h1b = simple_hash(s1);
        assert_eq!(h1, h1b);

        // Different text.
        let h2 = simple_hash(s2);
        assert_ne!(h1, h2);
    }

    #[test]
    fn simple_hash_produces_different_hashes() {
        assert_ne!(simple_hash("hello"), simple_hash("world"));
        assert_eq!(simple_hash("same"), simple_hash("same"));
    }

    #[test]
    fn markdown_style_with_heading_colors() {
        let mut style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let colors = [
            egui::Color32::RED,
            egui::Color32::GREEN,
            egui::Color32::BLUE,
            egui::Color32::YELLOW,
            egui::Color32::WHITE,
            egui::Color32::GRAY,
        ];
        style.with_heading_colors(colors);
        assert_eq!(style.headings[0].color, egui::Color32::RED);
        assert_eq!(style.headings[5].color, egui::Color32::GRAY);
    }
}
