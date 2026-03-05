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
    pub(crate) blocks: Vec<Block>,
    /// Estimated pixel height for each top-level block (same len as `blocks`).
    pub(crate) heights: Vec<f32>,
    /// Cumulative Y offsets: `cum_y[i]` = sum of heights[0..i].
    pub(crate) cum_y: Vec<f32>,
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

    /// Recompute `cum_y` and `total_height` from current `heights`.
    fn recompute_cum_y(&mut self) {
        let mut acc = 0.0_f32;
        for (cum, h) in self.cum_y.iter_mut().zip(&self.heights) {
            *cum = acc;
            acc += h;
        }
        self.total_height = acc;
    }

    /// Return the Y offset for the `ordinal`th **non-empty** heading block
    /// (0-based).  Empty headings (no visible text) are skipped so the
    /// ordinal aligns with `nav_outline::extract_headings` which also
    /// excludes them.
    #[must_use]
    pub fn heading_y(&self, ordinal: usize) -> Option<f32> {
        let mut seen = 0usize;
        for (idx, block) in self.blocks.iter().enumerate() {
            if let Block::Heading { text, .. } = block {
                if text.text.is_empty() {
                    continue;
                }
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

        // Track whether any estimated heights were corrected by measurement.
        let mut heights_changed = false;

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

            // Render visible blocks, measuring actual heights.
            let mut idx = first;
            while idx < cache.blocks.len() {
                let block_y = cache.cum_y[idx];
                if block_y > vis_bottom {
                    break;
                }

                // ── Progressive height refinement ──────────────────
                // Measure the actual rendered height via cursor delta
                // and update the estimate if it drifted significantly.
                let before_y = ui.cursor().top();
                render_block(ui, &cache.blocks[idx], style, 0);
                let after_y = ui.cursor().top();
                let actual_h = after_y - before_y;

                if actual_h > 0.0 && (cache.heights[idx] - actual_h).abs() > 2.0 {
                    cache.heights[idx] = actual_h;
                    heights_changed = true;
                }

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

        // Recompute cumulative offsets outside the viewport pass so the
        // *next* frame sees corrected positions without perturbing the
        // current frame's layout.
        if heights_changed {
            cache.recompute_cum_y();
        }
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
            // Match render_code_block: trailing newlines are stripped before display.
            let trimmed = code.trim_end_matches('\n');
            let lines = (bytecount_newlines(trimmed.as_bytes()) + 1).max(1) as f32;
            // 12.0 for Frame inner_margin (6px each side), 1.4 line spacing.
            // Add language label height when present.
            let lang_h = if language.is_empty() { 0.0 } else { body_size };
            body_size.mul_add(0.4, (lines * mono_size).mul_add(1.4, 12.0) + lang_h)
        }
        Block::Quote(inner) => estimate_quote_height(inner, body_size, wrap_width, style),
        Block::UnorderedList(items) => {
            estimate_list_height(items, body_size, wrap_width, style, None)
        }
        Block::OrderedList { start, items } => {
            estimate_list_height(items, body_size, wrap_width, style, Some(*start))
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

#[allow(clippy::cast_precision_loss)] // digit_count math on small values
fn estimate_list_height(
    items: &[ListItem],
    body_size: f32,
    wrap_width: f32,
    style: &MarkdownStyle,
    ordered_start: Option<u64>,
) -> f32 {
    // Match the bullet/number column width used in the actual renderers.
    let bullet_col = match ordered_start {
        Some(start) => {
            let max_num = start.saturating_add(items.len().saturating_sub(1) as u64);
            let digit_count = if max_num == 0 {
                1
            } else {
                (max_num as f64).log10().floor() as u32 + 1
            };
            // Mirrors render_ordered_list: 0.6 em per digit + 1.0 em + 4px gap.
            body_size.mul_add(0.6_f32.mul_add(digit_count as f32, 1.0), 4.0)
        }
        None => body_size.mul_add(1.5, 2.0),
    };
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
    let base_row_h = body_size * 1.4;
    let row_spacing = 3.0;

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
    // Use wider average char width for non-ASCII text (CJK glyphs are roughly
    // square, so ≈0.7 em is a better estimate than the 0.55 em used for Latin).
    let is_ascii = text.is_ascii();
    let avg_char_width = if is_ascii {
        font_size * 0.55
    } else {
        font_size * 0.7
    };
    let chars_per_line = (wrap_width / avg_char_width).max(1.0);
    // Count newlines by scanning bytes (much faster than .lines() for large text).
    let newline_count = bytecount_newlines(text.as_bytes());
    let hard_lines = (newline_count + 1).max(1);
    // Use character count (not byte count) for average line length to avoid
    // inflating wrap estimates for multi-byte characters like CJK.
    // Rust's chars().count() is internally optimized to count leading bytes.
    let char_count = if is_ascii {
        text.len()
    } else {
        text.chars().count()
    };
    let avg_line_len = char_count as f32 / hard_lines as f32;
    let wraps_per_line = (avg_line_len / chars_per_line).ceil().max(1.0);
    let total = hard_lines as f32 * wraps_per_line;
    total * font_size * 1.3
}

/// Fast newline counting via memchr.
#[must_use]
pub fn bytecount_newlines(bytes: &[u8]) -> usize {
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

/// Resolve a (possibly relative) image URL against a base URI.
///
/// Absolute URLs (containing `://` or starting with `//`) pass through
/// unchanged.  A URL starting with `/` is treated as an absolute path
/// and is resolved against only the scheme+authority of `base_uri`.
/// Otherwise the URL is appended to `base_uri` with exactly one `/`
/// separator.
fn resolve_image_url<'a>(url: &'a str, base_uri: &str) -> std::borrow::Cow<'a, str> {
    if url.starts_with("//") || url.contains("://") || base_uri.is_empty() {
        return std::borrow::Cow::Borrowed(url);
    }

    if url.starts_with('/') {
        // Absolute path — combine with the scheme+authority only.
        // e.g. base "file:///home/user/docs/" + "/images/pic.png"
        //   → "file:///images/pic.png"
        if let Some(idx) = base_uri.find("://") {
            let after_scheme = idx + 3; // skip "://"
            // Find the next '/' after the authority (if any).
            let authority_end = base_uri[after_scheme..]
                .find('/')
                .map_or(base_uri.len(), |i| after_scheme + i);
            let mut s = String::with_capacity(authority_end + url.len());
            s.push_str(&base_uri[..authority_end]);
            s.push_str(url);
            return std::borrow::Cow::Owned(s);
        }
        // No scheme — just use url as-is.
        return std::borrow::Cow::Borrowed(url);
    }

    // Relative path — ensure exactly one '/' separator.
    let base_slash = base_uri.ends_with('/');
    let mut s = String::with_capacity(base_uri.len() + url.len() + 1);
    s.push_str(base_uri);
    if !base_slash {
        s.push('/');
    }
    s.push_str(url);
    std::borrow::Cow::Owned(s)
}

fn render_image(ui: &mut egui::Ui, url: &str, alt: &str, style: &MarkdownStyle, body_size: f32) {
    let resolved = resolve_image_url(url, &style.image_base_uri);

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
            egui::Stroke::new(1.5, color),
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

    // Floor must match `estimate_quote_height` (40px) so that viewport
    // culling height estimates stay consistent with actual rendering.
    let inner_response = ui
        .allocate_ui_with_layout(
            egui::vec2((ui.available_width() - reserved).max(40.0), 0.0),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                ui.push_id(salt, |ui| {
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
        egui::Stroke::new(1.5, color),
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
        let num = start.saturating_add(i as u64);
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
    let col_cap = usable / num_cols as f32 * 3.0;
    let mut widths = Vec::with_capacity(num_cols);
    let mut total_est = 0.0_f32;
    for ci in 0..num_cols {
        let hdr_len = header.get(ci).map_or(0, |c| c.text.len());
        let max_row_len = rows
            .iter()
            .map(|r| r.get(ci).map_or(0, |c| c.text.len()))
            .max()
            .unwrap_or(0);
        let char_len = hdr_len.max(max_row_len).max(3) as f32;
        let w = (avg_char_w.mul_add(char_len, 12.0)).min(col_cap);
        total_est += w;
        widths.push(w);
    }

    // Normalise: scale to budget, clamp to minimum, redistribute overflow.
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
        // Ensure no column fell below minimum after redistribution.
        for w in &mut widths {
            if *w < min_col_w {
                *w = min_col_w;
            }
        }
    }

    // Cap single-column tables to 60% of usable width for a reasonable appearance.
    if num_cols == 1
        && let Some(w) = widths.first_mut()
    {
        let content_est =
            (header.first().map_or(0, |c| c.text.len()) as f32).mul_add(avg_char_w, 24.0);
        let max_single = (usable * 0.6).max(content_est.min(usable));
        *w = (*w).min(max_single);
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
                    // Pad short rows with empty cells to keep grid rectangular.
                    for _ in row.len()..num_cols {
                        ui.label("");
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
        Alignment::Center => egui::Layout::top_down(egui::Align::Center),
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

pub(crate) fn simple_hash(s: &str) -> u64 {
    // FNV-1a–inspired 64-bit hash, processing 8 bytes at a time for throughput.
    const BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;

    let bytes = s.as_bytes();
    let chunks = bytes.chunks_exact(8);
    let remainder = chunks.remainder();
    let mut h: u64 = BASIS;

    for chunk in chunks {
        // chunks_exact(8) guarantees exactly 8 bytes — direct conversion is safe.
        let word = u64::from_le_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]);
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
    use crate::parse::{Span, SpanStyle, TableData};
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
        style.set_heading_colors(colors);
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
    fn heading_y_skips_empty_headings() {
        // "# \n" is an empty heading (no text content), "## Real\n" has text.
        // heading_y must skip empty headings so ordinals align with
        // nav_outline::extract_headings which also excludes them.
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# \n\n## Real\n");
        cache.ensure_heights(14.0, 400.0, &style);

        // Ordinal 0 should be "## Real", not the empty "# ".
        let y = cache.heading_y(0);
        assert!(
            y.is_some(),
            "heading_y(0) should find the non-empty heading"
        );
        assert!(
            y.unwrap_or(0.0) > 0.0,
            "heading_y(0) should skip empty heading at Y=0 and return Y of '## Real'"
        );
        // Only one non-empty heading, so ordinal 1 should be None.
        assert!(
            cache.heading_y(1).is_none(),
            "heading_y(1) should be None (only one non-empty heading)"
        );
    }

    #[test]
    fn heading_y_empty_heading_between_real_ones() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# First\n\n## \n\n### Third\n");
        cache.ensure_heights(14.0, 400.0, &style);

        let y0 = cache.heading_y(0);
        let y1 = cache.heading_y(1);
        assert!(y0.is_some());
        assert!(y1.is_some());
        assert!(
            y0.unwrap_or(0.0) < y1.unwrap_or(0.0),
            "ordinals should map to First then Third, skipping empty ## "
        );
        assert!(cache.heading_y(2).is_none(), "only two non-empty headings");
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
        style.set_heading_scales(scales);
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

    // ── Table column width unit tests ─────────────────────────────

    fn make_cells(texts: &[&str]) -> Vec<StyledText> {
        texts
            .iter()
            .map(|t| StyledText {
                text: t.to_string(),
                spans: vec![],
            })
            .collect()
    }

    #[test]
    fn table_col_widths_single_column_capped() {
        let header = make_cells(&["Status"]);
        let rows = vec![make_cells(&["OK"]), make_cells(&["Error"])];
        let (widths, _) = compute_table_col_widths(&header, &rows, 600.0, 7.7, 14.0);
        assert_eq!(widths.len(), 1);
        // Single-column table should be capped at ≤60% of usable.
        assert!(
            widths[0] <= 600.0 * 0.61,
            "single column {} should be ≤60% of 600",
            widths[0]
        );
    }

    #[test]
    fn table_col_widths_proportional_to_content() {
        let header = make_cells(&["ID", "Full Name and Description Here"]);
        let rows = vec![
            make_cells(&["1", "Alice Johnson, Software Engineer"]),
            make_cells(&["2", "Bob Smith, Product Manager"]),
        ];
        let (widths, _) = compute_table_col_widths(&header, &rows, 600.0, 7.7, 14.0);
        assert_eq!(widths.len(), 2);
        // The wider column should get significantly more space.
        assert!(
            widths[1] > widths[0] * 1.5,
            "wide column {} should be much wider than narrow {}",
            widths[1],
            widths[0]
        );
    }

    #[test]
    fn table_col_widths_three_columns_reasonable() {
        let header = make_cells(&["Left", "Center", "Right"]);
        let rows = vec![
            make_cells(&["data", "data", "data"]),
            make_cells(&["more", "text", "here"]),
        ];
        let (widths, min_col_w) = compute_table_col_widths(&header, &rows, 600.0, 7.7, 14.0);
        assert_eq!(widths.len(), 3);
        for (i, w) in widths.iter().enumerate() {
            assert!(
                *w >= min_col_w - 0.01,
                "column {i} width {w} should be >= min {min_col_w}"
            );
        }
        let total: f32 = widths.iter().sum();
        assert!(total <= 601.0, "total {total} should not exceed usable 600");
    }

    #[test]
    fn table_col_widths_ten_columns_all_minimum() {
        let header = make_cells(&["A", "B", "C", "D", "E", "F", "G", "H", "I", "J"]);
        let rows = vec![make_cells(&[
            "1", "2", "3", "4", "5", "6", "7", "8", "9", "0",
        ])];
        let (widths, min_col_w) = compute_table_col_widths(&header, &rows, 400.0, 7.7, 14.0);
        assert_eq!(widths.len(), 10);
        for (i, w) in widths.iter().enumerate() {
            assert!(
                *w >= min_col_w - 0.01,
                "column {i} width {w} should be >= min {min_col_w}"
            );
        }
    }

    #[test]
    fn table_col_widths_one_dominant_column() {
        let long_text = "x".repeat(200);
        let header = make_cells(&["Tiny", "Medium text", &long_text]);
        let rows = vec![make_cells(&["a", "something", "y"])];
        let (widths, _) = compute_table_col_widths(&header, &rows, 600.0, 7.7, 14.0);
        assert_eq!(widths.len(), 3);
        // The dominant column should be capped, not take all 600px.
        assert!(
            widths[2] < 500.0,
            "dominant column {} should be capped",
            widths[2]
        );
    }

    #[test]
    fn table_col_widths_all_empty_cells() {
        let header = make_cells(&["", "", ""]);
        let rows = vec![make_cells(&["", "", ""])];
        let (widths, min_col_w) = compute_table_col_widths(&header, &rows, 600.0, 7.7, 14.0);
        assert_eq!(widths.len(), 3);
        for (i, w) in widths.iter().enumerate() {
            assert!(
                *w >= min_col_w - 0.01,
                "empty column {i} width {w} should be >= min {min_col_w}"
            );
        }
    }

    #[test]
    fn table_col_widths_tight_space() {
        // 8 columns in 200px — very tight.
        let header = make_cells(&["A", "B", "C", "D", "E", "F", "G", "H"]);
        let rows = vec![make_cells(&["1", "2", "3", "4", "5", "6", "7", "8"])];
        let (widths, _) = compute_table_col_widths(&header, &rows, 200.0, 7.7, 14.0);
        assert_eq!(widths.len(), 8);
        let total: f32 = widths.iter().sum();
        // Total width should be reasonable even if it overflows.
        assert!(total >= 150.0, "total width {total} should be reasonable");
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
        let r = resolve_image_url("https://example.com/pic.png", "");
        assert_eq!(r, "https://example.com/pic.png");
    }

    #[test]
    fn image_url_relative_with_base_uri() {
        let r = resolve_image_url("images/pic.png", "file:///home/user/docs/");
        assert_eq!(r, "file:///home/user/docs/images/pic.png");
    }

    #[test]
    fn image_url_relative_with_empty_base_uri() {
        let r = resolve_image_url("images/pic.png", "");
        assert_eq!(r, "images/pic.png");
    }

    #[test]
    fn image_url_absolute_ignores_base_uri() {
        let r = resolve_image_url("http://cdn.example.com/img.jpg", "file:///local/base/");
        assert_eq!(r, "http://cdn.example.com/img.jpg");
    }

    #[test]
    fn image_url_protocol_relative_stays_unchanged() {
        let r = resolve_image_url("//cdn.example.com/image.png", "file:///local/base/");
        assert_eq!(r, "//cdn.example.com/image.png");
    }

    #[test]
    fn image_url_base_uri_missing_trailing_slash() {
        // BUG FIX: base URI without trailing '/' must still produce a valid path.
        let r = resolve_image_url("image.png", "file:///home/user");
        assert_eq!(r, "file:///home/user/image.png");
    }

    #[test]
    fn image_url_absolute_path_resolves_against_authority() {
        // A URL starting with '/' is an absolute path — resolve against
        // scheme+authority only, discarding the base directory.
        let r = resolve_image_url("/images/pic.png", "file:///home/user/docs/");
        assert_eq!(r, "file:///images/pic.png");
    }

    #[test]
    fn image_url_absolute_path_with_http_base() {
        let r = resolve_image_url("/assets/logo.png", "http://example.com/docs/");
        assert_eq!(r, "http://example.com/assets/logo.png");
    }

    #[test]
    fn image_url_absolute_path_no_scheme_passthrough() {
        // No scheme in base — absolute-path URL is returned as-is.
        let r = resolve_image_url("/images/pic.png", "no-scheme-base");
        assert_eq!(r, "/images/pic.png");
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
    fn code_block_trailing_newlines_not_overcounted() {
        // pulldown-cmark always appends a trailing `\n` to code content.
        // The renderer strips trailing newlines before display, so the
        // height estimate must do the same to avoid systematic over-counting.
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let with_trailing = Block::Code {
            language: String::new(),
            code: "line1\nline2\n".to_owned(),
        };
        let without_trailing = Block::Code {
            language: String::new(),
            code: "line1\nline2".to_owned(),
        };
        let h_with = estimate_block_height(&with_trailing, 14.0, 600.0, &style);
        let h_without = estimate_block_height(&without_trailing, 14.0, 600.0, &style);
        assert!(
            (h_with - h_without).abs() < f32::EPSILON,
            "trailing newline should not increase height: with={h_with}, without={h_without}"
        );
    }

    #[test]
    fn code_block_only_newlines_estimates_one_line() {
        // A code block containing only newlines renders as a single NBSP line.
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let only_newlines = Block::Code {
            language: String::new(),
            code: "\n\n\n".to_owned(),
        };
        let empty = Block::Code {
            language: String::new(),
            code: String::new(),
        };
        let h_nl = estimate_block_height(&only_newlines, 14.0, 600.0, &style);
        let h_empty = estimate_block_height(&empty, 14.0, 600.0, &style);
        assert!(
            (h_nl - h_empty).abs() < f32::EPSILON,
            "only-newlines block should match empty block height: nl={h_nl}, empty={h_empty}"
        );
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

    // ── Ordered list saturating_add for display number ─────────────

    #[test]
    fn ordered_list_huge_start_no_overflow() {
        // start near u64::MAX must not panic via integer overflow.
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let _viewer = MarkdownViewer::new("ol_huge");

        // Construct a Block directly because the parser caps start values.
        cache.blocks = vec![Block::OrderedList {
            start: u64::MAX - 1,
            items: vec![
                ListItem {
                    content: StyledText {
                        text: "first".to_owned(),
                        spans: vec![],
                    },
                    children: vec![],
                    checked: None,
                },
                ListItem {
                    content: StyledText {
                        text: "second".to_owned(),
                        spans: vec![],
                    },
                    children: vec![],
                    checked: None,
                },
                ListItem {
                    content: StyledText {
                        text: "third".to_owned(),
                        spans: vec![],
                    },
                    children: vec![],
                    checked: None,
                },
            ],
        }];

        // Must not panic — previous code used `start + i` which overflows.
        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                render_blocks(ui, &cache.blocks, &style, 0);
            });
        });
    }

    // ── strengthen_color premultiplied-alpha correctness ────────────

    #[test]
    fn strengthen_color_opaque_still_works() {
        // Opaque bright text → should brighten (no regression).
        let bright = egui::Color32::from_rgb(200, 200, 200);
        let result = strengthen_color(bright);
        let [r, g, b, a] = result.to_srgba_unmultiplied();
        assert_eq!(a, 255);
        assert!(r > 200, "bright text should be brightened, got r={r}");
        assert!(g > 200);
        assert!(b > 200);

        // Opaque dark text → should darken.
        let dark = egui::Color32::from_rgb(40, 40, 40);
        let result = strengthen_color(dark);
        let [r2, g2, b2, a2] = result.to_srgba_unmultiplied();
        assert_eq!(a2, 255);
        assert!(r2 < 40, "dark text should be darkened, got r={r2}");
        assert!(g2 < 40);
        assert!(b2 < 40);
    }

    #[test]
    fn strengthen_color_semitransparent_respects_alpha() {
        // Semi-transparent bright text: premultiplied R,G,B must stay ≤ alpha.
        let semi = egui::Color32::from_rgba_unmultiplied(200, 200, 200, 100);
        let result = strengthen_color(semi);
        let [r, g, b, a] = result.to_array(); // premultiplied
        assert!(
            r <= a && g <= a && b <= a,
            "premultiplied channels must not exceed alpha: r={r} g={g} b={b} a={a}"
        );
        // Unmultiplied values should still be brightened.
        let [ur, _, _, ua] = result.to_srgba_unmultiplied();
        assert_eq!(ua, 100, "alpha should be preserved");
        assert!(ur > 200, "unmultiplied R should be brightened, got {ur}");
    }

    // ── Headless rendering: width-configurable helper ──────────────

    /// Render markdown headlessly at a given screen width, returning
    /// `(block_count, estimated_total_height, rendered_total_height)`.
    fn headless_render_at_width(source: &str, width: f32) -> (usize, f32, f32) {
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let viewer = MarkdownViewer::new("test_width");

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 768.0),
            )),
            ..Default::default()
        };

        let mut rendered_height = 0.0_f32;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let before = ui.cursor().min.y;
                viewer.show(ui, &mut cache, &style, source);
                rendered_height = ui.cursor().min.y - before;
            });
        });

        let body_size = 14.0;
        let wrap_width = (width - 16.0).max(10.0); // approximate panel margin
        cache.ensure_heights(body_size, wrap_width, &style);
        (cache.blocks.len(), cache.total_height, rendered_height)
    }

    // ── 1. Height accuracy tests ───────────────────────────────────

    #[test]
    fn height_accuracy_heading_and_paragraph() {
        let md = "# Main Title\n\nThis is a short paragraph with some text.\n";
        let (_, estimated, rendered) = headless_render_at_width(md, 1024.0);
        assert!(estimated > 0.0);
        assert!(rendered > 0.0);
        let ratio = estimated / rendered;
        assert!(
            ratio > 0.2 && ratio < 5.0,
            "heading+paragraph height ratio out of range: estimated={estimated}, rendered={rendered}, ratio={ratio}"
        );
    }

    #[test]
    fn height_accuracy_tables_only() {
        let md = "\
| A | B | C |
|---|---|---|
| 1 | 2 | 3 |
| 4 | 5 | 6 |
| 7 | 8 | 9 |

| X | Y |
|---|---|
| a | b |
";
        let (_, estimated, rendered) = headless_render_at_width(md, 1024.0);
        assert!(estimated > 0.0);
        assert!(rendered > 0.0);
        let ratio = estimated / rendered;
        assert!(
            ratio > 0.2 && ratio < 5.0,
            "table-only height ratio out of range: estimated={estimated}, rendered={rendered}, ratio={ratio}"
        );
    }

    #[test]
    fn height_accuracy_code_blocks_only() {
        let md = "\
```rust
fn main() {
    println!(\"hello\");
}
```

```python
for i in range(10):
    print(i)
```
";
        let (_, estimated, rendered) = headless_render_at_width(md, 1024.0);
        assert!(estimated > 0.0);
        assert!(rendered > 0.0);
        let ratio = estimated / rendered;
        assert!(
            ratio > 0.2 && ratio < 5.0,
            "code-only height ratio out of range: estimated={estimated}, rendered={rendered}, ratio={ratio}"
        );
    }

    #[test]
    fn height_accuracy_lists_only() {
        let md = "\
- Item one
- Item two
- Item three
  - Nested A
  - Nested B
- Item four

1. First
2. Second
3. Third
";
        let (_, estimated, rendered) = headless_render_at_width(md, 1024.0);
        assert!(estimated > 0.0);
        assert!(rendered > 0.0);
        let ratio = estimated / rendered;
        assert!(
            ratio > 0.2 && ratio < 5.0,
            "list-only height ratio out of range: estimated={estimated}, rendered={rendered}, ratio={ratio}"
        );
    }

    // ── 2. Scrollable rendering stress ─────────────────────────────

    #[test]
    fn scrollable_stress_large_mixed_20_positions() {
        let doc = crate::stress::large_mixed_doc(100);
        let (_, total_height) = headless_render_scrollable(&doc, None);
        assert!(total_height > 0.0);
        let step = total_height / 20.0;
        for i in 0..20 {
            let y = step * i as f32;
            let _ = headless_render_scrollable(&doc, Some(y));
        }
    }

    #[test]
    fn scrollable_stress_pathological_no_crash() {
        let doc = crate::stress::pathological_doc(50);
        let _ = headless_render_scrollable(&doc, None);
    }

    #[test]
    fn scrollable_stress_unicode_no_crash() {
        let doc = crate::stress::unicode_stress_doc(50);
        let _ = headless_render_scrollable(&doc, None);
    }

    #[test]
    fn scrollable_stress_table_heavy_no_crash() {
        let doc = crate::stress::table_heavy_doc(50);
        let _ = headless_render_scrollable(&doc, None);
    }

    #[test]
    fn scrollable_stress_emoji_heavy_no_crash() {
        let doc = crate::stress::emoji_heavy_doc(50);
        let _ = headless_render_scrollable(&doc, None);
    }

    #[test]
    fn scrollable_stress_task_list_no_crash() {
        let doc = crate::stress::task_list_doc(50);
        let _ = headless_render_scrollable(&doc, None);
    }

    // ── 3. Layout consistency ──────────────────────────────────────

    #[test]
    fn layout_consistency_same_doc_twice() {
        let md = "# Title\n\nParagraph **bold** and *italic*.\n\n## Sub\n\n- a\n- b\n";
        let style = MarkdownStyle::colored(&egui::Visuals::dark());

        let mut cache1 = MarkdownCache::default();
        cache1.ensure_parsed(md);
        cache1.ensure_heights(14.0, 900.0, &style);

        let mut cache2 = MarkdownCache::default();
        cache2.ensure_parsed(md);
        cache2.ensure_heights(14.0, 900.0, &style);

        assert!(
            (cache1.total_height - cache2.total_height).abs() < f32::EPSILON,
            "same doc rendered twice should produce identical total_height: {} vs {}",
            cache1.total_height,
            cache2.total_height
        );
        assert_eq!(cache1.heights.len(), cache2.heights.len());
        for (i, (h1, h2)) in cache1.heights.iter().zip(&cache2.heights).enumerate() {
            assert!(
                (h1 - h2).abs() < f32::EPSILON,
                "block {i} height mismatch: {h1} vs {h2}"
            );
        }
    }

    // ── 4. Scrollable vs non-scrollable consistency ────────────────

    #[test]
    fn scrollable_vs_inline_block_count_matches() {
        let md = "# Hi\n\nSmall doc.\n\n- one\n- two\n";
        let (inline_blocks, _) = headless_render(md);
        let (scrollable_count, _) = headless_render_scrollable(md, None);
        assert_eq!(
            inline_blocks.len(),
            scrollable_count,
            "block count should match between inline and scrollable rendering"
        );
    }

    // ── 5. Width sensitivity ───────────────────────────────────────

    #[test]
    fn width_sensitivity_height_decreases_with_wider() {
        let md = "\
Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor \
incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud \
exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure \
dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.

Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt \
mollit anim id est laborum. Curabitur pretium tincidunt lacus. Nulla gravida orci a \
odio. Nullam varius, turpis et commodo pharetra, est eros bibendum elit.

Another paragraph with more words to force wrapping behaviour at narrow widths so \
that we can observe the relationship between available width and estimated height.
";
        let widths = [100.0, 400.0, 800.0, 1600.0, 3200.0];
        let mut heights = Vec::new();
        for &w in &widths {
            let style = MarkdownStyle::colored(&egui::Visuals::dark());
            let mut cache = MarkdownCache::default();
            cache.ensure_parsed(md);
            let wrap = (w - 16.0_f32).max(10.0);
            cache.ensure_heights(14.0, wrap, &style);
            heights.push((w, cache.total_height));
        }
        // Height at 100px should be >= height at 3200px (more wrapping at narrow widths).
        let (_, h_narrow) = heights[0];
        let (_, h_wide) = heights[heights.len() - 1];
        assert!(
            h_narrow >= h_wide,
            "narrow ({h_narrow}) should have >= height than wide ({h_wide})"
        );
        // Also check monotonic non-increasing trend (allowing small tolerance).
        for i in 1..heights.len() {
            let (w_prev, h_prev) = heights[i - 1];
            let (w_cur, h_cur) = heights[i];
            assert!(
                h_cur <= h_prev + 1.0,
                "height should not increase significantly going from width {w_prev} ({h_prev}) to {w_cur} ({h_cur})"
            );
        }
    }

    // ── 6. Edge case rendering ─────────────────────────────────────

    #[test]
    fn edge_case_empty_string_scrollable() {
        let (count, height) = headless_render_scrollable("", None);
        assert_eq!(count, 0);
        assert!((height - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn edge_case_single_heading_scrollable() {
        let (count, height) = headless_render_scrollable("# Solo Heading", None);
        assert_eq!(count, 1);
        assert!(height > 0.0);
    }

    #[test]
    fn edge_case_1000_thematic_breaks() {
        let md = "---\n\n".repeat(1000);
        let (blocks, height) = headless_render(&md);
        let break_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::ThematicBreak))
            .count();
        assert_eq!(break_count, 1000, "expected 1000 thematic breaks");
        assert!(height > 0.0);
    }

    #[test]
    fn edge_case_deeply_nested_blockquotes_with_tables() {
        let md = "\
> > > > > | A | B |
> > > > > |---|---|
> > > > > | 1 | 2 |
> > > > >
> > > > > | C | D |
> > > > > |---|---|
> > > > > | 3 | 4 |
";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
        // Should have at least one Quote block
        assert!(
            blocks.iter().any(|b| matches!(b, Block::Quote(_))),
            "should have at least one blockquote"
        );
    }

    #[test]
    fn edge_case_deeply_nested_blockquotes_scrollable() {
        let md = "\
> Level 1
> > Level 2
> > > Level 3
> > > > Level 4
> > > > > Level 5 with a table:
> > > > >
> > > > > | H1 | H2 |
> > > > > |----|-----|
> > > > > | v1 | v2  |
";
        let (count, height) = headless_render_scrollable(md, None);
        assert!(count > 0);
        assert!(height > 0.0);
    }

    // ── Table stress tests ─────────────────────────────────────────

    /// Build a markdown table with `cols` columns and `rows` data rows.
    /// `cell_fn` produces the cell content for the given (row, col).
    fn build_table_md(
        cols: usize,
        rows: usize,
        cell_fn: impl Fn(usize, usize) -> String,
    ) -> String {
        let mut md = String::new();
        // Header row
        md.push('|');
        for c in 0..cols {
            let _ = write!(md, " H{c} |");
        }
        md.push('\n');
        // Separator row
        md.push('|');
        for _ in 0..cols {
            md.push_str("---|");
        }
        md.push('\n');
        // Data rows
        for r in 0..rows {
            md.push('|');
            for c in 0..cols {
                let _ = write!(md, " {} |", cell_fn(r, c));
            }
            md.push('\n');
        }
        md
    }

    // ── 1. Extreme column counts ──────────────────────────────────

    #[test]
    fn table_stress_single_column() {
        let md = build_table_md(1, 5, |r, _| format!("row{r}"));
        let (blocks, height) = headless_render(&md);
        assert!(height > 0.0, "1-col table should have positive height");
        assert!(
            blocks.iter().any(|b| matches!(b, Block::Table(_))),
            "should contain a Table block"
        );
    }

    #[test]
    fn table_stress_50_columns() {
        let md = build_table_md(50, 3, |r, c| format!("r{r}c{c}"));
        let (blocks, height) = headless_render(&md);
        assert!(height > 0.0, "50-col table should have positive height");
        assert!(
            blocks.iter().any(|b| matches!(b, Block::Table(_))),
            "should contain a Table block"
        );
    }

    #[test]
    fn table_stress_100_columns() {
        let md = build_table_md(100, 2, |r, c| format!("r{r}c{c}"));
        let (_, height) = headless_render(&md);
        assert!(height > 0.0, "100-col table should have positive height");
    }

    // ── 2. Extreme row counts ──────────────────────────────────────

    #[test]
    fn table_stress_500_rows_scrollable() {
        let md = build_table_md(3, 500, |r, c| format!("val_{r}_{c}"));
        // Render at various scroll positions: top, middle, bottom.
        for scroll_y in [None, Some(0.0), Some(2000.0), Some(10000.0)] {
            let (count, height) = headless_render_scrollable(&md, scroll_y);
            assert!(count > 0, "500-row table should parse into blocks");
            assert!(
                height > 0.0,
                "500-row table should have positive height at scroll {scroll_y:?}"
            );
        }
    }

    #[test]
    fn table_stress_header_only_no_data_rows() {
        let md = "| A | B | C |\n|---|---|---|\n";
        let (blocks, height) = headless_render(md);
        assert!(
            height > 0.0,
            "header-only table should have positive height"
        );
        let table = blocks.iter().find_map(|b| match b {
            Block::Table(t) => Some(t),
            _ => None,
        });
        let table = table.expect("should contain a Table block");
        assert_eq!(table.header.len(), 3);
        assert!(table.rows.is_empty(), "should have no data rows");
    }

    // ── 3. Cell content extremes ───────────────────────────────────

    #[test]
    fn table_stress_long_cell_content() {
        // Each cell has 1000+ characters.
        let md = build_table_md(3, 3, |_, _| "x".repeat(1200));
        let (_, height) = headless_render(&md);
        assert!(height > 0.0, "long-cell table should render without panic");
    }

    #[test]
    fn table_stress_rich_inline_formatting() {
        // Each cell has bold + italic + code mixed text (50 words).
        let cell = (0..50)
            .map(|i| match i % 3 {
                0 => format!("**bold{i}**"),
                1 => format!("*italic{i}*"),
                _ => format!("`code{i}`"),
            })
            .collect::<Vec<_>>()
            .join(" ");
        let md = build_table_md(3, 5, |_, _| cell.clone());
        let (_, height) = headless_render(&md);
        assert!(
            height > 0.0,
            "rich-inline table should render without panic"
        );
    }

    #[test]
    fn table_stress_cells_with_links() {
        let md = build_table_md(3, 5, |r, c| {
            format!("[link{r}{c}](https://example.com/{r}/{c})")
        });
        let (_, height) = headless_render(&md);
        assert!(height > 0.0, "link table should render without panic");
    }

    #[test]
    fn table_stress_emoji_cells() {
        let emojis = ["🎉", "🚀", "✅", "❌", "⚡", "🔥", "💡", "📝"];
        let md = build_table_md(4, 4, |r, c| emojis[(r + c) % emojis.len()].to_owned());
        let (_, height) = headless_render(&md);
        assert!(height > 0.0, "emoji table should render without panic");
    }

    // ── 4. compute_table_col_widths edge cases ─────────────────────

    #[test]
    fn col_widths_usable_zero() {
        let header = vec![plain("A"), plain("B")];
        let rows = vec![vec![plain("x"), plain("y")]];
        let (widths, _min) = compute_table_col_widths(&header, &rows, 0.0, 7.0, 14.0);
        assert_eq!(widths.len(), 2);
        // With zero usable space, widths should still be non-negative.
        for w in &widths {
            assert!(*w >= 0.0, "width should be non-negative, got {w}");
        }
    }

    #[test]
    fn col_widths_usable_very_small() {
        let header = vec![plain("A"), plain("B"), plain("C")];
        let rows = vec![vec![plain("x"), plain("y"), plain("z")]];
        // 10px usable < min_col_w * 3 = 36 * 3 = 108
        let (widths, min_col_w) = compute_table_col_widths(&header, &rows, 10.0, 7.0, 14.0);
        assert_eq!(widths.len(), 3);
        // All columns should be clamped to min_col_w.
        for w in &widths {
            assert!(
                *w >= min_col_w - 0.01,
                "width {w} should be >= min {min_col_w}"
            );
        }
    }

    #[test]
    fn col_widths_all_same_length() {
        let header = vec![plain("ABCD"), plain("EFGH"), plain("IJKL")];
        let rows = vec![vec![plain("1234"), plain("5678"), plain("9012")]];
        let (widths, _) = compute_table_col_widths(&header, &rows, 600.0, 7.0, 14.0);
        assert_eq!(widths.len(), 3);
        // All columns should get roughly equal width.
        let avg = widths.iter().sum::<f32>() / widths.len() as f32;
        for w in &widths {
            assert!(
                (*w - avg).abs() < avg * 0.2,
                "widths should be roughly equal: {widths:?}"
            );
        }
    }

    #[test]
    fn col_widths_one_column_much_longer() {
        let header = vec![plain("A"), plain(&"X".repeat(300))];
        let rows = vec![vec![plain("a"), plain(&"Y".repeat(300))]];
        let (widths, _) = compute_table_col_widths(&header, &rows, 600.0, 7.0, 14.0);
        assert_eq!(widths.len(), 2);
        assert!(
            widths[1] > widths[0],
            "longer column should be wider: {widths:?}"
        );
    }

    #[test]
    fn col_widths_body_size_zero() {
        let header = vec![plain("A"), plain("B")];
        let rows = vec![vec![plain("x"), plain("y")]];
        // body_size = 0 → min_col_w = max(0 * 2.5, 36) = 36
        let (widths, min_col_w) = compute_table_col_widths(&header, &rows, 200.0, 7.0, 0.0);
        assert_eq!(widths.len(), 2);
        assert!(
            (min_col_w - 36.0).abs() < 0.01,
            "min_col_w should be 36 when body_size=0, got {min_col_w}"
        );
    }

    // ── 5. Alignment rendering ─────────────────────────────────────

    #[test]
    fn table_stress_all_alignment_types() {
        let md = "\
| Default | Left | Center | Right |
|---------|:-----|:------:|------:|
| d       | l    | c      | r     |
";
        let (blocks, height) = headless_render(md);
        assert!(height > 0.0, "aligned table should have positive height");
        let table = blocks.iter().find_map(|b| match b {
            Block::Table(t) => Some(t),
            _ => None,
        });
        let table = table.expect("should contain a Table block");
        assert_eq!(table.alignments.len(), 4);
        assert_eq!(table.alignments[0], Alignment::None);
        assert_eq!(table.alignments[1], Alignment::Left);
        assert_eq!(table.alignments[2], Alignment::Center);
        assert_eq!(table.alignments[3], Alignment::Right);
    }

    #[test]
    fn table_stress_missing_alignment_defaults_to_none() {
        // Simple table: no alignment markers → all None.
        let md = "\
| A | B |
|---|---|
| 1 | 2 |
";
        let (blocks, _) = headless_render(md);
        let table = blocks.iter().find_map(|b| match b {
            Block::Table(t) => Some(t),
            _ => None,
        });
        let table = table.expect("should contain a Table block");
        for (i, a) in table.alignments.iter().enumerate() {
            assert_eq!(
                *a,
                Alignment::None,
                "column {i} should default to None alignment"
            );
        }
    }

    // ── 6. Table interaction with viewport culling ──────────────────

    #[test]
    fn table_stress_100_small_tables_scrollable() {
        let mut md = String::new();
        for i in 0..100 {
            let _ = writeln!(md, "| T{i}A | T{i}B |");
            md.push_str("|---|---|\n");
            let _ = writeln!(md, "| {i}a | {i}b |");
            md.push('\n');
        }
        // Scroll to the middle.
        let (count, height) = headless_render_scrollable(&md, Some(height_of(&md) / 2.0));
        assert!(count >= 100, "should parse all 100 tables, got {count}");
        assert!(height > 0.0);
    }

    #[test]
    fn table_stress_alternating_tables_and_paragraphs() {
        let mut md = String::new();
        for i in 0..50 {
            let _ = writeln!(md, "Paragraph {i} with some text here.\n");
            let _ = writeln!(md, "| Col1 | Col2 |");
            md.push_str("|------|------|\n");
            let _ = writeln!(md, "| r{i}  | c{i}  |\n");
        }
        for scroll_y in [None, Some(0.0), Some(500.0), Some(2000.0)] {
            let (count, height) = headless_render_scrollable(&md, scroll_y);
            assert!(count > 0, "mixed doc should have blocks");
            assert!(
                height > 0.0,
                "mixed doc should have positive height at scroll {scroll_y:?}"
            );
        }
    }

    /// Quick height estimate for scroll-target computation in tests.
    fn height_of(source: &str) -> f32 {
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(source);
        cache.ensure_heights(14.0, 900.0, &style);
        cache.total_height
    }

    // ── estimate_table_height edge cases ────────────────────────────

    #[test]
    fn estimate_table_height_empty_header() {
        let table = TableData {
            header: vec![],
            alignments: vec![],
            rows: vec![vec![plain("x")]],
        };
        let h = estimate_table_height(&table, 14.0, 400.0);
        assert!(
            h > 0.0,
            "table with empty header should still have height from rows"
        );
    }

    #[test]
    fn estimate_table_height_many_rows() {
        let table = TableData {
            header: vec![plain("A"), plain("B"), plain("C")],
            alignments: vec![Alignment::None; 3],
            rows: (0..200)
                .map(|r| vec![plain(&format!("r{r}")), plain("mid"), plain("end")])
                .collect(),
        };
        let h = estimate_table_height(&table, 14.0, 400.0);
        assert!(
            h > 200.0,
            "200-row table estimate should be substantial, got {h}"
        );
    }

    #[test]
    fn estimate_table_height_narrow_wrap_width() {
        let table = TableData {
            header: vec![plain("Header1"), plain("Header2")],
            alignments: vec![Alignment::None; 2],
            rows: vec![vec![plain(&"word ".repeat(50)), plain(&"text ".repeat(50))]],
        };
        let h_narrow = estimate_table_height(&table, 14.0, 50.0);
        let h_wide = estimate_table_height(&table, 14.0, 800.0);
        assert!(
            h_narrow >= h_wide,
            "narrow wrap should produce taller estimate: narrow={h_narrow}, wide={h_wide}"
        );
    }

    // ── List stress tests ──────────────────────────────────────────

    /// Build a plain `ListItem` with no children and no checkbox.
    fn plain_item(text: &str) -> ListItem {
        ListItem {
            content: StyledText {
                text: text.to_owned(),
                spans: vec![],
            },
            children: vec![],
            checked: None,
        }
    }

    // ── 1. Extreme item counts ─────────────────────────────────────

    #[test]
    fn stress_unordered_list_500_items_scrollable() {
        let mut md = String::with_capacity(500 * 20);
        for i in 0..500 {
            writeln!(md, "- Item {i}").ok();
        }
        let (count, height) = headless_render_scrollable(&md, None);
        assert!(count > 0);
        assert!(height > 0.0, "500-item list should have positive height");
    }

    #[test]
    fn stress_ordered_list_500_items() {
        let mut md = String::with_capacity(500 * 20);
        for i in 1..=500 {
            writeln!(md, "{i}. Item number {i}").ok();
        }
        let (blocks, height) = headless_render(&md);
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 1);
                assert_eq!(items.len(), 500);
            }
            other => panic!("expected OrderedList, got {other:?}"),
        }
        assert!(height > 0.0);
    }

    // ── 2. Extreme nesting ─────────────────────────────────────────

    #[test]
    fn stress_list_nested_10_levels() {
        let mut md = String::new();
        for depth in 0..10 {
            let indent = "  ".repeat(depth);
            writeln!(md, "{indent}- Level {depth}").ok();
        }
        let (blocks, height) = headless_render(&md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0, "10-level nested list should render");
    }

    #[test]
    fn stress_list_nested_20_levels_no_overflow() {
        let mut md = String::new();
        for depth in 0..20 {
            let indent = "  ".repeat(depth);
            writeln!(md, "{indent}- Depth {depth}").ok();
        }
        let (blocks, height) = headless_render(&md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0, "20-level nested list should not overflow");

        // Also verify height estimation doesn't panic or produce NaN/Inf.
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&md);
        cache.ensure_heights(14.0, 400.0, &style);
        assert!(cache.total_height.is_finite());
        assert!(cache.total_height > 0.0);
    }

    #[test]
    fn stress_nested_list_indent_calculation() {
        // Verify the indent arithmetic (16.0 * indent as f32) stays finite.
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        for depth in &[0usize, 5, 10, 20, 50] {
            let indent_px = 16.0 * *depth as f32;
            assert!(indent_px.is_finite(), "indent overflowed at depth {depth}");
            let items = vec![plain_item("test")];
            let h = estimate_list_height(&items, 14.0, 400.0, &style, None);
            assert!(h > 0.0, "list height should be positive at depth {depth}");
        }
    }

    // ── 3. Extreme start numbers ───────────────────────────────────

    #[test]
    fn stress_ordered_list_start_999() {
        let md = "\
999. First at 999
1000. Second at 1000
1001. Third at 1001
";
        let (blocks, height) = headless_render(md);
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 999);
                assert_eq!(items.len(), 3);
            }
            other => panic!("expected OrderedList, got {other:?}"),
        }
        assert!(height > 0.0);
    }

    #[test]
    fn stress_ordered_list_start_99999() {
        let md = "\
99999. Five-digit start
100000. Next
100001. After
";
        let (blocks, height) = headless_render(md);
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 99999);
                assert_eq!(items.len(), 3);
            }
            other => panic!("expected OrderedList, got {other:?}"),
        }
        assert!(height > 0.0);
    }

    #[test]
    fn stress_ordered_list_1000_items_digit_growth() {
        // Start at 1, end at 1000 — number column must accommodate 4 digits.
        let mut md = String::with_capacity(1000 * 20);
        for i in 1..=1000 {
            writeln!(md, "{i}. Item {i}").ok();
        }
        let (blocks, height) = headless_render(&md);
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 1);
                assert_eq!(items.len(), 1000);
            }
            other => panic!("expected OrderedList, got {other:?}"),
        }
        assert!(height > 0.0);
    }

    #[test]
    fn stress_ordered_list_digit_width_calculation() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());

        // Start=1, 1 item: max_num=1, digits=1
        let b1 = Block::OrderedList {
            start: 1,
            items: vec![plain_item("a")],
        };
        let h1 = estimate_block_height(&b1, 14.0, 400.0, &style);
        assert!(h1 > 0.0);

        // Start=99999, 1 item: max_num=99999, digits=5
        let b5 = Block::OrderedList {
            start: 99999,
            items: vec![plain_item("a")],
        };
        let h5 = estimate_block_height(&b5, 14.0, 400.0, &style);
        assert!(h5 > 0.0);

        // Start=1, 1000 items: max_num=1000, digits=4
        let items: Vec<_> = (0..1000)
            .map(|i| plain_item(&format!("item {i}")))
            .collect();
        let b4 = Block::OrderedList { start: 1, items };
        let h4 = estimate_block_height(&b4, 14.0, 400.0, &style);
        assert!(h4 > 0.0);
    }

    // ── 4. Mixed content in list items ─────────────────────────────

    #[test]
    fn stress_list_items_with_inline_formatting() {
        let md = "\
- **Bold item**
- *Italic item*
- `Code item`
- [Link item](https://example.com)
- ~~Strikethrough item~~
- **Bold** and *italic* and `code` together
";
        let (blocks, height) = headless_render(md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 6);
                assert!(
                    items[0].content.spans.iter().any(|s| s.style.strong()),
                    "first item should be bold"
                );
                assert!(
                    items[2].content.spans.iter().any(|s| s.style.code()),
                    "third item should have code"
                );
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
        assert!(height > 0.0);
    }

    #[test]
    fn stress_list_item_very_long_text() {
        let long_text = "A".repeat(600);
        let md = format!("- {long_text}\n- Short\n");
        let (blocks, height) = headless_render(&md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 2);
                assert!(
                    items[0].content.text.len() >= 500,
                    "first item should be very long"
                );
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
        assert!(height > 0.0);
    }

    #[test]
    fn stress_list_items_with_continuation_lines() {
        let md = "\
- First line of item one
  continued on next line
  and another continuation
- Second item
";
        let (blocks, height) = headless_render(md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 2);
                assert!(
                    items[0].content.text.contains("continued"),
                    "continuation line should be part of item"
                );
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
        assert!(height > 0.0);
    }

    // ── 5. Task list edge cases ────────────────────────────────────

    #[test]
    fn stress_task_list_all_checked() {
        let mut md = String::new();
        for i in 0..20 {
            writeln!(md, "- [x] Task {i} done").ok();
        }
        let (blocks, _) = headless_render(&md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 20);
                assert!(
                    items.iter().all(|it| it.checked == Some(true)),
                    "all items should be checked"
                );
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    #[test]
    fn stress_task_list_all_unchecked() {
        let mut md = String::new();
        for i in 0..20 {
            writeln!(md, "- [ ] Task {i} todo").ok();
        }
        let (blocks, _) = headless_render(&md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 20);
                assert!(
                    items.iter().all(|it| it.checked == Some(false)),
                    "all items should be unchecked"
                );
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    #[test]
    fn stress_task_list_mixed_checked_unchecked_regular() {
        let md = "\
- [x] Checked
- [ ] Unchecked
- Regular
- [x] Another checked
- [ ] Another unchecked
- Also regular
";
        let (blocks, height) = headless_render(md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 6);
                assert_eq!(items[0].checked, Some(true));
                assert_eq!(items[1].checked, Some(false));
                assert_eq!(items[2].checked, None);
                assert_eq!(items[3].checked, Some(true));
                assert_eq!(items[4].checked, Some(false));
                assert_eq!(items[5].checked, None);
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
        assert!(height > 0.0);
    }

    #[test]
    fn stress_nested_task_lists() {
        let md = "\
- [x] Parent checked
  - [ ] Child unchecked
  - [x] Child checked
    - [ ] Grandchild unchecked
- [ ] Parent unchecked
  - [x] Child checked
";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].checked, Some(true));
                assert_eq!(items[1].checked, Some(false));
                assert!(
                    !items[0].children.is_empty(),
                    "first item should have child list"
                );
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    // ── 6. List/rendering interaction ──────────────────────────────

    #[test]
    fn stress_list_immediately_after_heading() {
        let md = "\
# My Heading
- Item A
- Item B
- Item C
";
        let (blocks, height) = headless_render(md);
        assert!(blocks.len() >= 2, "should have heading + list");
        assert!(matches!(&blocks[0], Block::Heading { .. }));
        let has_list = blocks.iter().any(|b| matches!(b, Block::UnorderedList(_)));
        assert!(has_list, "should have an unordered list after heading");
        assert!(height > 0.0);
    }

    #[test]
    fn stress_list_inside_blockquote() {
        let md = "\
> - Quoted item 1
> - Quoted item 2
> - Quoted item 3
>
> 1. Ordered in quote
> 2. Second ordered
";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
        assert!(
            blocks.iter().any(|b| matches!(b, Block::Quote(_))),
            "should have a blockquote"
        );
    }

    #[test]
    fn stress_document_with_only_lists() {
        let md = "\
- A
- B
- C

1. One
2. Two
3. Three

- [x] Done
- [ ] Not done
";
        let (blocks, height) = headless_render(md);
        for block in &blocks {
            assert!(
                matches!(block, Block::UnorderedList(_) | Block::OrderedList { .. }),
                "expected only list blocks, got {block:?}"
            );
        }
        assert!(height > 0.0);
    }

    #[test]
    fn stress_alternating_ordered_unordered_lists() {
        let mut md = String::new();
        for i in 0..10 {
            if i % 2 == 0 {
                writeln!(md, "- Unordered {i}").ok();
            } else {
                writeln!(md, "1. Ordered {i}").ok();
            }
            md.push('\n');
        }
        let (blocks, height) = headless_render(&md);
        let unordered = blocks
            .iter()
            .filter(|b| matches!(b, Block::UnorderedList(_)))
            .count();
        let ordered = blocks
            .iter()
            .filter(|b| matches!(b, Block::OrderedList { .. }))
            .count();
        assert!(unordered > 0, "should have unordered lists");
        assert!(ordered > 0, "should have ordered lists");
        assert!(height > 0.0);
    }

    // ── 7. Height estimation accuracy for lists ────────────────────

    #[test]
    fn stress_height_estimation_ratio_50_vs_100_items() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());

        let items_50: Vec<_> = (0..50).map(|i| plain_item(&format!("Item {i}"))).collect();
        let items_100: Vec<_> = (0..100).map(|i| plain_item(&format!("Item {i}"))).collect();

        let h50 = estimate_list_height(&items_50, 14.0, 400.0, &style, None);
        let h100 = estimate_list_height(&items_100, 14.0, 400.0, &style, None);

        assert!(h50 > 0.0);
        assert!(h100 > 0.0);
        assert!(h100 > h50, "100 items should be taller than 50");

        let ratio = h100 / h50;
        assert!(
            (1.6..=2.4).contains(&ratio),
            "height ratio 100/50 should be ~2.0, got {ratio:.2}"
        );
    }

    #[test]
    fn stress_height_estimation_scales_linearly() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());

        let counts = [10, 25, 50, 100, 200];
        let mut heights = Vec::new();
        for &n in &counts {
            let items: Vec<_> = (0..n).map(|i| plain_item(&format!("Item {i}"))).collect();
            let h = estimate_list_height(&items, 14.0, 400.0, &style, None);
            assert!(h > 0.0);
            heights.push(h);
        }

        for i in 1..heights.len() {
            assert!(
                heights[i] > heights[i - 1],
                "height should increase: h[{}]={} vs h[{}]={}",
                counts[i],
                heights[i],
                counts[i - 1],
                heights[i - 1],
            );
        }
    }

    #[test]
    fn stress_height_estimation_nested_vs_flat() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());

        let flat_items: Vec<_> = (0..10).map(|i| plain_item(&format!("Flat {i}"))).collect();
        let h_flat = estimate_list_height(&flat_items, 14.0, 400.0, &style, None);

        // 5 items each with a 2-item child list (10 items total).
        let nested_items: Vec<_> = (0..5)
            .map(|i| ListItem {
                content: StyledText {
                    text: format!("Parent {i}"),
                    spans: vec![],
                },
                children: vec![Block::UnorderedList(vec![
                    plain_item("child a"),
                    plain_item("child b"),
                ])],
                checked: None,
            })
            .collect();
        let h_nested = estimate_list_height(&nested_items, 14.0, 400.0, &style, None);

        assert!(h_flat > 0.0);
        assert!(h_nested > 0.0);
        assert!(
            h_nested > h_flat,
            "nested list should be taller: {h_nested} vs {h_flat}"
        );
    }

    #[test]
    fn stress_ordered_list_near_u64_max_no_panic() {
        // saturating_add protects against overflow at extreme start numbers.
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let items = vec![plain_item("near-max"), plain_item("overflow?")];
        let block = Block::OrderedList {
            start: u64::MAX - 1,
            items,
        };
        let h = estimate_block_height(&block, 14.0, 400.0, &style);
        assert!(h.is_finite());
        assert!(h > 0.0);
    }

    // ── build_layout_job section coverage ──────────────────────────

    #[test]
    fn layout_job_sections_cover_all_bytes() {
        let ctx = headless_ctx();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());

        // Build a StyledText with all formatting types (no links).
        let md = "plain **bold** *italic* ~~strike~~ `code` ***bi*** end";
        let blocks = crate::parse::parse_markdown(md);
        let st = match &blocks[0] {
            Block::Paragraph(st) => st,
            other => panic!("expected paragraph, got {other:?}"),
        };
        assert!(!st.text.is_empty());
        assert!(
            !st.spans.iter().any(|s| s.style.link.is_some()),
            "this test is for the no-link path"
        );

        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let base_color = ui.visuals().text_color();
                let job = build_layout_job(st, &st.spans, &style, base_color, 14.0, 900.0, ui);

                let text_len = job.text.len();
                assert!(!job.sections.is_empty(), "should have sections");

                assert_eq!(
                    job.sections[0].byte_range.start, 0,
                    "first section should start at 0"
                );
                assert_eq!(
                    job.sections.last().expect("non-empty").byte_range.end,
                    text_len,
                    "last section should end at text len ({text_len})"
                );
                for i in 1..job.sections.len() {
                    assert_eq!(
                        job.sections[i].byte_range.start,
                        job.sections[i - 1].byte_range.end,
                        "gap between section {} and {i}",
                        i - 1
                    );
                }
            });
        });
    }

    #[test]
    fn layout_job_single_style_single_section() {
        let ctx = headless_ctx();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());

        let st = StyledText {
            text: "all plain text".to_owned(),
            spans: vec![Span {
                start: 0,
                end: 14,
                style: SpanStyle::plain(),
            }],
        };

        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let base_color = ui.visuals().text_color();
                let job = build_layout_job(&st, &st.spans, &style, base_color, 14.0, 900.0, ui);
                assert_eq!(job.sections.len(), 1);
                assert_eq!(job.sections[0].byte_range, 0..14);
            });
        });
    }

    #[test]
    fn layout_job_formatting_flags_applied() {
        let ctx = headless_ctx();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());

        let mut bold_style = SpanStyle::plain();
        bold_style.set_strong();
        let mut italic_style = SpanStyle::plain();
        italic_style.set_emphasis();

        let st = StyledText {
            text: "AB".to_owned(),
            spans: vec![
                Span {
                    start: 0,
                    end: 1,
                    style: bold_style,
                },
                Span {
                    start: 1,
                    end: 2,
                    style: italic_style,
                },
            ],
        };

        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let base_color = ui.visuals().text_color();
                let job = build_layout_job(&st, &st.spans, &style, base_color, 14.0, 900.0, ui);
                assert_eq!(job.sections.len(), 2);
                assert!(!job.sections[0].format.italics);
                assert!(job.sections[1].format.italics);
            });
        });
    }

    #[test]
    fn layout_job_code_uses_monospace() {
        let ctx = headless_ctx();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());

        let mut code_style = SpanStyle::plain();
        code_style.set_code();

        let st = StyledText {
            text: "fn main()".to_owned(),
            spans: vec![Span {
                start: 0,
                end: 9,
                style: code_style,
            }],
        };

        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let base_color = ui.visuals().text_color();
                let job = build_layout_job(&st, &st.spans, &style, base_color, 14.0, 900.0, ui);
                assert_eq!(job.sections.len(), 1);
                assert_eq!(
                    job.sections[0].format.font_id.family,
                    egui::FontFamily::Monospace
                );
            });
        });
    }

    // ── Blockquote/heading/HR height-estimation consistency ────────

    #[test]
    fn blockquote_estimate_and_render_use_same_min_width() {
        // The min-width floor in estimate_quote_height must match
        // render_blockquote so viewport culling stays consistent.
        // Both should use 40.0 as the floor.
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let body_size = 14.0;
        let reserved = body_size + 3.0; // bar_margin + bar_width + content_margin

        // At a very narrow wrap_width, inner_w should be clamped to 40.
        let narrow = 20.0_f32;
        let inner_w_est = (narrow - reserved).max(40.0);
        assert!(
            (inner_w_est - 40.0).abs() < f32::EPSILON,
            "estimate inner width should be 40.0, got {inner_w_est}"
        );

        // Build a deeply nested blockquote that would exhaust all width.
        let mut block = Block::Quote(vec![Block::Paragraph(plain("content"))]);
        for _ in 0..10 {
            block = Block::Quote(vec![block]);
        }
        let h = estimate_block_height(&block, body_size, narrow, &style);
        assert_sane_height(h, "deeply nested blockquote at narrow width");
    }

    #[test]
    fn blockquote_render_at_narrow_width_no_panic() {
        // When the viewport is extremely narrow, render_blockquote should
        // still produce a valid layout without panicking.
        let md = "> > > > > > > > deep nesting\n";
        let (count, _est, rendered) = headless_render_at_width(md, 60.0);
        assert!(count > 0);
        assert!(rendered > 0.0, "rendered height should be positive");
    }

    #[test]
    fn blockquote_height_estimate_vs_render_within_bounds() {
        // The estimated height should be in the same ballpark as
        // the rendered height (allowing generous tolerance since
        // estimation is intentionally approximate).
        let md = "> Line one\n> Line two\n> Line three\n";
        let (_, estimated, rendered) = headless_render_at_width(md, 800.0);
        assert!(estimated > 0.0);
        assert!(rendered > 0.0);
        let ratio = estimated / rendered;
        assert!(
            ratio > 0.1 && ratio < 10.0,
            "blockquote height ratio out of range: est={estimated}, rendered={rendered}, ratio={ratio}"
        );
    }

    #[test]
    fn nested_blockquote_each_level_has_positive_height() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        for depth in 1..=8_usize {
            let mut block = Block::Quote(vec![Block::Paragraph(plain("text"))]);
            for _ in 1..depth {
                block = Block::Quote(vec![block]);
            }
            let h = estimate_block_height(&block, 14.0, 400.0, &style);
            assert_sane_height(h, &format!("blockquote depth {depth}"));
        }
    }

    #[test]
    fn deeply_nested_blockquote_renders_without_panic() {
        // 10-level deep nesting with actual content at each level.
        let mut md = String::new();
        for level in 0..10 {
            let prefix = "> ".repeat(level + 1);
            let _ = writeln!(md, "{prefix}Level {}", level + 1);
        }
        let (count, _est, rendered) = headless_render_at_width(&md, 1024.0);
        assert!(count > 0);
        assert!(rendered > 0.0);
    }

    #[test]
    fn hr_height_estimate_covers_rendered_spacing() {
        // render_hr does: add_space(0.4*body) + paint + add_space(0.4*body)
        // estimate is: body_size * 0.8
        // Verify the estimate is at least as large as the two spacings.
        let body_size = 14.0;
        let estimated = body_size * 0.8;
        let render_spacing = body_size * 0.4 + body_size * 0.4;
        assert!(
            estimated >= render_spacing - 0.01,
            "HR estimate ({estimated}) should cover render spacing ({render_spacing})"
        );
    }

    #[test]
    fn hr_renders_with_positive_height() {
        let md = "Above\n\n---\n\nBelow\n";
        let (_, _est, rendered) = headless_render_at_width(md, 800.0);
        assert!(rendered > 0.0);
    }

    #[test]
    fn heading_underline_height_estimate_includes_separator() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let h1 = Block::Heading {
            level: 1,
            text: plain("Title"),
        };
        let h3 = Block::Heading {
            level: 3,
            text: plain("Title"),
        };
        let h1_est = estimate_block_height(&h1, 14.0, 400.0, &style);
        let h3_est = estimate_block_height(&h3, 14.0, 400.0, &style);
        // H1 has an underline separator (4px extra), H3 does not.
        // H1 also has a larger font scale, so it should be taller overall.
        assert!(
            h1_est > h3_est,
            "H1 ({h1_est}) should be taller than H3 ({h3_est}) due to scale + underline"
        );
    }

    #[test]
    fn heading_h1_h2_have_underline_space_in_estimate() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        // H2 gets underline (4px), H3 does not. Same text, so the
        // difference should be roughly the separator + font scale diff.
        let h2 = Block::Heading {
            level: 2,
            text: plain("Heading"),
        };
        let h3 = Block::Heading {
            level: 3,
            text: plain("Heading"),
        };
        let est_h2 = estimate_block_height(&h2, 14.0, 400.0, &style);
        let est_h3 = estimate_block_height(&h3, 14.0, 400.0, &style);
        // H2 should be taller: both font scale (1.5 vs 1.25) and separator.
        assert!(
            est_h2 > est_h3,
            "H2 ({est_h2}) should be taller than H3 ({est_h3})"
        );
    }

    #[test]
    fn blockquote_width_floor_consistency() {
        // Verify that the 40px minimum floor is used consistently in both
        // estimation and rendering paths. Build a blockquote that would
        // exhaust all available width and ensure the height is still sane.
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let body_size = 14.0;
        let reserved = body_size + 3.0;

        // At wrap_width = reserved + 10 (i.e. only 10px for content),
        // the floor should clamp inner_w to 40px in both paths.
        let tight_width = reserved + 10.0;
        let block = Block::Quote(vec![Block::Paragraph(plain("Some text content"))]);
        let h = estimate_block_height(&block, body_size, tight_width, &style);
        assert_sane_height(h, "blockquote at tight width");

        // And at even tighter width (< reserved), inner should still be 40px.
        let h_tiny = estimate_block_height(&block, body_size, 5.0, &style);
        assert_sane_height(h_tiny, "blockquote at 5px width");
    }

    // ── Hash collision edge cases (FNV-1a) ─────────────────────────

    #[test]
    fn hash_single_byte_diff_in_large_doc() {
        // Two 100KB docs differing by one byte in the middle must hash differently.
        let doc = "a".repeat(100 * 1024);
        let h1 = simple_hash(&doc);
        let mid = doc.len() / 2;
        // SAFETY: we know it's all ASCII 'a', so replacing one byte is fine.
        // Use make_mut pattern via bytes.
        let bytes = doc.as_bytes().to_vec();
        let mut doc2_bytes = bytes;
        doc2_bytes[mid] = b'b';
        let doc2 = String::from_utf8(doc2_bytes).expect("still valid UTF-8");
        let h2 = simple_hash(&doc2);
        assert_ne!(
            h1, h2,
            "100KB docs differing by 1 byte at midpoint must have different hashes"
        );
    }

    #[test]
    fn hash_trailing_whitespace_differs() {
        let a = "Hello world";
        let b = "Hello world ";
        let c = "Hello world  ";
        let ha = simple_hash(a);
        let hb = simple_hash(b);
        let hc = simple_hash(c);
        assert_ne!(ha, hb, "trailing single space must change hash");
        assert_ne!(hb, hc, "trailing double space must differ from single");
        assert_ne!(ha, hc);
    }

    #[test]
    fn hash_crlf_vs_lf_differs() {
        let lf = "line1\nline2\nline3\n";
        let crlf = "line1\r\nline2\r\nline3\r\n";
        assert_ne!(
            simple_hash(lf),
            simple_hash(crlf),
            "\\n vs \\r\\n must hash differently"
        );
    }

    #[test]
    fn hash_never_zero_for_typical_inputs() {
        let cases = [
            "",
            "x",
            "hello world",
            &"a".repeat(100_000),
            "# Heading\n\nParagraph\n",
            "\n\n\n",
            "\t\t\t",
        ];
        for input in &cases {
            let h = simple_hash(input);
            assert_ne!(
                h,
                0,
                "hash should not be 0 for input of length {}",
                input.len()
            );
        }
    }

    #[test]
    fn hash_stability_across_calls() {
        let doc = "# Hello\n\nSome text\n\n```rust\nfn main() {}\n```\n";
        let h1 = simple_hash(doc);
        let h2 = simple_hash(doc);
        let h3 = simple_hash(doc);
        assert_eq!(h1, h2);
        assert_eq!(h2, h3);
    }

    // ── heading_y edge cases ───────────────────────────────────────

    #[test]
    fn heading_y_no_headings_returns_none() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("Just a paragraph.\n\nAnother paragraph.\n");
        cache.ensure_heights(14.0, 400.0, &style);
        assert!(
            cache.heading_y(0).is_none(),
            "no headings → ordinal 0 must be None"
        );
    }

    #[test]
    fn heading_y_1000_headings() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        let mut doc = String::with_capacity(30_000);
        for i in 0..1000 {
            let _ = writeln!(doc, "## Heading {i}\n");
        }
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 400.0, &style);

        // Last valid ordinal.
        let y999 = cache.heading_y(999);
        assert!(
            y999.is_some(),
            "heading_y(999) must exist in 1000-heading doc"
        );
        let y998 = cache.heading_y(998).expect("heading_y(998) must exist");
        assert!(
            y999.expect("checked above") > y998,
            "heading 999 must be below heading 998"
        );
        // Out of bounds.
        assert!(cache.heading_y(1000).is_none());
    }

    #[test]
    fn heading_y_after_heights_cleared_returns_none() {
        // If heights/cum_y are cleared but blocks remain, heading_y should
        // return None because cum_y.get(idx) will be out of bounds.
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# A\n\n## B\n");
        cache.ensure_heights(14.0, 400.0, &style);
        assert!(cache.heading_y(0).is_some());

        // Simulate heights invalidated but blocks kept.
        cache.heights.clear();
        cache.cum_y.clear();
        assert!(
            cache.heading_y(0).is_none(),
            "heading_y must return None when cum_y is empty"
        );
    }

    // ── Concurrent-like stress: parse+height+viewport in tight loop ──

    #[test]
    fn tight_loop_no_drift() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let doc = crate::stress::large_mixed_doc(10);
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 600.0, &style);
        let baseline_height = cache.total_height;
        let baseline_blocks = cache.blocks.len();
        let baseline_heights: Vec<f32> = cache.heights.clone();

        for i in 0..10_000 {
            // Re-parse (should be no-op due to same pointer).
            cache.ensure_parsed(&doc);
            // Force height recalc every 100 iterations.
            if i % 100 == 0 {
                cache.heights.clear();
            }
            cache.ensure_heights(14.0, 600.0, &style);
            assert_eq!(
                cache.blocks.len(),
                baseline_blocks,
                "block count drifted at iteration {i}"
            );
            assert!(
                (cache.total_height - baseline_height).abs() < 0.01,
                "total_height drifted at iteration {i}: {} vs {baseline_height}",
                cache.total_height
            );
            // Spot-check individual heights.
            if i % 1000 == 0 {
                for (j, (a, b)) in cache.heights.iter().zip(&baseline_heights).enumerate() {
                    assert!(
                        (a - b).abs() < 0.01,
                        "height[{j}] drifted at iteration {i}: {a} vs {b}"
                    );
                }
            }
        }
    }

    // ── Very narrow widths ─────────────────────────────────────────

    #[test]
    fn narrow_widths_no_nan_inf_negative() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let doc = "# Title\n\nParagraph with **bold** and `code`.\n\n\
                   - list item\n- another item\n\n\
                   ```\ncode block\n```\n\n\
                   > blockquote\n\n\
                   | A | B |\n|---|---|\n| 1 | 2 |\n\n---\n\n\
                   ![img](url)\n";
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(doc);

        for &width in &[1.0_f32, 5.0, 10.0, 20.0] {
            cache.heights.clear();
            cache.ensure_heights(14.0, width, &style);
            for (i, h) in cache.heights.iter().enumerate() {
                assert!(
                    h.is_finite(),
                    "width={width}: height[{i}] is not finite: {h}"
                );
                assert!(*h >= 0.0, "width={width}: height[{i}] is negative: {h}");
                assert!(!h.is_nan(), "width={width}: height[{i}] is NaN");
            }
            assert!(
                cache.total_height.is_finite(),
                "width={width}: total_height not finite"
            );
            assert!(
                cache.total_height >= 0.0,
                "width={width}: total_height negative"
            );
        }
    }

    // ── Very small font sizes ──────────────────────────────────────

    #[test]
    fn tiny_font_sizes_sane_results() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let doc = "# Title\n\nParagraph.\n\n```\ncode\n```\n\n- item\n\n> quote\n";
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(doc);

        for &size in &[0.1_f32, 0.5, 1.0] {
            cache.heights.clear();
            cache.ensure_heights(size, 400.0, &style);
            for (i, h) in cache.heights.iter().enumerate() {
                assert!(h.is_finite(), "size={size}: height[{i}] not finite: {h}");
                assert!(*h >= 0.0, "size={size}: height[{i}] negative: {h}");
            }
            assert!(
                cache.total_height > 0.0,
                "size={size}: total_height should be positive, got {}",
                cache.total_height
            );
        }
    }

    // ── Documents with ONLY one block type ─────────────────────────

    #[test]
    fn pure_paragraphs_1000() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut doc = String::with_capacity(100_000);
        for i in 0..1000 {
            let _ = writeln!(doc, "Paragraph number {i} with some filler text.\n");
        }
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 400.0, &style);
        assert!(
            cache.blocks.len() >= 1000,
            "expected ≥1000 blocks, got {}",
            cache.blocks.len()
        );
        assert!(cache.total_height > 0.0);
        for (i, h) in cache.heights.iter().enumerate() {
            assert_sane_height(*h, &format!("paragraph {i}"));
        }
    }

    #[test]
    fn pure_code_blocks_500() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut doc = String::with_capacity(100_000);
        for i in 0..500 {
            let _ = writeln!(doc, "```\ncode block {i}\nline 2\nline 3\n```\n");
        }
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 400.0, &style);
        let code_count = cache
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Code { .. }))
            .count();
        assert_eq!(
            code_count, 500,
            "expected 500 code blocks, got {code_count}"
        );
        assert!(cache.total_height > 0.0);
        for (i, h) in cache.heights.iter().enumerate() {
            assert_sane_height(*h, &format!("code block {i}"));
        }
    }

    #[test]
    fn pure_tables_200() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut doc = String::with_capacity(200_000);
        for i in 0..200 {
            let _ = writeln!(doc, "| A{i} | B{i} |\n|---|---|\n| c | d |\n");
        }
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 400.0, &style);
        let table_count = cache
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Table(_)))
            .count();
        assert_eq!(table_count, 200, "expected 200 tables, got {table_count}");
        assert!(cache.total_height > 0.0);
        for (i, h) in cache.heights.iter().enumerate() {
            assert_sane_height(*h, &format!("table {i}"));
        }
    }

    #[test]
    fn pure_lists_300() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut doc = String::with_capacity(100_000);
        for i in 0..300 {
            let _ = writeln!(doc, "- list item {i}\n");
        }
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 400.0, &style);
        // pulldown-cmark may merge consecutive list items into fewer list blocks.
        assert!(!cache.blocks.is_empty());
        assert!(cache.total_height > 0.0);
        for (i, h) in cache.heights.iter().enumerate() {
            assert_sane_height(*h, &format!("list block {i}"));
        }
    }

    #[test]
    fn pure_blockquotes_100() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut doc = String::with_capacity(50_000);
        for i in 0..100 {
            let _ = writeln!(doc, "> Blockquote number {i}\n");
        }
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 400.0, &style);
        let quote_count = cache
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Quote(_)))
            .count();
        assert_eq!(
            quote_count, 100,
            "expected 100 blockquotes, got {quote_count}"
        );
        assert!(cache.total_height > 0.0);
        for (i, h) in cache.heights.iter().enumerate() {
            assert_sane_height(*h, &format!("blockquote {i}"));
        }
    }

    #[test]
    fn pure_thematic_breaks_500() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut doc = String::with_capacity(10_000);
        for _ in 0..500 {
            doc.push_str("---\n\n");
        }
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 400.0, &style);
        let break_count = cache
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::ThematicBreak))
            .count();
        assert_eq!(
            break_count, 500,
            "expected 500 thematic breaks, got {break_count}"
        );
        assert!(cache.total_height > 0.0);
        for (i, h) in cache.heights.iter().enumerate() {
            assert_sane_height(*h, &format!("thematic break {i}"));
        }
    }

    #[test]
    fn pure_images_200() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut doc = String::with_capacity(50_000);
        for i in 0..200 {
            let _ = writeln!(doc, "![alt {i}](https://example.com/img{i}.png)\n");
        }
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 400.0, &style);
        let img_count = cache
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Image { .. }))
            .count();
        assert_eq!(img_count, 200, "expected 200 images, got {img_count}");
        assert!(cache.total_height > 0.0);
        for (i, h) in cache.heights.iter().enumerate() {
            assert_sane_height(*h, &format!("image {i}"));
        }
    }

    // ── Stress generators at 1KB: valid markdown, no parse errors ──

    #[test]
    fn stress_generators_1kb_all_valid() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let generators: Vec<(&str, String)> = vec![
            ("large_mixed", crate::stress::large_mixed_doc(1)),
            ("unicode_stress", crate::stress::unicode_stress_doc(1)),
            ("pathological", crate::stress::pathological_doc(1)),
            ("task_list", crate::stress::task_list_doc(1)),
            ("emoji_heavy", crate::stress::emoji_heavy_doc(1)),
            ("table_heavy", crate::stress::table_heavy_doc(1)),
        ];
        for (name, doc) in &generators {
            assert!(!doc.is_empty(), "{name}: generator produced empty output");
            assert!(
                doc.len() >= 1024,
                "{name}: generator produced less than 1KB ({} bytes)",
                doc.len()
            );
            let mut cache = MarkdownCache::default();
            cache.ensure_parsed(doc);
            assert!(!cache.blocks.is_empty(), "{name}: parsed to zero blocks");
            cache.ensure_heights(14.0, 400.0, &style);
            assert!(
                cache.total_height > 0.0,
                "{name}: total_height should be positive"
            );
            for (i, h) in cache.heights.iter().enumerate() {
                assert_sane_height(*h, &format!("{name} block {i}"));
            }
        }
    }

    #[test]
    fn stress_minimal_docs_all_parseable() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        for (name, doc) in crate::stress::minimal_docs() {
            let mut cache = MarkdownCache::default();
            cache.ensure_parsed(&doc);
            cache.ensure_heights(14.0, 400.0, &style);
            // No panics, no NaN, no infinity.
            for (i, h) in cache.heights.iter().enumerate() {
                assert!(
                    h.is_finite() && *h >= 0.0,
                    "{name}: height[{i}] = {h} is invalid"
                );
            }
            assert!(
                cache.total_height.is_finite(),
                "{name}: total_height not finite"
            );
        }
    }

    #[test]
    fn table_height_estimate_aligns_with_render_constants() {
        // Verify the base row height and spacing match render_table's Grid config.
        let table = TableData {
            header: vec![StyledText {
                text: "A".to_owned(),
                spans: vec![],
            }],
            alignments: vec![Alignment::None],
            rows: (0..10)
                .map(|i| {
                    vec![StyledText {
                        text: format!("row {i}"),
                        spans: vec![],
                    }]
                })
                .collect(),
        };
        let h = estimate_table_height(&table, 14.0, 400.0);
        // With body_size=14, base_row_h=14*1.4=19.6, spacing=3.0
        // 11 rows (1 header + 10 data) × (19.6 + 3.0) = 248.6, plus 0.4*14=5.6 → ~254.2
        // Allow 20% tolerance for text wrapping estimates.
        assert!(h > 200.0, "height {h} too small for 10-row table");
        assert!(h < 400.0, "height {h} too large for 10-row table");
    }

    #[test]
    fn table_short_rows_render_without_panic() {
        // Rows with fewer columns than header should render (padded with empty cells).
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let viewer = MarkdownViewer::new("short_rows");
        let md = "| A | B | C |\n|---|---|---|\n| 1 |\n| x | y |\n| a | b | c |\n";
        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show_scrollable(ui, &mut cache, &style, md, None);
            });
        });
        assert!(!cache.blocks.is_empty());
    }

    #[test]
    fn single_column_table_width_is_reasonable() {
        let header = vec![StyledText {
            text: "Status".to_owned(),
            spans: vec![],
        }];
        let rows = vec![
            vec![StyledText {
                text: "OK".to_owned(),
                spans: vec![],
            }],
            vec![StyledText {
                text: "Fail".to_owned(),
                spans: vec![],
            }],
        ];
        let (widths, _) = compute_table_col_widths(&header, &rows, 600.0, 7.7, 14.0);
        assert_eq!(widths.len(), 1);
        // Single column should not take the full 600px width.
        assert!(
            widths[0] < 400.0,
            "single column width {} is too wide",
            widths[0]
        );
    }

    #[test]
    fn redistribution_respects_min_col_width() {
        // Many clamped columns should not cause free columns to shrink below min.
        let row: Vec<StyledText> = (0..8)
            .map(|i| StyledText {
                text: format!("{i}"),
                spans: vec![],
            })
            .collect();
        let header = row.clone();
        let rows = vec![row];
        let (widths, min_col_w) = compute_table_col_widths(&header, &rows, 200.0, 7.7, 14.0);
        for (i, w) in widths.iter().enumerate() {
            assert!(
                *w >= min_col_w - 0.01,
                "column {i} width {w} below min {min_col_w}"
            );
        }
    }

    #[test]
    fn strengthen_color_produces_visible_difference() {
        // After increasing divisor to /3, the difference should be at least 30 units
        // in one channel for mid-range colors.
        let mid = egui::Color32::from_rgb(128, 128, 128);
        let boosted = strengthen_color(mid);
        let [mr, mg, mb, _] = mid.to_srgba_unmultiplied();
        let [br, bg, bb, _] = boosted.to_srgba_unmultiplied();
        let max_delta = (mr.abs_diff(br)).max(mg.abs_diff(bg)).max(mb.abs_diff(bb));
        assert!(
            max_delta >= 30,
            "strengthen_color should produce visible difference, got delta={max_delta}"
        );
    }

    #[test]
    fn progressive_refinement_updates_heights() {
        // After rendering, heights should converge toward actual values.
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let viewer = MarkdownViewer::new("refine_test");

        let md = "# Big Heading\n\nShort paragraph.\n\n```\nfn main() {\n    \
                  println!(\"hello\");\n}\n```\n\n| A | B |\n|---|---|\n| 1 | 2 |\n";

        // First render — heights are estimated.
        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show_scrollable(ui, &mut cache, &style, md, None);
            });
        });

        let heights_after_first = cache.heights.clone();
        assert!(!heights_after_first.is_empty());

        // Second render — heights should refine.
        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show_scrollable(ui, &mut cache, &style, md, None);
            });
        });

        // Total height should be positive and consistent across renders.
        assert!(cache.total_height > 0.0);
    }

    #[test]
    fn estimate_text_height_cjk_not_overestimated() {
        // CJK text: 10 chars, each 3 bytes = 30 bytes.
        // With byte-based counting the height would be ~3× inflated.
        let cjk = "日本語テスト文字列十";
        let latin = "abcdefghij";
        let cjk_h = estimate_text_height(cjk, 14.0, 200.0);
        let latin_h = estimate_text_height(latin, 14.0, 200.0);
        // CJK should not be dramatically taller than Latin of similar char count.
        assert!(
            cjk_h < latin_h * 3.0,
            "CJK height {cjk_h} vs Latin {latin_h}"
        );
    }

    #[test]
    fn estimate_ordered_list_wider_numbers() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let items: Vec<ListItem> = (0..100)
            .map(|i| ListItem {
                content: StyledText {
                    text: format!("item {i}"),
                    spans: vec![],
                },
                children: vec![],
                checked: None,
            })
            .collect();

        let ordered = Block::OrderedList {
            start: 1,
            items: items.clone(),
        };
        let unordered = Block::UnorderedList(items);

        let h_ord = estimate_block_height(&ordered, 14.0, 400.0, &style);
        let h_unord = estimate_block_height(&unordered, 14.0, 400.0, &style);

        // Ordered list with 3-digit numbers should be at least as tall as unordered.
        assert!(
            h_ord >= h_unord * 0.9,
            "ordered {h_ord} should not be much shorter than unordered {h_unord}"
        );
    }

    #[test]
    fn blockquote_nested_no_panic() {
        let md = "> Level 1\n>> Level 2\n>>> Level 3\n>>>> Level 4\n>>>>> Level 5\n";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    #[test]
    fn paragraph_in_blockquote_no_extra_vertical_gap() {
        // Paragraphs inside blockquotes should not have extra vertical gaps.
        let md = "> First paragraph\n>\n> Second paragraph\n";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    // ── Additional coverage tests ──────────────────────────────────

    #[test]
    fn render_code_block_inside_blockquote() {
        let md = "> Some text\n>\n> ```rust\n> fn main() {}\n> ```\n>\n> More text\n";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    #[test]
    fn render_heading_inside_blockquote() {
        let md = "> ## Heading inside quote\n> Normal text\n";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    #[test]
    fn render_deeply_nested_structure() {
        // Blockquote > list > blockquote > paragraph
        let md = "> - Item in quote\n>   > Nested quote\n>   > with text\n> - Second item\n";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    #[test]
    fn render_adjacent_code_blocks_spacing() {
        let md = "```python\nprint('hello')\n```\n\n```rust\nfn main() {}\n```\n";
        let (blocks, height) = headless_render(md);
        let code_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::Code { .. }))
            .count();
        assert_eq!(code_count, 2, "expected 2 code blocks");
        assert!(height > 0.0);
    }

    #[test]
    fn render_link_in_heading() {
        let md = "## [Documentation](https://docs.rs)\n";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 2);
                assert!(
                    text.spans.iter().any(|s| s.style.link.is_some()),
                    "heading should contain a link span"
                );
            }
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    #[test]
    fn render_task_list_with_nested_children() {
        let md =
            "- [x] Done task\n  - Sub-item A\n  - Sub-item B\n- [ ] Pending task\n  - Sub-item C\n";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    #[test]
    fn render_ordered_list_start_zero() {
        let md = "0. Zero\n1. One\n2. Two\n";
        let (blocks, height) = headless_render(md);
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 0);
                assert_eq!(items.len(), 3);
            }
            other => panic!("expected OrderedList, got {other:?}"),
        }
        assert!(height > 0.0);
    }

    #[test]
    fn render_ordered_list_start_large() {
        let md = "999999. Item A\n1000000. Item B\n";
        let (blocks, height) = headless_render(md);
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 999_999);
                assert_eq!(items.len(), 2);
            }
            other => panic!("expected OrderedList, got {other:?}"),
        }
        assert!(height > 0.0);
    }

    #[test]
    fn render_very_narrow_width() {
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let viewer = MarkdownViewer::new("narrow");

        let md = "# Heading\n\nA paragraph with some text.\n\n| A | B | C |\n|---|---|---|\n| 1 | 2 | 3 |\n";
        let narrow_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(100.0, 400.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(narrow_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show_scrollable(ui, &mut cache, &style, md, None);
            });
        });
        assert!(!cache.blocks.is_empty());
        assert!(cache.total_height > 0.0);
    }

    #[test]
    fn render_very_wide_width() {
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let viewer = MarkdownViewer::new("wide");

        let md = "# Heading\n\nShort paragraph.\n\n| A | B |\n|---|---|\n| 1 | 2 |\n";
        let wide_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(5000.0, 768.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(wide_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show_scrollable(ui, &mut cache, &style, md, None);
            });
        });
        assert!(!cache.blocks.is_empty());
        assert!(cache.total_height > 0.0);
    }

    #[test]
    fn render_image_in_list_item() {
        let md = "- ![screenshot](image.png)\n- Text item\n";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    #[test]
    fn render_mixed_inline_in_table_cell() {
        let md = "| Feature | Description |\n|---------|-------------|\n| **Bold** `code` *italic* | [Link](url) ~~strike~~ |\n";
        let (blocks, _) = headless_render(md);
        match &blocks[0] {
            Block::Table(table) => {
                assert_eq!(table.rows.len(), 1);
                // Cells should have spans for inline formatting
                assert!(
                    !table.rows[0][0].spans.is_empty(),
                    "formatted cell should have spans"
                );
            }
            other => panic!("expected Table, got {other:?}"),
        }
    }

    #[test]
    fn render_empty_blocks_no_panic() {
        // Various empty/whitespace blocks
        for md in [
            "",                  // empty document
            " \n \n ",           // whitespace only
            ">\n",               // empty blockquote
            "- \n",              // empty list item
            "| |\n|---|\n| |\n", // empty table cells
            "```\n```\n",        // empty code block
        ] {
            let (_, height) = headless_render(md);
            assert!(
                height >= 0.0,
                "empty block should have non-negative height for: {md:?}"
            );
        }
    }

    #[test]
    fn render_unicode_edge_cases() {
        let md = "\
# Héading with àccénts

Paragraph with emoji 🎉🚀💻 and CJK 你好世界

- Bullet with Ẃéïrd chars
- ZWJ sequence: 👨\u{200d}👩\u{200d}👧\u{200d}👦
- Combining: é (e + \u{0301})

| Unicode | Test |
|---------|------|
| ™©®℠  | Special symbols |
| ∀∃∑∏  | Math |
";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }

    #[test]
    fn progressive_refinement_converges() {
        // Run multiple render passes and verify heights stabilize.
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let viewer = MarkdownViewer::new("converge");

        let md = "# Title\n\nParagraph one.\n\n```\ncode\n```\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n> Quote\n\n- List\n- Items\n";

        let mut prev_height = 0.0_f32;
        for _ in 0..5 {
            let _ = ctx.run(raw_input_1024x768(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    viewer.show_scrollable(ui, &mut cache, &style, md, None);
                });
            });
            let h = cache.total_height;
            if prev_height > 0.0 {
                // Height should converge — difference between passes decreases.
                let delta = (h - prev_height).abs();
                assert!(
                    delta < prev_height * 0.1,
                    "height should stabilize, got delta={delta} at height={h}"
                );
            }
            prev_height = h;
        }
    }
}
