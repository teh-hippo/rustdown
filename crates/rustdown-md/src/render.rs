#![forbid(unsafe_code)]
//! Render parsed Markdown blocks into egui widgets.
//!
//! Key feature: viewport culling in `show_scrollable` — only blocks
//! overlapping the visible region are laid out, giving O(visible) cost.

use crate::parse::{Alignment, Block, ListItem, SpanKind, StyledText, parse_markdown};
use crate::style::MarkdownStyle;

// ── Cache ──────────────────────────────────────────────────────────

/// Cached pre-parsed blocks, height estimates, and the source hash.
#[derive(Default)]
pub struct MarkdownCache {
    text_hash: u64,
    text_len: usize,
    text_ptr: usize,
    pub(crate) blocks: Vec<Block>,
    /// Estimated pixel height for each top-level block (same len as `blocks`).
    pub(crate) heights: Vec<f32>,
    /// Cumulative Y offsets: `cum_y[i]` = sum of heights[0..i].
    pub(crate) cum_y: Vec<f32>,
    /// Total estimated height of all blocks.
    pub(crate) total_height: f32,
    /// The body font size used when heights were estimated.
    height_body_size: f32,
    /// The wrap width used when heights were estimated.
    height_wrap_width: f32,
}

impl MarkdownCache {
    /// Invalidate the cache so the next render re-parses.
    pub fn clear(&mut self) {
        self.text_hash = 0;
        self.text_len = 0;
        self.text_ptr = 0;
        self.blocks.clear();
        self.heights.clear();
        self.cum_y.clear();
        self.total_height = 0.0;
        self.height_body_size = 0.0;
        self.height_wrap_width = 0.0;
    }

    pub(crate) fn ensure_parsed(&mut self, source: &str) {
        // Fast pointer+length check: if the source is the same allocation
        // and length, skip hash entirely (common in frame-to-frame rendering).
        let ptr = source.as_ptr() as usize;
        let len = source.len();
        if ptr == self.text_ptr && len == self.text_len {
            return;
        }

        // Length changed → definitely new content.
        if len != self.text_len {
            self.text_len = len;
            self.text_ptr = ptr;
            self.text_hash = simple_hash(source);
            self.blocks = parse_markdown(source);
            self.heights.clear();
            self.cum_y.clear();
            self.total_height = 0.0;
            return;
        }

        // Same length, different pointer → check hash.
        let hash = simple_hash(source);
        self.text_ptr = ptr;
        if self.text_hash == hash {
            return;
        }
        self.text_hash = hash;
        self.blocks = parse_markdown(source);
        self.heights.clear();
        self.cum_y.clear();
        self.total_height = 0.0;
    }

    pub(crate) fn ensure_heights(
        &mut self,
        body_size: f32,
        wrap_width: f32,
        style: &MarkdownStyle,
    ) {
        let size_bits = body_size.to_bits();
        let width_bits = wrap_width.to_bits();
        if !self.heights.is_empty()
            && self.height_body_size.to_bits() == size_bits
            && self.height_wrap_width.to_bits() == width_bits
        {
            return;
        }
        self.height_body_size = body_size;
        self.height_wrap_width = wrap_width;
        self.heights.clear();
        self.heights.reserve(self.blocks.len());
        for block in &self.blocks {
            self.heights
                .push(estimate_block_height(block, body_size, wrap_width, style));
        }
        // Build cumulative offsets.
        self.cum_y.clear();
        self.cum_y.reserve(self.blocks.len());
        let mut acc = 0.0_f32;
        for h in &self.heights {
            self.cum_y.push(acc);
            acc += h;
        }
        self.total_height = acc;
    }
}

// ── Viewer widget ──────────────────────────────────────────────────

/// The main Markdown viewer widget.
pub struct MarkdownViewer {
    id_salt: &'static str,
}

impl MarkdownViewer {
    #[must_use]
    pub const fn new(id_salt: &'static str) -> Self {
        Self { id_salt }
    }

    /// Render markdown in a scrollable area with **viewport culling**.
    ///
    /// Only blocks overlapping the visible viewport are actually rendered;
    /// off-screen blocks are replaced by empty space allocations.
    pub fn show_scrollable(
        &self,
        ui: &mut egui::Ui,
        cache: &mut MarkdownCache,
        style: &MarkdownStyle,
        source: &str,
    ) {
        cache.ensure_parsed(source);

        let body_size = ui.text_style_height(&egui::TextStyle::Body);
        let wrap_width = ui.available_width();
        cache.ensure_heights(body_size, wrap_width, style);

        if cache.blocks.is_empty() {
            return;
        }

        egui::ScrollArea::vertical()
            .id_salt(self.id_salt)
            .show_viewport(ui, |ui, viewport| {
                // Allocate total height so scroll thumb is correct.
                ui.set_min_height(cache.total_height);

                let vis_top = viewport.min.y;
                let vis_bottom = viewport.max.y;

                // Binary search for first visible block.
                let first = match cache.cum_y.binary_search_by(|y| {
                    y.partial_cmp(&vis_top).unwrap_or(std::cmp::Ordering::Equal)
                }) {
                    Ok(i) => i,
                    Err(i) => i.saturating_sub(1),
                };

                // Allocate space for all blocks above viewport.
                if first > 0 {
                    let skip_h = cache.cum_y[first];
                    ui.add_space(skip_h);
                }

                // Render visible blocks.
                let mut idx = first;
                while idx < cache.blocks.len() {
                    let block_y = cache.cum_y[idx];
                    if block_y > vis_bottom {
                        break;
                    }
                    render_block(ui, &cache.blocks[idx], style, 0);
                    idx += 1;
                }

                // Allocate space for blocks below viewport.
                if idx < cache.blocks.len() {
                    let rendered_bottom = if idx > 0 {
                        cache.cum_y[idx - 1] + cache.heights[idx - 1]
                    } else {
                        0.0
                    };
                    let remaining = cache.total_height - rendered_bottom;
                    if remaining > 0.0 {
                        ui.add_space(remaining);
                    }
                }
            });
    }

    /// Render markdown inline (no scroll area, no culling).
    pub fn show(
        &self,
        ui: &mut egui::Ui,
        cache: &mut MarkdownCache,
        style: &MarkdownStyle,
        source: &str,
    ) {
        cache.ensure_parsed(source);
        render_blocks(ui, &cache.blocks, style, 0);
    }
}

// ── Height estimation ──────────────────────────────────────────────

/// Estimate pixel height for a top-level block without actually laying it out.
/// Errs on the side of *over*-estimating so that blocks are never clipped.
fn estimate_block_height(
    block: &Block,
    body_size: f32,
    wrap_width: f32,
    style: &MarkdownStyle,
) -> f32 {
    match block {
        Block::Heading { level, text } => {
            let idx = (*level as usize).saturating_sub(1).min(5);
            let size = body_size * style.headings[idx].font_scale;
            let text_h = estimate_text_height(&text.text, size, wrap_width);
            let sep = if *level <= 2 { 4.0 } else { 0.0 };
            size.mul_add(0.45, text_h) + sep
        }
        Block::Paragraph(text) => {
            body_size.mul_add(0.4, estimate_text_height(&text.text, body_size, wrap_width))
        }
        Block::Code { code, .. } => {
            let mono_size = body_size * 0.9;
            let lines = code.lines().count().max(1) as f32;
            body_size.mul_add(0.4, (lines * mono_size).mul_add(1.4, 12.0))
        }
        Block::Quote(inner) => {
            let inner_h: f32 = inner
                .iter()
                .map(|b| estimate_block_height(b, body_size, wrap_width - 20.0, style))
                .sum();
            body_size.mul_add(0.3, inner_h)
        }
        Block::UnorderedList(items) | Block::OrderedList { items, .. } => {
            let item_h: f32 = items
                .iter()
                .map(|item| {
                    let text_h =
                        estimate_text_height(&item.content.text, body_size, wrap_width - 40.0);
                    let child_h: f32 = item
                        .children
                        .iter()
                        .map(|b| estimate_block_height(b, body_size, wrap_width - 40.0, style))
                        .sum();
                    body_size.mul_add(0.2, text_h + child_h)
                })
                .sum();
            body_size.mul_add(0.2, item_h)
        }
        Block::ThematicBreak => body_size * 0.8,
        Block::Table { header, rows, .. } => {
            let row_h = body_size * 1.6;
            let hdr = if header.is_empty() { 0.0 } else { row_h };
            body_size.mul_add(0.4, (rows.len() as f32).mul_add(row_h, hdr))
        }
        Block::Image { .. } => body_size * 1.8,
    }
}

/// Rough text height estimate: characters / chars-per-line -> line count -> height.
fn estimate_text_height(text: &str, font_size: f32, wrap_width: f32) -> f32 {
    if text.is_empty() {
        return font_size;
    }
    let avg_char_width = font_size * 0.55;
    let chars_per_line = (wrap_width / avg_char_width).max(1.0);
    // Single pass over lines: count hard lines and estimate soft-wrapped lines.
    let mut hard_lines = 0_usize;
    let mut soft_lines = 0.0_f32;
    for line in text.lines() {
        hard_lines += 1;
        soft_lines += (line.len() as f32 / chars_per_line).ceil().max(1.0);
    }
    hard_lines = hard_lines.max(1);
    let total = soft_lines.max(hard_lines as f32);
    total * font_size * 1.3
}

// ── Block rendering ────────────────────────────────────────────────

fn render_blocks(ui: &mut egui::Ui, blocks: &[Block], style: &MarkdownStyle, indent: usize) {
    for block in blocks {
        render_block(ui, block, style, indent);
    }
}

fn render_block(ui: &mut egui::Ui, block: &Block, style: &MarkdownStyle, indent: usize) {
    let body_size = ui.text_style_height(&egui::TextStyle::Body);

    match block {
        Block::Heading { level, text } => {
            let idx = (*level as usize).saturating_sub(1).min(5);
            let hs = &style.headings[idx];
            let size = body_size * hs.font_scale;

            ui.add_space(size * 0.3);
            render_styled_text_ex(ui, text, style, Some(size), Some(hs.color));
            ui.add_space(size * 0.15);

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
                ui.add_space(16.0 * indent as f32);
            }
            render_styled_text(ui, text, style);
            ui.add_space(body_size * 0.4);
        }

        Block::Code { code, .. } => {
            let bg = style.code_bg.unwrap_or_else(|| ui.visuals().faint_bg_color);
            let available = ui.available_width();
            egui::Frame::NONE
                .fill(bg)
                .corner_radius(4.0)
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    ui.set_min_width(available - 12.0);
                    let mono = egui::FontId::new(body_size * 0.9, egui::FontFamily::Monospace);
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

            let rect_before = ui.available_rect_before_wrap();
            let bar_x = rect_before.min.x + 4.0;

            let inner_response = ui
                .allocate_ui_with_layout(
                    egui::vec2(ui.available_width() - 20.0, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        ui.indent("bq", |ui| {
                            render_blocks(ui, inner, style, indent + 1);
                        });
                    },
                )
                .response;

            let bar_top = inner_response.rect.min.y;
            let bar_bottom = inner_response.rect.max.y;
            ui.painter().line_segment(
                [egui::pos2(bar_x, bar_top), egui::pos2(bar_x, bar_bottom)],
                egui::Stroke::new(3.0, bar_color),
            );
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

// ── Inline text rendering ──────────────────────────────────────────

/// Inline formatting properties resolved from a `SpanKind`.
struct SpanFormat {
    font_family: egui::FontFamily,
    color: egui::Color32,
    background: egui::Color32,
    underline: bool,
    strikethrough: bool,
    italics: bool,
}

impl SpanFormat {
    fn resolve(
        kind: &SpanKind,
        style: &MarkdownStyle,
        base_color: egui::Color32,
        ui: &egui::Ui,
    ) -> Self {
        match kind {
            SpanKind::Plain | SpanKind::Strong => Self {
                font_family: egui::FontFamily::Proportional,
                color: base_color,
                background: egui::Color32::TRANSPARENT,
                underline: false,
                strikethrough: false,
                italics: false,
            },
            SpanKind::Emphasis => Self {
                font_family: egui::FontFamily::Proportional,
                color: base_color,
                background: egui::Color32::TRANSPARENT,
                underline: false,
                strikethrough: false,
                italics: true,
            },
            SpanKind::Strikethrough => Self {
                font_family: egui::FontFamily::Proportional,
                color: base_color,
                background: egui::Color32::TRANSPARENT,
                underline: false,
                strikethrough: true,
                italics: false,
            },
            SpanKind::Code => Self {
                font_family: egui::FontFamily::Monospace,
                color: base_color,
                background: style.code_bg.unwrap_or_else(|| ui.visuals().faint_bg_color),
                underline: false,
                strikethrough: false,
                italics: false,
            },
            SpanKind::Link(_) => Self {
                font_family: egui::FontFamily::Proportional,
                color: style
                    .link_color
                    .unwrap_or_else(|| ui.visuals().hyperlink_color),
                background: egui::Color32::TRANSPARENT,
                underline: true,
                strikethrough: false,
                italics: false,
            },
        }
    }
}

fn render_styled_text(ui: &mut egui::Ui, st: &StyledText, style: &MarkdownStyle) {
    render_styled_text_ex(ui, st, style, None, None);
}

fn render_styled_text_ex(
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
    let wrap_width = ui.available_width();

    let job = if st.spans.is_empty() {
        // Fast path: single format for entire text, no span resolution.
        egui::text::LayoutJob::simple(
            st.text.clone(),
            egui::FontId::new(size, egui::FontFamily::Proportional),
            base_color,
            wrap_width,
        )
    } else {
        let mut job = egui::text::LayoutJob {
            text: st.text.clone(),
            sections: Vec::with_capacity(st.spans.len()),
            wrap: egui::text::TextWrapping {
                max_width: wrap_width,
                ..Default::default()
            },
            ..Default::default()
        };

        for span in &st.spans {
            let sf = SpanFormat::resolve(&span.kind, style, base_color, ui);
            let span_size = if matches!(span.kind, SpanKind::Code) {
                size * 0.9
            } else {
                size
            };
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
            job.sections.push(egui::text::LayoutSection {
                leading_space: 0.0,
                byte_range: span.start..span.end,
                format,
            });
        }
        job
    };

    let galley = ui.fonts_mut(|f| f.layout_job(job));
    ui.label(galley);
}

// ── List rendering ─────────────────────────────────────────────────

fn render_unordered_list(
    ui: &mut egui::Ui,
    items: &[ListItem],
    style: &MarkdownStyle,
    indent: usize,
) {
    let bullet = match indent {
        0 => "\u{2022}",
        1 => "\u{25E6}",
        _ => "\u{25AA}",
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

// ── Table rendering ────────────────────────────────────────────────

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
            for (i, cell) in header.iter().enumerate() {
                let align = alignments.get(i).copied().unwrap_or(Alignment::None);
                render_table_cell(ui, cell, style, align, true);
            }
            ui.end_row();

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
        render_styled_text_ex(ui, cell, style, Some(body_size), None);
    } else {
        render_styled_text(ui, cell, style);
    }
}

// ── Utilities ──────────────────────────────────────────────────────

pub(crate) fn simple_hash(s: &str) -> u64 {
    // FNV-1a 64-bit.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in s.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write;

    #[test]
    fn cache_invalidates_on_text_change() {
        let mut cache = MarkdownCache::default();
        let s1 = "# Hello";
        let s2 = "# World";

        let h1 = simple_hash(s1);
        cache.blocks = crate::parse::parse_markdown(s1);
        cache.text_hash = h1;
        assert_eq!(cache.blocks.len(), 1);

        let h1b = simple_hash(s1);
        assert_eq!(h1, h1b);

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

    #[test]
    fn estimate_text_height_basic() {
        let h = estimate_text_height("Hello World", 14.0, 200.0);
        assert!(h > 0.0);
        assert!(h < 100.0);
    }

    #[test]
    fn estimate_text_height_wrapping() {
        let short = estimate_text_height("Hi", 14.0, 200.0);
        let long = estimate_text_height(&"word ".repeat(100), 14.0, 200.0);
        assert!(long > short);
    }

    #[test]
    fn estimate_block_height_heading() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Heading {
            level: 1,
            text: StyledText {
                text: "Title".to_owned(),
                spans: vec![],
            },
        };
        let h = estimate_block_height(&block, 14.0, 400.0, &style);
        assert!(h > 20.0);
    }

    #[test]
    fn cache_heights_invalidate_on_size_change() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# Hello\n\nParagraph");
        cache.ensure_heights(14.0, 400.0, &style);
        let h1 = cache.total_height;

        cache.ensure_heights(28.0, 400.0, &style);
        let h2 = cache.total_height;
        assert!(h2 > h1, "larger font should produce larger total height");
    }

    #[test]
    fn cum_y_correct() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# H1\n\nPara 1\n\nPara 2");
        cache.ensure_heights(14.0, 400.0, &style);

        assert_eq!(cache.cum_y.len(), cache.blocks.len());
        assert!((cache.cum_y[0] - 0.0).abs() < f32::EPSILON);
        for i in 1..cache.cum_y.len() {
            let expected = cache.cum_y[i - 1] + cache.heights[i - 1];
            assert!(
                (cache.cum_y[i] - expected).abs() < 0.01,
                "cum_y[{i}] should be sum of previous heights"
            );
        }
    }

    #[test]
    fn estimate_height_paragraph() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Paragraph(StyledText {
            text: "Hello world".to_owned(),
            spans: vec![],
        });
        let h = estimate_block_height(&block, 14.0, 400.0, &style);
        assert!(h > 0.0);
    }

    #[test]
    fn estimate_height_code_block() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Code {
            language: "rust".to_owned(),
            code: "fn main() {}\n".to_owned(),
        };
        let h = estimate_block_height(&block, 14.0, 400.0, &style);
        assert!(h > 0.0);
    }

    #[test]
    fn estimate_height_blockquote() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Quote(vec![Block::Paragraph(StyledText {
            text: "quoted".to_owned(),
            spans: vec![],
        })]);
        let h = estimate_block_height(&block, 14.0, 400.0, &style);
        assert!(h > 0.0);
    }

    #[test]
    fn estimate_height_list() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::UnorderedList(vec![ListItem {
            content: StyledText {
                text: "item".to_owned(),
                spans: vec![],
            },
            children: vec![],
        }]);
        let h = estimate_block_height(&block, 14.0, 400.0, &style);
        assert!(h > 0.0);
    }

    #[test]
    fn estimate_height_table() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Table {
            header: vec![StyledText {
                text: "Col".to_owned(),
                spans: vec![],
            }],
            alignments: vec![Alignment::None],
            rows: vec![vec![StyledText {
                text: "val".to_owned(),
                spans: vec![],
            }]],
        };
        let h = estimate_block_height(&block, 14.0, 400.0, &style);
        assert!(h > 0.0);
    }

    #[test]
    fn estimate_height_thematic_break() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::ThematicBreak;
        let h = estimate_block_height(&block, 14.0, 400.0, &style);
        assert!(h > 0.0);
    }

    #[test]
    fn estimate_height_image() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Image {
            url: "https://img.png".to_owned(),
            alt: "alt".to_owned(),
        };
        let h = estimate_block_height(&block, 14.0, 400.0, &style);
        assert!(h > 0.0);
    }

    #[test]
    fn estimate_height_ordered_list() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::OrderedList {
            start: 1,
            items: vec![ListItem {
                content: StyledText {
                    text: "first".to_owned(),
                    spans: vec![],
                },
                children: vec![],
            }],
        };
        let h = estimate_block_height(&block, 14.0, 400.0, &style);
        assert!(h > 0.0);
    }

    #[test]
    fn cache_clear_resets_all() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# Title\n\nBody text");
        cache.ensure_heights(14.0, 400.0, &style);
        assert!(!cache.blocks.is_empty());

        cache.clear();
        assert!(cache.blocks.is_empty());
        assert!(cache.heights.is_empty());
        assert!(cache.cum_y.is_empty());
        assert!((cache.total_height - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn style_with_heading_scales() {
        let mut style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let scales = [3.0, 2.5, 2.0, 1.5, 1.2, 1.0];
        style.with_heading_scales(scales);
        for (i, &expected) in scales.iter().enumerate() {
            assert!(
                (style.headings[i].font_scale - expected).abs() < f32::EPSILON,
                "heading {i} scale mismatch"
            );
        }
    }

    #[test]
    fn height_estimation_perf() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();

        // Build a ~50KB document.
        let mut doc = String::with_capacity(50_000);
        for i in 0..200 {
            write!(doc, "## Heading {i}\n\n").ok();
            doc.push_str("Lorem ipsum dolor sit amet, consectetur adipiscing elit. ");
            doc.push_str("Sed do eiusmod tempor incididunt.\n\n");
            if i % 5 == 0 {
                doc.push_str("```\ncode block\n```\n\n");
            }
        }

        cache.ensure_parsed(&doc);
        let iterations = 1000;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            cache.heights.clear();
            cache.ensure_heights(14.0, 600.0, &style);
        }
        let elapsed = start.elapsed();
        let per_iter = elapsed / iterations;
        assert!(
            per_iter.as_micros() < 500,
            "height estimation too slow: {per_iter:?} for {} blocks",
            cache.blocks.len()
        );
    }
}
