#![forbid(unsafe_code)]
//! Render parsed Markdown blocks into egui widgets.
//!
//! Key feature: viewport culling in `show_scrollable` — only blocks
//! overlapping the visible region are laid out, giving O(visible) cost.

use crate::parse::{Alignment, Block, ListItem, SpanStyle, StyledText, parse_markdown};
use crate::style::MarkdownStyle;

// ── Cache ──────────────────────────────────────────────────────────

/// Cached pre-parsed blocks, height estimates, and the source hash.
#[derive(Default)]
pub struct MarkdownCache {
    text_hash: u64,
    text_len: usize,
    text_ptr: usize,
    pub blocks: Vec<Block>,
    /// Estimated pixel height for each top-level block (same len as `blocks`).
    pub heights: Vec<f32>,
    /// Cumulative Y offsets: `cum_y[i]` = sum of heights[0..i].
    pub cum_y: Vec<f32>,
    /// Total estimated height of all blocks.
    pub total_height: f32,
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

    pub fn ensure_parsed(&mut self, source: &str) {
        // Fast pointer+length check: if the source is the same allocation
        // and length, skip hash entirely (common in frame-to-frame rendering).
        let ptr = source.as_ptr() as usize;
        let len = source.len();
        if ptr == self.text_ptr && len == self.text_len {
            return;
        }

        // Pointer changed — compute hash once and compare.
        let hash = simple_hash(source);
        self.text_ptr = ptr;
        self.text_len = len;

        if self.text_hash == hash {
            return;
        }

        // Content actually changed — re-parse.
        self.text_hash = hash;
        self.blocks = parse_markdown(source);
        self.heights.clear();
        self.cum_y.clear();
        self.total_height = 0.0;
    }

    pub fn ensure_heights(&mut self, body_size: f32, wrap_width: f32, style: &MarkdownStyle) {
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
    ///
    /// If `scroll_to_y` is `Some(y)`, the scroll area will jump to that offset.
    pub fn show_scrollable(
        &self,
        ui: &mut egui::Ui,
        cache: &mut MarkdownCache,
        style: &MarkdownStyle,
        source: &str,
        scroll_to_y: Option<f32>,
    ) {
        cache.ensure_parsed(source);

        let body_size = ui.text_style_height(&egui::TextStyle::Body);
        let wrap_width = ui.available_width();
        cache.ensure_heights(body_size, wrap_width, style);

        if cache.blocks.is_empty() {
            return;
        }

        let mut scroll_area = egui::ScrollArea::vertical()
            .id_salt(self.id_salt)
            .auto_shrink([false, false]);

        if let Some(y) = scroll_to_y {
            scroll_area = scroll_area.vertical_scroll_offset(y);
        }

        scroll_area.show_viewport(ui, |ui, viewport| {
            // Allocate total height so scroll thumb is correct.
            ui.set_min_height(cache.total_height);

            let vis_top = viewport.min.y;
            let vis_bottom = viewport.max.y;

            // Binary search for first visible block.
            let first = match cache
                .cum_y
                .binary_search_by(|y| y.partial_cmp(&vis_top).unwrap_or(std::cmp::Ordering::Equal))
            {
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
                let remaining = cache.total_height - cache.cum_y[idx];
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
#[allow(clippy::cast_precision_loss)] // UI math — counts are small
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
            let lines = (bytecount_newlines(code.as_bytes()) + 1).max(1) as f32;
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
            let num_cols = header.len().max(1);
            let col_width = (wrap_width / num_cols as f32).max(40.0);
            let base_row_h = body_size * 1.6;
            let hdr = if header.is_empty() {
                0.0
            } else {
                header
                    .iter()
                    .map(|c| estimate_text_height(&c.text, body_size, col_width).max(base_row_h))
                    .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                    .unwrap_or(base_row_h)
            };
            let rows_h: f32 = rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|c| {
                            estimate_text_height(&c.text, body_size, col_width).max(base_row_h)
                        })
                        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                        .unwrap_or(base_row_h)
                })
                .sum();
            body_size.mul_add(0.4, hdr + rows_h)
        }
        Block::Image { .. } => body_size * 1.8,
    }
}

/// Rough text height estimate using byte-level newline counting.
/// Avoids `.lines()` iteration for better throughput on large texts.
#[allow(clippy::cast_precision_loss)] // UI math — counts are small
fn estimate_text_height(text: &str, font_size: f32, wrap_width: f32) -> f32 {
    if text.is_empty() {
        return font_size;
    }
    let avg_char_width = font_size * 0.55;
    let chars_per_line = (wrap_width / avg_char_width).max(1.0);
    let total_len = text.len();
    // Count newlines by scanning bytes (much faster than .lines() for large text).
    let newline_count = bytecount_newlines(text.as_bytes());
    let hard_lines = (newline_count + 1).max(1);
    // Estimate average line length for soft-wrapping calculation.
    let avg_line_len = total_len as f32 / hard_lines as f32;
    let wraps_per_line = (avg_line_len / chars_per_line).ceil().max(1.0);
    let total = hard_lines as f32 * wraps_per_line;
    total * font_size * 1.3
}

/// Fast newline counting via byte scan.
fn bytecount_newlines(bytes: &[u8]) -> usize {
    // Process 8 bytes at a time for throughput.
    let mut count = 0_usize;
    let chunks = bytes.chunks_exact(8);
    let remainder = chunks.remainder();
    for chunk in chunks {
        // Safety: chunks_exact guarantees 8 bytes.
        let word = u64::from_ne_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]);
        // SWAR: broadcast 0x0A (newline) to each byte, XOR to find matches,
        // then use the standard "has zero byte" trick.
        let xor = word ^ 0x0A0A_0A0A_0A0A_0A0A;
        let has_zero = (xor.wrapping_sub(0x0101_0101_0101_0101)) & !xor & 0x8080_8080_8080_8080;
        count += has_zero.count_ones() as usize;
    }
    for &b in remainder {
        if b == b'\n' {
            count += 1;
        }
    }
    count
}

// ── Block rendering ────────────────────────────────────────────────

fn render_blocks(ui: &mut egui::Ui, blocks: &[Block], style: &MarkdownStyle, indent: usize) {
    for block in blocks {
        render_block(ui, block, style, indent);
    }
}

#[allow(clippy::cast_precision_loss)] // UI math — indent/count values are small
fn render_block(ui: &mut egui::Ui, block: &Block, style: &MarkdownStyle, indent: usize) {
    let body_size = ui.text_style_height(&egui::TextStyle::Body);

    match block {
        Block::Heading { level, text } => {
            render_heading(ui, *level, text, style, body_size);
        }

        Block::Paragraph(text) => {
            if indent > 0 {
                ui.add_space(16.0 * indent as f32);
            }
            render_styled_text(ui, text, style);
            ui.add_space(body_size * 0.4);
        }

        Block::Code { language, code } => {
            render_code_block(ui, language, code, style, body_size);
        }

        Block::Quote(inner) => {
            render_blockquote(ui, inner, style, indent, body_size);
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
            render_hr(ui, style, body_size);
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

fn render_heading(
    ui: &mut egui::Ui,
    level: u8,
    text: &StyledText,
    style: &MarkdownStyle,
    body_size: f32,
) {
    let idx = (level as usize).saturating_sub(1).min(5);
    let hs = &style.headings[idx];
    let size = body_size * hs.font_scale;

    ui.add_space(size * 0.3);
    render_styled_text_ex(ui, text, style, Some(size), Some(hs.color));
    ui.add_space(size * 0.15);

    if level <= 2 {
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

fn render_code_block(
    ui: &mut egui::Ui,
    language: &str,
    code: &str,
    style: &MarkdownStyle,
    body_size: f32,
) {
    let bg = style.code_bg.unwrap_or_else(|| ui.visuals().faint_bg_color);
    let available = ui.available_width();
    if !language.is_empty() {
        ui.label(egui::RichText::new(language).small().weak());
    }
    egui::Frame::NONE
        .fill(bg)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.set_min_width(available - 12.0);
            egui::ScrollArea::horizontal().show(ui, |ui| {
                let mono = egui::FontId::new(body_size * 0.9, egui::FontFamily::Monospace);
                ui.label(
                    egui::RichText::new(code.trim_end())
                        .font(mono)
                        .color(ui.visuals().text_color()),
                );
            });
        });
    ui.add_space(body_size * 0.4);
}

fn render_blockquote(
    ui: &mut egui::Ui,
    inner: &[Block],
    style: &MarkdownStyle,
    indent: usize,
    body_size: f32,
) {
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

fn render_hr(ui: &mut egui::Ui, style: &MarkdownStyle, body_size: f32) {
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

// ── Inline text rendering ──────────────────────────────────────────

/// Inline formatting properties resolved from a composite `SpanStyle`.
#[allow(clippy::struct_excessive_bools)]
struct SpanFormat {
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

    let has_links = st.spans.iter().any(|s| s.style.link.is_some());

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
fn render_text_with_links(
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
        for span in &st.spans {
            let text = &st.text[span.start..span.end];
            if let Some(ref url) = span.style.link {
                // Render as clickable hyperlink.
                let font = egui::FontId::new(size, egui::FontFamily::Proportional);
                let rt = egui::RichText::new(text).font(font);
                let rt = if span.style.emphasis() {
                    rt.italics()
                } else {
                    rt
                };
                ui.hyperlink_to(rt, url.as_ref());
            } else {
                // Render as label with formatting.
                let font_family = if span.style.code() {
                    egui::FontFamily::Monospace
                } else {
                    egui::FontFamily::Proportional
                };
                let span_size = if span.style.code() { size * 0.9 } else { size };
                let mut rt =
                    egui::RichText::new(text).font(egui::FontId::new(span_size, font_family));

                let color = if span.style.strong() {
                    strengthen_color(base_color)
                } else {
                    base_color
                };
                rt = rt.color(color);

                if span.style.emphasis() {
                    rt = rt.italics();
                }
                if span.style.strikethrough() {
                    rt = rt.strikethrough();
                }
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
fn build_layout_job(
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
            byte_range: span.start..span.end,
            format,
        });
    }
    job
}

// ── List rendering ─────────────────────────────────────────────────

#[allow(clippy::cast_precision_loss)] // UI math — indent values are small
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
    let indent_px = 16.0 * indent as f32;
    let body_size = ui.text_style_height(&egui::TextStyle::Body);

    for item in items {
        ui.horizontal(|ui| {
            ui.add_space(indent_px);
            let bullet_text = match item.checked {
                Some(true) => "\u{2611}",
                Some(false) => "\u{2610}",
                None => bullet,
            };
            // Fixed-width bullet column aligned to body text.
            ui.allocate_ui_with_layout(
                egui::vec2(body_size * 1.2, body_size),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.label(bullet_text);
                },
            );
            ui.add_space(4.0);
            ui.vertical(|ui| {
                render_styled_text(ui, &item.content, style);
            });
        });
        if !item.children.is_empty() {
            render_blocks(ui, &item.children, style, indent + 1);
        }
    }
}

#[allow(clippy::cast_precision_loss)] // UI math — indent values are small
fn render_ordered_list(
    ui: &mut egui::Ui,
    start: u64,
    items: &[ListItem],
    style: &MarkdownStyle,
    indent: usize,
) {
    let indent_px = 16.0 * indent as f32;
    let body_size = ui.text_style_height(&egui::TextStyle::Body);
    let mut num_buf = String::with_capacity(8);

    for (i, item) in items.iter().enumerate() {
        let num = start + i as u64;
        ui.horizontal(|ui| {
            ui.add_space(indent_px);
            // Fixed-width number column, right-aligned for neat stacking.
            let num_width = body_size * 2.0;
            ui.allocate_ui_with_layout(
                egui::vec2(num_width, body_size),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    match item.checked {
                        Some(true) => ui.label("\u{2611}"),
                        Some(false) => ui.label("\u{2610}"),
                        None => {
                            use std::fmt::Write;
                            num_buf.clear();
                            let _ = write!(num_buf, "{num}.");
                            ui.label(&*num_buf)
                        }
                    };
                },
            );
            ui.add_space(4.0);
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

#[allow(clippy::cast_precision_loss)] // UI math — column count is small
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
    let body_size = ui.text_style_height(&egui::TextStyle::Body);

    egui::Grid::new(ui.next_auto_id())
        .striped(true)
        .min_col_width(col_width)
        .min_row_height(body_size * 1.4)
        .show(ui, |ui| {
            for (i, cell) in header.iter().enumerate() {
                let align = alignments.get(i).copied().unwrap_or(Alignment::None);
                render_table_cell(ui, cell, style, align, true);
            }
            ui.end_row();

            for row in rows {
                for (i, cell) in row.iter().take(num_cols).enumerate() {
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
    align: Alignment,
    is_header: bool,
) {
    let layout = match align {
        Alignment::Right => egui::Layout::right_to_left(egui::Align::TOP),
        Alignment::Center => egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
        Alignment::Left | Alignment::None => egui::Layout::left_to_right(egui::Align::TOP),
    };
    ui.with_layout(layout, |ui| {
        if is_header {
            let body_size = ui.text_style_height(&egui::TextStyle::Body);
            render_styled_text_ex(ui, cell, style, Some(body_size), None);
        } else {
            render_styled_text(ui, cell, style);
        }
    });
}

// ── Utilities ──────────────────────────────────────────────────────

/// Approximate "bold" by brightening/saturating a colour.
/// egui has no bold font weight, so we visually distinguish strong text.
fn strengthen_color(color: egui::Color32) -> egui::Color32 {
    let [red, green, blue, alpha] = color.to_array();
    let boost = |val: u8| {
        // Max boost is (255 - 0) / 5 = 51, so this always fits in u8.
        let delta = (u16::from(255_u8.saturating_sub(val))) / 5;
        val.saturating_add(delta.min(255) as u8)
    };
    egui::Color32::from_rgba_premultiplied(boost(red), boost(green), boost(blue), alpha)
}

pub fn simple_hash(s: &str) -> u64 {
    // FNV-1a–inspired 64-bit hash, processing 8 bytes at a time for throughput.
    const BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;

    let bytes = s.as_bytes();
    let chunks = bytes.chunks_exact(8);
    let remainder = chunks.remainder();
    let mut h: u64 = BASIS;

    for chunk in chunks {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(chunk);
        let word = u64::from_le_bytes(buf);
        h ^= word;
        h = h.wrapping_mul(PRIME);
    }

    for &b in remainder {
        h ^= u64::from(b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::panic)]
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
        let _ = style.with_heading_colors(colors);
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
            checked: None,
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
                checked: None,
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
        let _ = style.with_heading_scales(scales);
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
        if !cfg!(debug_assertions) {
            assert!(
                per_iter.as_micros() < 500,
                "height estimation too slow: {per_iter:?} for {} blocks",
                cache.blocks.len()
            );
        }
    }

    #[test]
    fn estimate_height_with_task_list() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::UnorderedList(vec![
            ListItem {
                content: StyledText {
                    text: "checked".to_owned(),
                    spans: vec![],
                },
                children: vec![],
                checked: Some(true),
            },
            ListItem {
                content: StyledText {
                    text: "unchecked".to_owned(),
                    spans: vec![],
                },
                children: vec![],
                checked: Some(false),
            },
        ]);
        let h = estimate_block_height(&block, 14.0, 400.0, &style);
        assert!(h > 0.0);
    }

    // ── Headless rendering validation tests ────────────────────────

    /// Create a headless egui context primed for rendering.
    fn headless_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        // Warm up so fonts are available.
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        ctx
    }

    fn raw_input_1024x768() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1024.0, 768.0),
            )),
            ..Default::default()
        }
    }

    /// Render markdown in a headless frame and return `(blocks, total_height)`.
    fn headless_render(source: &str) -> (Vec<Block>, f32) {
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let viewer = MarkdownViewer::new("test");

        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show(ui, &mut cache, &style, source);
            });
        });

        // Ensure heights are computed (show() does parse but not heights).
        cache.ensure_heights(14.0, 900.0, &style);
        let total_height = cache.total_height;
        (cache.blocks.drain(..).collect(), total_height)
    }

    /// Render markdown in scrollable mode and return cache state.
    fn headless_render_scrollable(source: &str, scroll_to_y: Option<f32>) -> (usize, f32) {
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let viewer = MarkdownViewer::new("test_scroll");

        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show_scrollable(ui, &mut cache, &style, source, scroll_to_y);
            });
        });

        (cache.blocks.len(), cache.total_height)
    }

    // ── Images ─────────────────────────────────────────────────────

    #[test]
    fn render_image_with_alt() {
        let md = "![Alt text](image.png)";
        let (blocks, _) = headless_render(md);
        assert_eq!(blocks.len(), 1, "expected 1 block for image");
        match &blocks[0] {
            Block::Image { url, alt } => {
                assert_eq!(url, "image.png");
                assert_eq!(alt, "Alt text");
            }
            other => panic!("expected Image block, got {other:?}"),
        }
    }

    #[test]
    fn render_image_without_alt() {
        let md = "![](https://example.com/pic.jpg)";
        let (blocks, _) = headless_render(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Image { url, alt } => {
                assert_eq!(url, "https://example.com/pic.jpg");
                assert!(alt.is_empty());
            }
            other => panic!("expected Image block, got {other:?}"),
        }
    }

    #[test]
    fn render_multiple_images_no_panic() {
        let md = "\
![Tiny](tiny.png)

![Small](small.png)

![Large](large.png)

![Missing](not-found.png)
";
        let (blocks, height) = headless_render(md);
        assert!(blocks.len() >= 4, "expected at least 4 image blocks");
        assert!(height > 0.0);
    }

    // ── Heading colours ────────────────────────────────────────────

    #[test]
    fn heading_colors_applied_in_colored_style() {
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        for (i, expected) in crate::DARK_HEADING_COLORS.iter().enumerate() {
            assert_eq!(
                style.headings[i].color, *expected,
                "heading {i} colour should match DARK palette"
            );
        }
    }

    #[test]
    fn heading_font_scales_descend() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        for i in 0..5 {
            assert!(
                style.headings[i].font_scale >= style.headings[i + 1].font_scale,
                "heading {i} scale should be >= heading {} scale",
                i + 1
            );
        }
    }

    #[test]
    fn all_heading_levels_render_without_panic() {
        let md = "\
# H1 heading
## H2 heading
### H3 heading
#### H4 heading
##### H5 heading
###### H6 heading

Normal paragraph.
";
        let (blocks, height) = headless_render(md);
        let heading_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::Heading { .. }))
            .count();
        assert_eq!(heading_count, 6, "expected 6 headings");
        assert!(height > 0.0);
    }

    // ── Tables ─────────────────────────────────────────────────────

    #[test]
    fn render_simple_table() {
        let md = "\
| Name  | Age |
|-------|-----|
| Alice | 30  |
| Bob   | 25  |
";
        let (blocks, height) = headless_render(md);
        let table_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::Table { .. }))
            .count();
        assert_eq!(table_count, 1);
        assert!(height > 0.0);
    }

    #[test]
    fn render_wide_table_no_panic() {
        let md = "\
| A | B | C | D | E | F | G | H | I | J | K | L |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 |
| a | b | c | d | e | f | g | h | i | j | k | l |
";
        let (blocks, height) = headless_render(md);
        assert_eq!(
            blocks
                .iter()
                .filter(|b| matches!(b, Block::Table { .. }))
                .count(),
            1
        );
        assert!(height > 0.0);
    }

    #[test]
    fn render_table_with_empty_cells() {
        let md = "\
| Feature | Yes | No |
|---------|-----|----|
| A       | ✅  |    |
|         |     |    |
| C       |     | ❌ |
";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::Table { rows, .. } => {
                assert_eq!(rows.len(), 3, "expected 3 data rows");
                // Row 2 should have empty text
                assert!(
                    rows[1][0].text.trim().is_empty(),
                    "middle row col 0 should be empty"
                );
            }
            other => panic!("expected Table, got {other:?}"),
        }
    }

    #[test]
    fn render_table_with_code_in_cells() {
        let md = "\
| Function | Type |
|----------|------|
| `parse`  | `fn()`|
";
        let (blocks, _) = headless_render(md);
        assert!(matches!(&blocks[0], Block::Table { .. }));
    }

    #[test]
    fn render_minimal_table() {
        let md = "\
| A |
|---|
| 1 |
";
        let (blocks, _) = headless_render(md);
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn render_table_alignment() {
        let md = "\
| Left | Center | Right |
|:-----|:------:|------:|
| l    | c      | r     |
";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::Table { alignments, .. } => {
                assert_eq!(alignments[0], Alignment::Left);
                assert_eq!(alignments[1], Alignment::Center);
                assert_eq!(alignments[2], Alignment::Right);
            }
            other => panic!("expected Table, got {other:?}"),
        }
    }

    #[test]
    fn render_table_column_mismatch_no_panic() {
        // Rows with more columns than header should render without panic
        // (extra columns are silently dropped).
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let viewer = MarkdownViewer::new("table_mismatch");

        let md = "\
| A | B |
|---|---|
| 1 | 2 | 3 | 4 |
| x |
";
        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show_scrollable(ui, &mut cache, &style, md, None);
            });
        });
        assert!(!cache.blocks.is_empty());
    }

    // ── Lists ──────────────────────────────────────────────────────

    #[test]
    fn render_simple_bullet_list() {
        let md = "\
- Item one
- Item two
- Item three
";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 3);
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    #[test]
    fn render_nested_bullet_list() {
        let md = "\
- Top
  - Middle
    - Deep
      - Deepest
  - Back to middle
- Back to top
";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
        // Top-level list should have 2 items
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 2, "expected 2 top-level items");
                // First item should have nested children
                assert!(
                    !items[0].children.is_empty(),
                    "first item should have children"
                );
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    #[test]
    fn render_ordered_list_double_digits() {
        let md = "\
1. First
2. Second
3. Third
4. Fourth
5. Fifth
6. Sixth
7. Seventh
8. Eighth
9. Ninth
10. Tenth
11. Eleventh
";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 1);
                assert_eq!(items.len(), 11);
            }
            other => panic!("expected OrderedList, got {other:?}"),
        }
    }

    #[test]
    fn render_task_list() {
        let md = "\
- [x] Done
- [ ] Not done
- Regular
";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items[0].checked, Some(true));
                assert_eq!(items[1].checked, Some(false));
                assert_eq!(items[2].checked, None);
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    #[test]
    fn render_mixed_list_types() {
        let md = "\
1. Ordered parent
   - Unordered child
   - Another child
2. Second ordered
";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    // ── Code blocks ────────────────────────────────────────────────

    #[test]
    fn render_small_code_block() {
        let md = "```rust\nfn main() {}\n```";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::Code { language, code } => {
                assert_eq!(language, "rust");
                assert!(code.contains("fn main()"));
            }
            other => panic!("expected Code, got {other:?}"),
        }
    }

    #[test]
    fn render_large_code_block_no_panic() {
        let mut code_lines = String::from("```python\n");
        for i in 0..200 {
            writeln!(code_lines, "line_{i} = {i} * 2").ok();
        }
        code_lines.push_str("```\n");

        let (blocks, height) = headless_render(&code_lines);
        assert!(matches!(&blocks[0], Block::Code { .. }));
        assert!(
            height > 100.0,
            "large code block should have significant height"
        );
    }

    #[test]
    fn render_code_in_heading() {
        let md = "## The `render()` Function";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 2);
                assert!(text.text.contains("render()"));
                // Should have code span
                assert!(
                    text.spans.iter().any(|s| s.style.code()),
                    "heading should contain code span"
                );
            }
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    // ── Blockquotes ────────────────────────────────────────────────

    #[test]
    fn render_nested_blockquote() {
        let md = "\
> Level 1
> > Level 2
> > > Level 3
";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    // ── Unicode & special characters ───────────────────────────────

    #[test]
    fn render_emoji_no_panic() {
        let md = "\
# 🎨 Emoji Heading

🚀 Rocket 💡 Lightbulb 🔧 Wrench ⚙️ Gear

- 🔴 Red
- 🟢 Green
- 🔵 Blue
";
        let (blocks, height) = headless_render(md);
        assert!(blocks.len() >= 3);
        assert!(height > 0.0);
    }

    #[test]
    fn render_cjk_no_panic() {
        let md = "\
## 日本語テスト

中文测试 (Chinese test)

한국어 테스트 (Korean test)
";
        let (blocks, _) = headless_render(md);
        assert!(!blocks.is_empty());
    }

    #[test]
    fn render_math_symbols_no_panic() {
        let md = "∑(i=1 to n) of xᵢ² = α·β + γ/δ ± ε\n\n∀x ∈ ℝ, ∃y : x² + y² = r²";
        let (blocks, _) = headless_render(md);
        assert!(!blocks.is_empty());
    }

    #[test]
    fn render_rtl_no_panic() {
        let md = "بسم الله الرحمن الرحيم\n\nשלום עולם";
        let (blocks, _) = headless_render(md);
        assert!(!blocks.is_empty());
    }

    #[test]
    fn render_zero_width_chars_no_panic() {
        // ZWJ, soft hyphen, NBSP
        let md = "zero\u{200D}width\u{200D}joiner\n\nsoft\u{00AD}hyphen\n\nnon\u{00A0}breaking";
        let (blocks, _) = headless_render(md);
        assert!(!blocks.is_empty());
    }

    #[test]
    fn render_box_drawing_no_panic() {
        let md = "\
```
┌─────────┐
│ Content  │
├─────────┤
│ ▲ ▼ ◆ ● │
└─────────┘
```
";
        let (blocks, _) = headless_render(md);
        assert!(!blocks.is_empty());
    }

    // ── Horizontal rules ───────────────────────────────────────────

    #[test]
    fn render_thematic_breaks() {
        let md = "Above\n\n---\n\nMiddle\n\n***\n\nBelow";
        let (blocks, _) = headless_render(md);
        let break_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::ThematicBreak))
            .count();
        assert_eq!(break_count, 2);
    }

    // ── Scrollable rendering ───────────────────────────────────────

    #[test]
    fn scrollable_render_basic() {
        let md = "# Title\n\nParagraph text.\n\n## Section 2\n\nMore text.";
        let (block_count, total_height) = headless_render_scrollable(md, None);
        assert!(block_count >= 4);
        assert!(total_height > 0.0);
    }

    #[test]
    fn scrollable_render_with_scroll_offset() {
        let mut doc = String::with_capacity(10_000);
        for i in 0..50 {
            write!(doc, "## Section {i}\n\nContent for section {i}.\n\n").ok();
        }
        let (_, total_height) = headless_render_scrollable(&doc, None);
        assert!(total_height > 200.0);

        // Scroll to middle — should not panic
        let (_, _) = headless_render_scrollable(&doc, Some(total_height / 2.0));
        // Scroll to near end — should not panic
        let (_, _) = headless_render_scrollable(&doc, Some(total_height - 50.0));
        // Scroll past end — should not panic
        let (_, _) = headless_render_scrollable(&doc, Some(total_height + 1000.0));
    }

    #[test]
    fn scrollable_render_empty_doc() {
        let (block_count, total_height) = headless_render_scrollable("", None);
        assert_eq!(block_count, 0);
        assert!((total_height - 0.0).abs() < f32::EPSILON);
    }

    // ── Stress tests with headless rendering ───────────────────────

    #[test]
    fn render_stress_large_mixed_doc() {
        let doc = crate::stress::large_mixed_doc(100);
        let (blocks, height) = headless_render(&doc);
        assert!(blocks.len() > 50, "100KB doc should have many blocks");
        assert!(height > 500.0);
    }

    #[test]
    fn render_stress_unicode_doc() {
        let doc = crate::stress::unicode_stress_doc(50);
        let (blocks, height) = headless_render(&doc);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    #[test]
    fn render_stress_table_heavy_doc() {
        let doc = crate::stress::table_heavy_doc(50);
        let (blocks, height) = headless_render(&doc);
        let table_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::Table { .. }))
            .count();
        assert!(table_count > 0, "table-heavy doc should have tables");
        assert!(height > 0.0);
    }

    #[test]
    fn render_stress_emoji_doc() {
        let doc = crate::stress::emoji_heavy_doc(50);
        let (blocks, height) = headless_render(&doc);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    #[test]
    fn render_stress_task_list_doc() {
        let doc = crate::stress::task_list_doc(50);
        let (blocks, height) = headless_render(&doc);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    #[test]
    fn render_stress_pathological_doc() {
        let doc = crate::stress::pathological_doc(50);
        let (blocks, height) = headless_render(&doc);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    #[test]
    fn render_stress_minimal_edge_cases() {
        for (label, doc) in crate::stress::minimal_docs() {
            let (_, height) = headless_render(&doc);
            assert!(
                height >= 0.0,
                "minimal doc '{label}' should render with non-negative height"
            );
        }
    }

    // ── Viewport culling correctness ───────────────────────────────

    #[test]
    fn viewport_culling_height_matches_inline() {
        let mut doc = String::with_capacity(5_000);
        for i in 0..20 {
            write!(
                doc,
                "## Section {i}\n\nParagraph content for section {i}.\n\n"
            )
            .ok();
        }
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let mut cache1 = MarkdownCache::default();
        let mut cache2 = MarkdownCache::default();

        cache1.ensure_parsed(&doc);
        cache2.ensure_parsed(&doc);

        cache1.ensure_heights(14.0, 900.0, &style);
        cache2.ensure_heights(14.0, 900.0, &style);

        // Both caches should agree on total height
        assert!(
            (cache1.total_height - cache2.total_height).abs() < f32::EPSILON,
            "two caches parsing the same content should produce the same total height"
        );
        assert_eq!(cache1.blocks.len(), cache2.blocks.len());
    }

    #[test]
    fn cum_y_monotonically_increases() {
        let doc = crate::stress::large_mixed_doc(50);
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 900.0, &style);

        for i in 1..cache.cum_y.len() {
            assert!(
                cache.cum_y[i] >= cache.cum_y[i - 1],
                "cum_y should be monotonically non-decreasing at index {i}"
            );
        }
    }

    // ── Combined stress test (the full markdown file) ──────────────

    #[test]
    fn render_comprehensive_stress_test() {
        let md = include_str!("../../../test-assets/stress-test.md");
        let (blocks, height) = headless_render(md);
        assert!(
            blocks.len() > 30,
            "stress test should produce many blocks, got {}",
            blocks.len()
        );
        assert!(
            height > 1000.0,
            "comprehensive stress test should have significant height"
        );

        // Verify key block types are present
        let has_heading = blocks.iter().any(|b| matches!(b, Block::Heading { .. }));
        let has_table = blocks.iter().any(|b| matches!(b, Block::Table { .. }));
        let has_code = blocks.iter().any(|b| matches!(b, Block::Code { .. }));
        let has_list = blocks.iter().any(|b| matches!(b, Block::UnorderedList(_)));
        let has_ordered = blocks
            .iter()
            .any(|b| matches!(b, Block::OrderedList { .. }));
        let has_quote = blocks.iter().any(|b| matches!(b, Block::Quote(_)));
        let has_hr = blocks.iter().any(|b| matches!(b, Block::ThematicBreak));
        let has_image = blocks.iter().any(|b| matches!(b, Block::Image { .. }));

        assert!(has_heading, "stress test should have headings");
        assert!(has_table, "stress test should have tables");
        assert!(has_code, "stress test should have code blocks");
        assert!(has_list, "stress test should have unordered lists");
        assert!(has_ordered, "stress test should have ordered lists");
        assert!(has_quote, "stress test should have blockquotes");
        assert!(has_hr, "stress test should have thematic breaks");
        assert!(has_image, "stress test should have images");
    }

    #[test]
    fn scrollable_stress_test_all_positions() {
        let md = include_str!("../../../test-assets/stress-test.md");
        let (_, total_height) = headless_render_scrollable(md, None);
        assert!(total_height > 0.0);

        // Scroll through document at intervals — none should panic
        let step = total_height / 20.0;
        for i in 0..22 {
            let y = step * i as f32;
            let _ = headless_render_scrollable(md, Some(y));
        }
    }
}
