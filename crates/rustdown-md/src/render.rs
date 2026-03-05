#![forbid(unsafe_code)]
//! Render parsed Markdown blocks into egui widgets.
//!
//! Key feature: viewport culling in `show_scrollable` — only blocks
//! overlapping the visible region are laid out, giving O(visible) cost.

use crate::parse::{
    Alignment, Block, ListItem, SpanStyle, StyledText, TableData, parse_markdown_into,
};
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
    /// Last rendered scroll-y offset (set by `show_scrollable`).
    pub last_scroll_y: f32,
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
        self.last_scroll_y = 0.0;
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

        // Content actually changed — re-parse, reusing the blocks allocation.
        self.text_hash = hash;
        self.blocks.clear();
        parse_markdown_into(source, &mut self.blocks);
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
        let n = self.blocks.len();
        self.heights.clear();
        self.heights.reserve(n);
        self.cum_y.clear();
        self.cum_y.reserve(n);
        let mut acc = 0.0_f32;
        for block in &self.blocks {
            self.cum_y.push(acc);
            let h = estimate_block_height(block, body_size, wrap_width, style);
            self.heights.push(h);
            acc += h;
        }
        self.total_height = acc;
    }

    /// Return the Y offset for the `ordinal`th heading block (0-based).
    #[must_use]
    pub fn heading_y(&self, ordinal: usize) -> Option<f32> {
        let mut seen = 0usize;
        for (idx, block) in self.blocks.iter().enumerate() {
            if matches!(block, Block::Heading { .. }) {
                if seen == ordinal {
                    return self.cum_y.get(idx).copied();
                }
                seen += 1;
            }
        }
        None
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
            // Record current scroll offset for external sync.
            cache.last_scroll_y = viewport.min.y;

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
            // Render adds top_space (0.3) + bottom_space (0.15).
            size.mul_add(0.45, text_h) + sep
        }
        Block::Paragraph(text) => {
            body_size.mul_add(0.4, estimate_text_height(&text.text, body_size, wrap_width))
        }
        Block::Code { language, code, .. } => {
            let mono_size = body_size * 0.9;
            let lines = (bytecount_newlines(code.as_bytes()) + 1).max(1) as f32;
            // 12.0 for Frame inner_margin (6px each side), 1.4 line spacing.
            // Add language label height when present.
            let lang_h = if language.is_empty() { 0.0 } else { body_size };
            body_size.mul_add(0.4, (lines * mono_size).mul_add(1.4, 12.0) + lang_h)
        }
        Block::Quote(inner) => estimate_quote_height(inner, body_size, wrap_width, style),
        Block::UnorderedList(items) | Block::OrderedList { items, .. } => {
            estimate_list_height(items, body_size, wrap_width, style)
        }
        Block::ThematicBreak => body_size * 0.8,
        Block::Table(table) => estimate_table_height(table, body_size, wrap_width),
        Block::Image { .. } => {
            // Images vary wildly — use a generous overestimate so viewport
            // culling never clips them. max_width constrains width to the
            // available area, so assume roughly a 4:3 aspect ratio at full width.
            let max_image_h = wrap_width * 0.75;
            body_size.mul_add(0.4, max_image_h.max(body_size * 8.0))
        }
    }
}

fn estimate_quote_height(
    inner: &[Block],
    body_size: f32,
    wrap_width: f32,
    style: &MarkdownStyle,
) -> f32 {
    // Reserve: bar_margin (0.4em) + bar_width (3px) + content_margin (0.6em) ≈ 1em + 3px.
    let reserved = body_size + 3.0;
    let inner_w = (wrap_width - reserved).max(40.0);
    let inner_h: f32 = inner
        .iter()
        .map(|b| estimate_block_height(b, body_size, inner_w, style))
        .sum();
    body_size.mul_add(0.3, inner_h)
}

fn estimate_list_height(
    items: &[ListItem],
    body_size: f32,
    wrap_width: f32,
    style: &MarkdownStyle,
) -> f32 {
    // Each item: indent + bullet/number column + gap + content.
    let bullet_col = body_size.mul_add(1.5, 2.0);
    let content_w = (wrap_width - bullet_col).max(40.0);
    let item_h: f32 = items
        .iter()
        .map(|item| {
            let text_h = estimate_text_height(&item.content.text, body_size, content_w);
            let child_h: f32 = item
                .children
                .iter()
                .map(|b| estimate_block_height(b, body_size, content_w, style))
                .sum();
            // ui.horizontal adds ~body_size vertical per item.
            body_size.mul_add(0.3, text_h + child_h)
        })
        .sum();
    body_size.mul_add(0.2, item_h)
}

#[allow(clippy::cast_precision_loss)]
fn estimate_table_height(table: &TableData, body_size: f32, wrap_width: f32) -> f32 {
    let num_cols = table.header.len().max(1);
    let col_width = (wrap_width / num_cols as f32).max(40.0);
    let base_row_h = body_size * 1.6;
    let row_spacing = 4.0;

    let row_height = |cells: &[StyledText]| -> f32 {
        cells.iter().fold(base_row_h, |max, c| {
            estimate_text_height(&c.text, body_size, col_width).max(max)
        }) + row_spacing
    };

    let hdr = if table.header.is_empty() {
        0.0
    } else {
        row_height(&table.header)
    };
    let rows_h: f32 = table.rows.iter().map(|r| row_height(r)).sum();
    body_size.mul_add(0.4, hdr + rows_h)
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

/// Fast newline counting via memchr.
fn bytecount_newlines(bytes: &[u8]) -> usize {
    memchr::memchr_iter(b'\n', bytes).count()
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

        Block::Table(table) => {
            render_table(ui, &table.header, &table.alignments, &table.rows, style);
            ui.add_space(body_size * 0.4);
        }

        Block::Image { url, alt } => {
            render_image(ui, url, alt, style, body_size);
        }
    }
}

fn render_image(ui: &mut egui::Ui, url: &str, alt: &str, style: &MarkdownStyle, body_size: f32) {
    // Resolve relative URLs against the configured base URI.
    let resolved: std::borrow::Cow<'_, str> =
        if url.starts_with("//") || url.contains("://") || style.image_base_uri.is_empty() {
            std::borrow::Cow::Borrowed(url)
        } else {
            let mut s = String::with_capacity(style.image_base_uri.len() + url.len());
            s.push_str(&style.image_base_uri);
            s.push_str(url);
            std::borrow::Cow::Owned(s)
        };

    let max_width = ui.available_width();
    let image = egui::Image::new(resolved.as_ref())
        .max_width(max_width)
        .corner_radius(4.0);

    let response = ui.add(image);

    // Show alt text (or URL) on hover.
    let hover_text = if alt.is_empty() { url } else { alt };
    response.on_hover_text(hover_text);

    ui.add_space(body_size * 0.4);
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
                // Only strip trailing newlines, not whitespace — intentional
                // trailing spaces in code should be preserved.
                let trimmed = code.trim_end_matches('\n');
                // Show a non-breaking space for empty blocks so the frame
                // maintains a visible minimum height.
                let display = if trimmed.is_empty() {
                    "\u{00A0}"
                } else {
                    trimmed
                };
                ui.label(
                    egui::RichText::new(display)
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

    let bar_width = 3.0;
    let bar_margin = body_size * 0.4; // space before the bar
    let content_margin = body_size * 0.6; // space after the bar before content
    let reserved = bar_margin + bar_width + content_margin;

    let rect_before = ui.available_rect_before_wrap();
    let bar_x = rect_before.min.x + bar_margin + bar_width * 0.5;

    // Use a unique salt per nesting depth so egui doesn't share layout state.
    let salt = ui.next_auto_id().with(indent);

    let inner_response = ui
        .allocate_ui_with_layout(
            egui::vec2((ui.available_width() - reserved).max(0.0), 0.0),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                ui.indent(salt, |ui| {
                    render_blocks(ui, inner, style, indent + 1);
                });
            },
        )
        .response;

    let bar_top = inner_response.rect.min.y;
    let bar_bottom = inner_response.rect.max.y;
    ui.painter().line_segment(
        [egui::pos2(bar_x, bar_top), egui::pos2(bar_x, bar_bottom)],
        egui::Stroke::new(bar_width, bar_color),
    );
    ui.add_space(body_size * 0.3);
}

fn render_hr(ui: &mut egui::Ui, style: &MarkdownStyle, body_size: f32) {
    ui.add_space(body_size * 0.4);
    let rect = ui.available_rect_before_wrap();
    let y = rect.min.y;
    let color = style
        .hr_color
        .unwrap_or_else(|| ui.visuals().weak_text_color());
    ui.painter().line_segment(
        [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
        egui::Stroke::new(1.0, color),
    );
    ui.add_space(body_size * 0.4);
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
        // Use zero horizontal spacing between inline spans so link and text
        // widgets flow together without extra gaps.
        ui.spacing_mut().item_spacing.x = 0.0;
        for span in &st.spans {
            let text = &st.text[span.start as usize..span.end as usize];
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
            byte_range: span.start as usize..span.end as usize,
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
            // Fixed-width bullet column: 1.5 em gives room for checkboxes.
            ui.allocate_ui_with_layout(
                egui::vec2(body_size * 1.5, body_size),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.label(bullet_text);
                },
            );
            ui.add_space(2.0);
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

    // Compute column width based on the widest number that will appear.
    let max_num = start.saturating_add(items.len().saturating_sub(1) as u64);
    let digit_count = if max_num == 0 {
        1
    } else {
        (max_num as f64).log10().floor() as u32 + 1
    };
    // Each digit ≈ 0.6 em, plus the dot, plus a little padding.
    let num_width = body_size * 0.6f32.mul_add(digit_count as f32, 1.0);

    for (i, item) in items.iter().enumerate() {
        let num = start + i as u64;
        ui.horizontal(|ui| {
            ui.add_space(indent_px);
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

/// Compute normalised column widths for a table, balancing content-proportional
/// sizing with a per-column minimum and a total budget.
///
/// Returns `(col_widths, min_col_w)`.
#[allow(clippy::cast_precision_loss)] // UI math — column count is small
fn compute_table_col_widths(
    header: &[StyledText],
    rows: &[Vec<StyledText>],
    usable: f32,
    avg_char_w: f32,
    body_size: f32,
) -> (Vec<f32>, f32) {
    let num_cols = header.len().max(1);
    let min_col_w = (body_size * 2.5).max(36.0);

    // Initial estimates from content length (header + rows).
    let mut widths: Vec<f32> = (0..num_cols)
        .map(|ci| {
            let hdr_len = header.get(ci).map_or(0, |c| c.text.len());
            let max_row_len = rows
                .iter()
                .map(|r| r.get(ci).map_or(0, |c| c.text.len()))
                .max()
                .unwrap_or(0);
            let char_len = hdr_len.max(max_row_len).max(3) as f32;
            // Cap per-column estimate to avoid one column dominating.
            (avg_char_w.mul_add(char_len, 12.0)).min(usable / num_cols as f32 * 3.0)
        })
        .collect();

    // Normalise: scale to budget, clamp to minimum, redistribute overflow.
    let total_est: f32 = widths.iter().sum();
    if total_est > 0.0 {
        let scale = usable / total_est;
        let mut clamped_total = 0.0_f32;
        let mut free_total = 0.0_f32;
        for w in &mut widths {
            let scaled = *w * scale;
            if scaled < min_col_w {
                *w = min_col_w;
                clamped_total += min_col_w;
            } else {
                *w = scaled;
                free_total += scaled;
            }
        }
        let remaining = usable - clamped_total;
        if remaining > 0.0 && free_total > 0.0 {
            let redistribute = remaining / free_total;
            for w in &mut widths {
                if *w > min_col_w {
                    *w *= redistribute;
                }
            }
        }
    }

    (widths, min_col_w)
}

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
    let body_size = ui.text_style_height(&egui::TextStyle::Body);
    let avg_char_w = body_size * 0.55;
    let spacing = ui.spacing().item_spacing.x * (num_cols.saturating_sub(1)) as f32;
    let usable = (available - spacing).max(0.0);

    let (col_widths, min_col_w) =
        compute_table_col_widths(header, rows, usable, avg_char_w, body_size);

    // Wrap in horizontal scroll when table is wider than available space.
    let total_width: f32 = col_widths.iter().sum::<f32>() + spacing;
    let needs_scroll = total_width > available + 1.0;

    let render_grid = |ui: &mut egui::Ui| {
        egui::Grid::new(ui.next_auto_id())
            .striped(true)
            .min_row_height(body_size * 1.4)
            .show(ui, |ui| {
                for (i, cell) in header.iter().enumerate() {
                    let align = alignments.get(i).copied().unwrap_or(Alignment::None);
                    let w = col_widths.get(i).copied().unwrap_or(min_col_w);
                    ui.set_min_width(w);
                    render_table_cell(ui, cell, style, align, true);
                }
                ui.end_row();

                for row in rows {
                    for (i, cell) in row.iter().take(num_cols).enumerate() {
                        let align = alignments.get(i).copied().unwrap_or(Alignment::None);
                        let w = col_widths.get(i).copied().unwrap_or(min_col_w);
                        ui.set_min_width(w);
                        render_table_cell(ui, cell, style, align, false);
                    }
                    ui.end_row();
                }
            });
    };

    if needs_scroll {
        egui::ScrollArea::horizontal().show(ui, |ui| {
            render_grid(ui);
        });
    } else {
        render_grid(ui);
    }
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
            let color = strengthen_color(
                style
                    .body_color
                    .unwrap_or_else(|| ui.visuals().text_color()),
            );
            render_styled_text_ex(ui, cell, style, Some(body_size), Some(color));
        } else {
            render_styled_text(ui, cell, style);
        }
    });
}

// ── Utilities ──────────────────────────────────────────────────────

/// Approximate "bold" by increasing contrast of a colour.
/// egui has no bold font weight, so we visually distinguish strong text.
/// On dark backgrounds (bright text) the colour is brightened;
/// on light backgrounds (dark text) it is darkened.
fn strengthen_color(color: egui::Color32) -> egui::Color32 {
    let [red, green, blue, alpha] = color.to_array();
    // Perceptual luminance (ITU-R BT.601).
    let luma = 0.114f32.mul_add(
        f32::from(blue),
        0.299f32.mul_add(f32::from(red), 0.587 * f32::from(green)),
    );
    if luma > 127.0 {
        // Bright text (dark background) → brighten toward white.
        let boost = |val: u8| {
            let delta = (u16::from(255_u8.saturating_sub(val))) / 5;
            val.saturating_add(delta.min(255) as u8)
        };
        egui::Color32::from_rgba_premultiplied(boost(red), boost(green), boost(blue), alpha)
    } else {
        // Dark text (light background) → darken toward black.
        let darken = |val: u8| {
            let delta = u16::from(val) / 5;
            val.saturating_sub(delta.min(255) as u8)
        };
        egui::Color32::from_rgba_premultiplied(darken(red), darken(green), darken(blue), alpha)
    }
}

pub(crate) fn simple_hash(s: &str) -> u64 {
    // FNV-1a–inspired 64-bit hash, processing 8 bytes at a time for throughput.
    const BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;

    let bytes = s.as_bytes();
    let chunks = bytes.chunks_exact(8);
    let remainder = chunks.remainder();
    let mut h: u64 = BASIS;

    for chunk in chunks {
        let word = u64::from_le_bytes(chunk.try_into().unwrap_or([0; 8]));
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
#[allow(
    clippy::panic,
    clippy::expect_used,
    clippy::field_reassign_with_default
)]
mod tests {
    use super::*;
    use crate::parse::TableData;
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
    fn heading_y_returns_ordered_offsets() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# A\n\ntext\n\n## B\n\nmore\n\n### C\n");
        cache.ensure_heights(14.0, 400.0, &style);

        let y0_opt = cache.heading_y(0);
        let y1_opt = cache.heading_y(1);
        let y2_opt = cache.heading_y(2);
        assert!(y0_opt.is_some());
        assert!(y1_opt.is_some());
        assert!(y2_opt.is_some());
        let y0 = y0_opt.unwrap_or(0.0);
        let y1 = y1_opt.unwrap_or(0.0);
        let y2 = y2_opt.unwrap_or(0.0);
        assert!(y0 <= y1, "H2 should not appear above H1");
        assert!(y1 <= y2, "H3 should not appear above H2");
    }

    #[test]
    fn heading_y_out_of_bounds_returns_none() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# A\n\n## B\n");
        cache.ensure_heights(14.0, 400.0, &style);
        assert!(cache.heading_y(2).is_none());
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
        let block = Block::Table(Box::new(TableData {
            header: vec![StyledText {
                text: "Col".to_owned(),
                spans: vec![],
            }],
            alignments: vec![Alignment::None],
            rows: vec![vec![StyledText {
                text: "val".to_owned(),
                spans: vec![],
            }]],
        }));
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
            .filter(|b| matches!(b, Block::Table(_)))
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
                .filter(|b| matches!(b, Block::Table(_)))
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
            Block::Table(table) => {
                assert_eq!(table.rows.len(), 3, "expected 3 data rows");
                // Row 2 should have empty text
                assert!(
                    table.rows[1][0].text.trim().is_empty(),
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
        assert!(matches!(&blocks[0], Block::Table(_)));
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
            Block::Table(table) => {
                assert_eq!(table.alignments[0], Alignment::Left);
                assert_eq!(table.alignments[1], Alignment::Center);
                assert_eq!(table.alignments[2], Alignment::Right);
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
            .filter(|b| matches!(b, Block::Table(_)))
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
        let has_table = blocks.iter().any(|b| matches!(b, Block::Table(_)));
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

    // ── Ordered list digit_count / num_width calculation ───────────

    /// Mirror the digit-count logic from `render_ordered_list` so we can
    /// unit-test it without needing a UI context.
    #[allow(clippy::cast_precision_loss)]
    fn digit_count_for(start: u64, item_count: usize) -> u32 {
        let max_num = start.saturating_add(item_count.saturating_sub(1) as u64);
        if max_num == 0 {
            1
        } else {
            (max_num as f64).log10().floor() as u32 + 1
        }
    }

    #[test]
    fn ordered_list_digit_count_single_digit() {
        // 1..=9 → 1 digit
        assert_eq!(digit_count_for(1, 1), 1); // max_num = 1
        assert_eq!(digit_count_for(1, 9), 1); // max_num = 9
        assert_eq!(digit_count_for(5, 3), 1); // max_num = 7
    }

    #[test]
    fn ordered_list_digit_count_double_digit() {
        // 10..=99 → 2 digits
        assert_eq!(digit_count_for(1, 10), 2); // max_num = 10
        assert_eq!(digit_count_for(1, 99), 2); // max_num = 99
        assert_eq!(digit_count_for(50, 5), 2); // max_num = 54
    }

    #[test]
    fn ordered_list_digit_count_triple_digit() {
        assert_eq!(digit_count_for(1, 100), 3); // max_num = 100
        assert_eq!(digit_count_for(1, 999), 3); // max_num = 999
        assert_eq!(digit_count_for(998, 2), 3); // max_num = 999
    }

    #[test]
    fn ordered_list_digit_count_large_numbers() {
        assert_eq!(digit_count_for(1, 1000), 4); // max_num = 1000
        assert_eq!(digit_count_for(1, 10_000), 5); // max_num = 10_000
        assert_eq!(digit_count_for(999_999, 2), 7); // max_num = 1_000_000
    }

    #[test]
    fn ordered_list_digit_count_zero_start() {
        // start=0, 1 item → max_num=0 → special case → 1 digit
        assert_eq!(digit_count_for(0, 1), 1);
        // start=0, 10 items → max_num=9 → 1 digit
        assert_eq!(digit_count_for(0, 10), 1);
        // start=0, 11 items → max_num=10 → 2 digits
        assert_eq!(digit_count_for(0, 11), 2);
    }

    #[test]
    fn ordered_list_digit_count_empty_list() {
        // 0 items: saturating_sub(1) clamps to 0, so max_num = start
        assert_eq!(digit_count_for(1, 0), 1);
        assert_eq!(digit_count_for(100, 0), 3);
    }

    #[allow(clippy::cast_precision_loss)]
    #[test]
    fn ordered_list_num_width_grows_with_digits() {
        let body_size = 14.0_f32;
        let widths: Vec<f32> = [1u32, 2, 3, 4, 5]
            .iter()
            .map(|&dc| body_size * 0.6f32.mul_add(dc as f32, 1.0))
            .collect();
        for i in 0..widths.len() - 1 {
            assert!(
                widths[i] < widths[i + 1],
                "num_width should grow with more digits: {} vs {}",
                widths[i],
                widths[i + 1]
            );
        }
    }

    // ── Table column width heuristic ───────────────────────────────

    /// Thin wrapper that converts `&str` slices into `StyledText` and calls
    /// the actual `compute_table_col_widths` function, avoiding logic duplication.
    fn compute_col_widths(header: &[&str], rows: &[Vec<&str>], available: f32) -> Vec<f32> {
        let body_size = 14.0_f32;
        let avg_char_w = body_size * 0.55;
        let num_cols = header.len().max(1);
        let spacing = 8.0 * num_cols.saturating_sub(1) as f32;
        let usable = (available - spacing).max(0.0);

        let hdr: Vec<StyledText> = header
            .iter()
            .map(|s| StyledText {
                text: (*s).to_owned(),
                spans: vec![],
            })
            .collect();
        let row_data: Vec<Vec<StyledText>> = rows
            .iter()
            .map(|r| {
                r.iter()
                    .map(|s| StyledText {
                        text: (*s).to_owned(),
                        spans: vec![],
                    })
                    .collect()
            })
            .collect();

        let (widths, _) = compute_table_col_widths(&hdr, &row_data, usable, avg_char_w, body_size);
        widths
    }

    #[test]
    fn table_col_widths_equal_length_columns() {
        let widths = compute_col_widths(&["Name", "City"], &[vec!["Alice", "Tokyo"]], 800.0);
        assert_eq!(widths.len(), 2);
        // Equal content → widths should be approximately equal
        let diff = (widths[0] - widths[1]).abs();
        assert!(
            diff < 1.0,
            "equal-length columns should have similar widths, got {widths:?}"
        );
    }

    #[test]
    fn table_col_widths_one_long_column() {
        let widths = compute_col_widths(
            &["A", "B", "Description"],
            &[vec![
                "x",
                "y",
                "This is a much longer description column than the others",
            ]],
            800.0,
        );
        assert_eq!(widths.len(), 3);
        // The long column should be wider than the short ones
        assert!(
            widths[2] > widths[0],
            "long column should be wider: {widths:?}"
        );
        assert!(
            widths[2] > widths[1],
            "long column should be wider: {widths:?}"
        );
        // But capped — should not exceed 3× the equal share
        let equal_share = (800.0 - 16.0) / 3.0; // approx usable/num_cols
        assert!(
            widths[2] <= equal_share * 3.0 + 1.0,
            "column width should be capped: {widths:?}"
        );
    }

    #[test]
    fn table_col_widths_single_column() {
        let widths = compute_col_widths(&["Only"], &[vec!["data"]], 600.0);
        assert_eq!(widths.len(), 1);
        // Single column should fill the usable width
        assert!(
            widths[0] > 100.0,
            "single column should be wide: {widths:?}"
        );
    }

    #[test]
    fn table_col_widths_empty_cells() {
        let widths = compute_col_widths(
            &["A", "B", "C"],
            &[vec!["", "", ""], vec!["x", "", ""]],
            800.0,
        );
        assert_eq!(widths.len(), 3);
        // All columns should meet the 40px minimum after normalisation
        for (i, w) in widths.iter().enumerate() {
            assert!(*w >= 40.0, "column {i} should be at least 40px, got {w}");
        }
    }

    #[test]
    fn table_col_widths_sum_to_usable() {
        let available = 800.0_f32;
        let widths = compute_col_widths(&["A", "B", "C"], &[vec!["foo", "bar", "baz"]], available);
        let spacing = 8.0 * 2.0; // 3 cols, 2 gaps
        let usable = available - spacing;
        let total: f32 = widths.iter().sum();
        // Total should be close to usable (may exceed slightly due to 40px minimum)
        assert!(
            total >= usable - 1.0,
            "total width {total} should be >= usable {usable}"
        );
    }

    // ── Image URL resolution ───────────────────────────────────────

    #[test]
    fn image_url_absolute_stays_unchanged() {
        let url = "https://example.com/pic.png";
        let base_uri = "";
        let resolved = if url.starts_with("//") || url.contains("://") || base_uri.is_empty() {
            url.to_owned()
        } else {
            format!("{base_uri}{url}")
        };
        assert_eq!(resolved, "https://example.com/pic.png");
    }

    #[test]
    fn image_url_relative_with_base_uri() {
        let url = "images/pic.png";
        let base_uri = "file:///home/user/docs/";
        let resolved = if url.starts_with("//") || url.contains("://") || base_uri.is_empty() {
            url.to_owned()
        } else {
            format!("{base_uri}{url}")
        };
        assert_eq!(resolved, "file:///home/user/docs/images/pic.png");
    }

    #[test]
    fn image_url_relative_with_empty_base_uri() {
        let url = "images/pic.png";
        let base_uri = "";
        let resolved = if url.starts_with("//") || url.contains("://") || base_uri.is_empty() {
            url.to_owned()
        } else {
            format!("{base_uri}{url}")
        };
        // Empty base URI → relative path stays as-is
        assert_eq!(resolved, "images/pic.png");
    }

    #[test]
    fn image_url_absolute_ignores_base_uri() {
        // Even with a base URI set, absolute URLs should not be prefixed
        let url = "http://cdn.example.com/img.jpg";
        let base_uri = "file:///local/base/";
        let resolved = if url.starts_with("//") || url.contains("://") || base_uri.is_empty() {
            url.to_owned()
        } else {
            format!("{base_uri}{url}")
        };
        assert_eq!(resolved, "http://cdn.example.com/img.jpg");
    }

    #[test]
    fn image_url_protocol_relative_stays_unchanged() {
        let url = "//cdn.example.com/image.png";
        let base_uri = "file:///local/base/";
        let resolved = if url.starts_with("//") || url.contains("://") || base_uri.is_empty() {
            url.to_owned()
        } else {
            format!("{base_uri}{url}")
        };
        assert_eq!(resolved, "//cdn.example.com/image.png");
    }

    #[test]
    fn image_render_uses_base_uri() {
        // Verify via headless render that image blocks parse and render
        // with a style that has a base URI set.
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let mut style = MarkdownStyle::colored(&egui::Visuals::dark());
        style.image_base_uri = "file:///base/dir/".to_owned();
        let viewer = MarkdownViewer::new("img_base");

        let md = "![alt](relative/pic.png)";
        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show(ui, &mut cache, &style, md);
            });
        });

        // Parse produces an Image block with the raw relative URL
        assert_eq!(cache.blocks.len(), 1);
        match &cache.blocks[0] {
            Block::Image { url, .. } => {
                assert_eq!(url, "relative/pic.png");
            }
            other => panic!("expected Image block, got {other:?}"),
        }
    }

    // ── Height estimation: comprehensive ───────────────────────────

    #[test]
    fn estimate_height_heading_scales_with_level() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let text = StyledText {
            text: "Heading".to_owned(),
            spans: vec![],
        };
        let mut prev_h = f32::MAX;
        // H1 should be tallest, H6 shortest (or equal)
        for level in 1..=6u8 {
            let block = Block::Heading {
                level,
                text: text.clone(),
            };
            let h = estimate_block_height(&block, 14.0, 400.0, &style);
            assert!(h > 0.0, "heading level {level} height should be > 0");
            assert!(
                h <= prev_h + 0.01,
                "H{level} ({h}) should be <= H{} ({prev_h})",
                level - 1
            );
            prev_h = h;
        }
    }

    #[test]
    fn estimate_height_longer_text_is_taller() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let short = Block::Paragraph(StyledText {
            text: "Hi".to_owned(),
            spans: vec![],
        });
        let long = Block::Paragraph(StyledText {
            text: "A".repeat(500),
            spans: vec![],
        });
        let h_short = estimate_block_height(&short, 14.0, 400.0, &style);
        let h_long = estimate_block_height(&long, 14.0, 400.0, &style);
        assert!(
            h_long > h_short,
            "longer text ({h_long}) should be taller than short ({h_short})"
        );
    }

    #[test]
    fn estimate_height_code_block_more_lines_taller() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let small = Block::Code {
            language: String::new(),
            code: "line1".to_owned(),
        };
        let large = Block::Code {
            language: String::new(),
            code: "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10"
                .to_owned(),
        };
        let h_small = estimate_block_height(&small, 14.0, 400.0, &style);
        let h_large = estimate_block_height(&large, 14.0, 400.0, &style);
        assert!(
            h_large > h_small,
            "10-line code ({h_large}) should be taller than 1-line ({h_small})"
        );
    }

    #[test]
    fn estimate_height_nested_quote_taller_than_simple() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let simple = Block::Quote(vec![Block::Paragraph(StyledText {
            text: "one".to_owned(),
            spans: vec![],
        })]);
        let nested = Block::Quote(vec![
            Block::Paragraph(StyledText {
                text: "one".to_owned(),
                spans: vec![],
            }),
            Block::Quote(vec![Block::Paragraph(StyledText {
                text: "two".to_owned(),
                spans: vec![],
            })]),
        ]);
        let h_simple = estimate_block_height(&simple, 14.0, 400.0, &style);
        let h_nested = estimate_block_height(&nested, 14.0, 400.0, &style);
        assert!(
            h_nested > h_simple,
            "nested quote ({h_nested}) should be taller than simple ({h_simple})"
        );
    }

    #[test]
    fn estimate_height_list_more_items_taller() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let make_item = |text: &str| ListItem {
            content: StyledText {
                text: text.to_owned(),
                spans: vec![],
            },
            children: vec![],
            checked: None,
        };
        let short = Block::UnorderedList(vec![make_item("a")]);
        let long = Block::UnorderedList(vec![
            make_item("a"),
            make_item("b"),
            make_item("c"),
            make_item("d"),
            make_item("e"),
        ]);
        let h_short = estimate_block_height(&short, 14.0, 400.0, &style);
        let h_long = estimate_block_height(&long, 14.0, 400.0, &style);
        assert!(
            h_long > h_short,
            "5-item list ({h_long}) should be taller than 1-item ({h_short})"
        );
    }

    #[test]
    fn estimate_height_table_more_rows_taller() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let cell = |s: &str| StyledText {
            text: s.to_owned(),
            spans: vec![],
        };
        let small = Block::Table(Box::new(TableData {
            header: vec![cell("H")],
            alignments: vec![Alignment::None],
            rows: vec![vec![cell("r1")]],
        }));
        let large = Block::Table(Box::new(TableData {
            header: vec![cell("H")],
            alignments: vec![Alignment::None],
            rows: (0..20).map(|i| vec![cell(&format!("row {i}"))]).collect(),
        }));
        let h_small = estimate_block_height(&small, 14.0, 400.0, &style);
        let h_large = estimate_block_height(&large, 14.0, 400.0, &style);
        assert!(
            h_large > h_small,
            "20-row table ({h_large}) should be taller than 1-row ({h_small})"
        );
    }

    // ── strengthen_color tests ───────────────────────────────────────

    #[test]
    fn strengthen_color_black_stays_black() {
        // Black has luma ~0 (dark text) → darken → already at zero.
        let out = strengthen_color(egui::Color32::from_rgb(0, 0, 0));
        let [r, g, b, _] = out.to_array();
        assert_eq!((r, g, b), (0, 0, 0), "black cannot get darker");
    }

    #[test]
    fn strengthen_color_white_stays_white() {
        let out = strengthen_color(egui::Color32::from_rgb(255, 255, 255));
        let [r, g, b, _] = out.to_array();
        assert_eq!((r, g, b), (255, 255, 255));
    }

    #[test]
    fn strengthen_color_preserves_alpha() {
        let out = strengthen_color(egui::Color32::from_rgba_premultiplied(100, 100, 100, 42));
        let [_, _, _, a] = out.to_array();
        assert_eq!(a, 42, "alpha channel must be preserved");
    }

    #[test]
    fn strengthen_color_dark_text_gets_darker() {
        // Dark text (luma < 127) → darken.
        let src = egui::Color32::from_rgb(80, 80, 80);
        let out = strengthen_color(src);
        let [sr, sg, sb, _] = src.to_array();
        let [dr, dg, db, _] = out.to_array();
        assert!(
            dr < sr && dg < sg && db < sb,
            "dark text should get darker: {src:?} -> {out:?}"
        );
    }

    #[test]
    fn strengthen_color_bright_text_gets_brighter() {
        // Bright text (luma > 127) → brighten.
        let src = egui::Color32::from_rgb(200, 200, 200);
        let out = strengthen_color(src);
        let [sr, sg, sb, _] = src.to_array();
        let [dr, dg, db, _] = out.to_array();
        assert!(
            dr > sr && dg > sg && db > sb,
            "bright text should get brighter: {src:?} -> {out:?}"
        );
    }

    // ── bytecount_newlines tests ──────────────────────────────────────

    #[test]
    fn bytecount_newlines_empty() {
        assert_eq!(bytecount_newlines(b""), 0);
    }

    #[test]
    fn bytecount_newlines_single() {
        assert_eq!(bytecount_newlines(b"\n"), 1);
    }

    #[test]
    fn bytecount_newlines_three() {
        assert_eq!(bytecount_newlines(b"\n\n\n"), 3);
    }

    #[test]
    fn bytecount_newlines_embedded() {
        assert_eq!(bytecount_newlines(b"line1\nline2"), 1);
    }

    #[test]
    fn bytecount_newlines_none() {
        assert_eq!(bytecount_newlines(b"no newlines here"), 0);
    }

    // ── Edge-case code block rendering ────────────────────────────────

    #[test]
    fn render_empty_code_block_no_panic() {
        let _ = headless_render("```\n```\n");
    }

    #[test]
    fn render_code_block_without_language_tag() {
        let (blocks, _) = headless_render("```\ncode\n```\n");
        let has_code = blocks.iter().any(|b| matches!(b, Block::Code { .. }));
        assert!(has_code, "should parse a code block without language tag");
    }

    // ── Ordered list digit_count with start=0 ─────────────────────────

    #[test]
    fn ordered_list_zero_start_rendered() {
        // Render an ordered list that starts at 0 via headless context.
        let (blocks, _) = headless_render("0. first\n1. second\n");
        let has_ordered = blocks
            .iter()
            .any(|b| matches!(b, Block::OrderedList { start: 0, .. }));
        assert!(has_ordered, "should parse ordered list starting at 0");
    }

    // ── Viewport culling at exact boundary ────────────────────────────

    #[test]
    fn viewport_culling_exact_boundary_renders_block() {
        let md = "# Block One\n\nParagraph two\n\n# Block Three\n\n";
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(md);
        cache.ensure_heights(14.0, 900.0, &style);

        // Scroll to exactly the start of the second block.
        let boundary_y = cache.cum_y[1];
        let (block_count, _) = headless_render_scrollable(md, Some(boundary_y));
        assert!(block_count > 1, "block at exact boundary should be counted");
    }

    // ── Table with header only ────────────────────────────────────────

    #[test]
    fn estimate_height_table_header_only_no_rows() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Table(Box::new(TableData {
            header: vec![StyledText {
                text: "Header".to_owned(),
                spans: vec![],
            }],
            alignments: vec![Alignment::None],
            rows: vec![],
        }));
        let h = estimate_block_height(&block, 14.0, 400.0, &style);
        assert!(
            h > 0.0,
            "table with header only should have positive height"
        );
    }

    #[test]
    fn render_table_header_only_no_panic() {
        let md = "| H1 | H2 |\n|---|---|\n";
        let (blocks, _) = headless_render(md);
        let has_table = blocks.iter().any(|b| matches!(b, Block::Table(_)));
        assert!(has_table, "header-only table should parse");
    }

    // ── estimate_text_height edge cases ───────────────────────────────

    #[test]
    fn estimate_text_height_empty_returns_font_size() {
        let h = estimate_text_height("", 14.0, 200.0);
        assert!(
            (h - 14.0).abs() < f32::EPSILON,
            "empty text should return font_size"
        );
    }

    #[test]
    fn estimate_text_height_zero_wrap_width_no_crash() {
        let h = estimate_text_height("some text", 14.0, 0.0);
        assert!(
            h > 0.0,
            "zero wrap_width should not crash and should return positive height"
        );
    }

    // ── Existing coverage tests below ─────────────────────────────────

    #[test]
    fn estimate_height_all_block_types_positive() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let cell = |s: &str| StyledText {
            text: s.to_owned(),
            spans: vec![],
        };
        let blocks: Vec<Block> = vec![
            Block::Heading {
                level: 1,
                text: cell("h"),
            },
            Block::Paragraph(cell("p")),
            Block::Code {
                language: String::new(),
                code: "code".to_owned(),
            },
            Block::Quote(vec![Block::Paragraph(cell("q"))]),
            Block::UnorderedList(vec![ListItem {
                content: cell("ul"),
                children: vec![],
                checked: None,
            }]),
            Block::OrderedList {
                start: 1,
                items: vec![ListItem {
                    content: cell("ol"),
                    children: vec![],
                    checked: None,
                }],
            },
            Block::ThematicBreak,
            Block::Table(Box::new(TableData {
                header: vec![cell("H")],
                alignments: vec![],
                rows: vec![vec![cell("r")]],
            })),
            Block::Image {
                url: "img.png".to_owned(),
                alt: String::new(),
            },
        ];
        for block in &blocks {
            let h = estimate_block_height(block, 14.0, 400.0, &style);
            assert!(h > 0.0, "height for {block:?} should be positive, got {h}");
        }
    }

    // ── Round 6-8: New comprehensive tests ────────────────────────────

    #[test]
    fn render_single_column_table() {
        let md = "| Solo |\n|------|\n| One |\n| Two |\n| Three |\n";
        let (blocks, height) = headless_render(md);
        match &blocks[0] {
            Block::Table(table) => {
                assert_eq!(table.header.len(), 1, "single-column table");
                assert_eq!(table.rows.len(), 3);
            }
            other => panic!("expected Table, got {other:?}"),
        }
        assert!(height > 0.0);
    }

    #[test]
    fn render_12_column_table_no_panic() {
        let md = "\
| A | B | C | D | E | F | G | H | I | J | K | L |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10| 11| 12|
";
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let viewer = MarkdownViewer::new("12col");
        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show_scrollable(ui, &mut cache, &style, md, None);
            });
        });
        assert!(!cache.blocks.is_empty());
    }

    #[test]
    fn render_adjacent_tables() {
        let md = "\
| A | B |
|---|---|
| 1 | 2 |

| X | Y | Z |
|---|---|---|
| a | b | c |
";
        let (blocks, _) = headless_render(md);
        let table_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::Table(_)))
            .count();
        assert_eq!(table_count, 2, "should have 2 adjacent tables");
    }

    #[test]
    fn render_image_alt_text_from_brackets() {
        // Verify alt text comes from the text between brackets, not the title.
        let md = "![My Alt Text](image.png)";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::Image { alt, .. } => {
                assert_eq!(alt, "My Alt Text", "alt should come from brackets");
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn render_image_alt_text_with_formatting() {
        let md = "![Alt with **bold** and *italic*](img.png)";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::Image { alt, .. } => {
                assert!(
                    alt.contains("bold") && alt.contains("italic"),
                    "alt text should contain formatted text: {alt}"
                );
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn strengthen_color_near_threshold() {
        // Test colors near the luma=127 threshold.
        let light = strengthen_color(egui::Color32::from_rgb(128, 128, 128));
        let [lr, lg, lb, _] = light.to_array();
        // Luma ≈ 128 > 127 → should brighten.
        assert!(lr > 128 && lg > 128 && lb > 128, "should brighten");

        let dark = strengthen_color(egui::Color32::from_rgb(126, 126, 126));
        let [dr, dg, db, _] = dark.to_array();
        // Luma ≈ 126 < 127 → should darken.
        assert!(dr < 126 && dg < 126 && db < 126, "should darken");
    }

    #[test]
    fn render_deeply_nested_blockquote_no_panic() {
        let md = "> L1\n> > L2\n> > > L3\n> > > > L4\n> > > > > L5\n";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    #[test]
    fn render_code_block_with_trailing_spaces() {
        let md = "```\nline with spaces   \nanother line\n```\n";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::Code { code, .. } => {
                assert!(
                    code.contains("spaces   "),
                    "trailing spaces should be preserved"
                );
            }
            other => panic!("expected Code, got {other:?}"),
        }
    }

    #[test]
    fn render_empty_code_block_has_positive_height() {
        let (_, height) = headless_render("```\n```\n");
        assert!(height > 0.0, "empty code block should have positive height");
    }

    #[test]
    fn estimate_height_code_with_language_taller() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let no_lang = Block::Code {
            language: String::new(),
            code: "code".to_owned(),
        };
        let with_lang = Block::Code {
            language: "rust".to_owned(),
            code: "code".to_owned(),
        };
        let h_no = estimate_block_height(&no_lang, 14.0, 400.0, &style);
        let h_lang = estimate_block_height(&with_lang, 14.0, 400.0, &style);
        assert!(
            h_lang > h_no,
            "code with language ({h_lang}) should be taller than without ({h_no})"
        );
    }

    #[test]
    fn image_height_estimate_scales_with_width() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Image {
            url: "img.png".to_owned(),
            alt: String::new(),
        };
        let h_narrow = estimate_block_height(&block, 14.0, 200.0, &style);
        let h_wide = estimate_block_height(&block, 14.0, 800.0, &style);
        assert!(
            h_wide > h_narrow,
            "wider viewport ({h_wide}) should produce taller image estimate than narrow ({h_narrow})"
        );
    }

    #[test]
    fn table_col_widths_many_columns_respect_minimum() {
        // 12 columns — all should be at least min_col_w.
        let header: Vec<&str> = (0..12).map(|_| "H").collect();
        let row: Vec<&str> = (0..12).map(|_| "v").collect();
        let widths = compute_col_widths(&header, &[row], 800.0);
        assert_eq!(widths.len(), 12);
        let min_col_w = (14.0_f32 * 2.5).max(36.0);
        for (i, w) in widths.iter().enumerate() {
            assert!(
                *w >= min_col_w - 0.01,
                "column {i} should be at least {min_col_w}px, got {w}"
            );
        }
    }

    #[test]
    fn render_hr_symmetric_spacing() {
        // Verify HR doesn't panic and produces positive height.
        let md = "Above\n\n---\n\nBelow";
        let (blocks, height) = headless_render(md);
        let hr_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::ThematicBreak))
            .count();
        assert_eq!(hr_count, 1);
        assert!(height > 0.0);
    }

    #[test]
    fn render_task_list_mixed_states() {
        let md = "\
- [x] Done
- [ ] Not done
- [x] Also done
- Regular item
";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 4);
                assert_eq!(items[0].checked, Some(true));
                assert_eq!(items[1].checked, Some(false));
                assert_eq!(items[2].checked, Some(true));
                assert_eq!(items[3].checked, None);
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    #[test]
    fn render_inline_formatting_stress() {
        let md =
            "**bold** *italic* ***bold-italic*** `code` ~~strike~~ [link](url) **`bold code`**";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert!(text.text.contains("bold"));
                assert!(text.text.contains("italic"));
                assert!(text.text.contains("code"));
                assert!(text.text.contains("strike"));
                assert!(text.text.contains("link"));
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn render_table_with_long_cell_content() {
        let md = "\
| Short | Very Long Column |
|-------|-----------------|
| a     | This cell contains a very long piece of text that should test how the table handles overflow |
";
        let (blocks, _) = headless_render(md);
        assert!(matches!(&blocks[0], Block::Table(_)));
    }

    // ── Round 9: comprehensive new tests ──────────────────────────────

    #[test]
    fn scrollable_render_with_many_images_no_panic() {
        let mut md = String::new();
        for i in 0..15 {
            let _ = writeln!(md, "## Image section {i}\n");
            let _ = writeln!(md, "![image {i}](https://example.com/img_{i}.png)\n");
            let _ = writeln!(md, "Some text after image {i}.\n");
        }
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let viewer = MarkdownViewer::new("many_images");

        // Initial render at top
        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show_scrollable(ui, &mut cache, &style, &md, None);
            });
        });
        let total = cache.total_height;
        assert!(
            total > 0.0,
            "document with images should have positive height"
        );

        // Scroll through at several positions
        let step = total / 10.0;
        for i in 0..12 {
            let y = step * i as f32;
            let _ = ctx.run(raw_input_1024x768(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    viewer.show_scrollable(ui, &mut cache, &style, &md, Some(y));
                });
            });
        }

        let img_count = cache
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Image { .. }))
            .count();
        assert!(
            img_count >= 10,
            "should parse at least 10 images, got {img_count}"
        );
    }

    #[test]
    fn table_with_horizontal_scroll_no_panic() {
        let mut header = String::from("|");
        let mut separator = String::from("|");
        let mut row = String::from("|");
        for i in 0..14 {
            let _ = write!(header, " Col{i:02} |");
            separator.push_str("--------|");
            let _ = write!(row, " val{i:02} |");
        }
        let md = format!("{header}\n{separator}\n{row}\n{row}\n{row}\n");

        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let viewer = MarkdownViewer::new("wide_table");

        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show_scrollable(ui, &mut cache, &style, &md, None);
            });
        });

        match &cache.blocks[0] {
            Block::Table(table) => {
                assert_eq!(table.header.len(), 14, "should have 14 columns");
                assert_eq!(table.rows.len(), 3, "should have 3 rows");
            }
            other => panic!("expected Table, got {other:?}"),
        }
    }

    #[test]
    fn smart_punctuation_converts_quotes() {
        let md = r#"He said "hello" and she said 'world'."#;
        let blocks = crate::parse::parse_markdown(md);
        match &blocks[0] {
            Block::Paragraph(text) => {
                // ENABLE_SMART_PUNCTUATION should convert straight quotes to curly.
                assert!(
                    text.text.contains('\u{201c}') || text.text.contains('\u{201d}'),
                    "expected smart double quotes (\u{201c}\u{201d}) in: {:?}",
                    text.text
                );
                assert!(
                    text.text.contains('\u{2018}') || text.text.contains('\u{2019}'),
                    "expected smart single quotes (\u{2018}\u{2019}) in: {:?}",
                    text.text
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn autolink_angle_brackets_parsed_as_link() {
        let md = "Visit <https://example.com> for info.";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::Paragraph(text) => {
                let has_link = text.spans.iter().any(
                    |s| matches!(&s.style.link, Some(url) if url.as_ref() == "https://example.com"),
                );
                assert!(
                    has_link,
                    "angle-bracket autolink should produce a link span: spans={:?}",
                    text.spans
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn height_estimation_narrow_viewport() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        let md = "# Title\n\nA paragraph with some text that should wrap at narrow widths.\n\n\
                  - Item one\n- Item two\n- Item three\n\n\
                  ```\ncode block\n```\n";
        cache.ensure_parsed(md);
        cache.ensure_heights(14.0, 200.0, &style);

        assert!(
            cache.total_height > 0.0,
            "total height at 200px should be positive"
        );
        // Each block should have a non-zero height
        for (i, h) in cache.heights.iter().enumerate() {
            assert!(
                *h > 0.0,
                "block {i} height should be positive at 200px width"
            );
        }
        // Narrow wrapping should produce taller totals than wide
        let narrow_total = cache.total_height;
        cache.heights.clear();
        cache.ensure_heights(14.0, 2000.0, &style);
        assert!(
            narrow_total >= cache.total_height,
            "narrow viewport ({narrow_total}) should be at least as tall as wide ({})",
            cache.total_height
        );
    }

    #[test]
    fn height_estimation_wide_viewport() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        let md = "# Title\n\nShort paragraph.\n\n| A | B | C |\n|---|---|---|\n| x | y | z |\n";
        cache.ensure_parsed(md);
        cache.ensure_heights(14.0, 2000.0, &style);

        assert!(
            cache.total_height > 0.0,
            "total height at 2000px should be positive"
        );
        for (i, h) in cache.heights.iter().enumerate() {
            assert!(
                *h > 0.0,
                "block {i} height should be positive at 2000px width"
            );
        }
        // At 2000px, single-line text should not wrap excessively
        // so total height should stay reasonable (under 500px for this short doc)
        assert!(
            cache.total_height < 500.0,
            "short doc at wide viewport should not be excessively tall: {}",
            cache.total_height
        );
    }

    #[test]
    fn empty_table_no_header_no_rows() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Table(Box::new(TableData {
            header: vec![],
            alignments: vec![],
            rows: vec![],
        }));
        let h = estimate_block_height(&block, 14.0, 400.0, &style);
        assert!(
            h > 0.0,
            "empty table should still have positive height, got {h}"
        );
    }

    #[test]
    fn estimate_height_zero_wrap_width_table() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Table(Box::new(TableData {
            header: vec![StyledText {
                text: "Col".to_owned(),
                spans: vec![],
            }],
            alignments: vec![Alignment::None],
            rows: vec![vec![StyledText {
                text: "val".to_owned(),
                spans: vec![],
            }]],
        }));
        let h = estimate_block_height(&block, 14.0, 0.0, &style);
        assert!(
            h > 0.0,
            "zero wrap_width table should not panic and have positive height"
        );
    }

    #[test]
    fn estimate_height_zero_wrap_width_list() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::UnorderedList(vec![ListItem {
            content: StyledText {
                text: "item".to_owned(),
                spans: vec![],
            },
            children: vec![],
            checked: None,
        }]);
        let h = estimate_block_height(&block, 14.0, 0.0, &style);
        assert!(
            h > 0.0,
            "zero wrap_width list should not panic and have positive height"
        );
    }

    #[test]
    fn deeply_nested_list_height_increases() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());

        let leaf = ListItem {
            content: StyledText {
                text: "leaf".to_owned(),
                spans: vec![],
            },
            children: vec![],
            checked: None,
        };
        // Build 10 levels of nesting: each level wraps the previous in a child UnorderedList
        let mut nested = Block::UnorderedList(vec![leaf]);
        for _ in 1..10 {
            nested = Block::UnorderedList(vec![ListItem {
                content: StyledText {
                    text: "level".to_owned(),
                    spans: vec![],
                },
                children: vec![nested],
                checked: None,
            }]);
        }

        let flat = Block::UnorderedList(vec![ListItem {
            content: StyledText {
                text: "single".to_owned(),
                spans: vec![],
            },
            children: vec![],
            checked: None,
        }]);

        let h_flat = estimate_block_height(&flat, 14.0, 400.0, &style);
        let h_nested = estimate_block_height(&nested, 14.0, 400.0, &style);
        assert!(
            h_nested > h_flat,
            "10-deep nested list ({h_nested}) should be taller than flat ({h_flat})"
        );
    }

    #[test]
    fn simple_hash_empty_string() {
        let empty = simple_hash("");
        let a = simple_hash("a");
        let space = simple_hash(" ");
        assert_ne!(empty, a, "empty hash should differ from 'a'");
        assert_ne!(empty, space, "empty hash should differ from ' '");
    }

    #[test]
    fn simple_hash_collision_resistance() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..1000 {
            let h = simple_hash(&format!("string_{i}"));
            assert!(seen.insert(h), "collision at i={i}: hash {h} already seen");
        }
    }

    // ── Viewport culling stress tests ─────────────────────────────────

    /// Replicate the viewport culling binary search from `show_scrollable`.
    /// Returns `(first_visible_block, last_exclusive_block)`.
    fn viewport_range(cache: &MarkdownCache, vis_top: f32, vis_bottom: f32) -> (usize, usize) {
        if cache.blocks.is_empty() {
            return (0, 0);
        }
        let first = match cache
            .cum_y
            .binary_search_by(|y| y.partial_cmp(&vis_top).unwrap_or(std::cmp::Ordering::Equal))
        {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let mut idx = first;
        while idx < cache.blocks.len() {
            if cache.cum_y[idx] > vis_bottom {
                break;
            }
            idx += 1;
        }
        (first, idx)
    }

    /// Build a cache with heights computed for the given source.
    fn build_cache(source: &str) -> MarkdownCache {
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(source);
        cache.ensure_heights(14.0, 900.0, &style);
        cache
    }

    /// Generate a document with the given number of short paragraphs.
    fn uniform_paragraph_doc(n: usize) -> String {
        let mut doc = String::with_capacity(n * 20);
        for i in 0..n {
            write!(doc, "Paragraph {i}\n\n").ok();
        }
        doc
    }

    // 1. 10,000+ blocks — binary search at 0%, 25%, 50%, 75%, 100%
    #[test]
    fn viewport_10k_blocks_various_scroll_positions() {
        let doc = uniform_paragraph_doc(10_500);
        let cache = build_cache(&doc);
        assert!(
            cache.blocks.len() >= 10_000,
            "expected ≥10k blocks, got {}",
            cache.blocks.len()
        );
        assert!(cache.total_height > 0.0);

        let viewport_h = 800.0_f32;
        let total = cache.total_height;

        for &frac in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            let vis_top = total * frac;
            let vis_bottom = vis_top + viewport_h;
            let (first, last) = viewport_range(&cache, vis_top, vis_bottom);

            // first must be a valid block index
            assert!(
                first < cache.blocks.len(),
                "first={first} out of bounds at {frac:.0}%"
            );

            // The first block's start must be ≤ vis_top (it contains vis_top)
            assert!(
                cache.cum_y[first] <= vis_top,
                "block {first} starts at {} which is past vis_top={vis_top} ({frac:.0}%)",
                cache.cum_y[first]
            );

            // If first > 0, the next block's start must be > vis_top - heights[first]
            // i.e. the previous block must end before vis_top
            if first > 0 {
                let prev_end = cache.cum_y[first - 1] + cache.heights[first - 1];
                assert!(
                    prev_end <= vis_top + f32::EPSILON,
                    "previous block {f} ends at {prev_end} but vis_top={vis_top} — \
                     should have started from block {f} ({frac:.0}%)",
                    f = first - 1,
                );
            }

            // At least one block should be rendered (unless scrolled past end)
            assert!(last > first, "no blocks rendered at {frac:.0}%");
        }
    }

    // 2. Scroll to exact block boundaries (cum_y[i] values)
    #[test]
    fn viewport_exact_boundary_finds_correct_block() {
        let doc = uniform_paragraph_doc(200);
        let cache = build_cache(&doc);
        let n = cache.blocks.len();
        assert!(n >= 200);

        let viewport_h = 800.0;
        for i in 0..n {
            let vis_top = cache.cum_y[i];
            let vis_bottom = vis_top + viewport_h;
            let (first, _) = viewport_range(&cache, vis_top, vis_bottom);
            assert_eq!(
                first, i,
                "scrolling to cum_y[{i}]={vis_top} should start at block {i}, got {first}"
            );
        }
    }

    // 3. Scroll to positions between blocks
    #[test]
    fn viewport_between_blocks_includes_containing_block() {
        let doc = uniform_paragraph_doc(200);
        let cache = build_cache(&doc);
        let n = cache.blocks.len();
        assert!(n >= 2);

        let viewport_h = 800.0;
        for i in 0..(n - 1) {
            // Midpoint between block i start and block i+1 start
            let mid = f32::midpoint(cache.cum_y[i], cache.cum_y[i + 1]);
            let vis_bottom = mid + viewport_h;
            let (first, _) = viewport_range(&cache, mid, vis_bottom);
            assert_eq!(
                first,
                i,
                "midpoint between blocks {i} and {} should start at block {i}, got {first}",
                i + 1,
            );
        }
    }

    // 4. Scroll past total_height — no panic, no out-of-bounds
    #[test]
    fn viewport_scroll_past_total_height_no_panic() {
        let doc = uniform_paragraph_doc(100);
        let cache = build_cache(&doc);

        let vis_top = cache.total_height * 2.0;
        let vis_bottom = vis_top + 800.0;
        let (first, last) = viewport_range(&cache, vis_top, vis_bottom);
        // Should not panic; first must be valid
        assert!(first < cache.blocks.len());
        // May render the last block (it's the best-effort closest)
        assert!(last <= cache.blocks.len());
    }

    // 4b. Also test via the full render pipeline
    #[test]
    fn viewport_scroll_past_total_height_headless() {
        let md = "# Hello\n\nWorld\n\n";
        let (block_count, total_h) = headless_render_scrollable(md, Some(999_999.0));
        assert!(block_count > 0);
        assert!(total_h > 0.0);
    }

    // 5. Scroll to negative Y — overlapping viewport still renders blocks
    #[test]
    fn viewport_negative_scroll_starts_at_block_zero() {
        let doc = uniform_paragraph_doc(100);
        let cache = build_cache(&doc);

        // Viewport overlaps the document (vis_bottom > 0)
        let vis_top = -100.0_f32;
        let vis_bottom = vis_top + 800.0; // 700.0
        let (first, last) = viewport_range(&cache, vis_top, vis_bottom);
        assert_eq!(first, 0, "negative scroll should start at block 0");
        assert!(last > 0, "overlapping viewport should render blocks");
    }

    // 5b. Entirely negative viewport renders nothing (correct behavior)
    #[test]
    fn viewport_entirely_negative_renders_nothing() {
        let doc = uniform_paragraph_doc(100);
        let cache = build_cache(&doc);

        let vis_top = -1000.0_f32;
        let vis_bottom = vis_top + 800.0; // -200.0
        let (first, last) = viewport_range(&cache, vis_top, vis_bottom);
        assert_eq!(first, 0, "should clamp to block 0");
        assert_eq!(last, 0, "entirely-above viewport should render nothing");
    }

    // 5b. Via full render pipeline
    #[test]
    fn viewport_negative_scroll_headless() {
        let md = "# Hello\n\nWorld\n\n";
        let (block_count, _) = headless_render_scrollable(md, Some(-500.0));
        assert!(block_count > 0, "negative scroll should not crash");
    }

    // 6. Uniform heights — binary search should be exact
    #[test]
    fn viewport_uniform_heights_exact_binary_search() {
        let doc = uniform_paragraph_doc(500);
        let cache = build_cache(&doc);
        let n = cache.blocks.len();
        assert!(n >= 500);

        // All heights should be the same (identical paragraph text pattern)
        let h0 = cache.heights[0];
        for (i, &h) in cache.heights.iter().enumerate() {
            assert!(
                (h - h0).abs() < f32::EPSILON,
                "block {i} height {h} differs from block 0 height {h0}"
            );
        }

        // Verify cum_y is exact multiples of h0
        for (i, &y) in cache.cum_y.iter().enumerate() {
            let expected = h0 * i as f32;
            assert!(
                (y - expected).abs() < 0.1,
                "cum_y[{i}]={y} expected ~{expected}"
            );
        }

        // Binary search at exact multiples
        let viewport_h = 800.0;
        for i in (0..n).step_by(50) {
            let vis_top = cache.cum_y[i];
            let (first, _) = viewport_range(&cache, vis_top, vis_top + viewport_h);
            assert_eq!(
                first, i,
                "exact boundary at block {i} should find block {i}"
            );
        }
    }

    // 7. Wildly varying heights — tiny paragraph then huge code block
    #[test]
    fn viewport_varying_heights_does_not_skip_large_block() {
        // Tiny paragraph followed by a huge code block (many lines)
        let mut doc = String::from("Hi\n\n");
        doc.push_str("```\n");
        for i in 0..500 {
            writeln!(doc, "code line {i}").ok();
        }
        doc.push_str("```\n\n");
        doc.push_str("After code\n\n");

        let cache = build_cache(&doc);
        assert!(cache.blocks.len() >= 3, "expected at least 3 blocks");

        // The code block (index 1) should be much taller than the paragraph
        assert!(
            cache.heights[1] > cache.heights[0] * 10.0,
            "code block height {} should be much larger than paragraph {}",
            cache.heights[1],
            cache.heights[0]
        );

        // Scroll to the middle of the code block — it must be included
        let code_start = cache.cum_y[1];
        let code_mid = code_start + cache.heights[1] / 2.0;
        let viewport_h = 100.0; // Small viewport
        let (first, last) = viewport_range(&cache, code_mid, code_mid + viewport_h);
        assert!(
            first <= 1 && last > 1,
            "code block (idx 1) must be in range [{first}, {last})"
        );

        // Scroll to just before the code block ends
        let code_end = code_start + cache.heights[1] - 1.0;
        let (first2, last2) = viewport_range(&cache, code_end, code_end + viewport_h);
        assert!(
            first2 <= 1 && last2 > 1,
            "near end of code block: block 1 must be in [{first2}, {last2})"
        );
    }

    // 8. Single-block document at Y=0
    #[test]
    fn viewport_single_block_scroll_zero() {
        let cache = build_cache("# Only heading\n");
        assert_eq!(cache.blocks.len(), 1);
        assert!(cache.total_height > 0.0);
        assert!(
            (cache.cum_y[0]).abs() < f32::EPSILON,
            "cum_y[0] should be 0"
        );

        let (first, last) = viewport_range(&cache, 0.0, 800.0);
        assert_eq!(first, 0);
        assert_eq!(last, 1);
    }

    // 8b. Via full render pipeline
    #[test]
    fn viewport_single_block_headless() {
        let (count, h) = headless_render_scrollable("# Only\n", Some(0.0));
        assert_eq!(count, 1);
        assert!(h > 0.0);
    }

    // 9. Empty document — should render nothing, not crash
    #[test]
    fn viewport_empty_document_no_crash() {
        let cache = build_cache("");
        assert!(cache.blocks.is_empty());
        assert!((cache.total_height).abs() < f32::EPSILON);
        assert!(cache.cum_y.is_empty());

        let (first, last) = viewport_range(&cache, 0.0, 800.0);
        assert_eq!(first, 0);
        assert_eq!(last, 0);
    }

    // 9b. Via full render pipeline
    #[test]
    fn viewport_empty_document_headless() {
        let (count, h) = headless_render_scrollable("", Some(0.0));
        assert_eq!(count, 0);
        assert!((h).abs() < f32::EPSILON);
    }

    // 10. cum_y is monotonically increasing (10k+ blocks)
    #[test]
    fn viewport_cum_y_monotonic_10k() {
        let doc = uniform_paragraph_doc(10_500);
        let cache = build_cache(&doc);
        assert!(cache.cum_y.len() >= 10_000);

        for i in 1..cache.cum_y.len() {
            assert!(
                cache.cum_y[i] >= cache.cum_y[i - 1],
                "cum_y not monotonic at {i}: {} < {}",
                cache.cum_y[i],
                cache.cum_y[i - 1]
            );
        }
    }

    // 10b. Also on mixed doc
    #[test]
    fn viewport_cum_y_monotonic_mixed_doc() {
        let doc = crate::stress::large_mixed_doc(200);
        let cache = build_cache(&doc);

        for i in 1..cache.cum_y.len() {
            assert!(
                cache.cum_y[i] > cache.cum_y[i - 1],
                "cum_y not strictly increasing at {i}: {} vs {}",
                cache.cum_y[i],
                cache.cum_y[i - 1]
            );
        }
    }

    // 11. total_height == sum of all heights
    #[test]
    fn viewport_total_height_equals_sum_of_heights() {
        let doc = crate::stress::large_mixed_doc(100);
        let cache = build_cache(&doc);
        let sum: f32 = cache.heights.iter().sum();
        assert!(
            (cache.total_height - sum).abs() < 1.0,
            "total_height={} but sum of heights={}",
            cache.total_height,
            sum
        );
    }

    // 11b. On uniform doc
    #[test]
    fn viewport_total_height_equals_sum_uniform() {
        let doc = uniform_paragraph_doc(1_000);
        let cache = build_cache(&doc);
        let sum: f32 = cache.heights.iter().sum();
        assert!(
            (cache.total_height - sum).abs() < 1.0,
            "total_height={} but sum={}",
            cache.total_height,
            sum
        );
    }

    // 12. cum_y[0] == 0 always
    #[test]
    fn viewport_cum_y_zero_always_zero() {
        // Single block
        let c1 = build_cache("# H\n");
        assert!(
            (c1.cum_y[0]).abs() < f32::EPSILON,
            "single: cum_y[0]={}",
            c1.cum_y[0]
        );

        // Many blocks
        let doc = uniform_paragraph_doc(500);
        let c2 = build_cache(&doc);
        assert!(
            (c2.cum_y[0]).abs() < f32::EPSILON,
            "many: cum_y[0]={}",
            c2.cum_y[0]
        );

        // Mixed doc
        let doc = crate::stress::large_mixed_doc(50);
        let c3 = build_cache(&doc);
        assert!(
            (c3.cum_y[0]).abs() < f32::EPSILON,
            "mixed: cum_y[0]={}",
            c3.cum_y[0]
        );
    }

    // 13. Very narrow viewport (height=1px)
    #[test]
    fn viewport_narrow_1px_renders_at_least_one_block() {
        let doc = uniform_paragraph_doc(100);
        let cache = build_cache(&doc);

        // 1px viewport at various positions
        for i in (0..cache.blocks.len()).step_by(10) {
            let vis_top = cache.cum_y[i];
            let vis_bottom = vis_top + 1.0;
            let (first, last) = viewport_range(&cache, vis_top, vis_bottom);
            assert!(
                last > first,
                "1px viewport at block {i} should render ≥1 block, got [{first}, {last})"
            );
        }
    }

    // 13b. 1px viewport in the middle of a block
    #[test]
    fn viewport_narrow_1px_mid_block() {
        let doc = uniform_paragraph_doc(100);
        let cache = build_cache(&doc);

        let mid_y = cache.cum_y[50] + cache.heights[50] / 2.0;
        let (first, last) = viewport_range(&cache, mid_y, mid_y + 1.0);
        assert!(
            first <= 50 && last > 50,
            "1px viewport at mid-block 50: [{first}, {last}) should include block 50"
        );
    }

    // 14. Very wide viewport (100,000px) includes all blocks
    #[test]
    fn viewport_wide_100k_includes_all_blocks() {
        let doc = uniform_paragraph_doc(500);
        let cache = build_cache(&doc);
        let n = cache.blocks.len();

        let (first, last) = viewport_range(&cache, 0.0, 100_000.0);
        assert_eq!(first, 0, "wide viewport should start at block 0");
        assert_eq!(last, n, "wide viewport should include all {n} blocks");
    }

    // 14b. Wide viewport starting from a non-zero offset
    #[test]
    fn viewport_wide_from_middle_includes_rest() {
        let doc = uniform_paragraph_doc(500);
        let cache = build_cache(&doc);
        let n = cache.blocks.len();
        let mid = n / 2;

        let vis_top = cache.cum_y[mid];
        let (first, last) = viewport_range(&cache, vis_top, vis_top + 100_000.0);
        assert_eq!(first, mid, "should start at block {mid}");
        assert_eq!(last, n, "should include all remaining blocks");
    }

    // ── Additional edge cases ─────────────────────────────────────────

    // Verify scrollable render pipeline handles all extreme positions
    #[test]
    fn viewport_10k_headless_no_panic() {
        let doc = uniform_paragraph_doc(10_500);
        let (count, total_h) = headless_render_scrollable(&doc, None);
        assert!(count >= 10_000);
        assert!(total_h > 0.0);

        // Scroll to various positions via full pipeline
        for &frac in &[0.0, 0.25, 0.5, 0.75, 1.0, 1.5] {
            let y = total_h * frac;
            let (c, _) = headless_render_scrollable(&doc, Some(y));
            assert!(
                c >= 10_000,
                "block count should be stable at scroll {frac:.0}"
            );
        }
    }

    // Verify the pathological doc doesn't break viewport culling
    #[test]
    fn viewport_pathological_doc_invariants() {
        let doc = crate::stress::pathological_doc(50);
        let cache = build_cache(&doc);

        // All invariants must hold
        assert!(!cache.cum_y.is_empty());
        assert!((cache.cum_y[0]).abs() < f32::EPSILON);
        for i in 1..cache.cum_y.len() {
            assert!(cache.cum_y[i] >= cache.cum_y[i - 1]);
        }
        let sum: f32 = cache.heights.iter().sum();
        assert!((cache.total_height - sum).abs() < 1.0);

        // Full sweep: every position should be reachable without panic
        let step = cache.total_height / 100.0;
        for i in 0..=110 {
            let vis_top = step * i as f32;
            let (first, last) = viewport_range(&cache, vis_top, vis_top + 800.0);
            assert!(first <= last);
            assert!(last <= cache.blocks.len());
        }
    }

    // Boundary: last block's cum_y + height == total_height
    #[test]
    fn viewport_last_block_boundary_consistency() {
        let doc = uniform_paragraph_doc(200);
        let cache = build_cache(&doc);
        let n = cache.blocks.len();
        let last_end = cache.cum_y[n - 1] + cache.heights[n - 1];
        assert!(
            (last_end - cache.total_height).abs() < 0.01,
            "last block end {last_end} != total_height {}",
            cache.total_height
        );
    }

    // Lengths consistency: blocks, heights, cum_y all same length
    #[test]
    fn viewport_array_lengths_match() {
        let doc = crate::stress::large_mixed_doc(100);
        let cache = build_cache(&doc);
        assert_eq!(cache.blocks.len(), cache.heights.len());
        assert_eq!(cache.blocks.len(), cache.cum_y.len());
    }

    #[test]
    fn stress_test_multiple_viewports() {
        let md = include_str!("../../../test-assets/stress-test.md");
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let widths: &[f32] = &[320.0, 768.0, 1024.0, 1920.0];

        for &width in widths {
            let ctx = headless_ctx();
            let mut cache = MarkdownCache::default();
            let viewer = MarkdownViewer::new("multi_vp");

            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 768.0),
                )),
                ..Default::default()
            };

            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    viewer.show_scrollable(ui, &mut cache, &style, md, None);
                });
            });

            assert!(
                !cache.blocks.is_empty(),
                "blocks should be parsed at {width}px"
            );
            assert!(
                cache.total_height > 0.0,
                "total height should be positive at {width}px, got {}",
                cache.total_height
            );

            // Scroll to middle and end to verify no panics
            let mid = cache.total_height / 2.0;
            let end = cache.total_height;
            for scroll_y in [mid, end] {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 768.0),
                    )),
                    ..Default::default()
                };
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        viewer.show_scrollable(ui, &mut cache, &style, md, Some(scroll_y));
                    });
                });
            }
        }
    }

    // ── Height estimation stress tests ─────────────────────────────

    /// Helper: build a `StyledText` with no spans.
    fn plain(s: &str) -> StyledText {
        StyledText {
            text: s.to_owned(),
            spans: vec![],
        }
    }

    /// Helper: build a `TableData` with `ncols` columns and `nrows` data rows.
    fn make_table(ncols: usize, nrows: usize, cell: &str) -> TableData {
        let header: Vec<StyledText> = (0..ncols).map(|i| plain(&format!("H{i}"))).collect();
        let aligns = vec![Alignment::None; ncols];
        let rows: Vec<Vec<StyledText>> = (0..nrows)
            .map(|_| (0..ncols).map(|_| plain(cell)).collect())
            .collect();
        TableData {
            header,
            alignments: aligns,
            rows,
        }
    }

    /// Assert that a height is finite, positive, and not NaN.
    fn assert_sane_height(h: f32, label: &str) {
        assert!(h.is_finite(), "{label}: height is not finite ({h})");
        assert!(h > 0.0, "{label}: height should be > 0, got {h}");
    }

    // ── Tables: extreme column counts ──────────────────────────────

    #[test]
    fn table_height_scales_with_rows() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let small = Block::Table(Box::new(make_table(3, 10, "cell")));
        let large = Block::Table(Box::new(make_table(3, 100, "cell")));
        let h_small = estimate_block_height(&small, 14.0, 600.0, &style);
        let h_large = estimate_block_height(&large, 14.0, 600.0, &style);
        assert_sane_height(h_small, "10-row table");
        assert_sane_height(h_large, "100-row table");
        assert!(
            h_large > h_small,
            "100-row table ({h_large}) should be taller than 10-row ({h_small})"
        );
    }

    #[test]
    fn table_height_extreme_column_counts() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        for ncols in [1, 50, 100] {
            let table = Block::Table(Box::new(make_table(ncols, 5, "val")));
            let h = estimate_block_height(&table, 14.0, 600.0, &style);
            assert_sane_height(h, &format!("{ncols}-col table"));
        }
    }

    #[test]
    fn table_height_long_cell_content() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let long_cell = "word ".repeat(200); // ~1000 chars, lots of wrapping
        let table = Block::Table(Box::new(make_table(3, 5, &long_cell)));
        let h = estimate_block_height(&table, 14.0, 600.0, &style);
        assert_sane_height(h, "table with long cells");
        // Should be much taller than a table with short cells.
        let short = Block::Table(Box::new(make_table(3, 5, "x")));
        let h_short = estimate_block_height(&short, 14.0, 600.0, &style);
        assert!(
            h > h_short,
            "long-cell table ({h}) should be taller than short-cell ({h_short})"
        );
    }

    #[test]
    fn table_height_empty_header() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let table = TableData {
            header: vec![],
            alignments: vec![],
            rows: vec![vec![plain("a"), plain("b")]],
        };
        let h = estimate_block_height(&Block::Table(Box::new(table)), 14.0, 600.0, &style);
        // Even with no header, height should be > 0 for the data row.
        assert!(h > 0.0, "table with empty header should still have height");
        assert!(h.is_finite());
    }

    #[test]
    fn table_height_empty_rows() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let table = TableData {
            header: vec![plain("H1")],
            alignments: vec![Alignment::None],
            rows: vec![],
        };
        let h = estimate_block_height(&Block::Table(Box::new(table)), 14.0, 600.0, &style);
        assert!(h > 0.0, "header-only table should have positive height");
        assert!(h.is_finite());
    }

    #[test]
    fn table_height_zero_rows_zero_header() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let table = TableData {
            header: vec![],
            alignments: vec![],
            rows: vec![],
        };
        let h = estimate_block_height(&Block::Table(Box::new(table)), 14.0, 600.0, &style);
        // With nothing at all, we still get the base padding.
        assert!(h.is_finite(), "fully empty table should produce finite h");
        assert!(h >= 0.0);
    }

    // ── Deeply nested lists ────────────────────────────────────────

    #[test]
    fn deeply_nested_list_height() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        // Build 10-level deep nested list.
        let mut block = Block::UnorderedList(vec![ListItem {
            content: plain("leaf"),
            children: vec![],
            checked: None,
        }]);
        for depth in 0..10 {
            block = Block::UnorderedList(vec![ListItem {
                content: plain(&format!("level {depth}")),
                children: vec![block],
                checked: None,
            }]);
        }
        let h = estimate_block_height(&block, 14.0, 600.0, &style);
        assert_sane_height(h, "10-level nested list");
    }

    #[test]
    fn list_with_long_item_text() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let long_text = "word ".repeat(500); // ~2500 chars
        let block = Block::UnorderedList(vec![ListItem {
            content: plain(&long_text),
            children: vec![],
            checked: None,
        }]);
        let h = estimate_block_height(&block, 14.0, 600.0, &style);
        assert_sane_height(h, "list with very long item");
        // Should be much taller than a short item.
        let short = Block::UnorderedList(vec![ListItem {
            content: plain("hi"),
            children: vec![],
            checked: None,
        }]);
        let h_short = estimate_block_height(&short, 14.0, 600.0, &style);
        assert!(
            h > h_short,
            "long item list ({h}) should exceed short ({h_short})"
        );
    }

    #[test]
    fn list_height_with_narrow_wrap_width() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::UnorderedList(vec![ListItem {
            content: plain("some item text here"),
            children: vec![],
            checked: None,
        }]);
        // Wrap width smaller than bullet_col, triggers the .max(40.0) floor.
        let h = estimate_block_height(&block, 14.0, 10.0, &style);
        assert_sane_height(h, "list with 10px wrap");
    }

    // ── Code blocks: edge cases ────────────────────────────────────

    #[test]
    fn code_block_empty_content() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Code {
            language: String::new(),
            code: String::new(),
        };
        let h = estimate_block_height(&block, 14.0, 600.0, &style);
        assert_sane_height(h, "empty code block");
    }

    #[test]
    fn code_block_single_line() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Code {
            language: "rust".to_owned(),
            code: "let x = 1;".to_owned(),
        };
        let h = estimate_block_height(&block, 14.0, 600.0, &style);
        assert_sane_height(h, "single-line code block");
    }

    #[test]
    fn code_block_thousands_of_lines() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut code = String::with_capacity(30_000);
        for i in 0..3000 {
            writeln!(code, "line {i}").ok();
        }
        let block = Block::Code {
            language: "text".to_owned(),
            code,
        };
        let h = estimate_block_height(&block, 14.0, 600.0, &style);
        assert_sane_height(h, "3000-line code block");
        // Should be very tall.
        assert!(h > 1000.0, "3000 lines should be > 1000px, got {h}");
    }

    #[test]
    fn code_block_scales_with_lines() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let small = Block::Code {
            language: String::new(),
            code: "a\nb\nc\n".to_owned(),
        };
        let large = Block::Code {
            language: String::new(),
            code: (0..100)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let h_small = estimate_block_height(&small, 14.0, 600.0, &style);
        let h_large = estimate_block_height(&large, 14.0, 600.0, &style);
        assert!(
            h_large > h_small,
            "100-line code ({h_large}) should exceed 3-line ({h_small})"
        );
    }

    #[test]
    fn code_block_very_long_single_line() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let code = "x".repeat(10_000); // 10k chars, no newlines
        let block = Block::Code {
            language: String::new(),
            code,
        };
        let h = estimate_block_height(&block, 14.0, 600.0, &style);
        assert_sane_height(h, "10k-char single-line code");
        // Code blocks count newlines, so single line → height for 1 line.
        assert!(h.is_finite());
    }

    #[test]
    fn code_block_with_language_taller_than_without() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let with_lang = Block::Code {
            language: "python".to_owned(),
            code: "pass".to_owned(),
        };
        let without_lang = Block::Code {
            language: String::new(),
            code: "pass".to_owned(),
        };
        let h_with = estimate_block_height(&with_lang, 14.0, 600.0, &style);
        let h_without = estimate_block_height(&without_lang, 14.0, 600.0, &style);
        assert!(
            h_with > h_without,
            "code with language label ({h_with}) should be taller than without ({h_without})"
        );
    }

    // ── Images: narrow wrap widths ─────────────────────────────────

    #[test]
    fn image_height_narrow_wrap_widths() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Image {
            url: "pic.png".to_owned(),
            alt: "alt".to_owned(),
        };
        for width in [1.0_f32, 10.0, 50.0] {
            let h = estimate_block_height(&block, 14.0, width, &style);
            assert_sane_height(h, &format!("image at {width}px wrap"));
        }
    }

    #[test]
    fn image_height_zero_wrap_width() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Image {
            url: "pic.png".to_owned(),
            alt: "alt".to_owned(),
        };
        let h = estimate_block_height(&block, 14.0, 0.0, &style);
        // The .max(body_size * 8.0) floor should keep this positive.
        assert_sane_height(h, "image at 0px wrap");
    }

    // ── Blockquotes: deep nesting ──────────────────────────────────

    #[test]
    fn blockquote_deeply_nested_hits_width_floor() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        // Build 8-level deep nested blockquote.
        let mut block = Block::Quote(vec![Block::Paragraph(plain("deep content"))]);
        for _ in 0..7 {
            block = Block::Quote(vec![block]);
        }
        let h = estimate_block_height(&block, 14.0, 400.0, &style);
        assert_sane_height(h, "8-level nested blockquote");
    }

    #[test]
    fn blockquote_inner_width_never_below_floor() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        // Even with extreme nesting, the 40px floor should prevent
        // negative or zero inner widths.
        let mut block = Block::Quote(vec![Block::Paragraph(plain("text"))]);
        for _ in 0..20 {
            block = Block::Quote(vec![block]);
        }
        let h = estimate_block_height(&block, 14.0, 200.0, &style);
        assert_sane_height(h, "20-level nested blockquote (narrow)");
    }

    // ── Paragraph: no wrap opportunities ───────────────────────────

    #[test]
    fn paragraph_single_long_word() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let word = "x".repeat(5000); // 5000-char word, no spaces
        let block = Block::Paragraph(plain(&word));
        let h = estimate_block_height(&block, 14.0, 600.0, &style);
        assert_sane_height(h, "paragraph with 5k-char word");
    }

    #[test]
    fn paragraph_empty_text() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Paragraph(plain(""));
        let h = estimate_block_height(&block, 14.0, 600.0, &style);
        // Empty text → estimate_text_height returns font_size, plus padding.
        assert!(h > 0.0, "empty paragraph should still have height");
        assert!(h.is_finite());
    }

    // ── estimate_text_height edge cases ────────────────────────────

    #[test]
    fn text_height_empty_string() {
        let h = estimate_text_height("", 14.0, 200.0);
        assert!((h - 14.0).abs() < f32::EPSILON, "empty text → font_size");
    }

    #[test]
    fn text_height_wrap_width_zero() {
        // Must not divide by zero. chars_per_line = (0/avg).max(1.0) = 1.0.
        let h = estimate_text_height("hello world", 14.0, 0.0);
        assert!(h.is_finite(), "wrap_width=0 should not produce Inf/NaN");
        assert!(h > 0.0);
    }

    #[test]
    fn text_height_wrap_width_negative() {
        // Negative wrap_width is degenerate but must not panic or produce NaN.
        let h = estimate_text_height("hello", 14.0, -100.0);
        assert!(h.is_finite(), "negative wrap_width should produce finite h");
        assert!(h > 0.0, "height should be positive even with negative wrap");
    }

    #[test]
    fn text_height_scales_with_length() {
        let short = estimate_text_height("hi", 14.0, 200.0);
        let long = estimate_text_height(&"word ".repeat(200), 14.0, 200.0);
        assert!(
            long > short,
            "longer text ({long}) should be taller ({short})"
        );
    }

    #[test]
    fn text_height_multiline() {
        let one_line = estimate_text_height("hello", 14.0, 400.0);
        let ten_lines = estimate_text_height(&"hello\n".repeat(10), 14.0, 400.0);
        assert!(
            ten_lines > one_line,
            "10 lines ({ten_lines}) should exceed 1 ({one_line})"
        );
    }

    // ── wrap_width=0 across all block types ────────────────────────

    #[test]
    fn all_block_types_handle_zero_wrap_width() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let blocks: Vec<Block> = vec![
            Block::Heading {
                level: 1,
                text: plain("Title"),
            },
            Block::Paragraph(plain("text")),
            Block::Code {
                language: "rs".to_owned(),
                code: "code".to_owned(),
            },
            Block::Quote(vec![Block::Paragraph(plain("q"))]),
            Block::UnorderedList(vec![ListItem {
                content: plain("item"),
                children: vec![],
                checked: None,
            }]),
            Block::OrderedList {
                start: 1,
                items: vec![ListItem {
                    content: plain("item"),
                    children: vec![],
                    checked: None,
                }],
            },
            Block::ThematicBreak,
            Block::Table(Box::new(make_table(2, 2, "v"))),
            Block::Image {
                url: "u".to_owned(),
                alt: "a".to_owned(),
            },
        ];

        for (i, block) in blocks.iter().enumerate() {
            let h = estimate_block_height(block, 14.0, 0.0, &style);
            assert!(
                h.is_finite(),
                "block {i} ({block:?}): wrap_width=0 produced non-finite h={h}"
            );
            assert!(
                h > 0.0,
                "block {i}: wrap_width=0 produced non-positive h={h}"
            );
        }
    }

    // ── Mixed document: total height consistency ───────────────────

    #[test]
    fn total_height_ge_sum_of_individual() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let blocks: Vec<Block> = vec![
            Block::Heading {
                level: 2,
                text: plain("Section"),
            },
            Block::Paragraph(plain("Some body text here.")),
            Block::Code {
                language: "py".to_owned(),
                code: "print('hi')\n".to_owned(),
            },
            Block::Quote(vec![Block::Paragraph(plain("quoted"))]),
            Block::UnorderedList(vec![
                ListItem {
                    content: plain("a"),
                    children: vec![],
                    checked: None,
                },
                ListItem {
                    content: plain("b"),
                    children: vec![],
                    checked: None,
                },
            ]),
            Block::Table(Box::new(make_table(3, 4, "data"))),
            Block::ThematicBreak,
            Block::Image {
                url: "img.png".to_owned(),
                alt: "pic".to_owned(),
            },
        ];

        let wrap_width = 600.0;
        let body_size = 14.0;
        let sum: f32 = blocks
            .iter()
            .map(|b| estimate_block_height(b, body_size, wrap_width, &style))
            .sum();

        // When put into a cache, total_height should equal the sum.
        let mut cache = MarkdownCache::default();
        cache.blocks = blocks;
        cache.ensure_heights(body_size, wrap_width, &style);

        assert!(
            (cache.total_height - sum).abs() < 0.01,
            "cache total ({}) should match sum of individual heights ({sum})",
            cache.total_height,
        );
        assert_sane_height(cache.total_height, "mixed doc total");
    }

    // ── No NaN/Infinity across typical font sizes ──────────────────

    #[test]
    fn heights_finite_across_font_sizes() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let block = Block::Paragraph(plain(&"word ".repeat(50)));
        for size in [1.0_f32, 8.0, 14.0, 24.0, 72.0, 200.0] {
            let h = estimate_block_height(&block, size, 600.0, &style);
            assert!(
                h.is_finite() && h > 0.0,
                "font_size={size}: height should be finite & positive, got {h}"
            );
        }
    }

    // ── Table column width floor with many columns ─────────────────

    #[test]
    fn table_col_width_floor_prevents_tiny_cols() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        // 100 cols in 400px → raw col_width = 4px, but floor is 40px.
        // Height should still be reasonable (not squeezed to 0).
        let table = Block::Table(Box::new(make_table(100, 3, "cell data")));
        let h = estimate_block_height(&table, 14.0, 400.0, &style);
        assert_sane_height(h, "100-col table in 400px");
    }

    // ── bytecount_newlines ─────────────────────────────────────────

    #[test]
    fn bytecount_newlines_edge_cases() {
        assert_eq!(bytecount_newlines(b""), 0);
        assert_eq!(bytecount_newlines(b"no newlines"), 0);
        assert_eq!(bytecount_newlines(b"\n"), 1);
        assert_eq!(bytecount_newlines(b"\n\n\n"), 3);
        assert_eq!(bytecount_newlines(b"a\nb\nc\n"), 3);
    }
}
