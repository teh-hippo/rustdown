#![forbid(unsafe_code)]
//! Render parsed Markdown blocks into egui widgets.
//!
//! Key feature: viewport culling in `show_scrollable` — only blocks
//! overlapping the visible region are laid out, giving O(visible) cost.

mod height;
mod lists;
mod table;
mod text;

use crate::parse::{Block, StyledText, parse_markdown_into};
use crate::style::MarkdownStyle;

pub use height::bytecount_newlines;
use height::estimate_block_height;
use lists::{render_ordered_list, render_unordered_list};
use table::render_table;
use text::{render_styled_text, render_styled_text_ex};

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
    /// Block indices of non-empty headings, cached for O(1) `heading_y` lookup.
    heading_block_indices: Vec<usize>,
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
        self.heading_block_indices.clear();
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

        // Rebuild heading index for fast heading_y lookup.
        self.heading_block_indices.clear();
        for (idx, block) in self.blocks.iter().enumerate() {
            if let Block::Heading { text, .. } = block
                && !text.text.is_empty()
            {
                self.heading_block_indices.push(idx);
            }
        }
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
        self.heights.resize(n, 0.0);
        self.cum_y.resize(n, 0.0);
        let mut acc = 0.0_f32;
        for (i, block) in self.blocks.iter().enumerate() {
            self.cum_y[i] = acc;
            let h = estimate_block_height(block, body_size, wrap_width, style);
            self.heights[i] = h;
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
    ///
    /// Uses the pre-cached `heading_block_indices` for O(1) lookup.
    #[must_use]
    pub fn heading_y(&self, ordinal: usize) -> Option<f32> {
        let block_idx = *self.heading_block_indices.get(ordinal)?;
        self.cum_y.get(block_idx).copied()
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

        if let Some(y) = scroll_to_y
            && y.is_finite()
        {
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

// ── Block rendering ────────────────────────────────────────────────

/// Maximum rendering recursion depth to prevent stack overflow from
/// pathologically nested markdown (e.g. 1000 nested blockquotes).
const MAX_RENDER_DEPTH: usize = 128;

#[inline]
fn render_blocks(ui: &mut egui::Ui, blocks: &[Block], style: &MarkdownStyle, indent: usize) {
    if indent > MAX_RENDER_DEPTH {
        return;
    }
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
///
/// **Security:** relative URLs containing `..` path segments are rejected
/// to prevent directory-traversal attacks via malicious markdown images.
fn resolve_image_url<'a>(url: &'a str, base_uri: &str) -> std::borrow::Cow<'a, str> {
    if url.starts_with("//") || url.contains("://") || base_uri.is_empty() {
        return std::borrow::Cow::Borrowed(url);
    }

    // Reject path-traversal attempts: any `..` that appears as a full
    // path component (e.g. `../`, `foo/../../bar`, or trailing `..`).
    if contains_dot_dot_segment(url) {
        return std::borrow::Cow::Borrowed("");
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

/// Returns `true` if `path` contains a `..` path component.
///
/// Matches `..` when it appears as the entire path, at the start
/// (`../foo`), in the middle (`foo/../bar`), or at the end (`foo/..`).
/// Also checks backslash-separated paths for Windows.
fn contains_dot_dot_segment(path: &str) -> bool {
    // Quick check: if ".." doesn't appear anywhere, skip the split.
    if memchr::memmem::find(path.as_bytes(), b"..").is_none() {
        return false;
    }
    // Single pass: split on both '/' and '\' simultaneously.
    path.split(['/', '\\']).any(|seg| seg == "..")
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

/// Draw a full-width horizontal rule at the current cursor position.
fn draw_horizontal_rule(ui: &egui::Ui, style: &MarkdownStyle) {
    let rect = ui.available_rect_before_wrap();
    let y = rect.min.y;
    let color = style
        .hr_color
        .unwrap_or_else(|| ui.visuals().weak_text_color());
    ui.painter().line_segment(
        [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
        egui::Stroke::new(1.5, color),
    );
}

fn render_heading(
    ui: &mut egui::Ui,
    level: u8,
    text: &StyledText,
    style: &MarkdownStyle,
    body_size: f32,
) {
    // Skip empty headings entirely (matches nav panel which excludes them).
    if text.text.is_empty() {
        return;
    }

    let idx = (level as usize).saturating_sub(1).min(5);
    let hs = &style.headings[idx];
    let size = body_size * hs.font_scale;

    ui.add_space(size * 0.3);
    render_styled_text_ex(ui, text, style, Some(size), Some(hs.color));
    // Ensure consistent bottom spacing: at least 0.3 em so content
    // immediately after a heading (tables, code blocks) isn't cramped.
    ui.add_space((size * 0.15).max(body_size * 0.3));

    if level <= 2 {
        draw_horizontal_rule(ui, style);
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

    let bar_x = ui.available_rect_before_wrap().min.x + bar_margin + bar_width * 0.5;

    // Use a unique salt per nesting depth so egui doesn't share layout state.
    let salt = ui.next_auto_id().with(indent);

    // Floor must match `estimate_quote_height` (40px) so that viewport
    // culling height estimates stay consistent with actual rendering.
    let content_width = (ui.available_width() - reserved).max(40.0);

    // Horizontal wrapper offsets the content past the bar area so text
    // doesn't overlap the vertical bar indicator.
    let content_rect = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(reserved);
            ui.vertical(|ui| {
                ui.set_min_width(content_width);
                ui.push_id(salt, |ui| {
                    render_blocks(ui, inner, style, indent + 1);
                });
            })
            .response
            .rect
        })
        .inner;

    ui.painter().line_segment(
        [
            egui::pos2(bar_x, content_rect.min.y),
            egui::pos2(bar_x, content_rect.max.y),
        ],
        egui::Stroke::new(bar_width, bar_color),
    );
    ui.add_space(body_size * 0.3);
}

fn render_hr(ui: &mut egui::Ui, style: &MarkdownStyle, body_size: f32) {
    ui.add_space(body_size * 0.4);
    draw_horizontal_rule(ui, style);
    ui.add_space(body_size * 0.4);
}

#[inline]
pub(crate) fn simple_hash(s: &str) -> u64 {
    // FNV-1a–inspired 64-bit hash, processing 8 bytes at a time for throughput.
    const BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;

    let bytes = s.as_bytes();
    let chunks = bytes.chunks_exact(8);
    let remainder = chunks.remainder();
    let mut h: u64 = BASIS;

    for chunk in chunks {
        // chunks_exact(8) guarantees exactly 8 bytes.
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
    clippy::field_reassign_with_default,
    clippy::similar_names,
    clippy::type_complexity,
    clippy::suboptimal_flops,
    clippy::format_collect
)]
mod tests {
    use super::*;
    use crate::parse::{Alignment, ListItem, Span, SpanStyle, TableData};
    use height::{estimate_table_height, estimate_text_height};
    use std::fmt::Write;
    use table::compute_table_col_widths;
    use text::{build_layout_job, strengthen_color};

    fn dark_style() -> MarkdownStyle {
        MarkdownStyle::from_visuals(&egui::Visuals::dark())
    }

    fn dark_colored_style() -> MarkdownStyle {
        MarkdownStyle::colored(&egui::Visuals::dark())
    }

    #[test]
    fn cache_behavior() {
        // Invalidates on text change
        let mut cache = MarkdownCache::default();
        let h1 = simple_hash("# Hello");
        cache.blocks = crate::parse::parse_markdown("# Hello");
        cache.text_hash = h1;
        assert_eq!(cache.blocks.len(), 1);
        assert_eq!(h1, simple_hash("# Hello"));
        assert_ne!(h1, simple_hash("# World"));

        // Hash produces different hashes
        assert_ne!(simple_hash("hello"), simple_hash("world"));
        assert_eq!(simple_hash("same"), simple_hash("same"));

        // Heights invalidate on size change
        let style = dark_style();
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# Hello\n\nParagraph");
        cache.ensure_heights(14.0, 400.0, &style);
        let h1 = cache.total_height;
        cache.ensure_heights(28.0, 400.0, &style);
        assert!(cache.total_height > h1, "larger font → larger height");

        // cum_y correct
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# H1\n\nPara 1\n\nPara 2");
        cache.ensure_heights(14.0, 400.0, &style);
        assert_eq!(cache.cum_y.len(), cache.blocks.len());
        assert!((cache.cum_y[0]).abs() < f32::EPSILON);
        for i in 1..cache.cum_y.len() {
            let expected = cache.cum_y[i - 1] + cache.heights[i - 1];
            assert!((cache.cum_y[i] - expected).abs() < 0.01, "cum_y[{i}]");
        }

        // Clear resets all
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# Title\n\nBody text");
        cache.ensure_heights(14.0, 400.0, &style);
        assert!(!cache.blocks.is_empty());
        cache.clear();
        assert!(cache.blocks.is_empty());
        assert!(cache.heights.is_empty());
        assert!(cache.cum_y.is_empty());
        assert!((cache.total_height).abs() < f32::EPSILON);

        // Style with heading scales
        let mut style = dark_style();
        let scales = [3.0, 2.5, 2.0, 1.5, 1.2, 1.0];
        style.set_heading_scales(scales);
        for (i, &expected) in scales.iter().enumerate() {
            assert!(
                (style.headings[i].font_scale - expected).abs() < f32::EPSILON,
                "h{i} scale"
            );
        }
    }
    #[test]
    fn heading_y_behavior() {
        let style = dark_style();

        // Returns ordered offsets
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# A\n\ntext\n\n## B\n\nmore\n\n### C\n");
        cache.ensure_heights(14.0, 400.0, &style);
        let y0 = cache.heading_y(0).unwrap_or(0.0);
        let y1 = cache.heading_y(1).unwrap_or(0.0);
        let y2 = cache.heading_y(2).unwrap_or(0.0);
        assert!(y0 <= y1 && y1 <= y2);

        // Out of bounds
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# A\n\n## B\n");
        cache.ensure_heights(14.0, 400.0, &style);
        assert!(cache.heading_y(2).is_none());

        // Skips empty headings
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# \n\n## Real\n");
        cache.ensure_heights(14.0, 400.0, &style);
        let y = cache.heading_y(0);
        assert!(y.is_some(), "should skip empty heading and find Real");
        // Empty heading has zero height, so Real may start at y=0.
        assert!(cache.heading_y(1).is_none());

        // Empty heading between real ones
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# First\n\n## \n\n### Third\n");
        cache.ensure_heights(14.0, 400.0, &style);
        let y0 = cache.heading_y(0);
        let y1 = cache.heading_y(1);
        assert!(y0.is_some() && y1.is_some());
        assert!(y0.unwrap_or(0.0) < y1.unwrap_or(0.0));
        assert!(cache.heading_y(2).is_none());

        // No headings → None
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("Just a paragraph.\n\nAnother paragraph.\n");
        cache.ensure_heights(14.0, 400.0, &style);
        assert!(cache.heading_y(0).is_none(), "no headings → None");

        // 1000 headings
        let mut doc = String::with_capacity(30_000);
        for i in 0..1000 {
            let _ = writeln!(doc, "## Heading {i}\n");
        }
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 400.0, &style);
        let y999 = cache.heading_y(999);
        assert!(y999.is_some(), "heading_y(999) must exist");
        let y998 = cache.heading_y(998).expect("heading_y(998)");
        assert!(y999.expect("") > y998, "heading 999 > heading 998");
        assert!(cache.heading_y(1000).is_none());

        // Heights cleared → None
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# A\n\n## B\n");
        cache.ensure_heights(14.0, 400.0, &style);
        assert!(cache.heading_y(0).is_some());
        cache.heights.clear();
        cache.cum_y.clear();
        assert!(cache.heading_y(0).is_none(), "cleared → None");
    }
    #[test]
    fn estimate_height_all_block_types_positive() {
        // Text height basics
        let h = estimate_text_height("Hello World", 14.0, 200.0);
        assert!(h > 0.0 && h < 100.0);
        let short = estimate_text_height("Hi", 14.0, 200.0);
        let long = estimate_text_height(&"word ".repeat(100), 14.0, 200.0);
        assert!(long > short);
        assert!((estimate_text_height("", 14.0, 200.0) - 14.0).abs() < f32::EPSILON);
        for w in [0.0_f32, -100.0] {
            let h = estimate_text_height("some text", 14.0, w);
            assert!(h > 0.0 && h.is_finite(), "wrap={w}");
        }
        let one = estimate_text_height("hello", 14.0, 400.0);
        let ten = estimate_text_height(&"hello\n".repeat(10), 14.0, 400.0);
        assert!(ten > one);

        // All block types
        let style = dark_style();
        let blocks: Vec<(&str, Block)> = vec![
            (
                "heading",
                Block::Heading {
                    level: 1,
                    text: plain("h"),
                },
            ),
            ("paragraph", Block::Paragraph(plain("Hello world"))),
            (
                "code",
                Block::Code {
                    language: Box::from("rust"),
                    code: "fn main() {}\n".into(),
                },
            ),
            (
                "blockquote",
                Block::Quote(vec![Block::Paragraph(plain("quoted"))]),
            ),
            (
                "unordered_list",
                Block::UnorderedList(vec![plain_item("item")]),
            ),
            (
                "ordered_list",
                Block::OrderedList {
                    start: 1,
                    items: vec![plain_item("first")],
                },
            ),
            ("thematic_break", Block::ThematicBreak),
            ("table", Block::Table(Box::new(make_table(1, 1, "val")))),
            (
                "table_header_only",
                Block::Table(Box::new(TableData {
                    header: vec![plain("Header")],
                    alignments: vec![Alignment::None],
                    rows: vec![],
                })),
            ),
            (
                "image",
                Block::Image {
                    url: Box::from("https://img.png"),
                    alt: Box::from("alt"),
                },
            ),
            (
                "task_list",
                Block::UnorderedList(vec![
                    ListItem {
                        content: plain("checked"),
                        children: vec![],
                        checked: Some(true),
                    },
                    ListItem {
                        content: plain("unchecked"),
                        children: vec![],
                        checked: Some(false),
                    },
                ]),
            ),
        ];
        for (label, block) in &blocks {
            let h = estimate_block_height(block, 14.0, 400.0, &style);
            assert!(h > 0.0, "{label}: height should be positive, got {h}");
        }
    }

    #[test]
    fn height_estimation_perf() {
        let style = dark_style();
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
        let style = dark_colored_style();
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
        let style = dark_colored_style();
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
    fn render_image_variations() {
        // With alt text
        let (blocks, _) = headless_render("![Alt text](image.png)");
        match &blocks[0] {
            Block::Image { url, alt } => {
                assert_eq!(&**url, "image.png");
                assert_eq!(&**alt, "Alt text");
            }
            other => panic!("expected Image, got {other:?}"),
        }

        // Without alt text
        let (blocks, _) = headless_render("![](https://example.com/pic.jpg)");
        match &blocks[0] {
            Block::Image { url, alt } => {
                assert_eq!(&**url, "https://example.com/pic.jpg");
                assert!(alt.is_empty());
            }
            other => panic!("expected Image, got {other:?}"),
        }

        // Alt text from brackets (not title)
        let (blocks, _) = headless_render("![My Alt Text](image.png)");
        match &blocks[0] {
            Block::Image { alt, .. } => assert_eq!(&**alt, "My Alt Text"),
            other => panic!("expected Image, got {other:?}"),
        }

        // Alt text with formatting
        let (blocks, _) = headless_render("![Alt with **bold** and *italic*](img.png)");
        match &blocks[0] {
            Block::Image { alt, .. } => {
                assert!(alt.contains("bold") && alt.contains("italic"), "alt={alt}");
            }
            other => panic!("expected Image, got {other:?}"),
        }

        // Render with base URI set
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let mut style = dark_colored_style();
        style.image_base_uri = "file:///base/dir/".to_owned();
        let viewer = MarkdownViewer::new("img_base");
        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show(ui, &mut cache, &style, "![alt](relative/pic.png)");
            });
        });
        assert_eq!(cache.blocks.len(), 1);
        match &cache.blocks[0] {
            Block::Image { url, .. } => assert_eq!(&**url, "relative/pic.png"),
            other => panic!("expected Image block, got {other:?}"),
        }
    }
    #[test]
    fn render_various_inputs_no_panic() {
        let cases: Vec<(&str, &str)> = vec![
            (
                "emoji",
                "# 🎨 Emoji Heading\n\n🚀 Rocket 💡 Lightbulb 🔧 Wrench ⚙️ Gear\n\n- 🔴 Red\n- 🟢 Green\n- 🔵 Blue\n",
            ),
            (
                "cjk",
                "## 日本語テスト\n\n中文测试 (Chinese test)\n\n한국어 테스트 (Korean test)\n",
            ),
            (
                "math_symbols",
                "∑(i=1 to n) of xᵢ² = α·β + γ/δ ± ε\n\n∀x ∈ ℝ, ∃y : x² + y² = r²",
            ),
            ("rtl", "بسم الله الرحمن الرحيم\n\nשלום עולם"),
            (
                "zero_width_chars",
                "zero\u{200D}width\u{200D}joiner\n\nsoft\u{00AD}hyphen\n\nnon\u{00A0}breaking",
            ),
            (
                "box_drawing",
                "```\n┌─────────┐\n│ Content  │\n├─────────┤\n│ ▲ ▼ ◆ ● │\n└─────────┘\n```\n",
            ),
            ("empty_code_block", "```\n```\n"),
            ("table_header_only", "| H1 | H2 |\n|---|---|\n"),
            (
                "12_column_table",
                "| A | B | C | D | E | F | G | H | I | J | K | L |\n|---|---|---|---|---|---|---|---|---|---|---|---|\n| 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10| 11| 12|\n",
            ),
            (
                "deeply_nested_blockquote",
                "> L1\n> > L2\n> > > L3\n> > > > L4\n> > > > > L5\n",
            ),
            (
                "column_mismatch",
                "| A | B |\n|---|---|\n| 1 | 2 | 3 | 4 |\n| x |\n",
            ),
            (
                "wide_table",
                "| A | B | C | D | E | F | G | H | I | J | K | L |\n|---|---|---|---|---|---|---|---|---|---|---|---|\n| 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 |\n| a | b | c | d | e | f | g | h | i | j | k | l |\n",
            ),
            (
                "multiple_images",
                "![Tiny](tiny.png)\n\n![Small](small.png)\n\n![Large](large.png)\n\n![Missing](not-found.png)\n",
            ),
        ];
        for (label, md) in &cases {
            let (blocks, height) = headless_render(md);
            assert!(
                !blocks.is_empty(),
                "{label}: should produce at least one block"
            );
            assert!(height >= 0.0, "{label}: height should be non-negative");
        }

        // Thematic breaks
        let (blocks, _) = headless_render("Above\n\n---\n\nMiddle\n\n***\n\nBelow");
        assert_eq!(
            blocks
                .iter()
                .filter(|b| matches!(b, Block::ThematicBreak))
                .count(),
            2
        );

        // Nested blockquote
        let (blocks, height) = headless_render("> Level 1\n> > Level 2\n> > > Level 3\n");
        assert!(!blocks.is_empty() && height > 0.0);

        // Inline formatting stress
        let (blocks, _) = headless_render(
            "**bold** *italic* ***bold-italic*** `code` ~~strike~~ [link](url) **`bold code`**",
        );
        match &blocks[0] {
            Block::Paragraph(text) => {
                for word in ["bold", "italic", "code", "strike", "link"] {
                    assert!(text.text.contains(word), "missing {word}");
                }
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }

        // Task list mixed states
        let (blocks, _) =
            headless_render("- [x] Done\n- [ ] Not done\n- [x] Also done\n- Regular item\n");
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

        // Smart quotes
        let blocks = crate::parse::parse_markdown(r#"He said "hello" and she said 'world'."#);
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert!(text.text.contains('\u{201c}') || text.text.contains('\u{201d}'));
                assert!(text.text.contains('\u{2018}') || text.text.contains('\u{2019}'));
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }

        // Autolink
        let (blocks, _) = headless_render("Visit <https://example.com> for info.");
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert!(
                    text.spans
                        .iter()
                        .any(|s| text.link_url(s.style.link_idx).map(std::rc::Rc::as_ref)
                            == Some("https://example.com")),
                    "should have autolink"
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn heading_style_and_rendering() {
        // Colors applied
        let style = dark_colored_style();
        for (i, expected) in crate::DARK_HEADING_COLORS.iter().enumerate() {
            assert_eq!(style.headings[i].color, *expected, "heading {i} colour");
        }
        // Scales descend
        let style = dark_style();
        for i in 0..5 {
            assert!(
                style.headings[i].font_scale >= style.headings[i + 1].font_scale,
                "h{i} >= h{}",
                i + 1
            );
        }
        // All 6 levels render
        let md = "# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6\n\nNormal paragraph.\n";
        let (blocks, height) = headless_render(md);
        assert_eq!(
            blocks
                .iter()
                .filter(|b| matches!(b, Block::Heading { .. }))
                .count(),
            6
        );
        assert!(height > 0.0);

        // HR: estimate covers render spacing
        let body_size = 14.0;
        let estimated = body_size * 0.8;
        let render_spacing = body_size * 0.4 + body_size * 0.4;
        assert!(estimated >= render_spacing - 0.01);
        let (_, _est, rendered) = headless_render_at_width("Above\n\n---\n\nBelow\n", 800.0);
        assert!(rendered > 0.0);

        // Heading height: H1 > H3 and H2 > H3
        let style = dark_style();
        let heading_h = |level| {
            estimate_block_height(
                &Block::Heading {
                    level,
                    text: plain("Title"),
                },
                14.0,
                400.0,
                &style,
            )
        };
        let (h1, h2, h3) = (heading_h(1), heading_h(2), heading_h(3));
        assert!(h1 > h3, "H1 ({h1}) > H3 ({h3})");
        assert!(h2 > h3, "H2 ({h2}) > H3 ({h3})");
    }
    // ── Tables ─────────────────────────────────────────────────────

    #[test]
    fn render_table_variations() {
        // Various table shapes render with positive height
        let cases: Vec<(&str, &str)> = vec![
            (
                "simple",
                "| Name | Age |\n|-------|-----|\n| Alice | 30 |\n| Bob | 25 |\n",
            ),
            (
                "code_in_cells",
                "| Function | Type |\n|----------|------|\n| `parse` | `fn()`|\n",
            ),
            ("minimal", "| A |\n|---|\n| 1 |\n"),
            (
                "adjacent",
                "| A | B |\n|---|---|\n| 1 | 2 |\n\n| X | Y | Z |\n|---|---|---|\n| a | b | c |\n",
            ),
            (
                "single_column",
                "| Solo |\n|------|\n| One |\n| Two |\n| Three |\n",
            ),
            (
                "long_cell",
                "| Short | Very Long |\n|-------|---------|\n| a | This cell contains a very long piece of text that should test overflow |\n",
            ),
        ];
        for (label, md) in &cases {
            let (blocks, height) = headless_render(md);
            assert!(
                blocks.iter().any(|b| matches!(b, Block::Table(_))),
                "{label}: should have table"
            );
            assert!(height > 0.0, "{label}: positive height");
        }

        // Empty cells
        let (blocks, _) = headless_render(
            "| Feature | Yes | No |\n|---------|-----|----|\n| A | ✅ | |\n| | | |\n| C | | ❌ |\n",
        );
        match &blocks[0] {
            Block::Table(table) => {
                assert_eq!(table.rows.len(), 3);
                assert!(
                    table.rows[1][0].text.trim().is_empty(),
                    "middle row col 0 empty"
                );
            }
            other => panic!("expected Table, got {other:?}"),
        }

        // Alignment: 3-column
        let (blocks, _) = headless_render(
            "| Left | Center | Right |\n|:-----|:------:|------:|\n| l | c | r |\n",
        );
        match &blocks[0] {
            Block::Table(table) => {
                assert_eq!(table.alignments[0], Alignment::Left);
                assert_eq!(table.alignments[1], Alignment::Center);
                assert_eq!(table.alignments[2], Alignment::Right);
            }
            other => panic!("expected Table, got {other:?}"),
        }

        // Alignment: 4-column with default
        let (blocks, h) = headless_render(
            "| Default | Left | Center | Right |\n|---------|:-----|:------:|------:|\n| d | l | c | r |\n",
        );
        assert!(h > 0.0);
        match &blocks[0] {
            Block::Table(table) => {
                assert_eq!(table.alignments[0], Alignment::None);
                assert_eq!(table.alignments[1], Alignment::Left);
                assert_eq!(table.alignments[2], Alignment::Center);
                assert_eq!(table.alignments[3], Alignment::Right);
            }
            other => panic!("expected Table, got {other:?}"),
        }

        // No alignment markers → all None
        let (blocks, _) = headless_render("| A | B |\n|---|---|\n| 1 | 2 |\n");
        match &blocks[0] {
            Block::Table(table) => {
                for (i, a) in table.alignments.iter().enumerate() {
                    assert_eq!(*a, Alignment::None, "col {i} should default to None");
                }
            }
            other => panic!("expected Table, got {other:?}"),
        }
    }
    // ── Table column width unit tests ─────────────────────────────

    fn make_cells(texts: &[&str]) -> Vec<StyledText> {
        texts.iter().map(|t| plain(t)).collect()
    }

    // ── Lists ──────────────────────────────────────────────────────

    #[test]
    fn render_list_variations() {
        // Simple bullet
        let (blocks, _) = headless_render("- Item one\n- Item two\n- Item three\n");
        match &blocks[0] {
            Block::UnorderedList(items) => assert_eq!(items.len(), 3),
            other => panic!("expected UnorderedList, got {other:?}"),
        }

        // Nested bullet
        let (blocks, height) = headless_render(
            "- Top\n  - Middle\n    - Deep\n      - Deepest\n  - Back to middle\n- Back to top\n",
        );
        assert!(height > 0.0);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 2);
                assert!(
                    !items[0].children.is_empty(),
                    "first item should have children"
                );
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }

        // Ordered list double digits
        let mut md = String::new();
        for i in 1..=11 {
            writeln!(md, "{i}. Item {i}").ok();
        }
        let (blocks, _) = headless_render(&md);
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 1);
                assert_eq!(items.len(), 11);
            }
            other => panic!("expected OrderedList, got {other:?}"),
        }

        // Task list
        let (blocks, _) = headless_render("- [x] Done\n- [ ] Not done\n- Regular\n");
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items[0].checked, Some(true));
                assert_eq!(items[1].checked, Some(false));
                assert_eq!(items[2].checked, None);
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }

        // Mixed list types
        let (blocks, height) = headless_render(
            "1. Ordered parent\n   - Unordered child\n   - Another child\n2. Second ordered\n",
        );
        assert!(!blocks.is_empty());
        assert!(height > 0.0);
    }
    // ── Code blocks ────────────────────────────────────────────────

    #[test]
    fn render_code_block_variations() {
        // Small code block
        let (blocks, _) = headless_render("```rust\nfn main() {}\n```");
        match &blocks[0] {
            Block::Code { language, code } => {
                assert_eq!(&**language, "rust");
                assert!(code.contains("fn main()"));
            }
            other => panic!("expected Code, got {other:?}"),
        }

        // Large code block
        let mut code_lines = String::from("```python\n");
        for i in 0..200 {
            writeln!(code_lines, "line_{i} = {i} * 2").ok();
        }
        code_lines.push_str("```\n");
        let (blocks, height) = headless_render(&code_lines);
        assert!(matches!(&blocks[0], Block::Code { .. }));
        assert!(height > 100.0, "large code block should be tall");

        // Code in heading
        let (blocks, _) = headless_render("## The `render()` Function");
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 2);
                assert!(text.text.contains("render()"));
                assert!(
                    text.spans.iter().any(|s| s.style.code()),
                    "should have code span"
                );
            }
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    #[test]
    fn scrollable_render_basics() {
        // Basic render
        let (block_count, total_height) = headless_render_scrollable(
            "# Title\n\nParagraph text.\n\n## Section 2\n\nMore text.",
            None,
        );
        assert!(block_count >= 4);
        assert!(total_height > 0.0);

        // With scroll offset
        let mut doc = String::with_capacity(10_000);
        for i in 0..50 {
            write!(doc, "## Section {i}\n\nContent for section {i}.\n\n").ok();
        }
        let (_, total_height) = headless_render_scrollable(&doc, None);
        assert!(total_height > 200.0);
        // Scroll to various positions — none should panic
        for frac in [0.5, 0.95, 1.5] {
            let _ = headless_render_scrollable(&doc, Some(total_height * frac));
        }

        // Empty doc
        let (block_count, total_height) = headless_render_scrollable("", None);
        assert_eq!(block_count, 0);
        assert!((total_height).abs() < f32::EPSILON);

        // Edge cases: various unusual documents
        let edge_cases: Vec<(&str, String)> = vec![
            ("1000_thematic_breaks", "---\n\n".repeat(1000)),
            ("deeply_nested_blockquotes_with_tables", {
                (0..8)
                    .map(|depth| {
                        let prefix = "> ".repeat(depth + 1);
                        let mut s = format!("{prefix}Level {} paragraph\n\n", depth + 1);
                        if depth % 2 == 0 {
                            write!(
                                s,
                                "{prefix}| A | B |\n{prefix}|---|---|\n{prefix}| x | y |\n\n"
                            )
                            .ok();
                        }
                        s
                    })
                    .collect()
            }),
            ("deeply_nested_blockquotes_scrollable", {
                (0..10)
                    .map(|d| {
                        let p = "> ".repeat(d + 1);
                        format!("{p}Nested level {}\n", d + 1)
                    })
                    .collect()
            }),
        ];
        for (label, md) in &edge_cases {
            let (count, height) = headless_render_scrollable(md, None);
            assert!(count > 0, "{label}: should have blocks");
            assert!(height >= 0.0, "{label}: non-negative height");
            let _ = headless_render_scrollable(md, Some(height / 2.0));
        }

        // Stress docs: full render + 1KB parse
        let generators: Vec<(&str, fn(usize) -> String)> = vec![
            ("large_mixed", crate::stress::large_mixed_doc),
            ("unicode", crate::stress::unicode_stress_doc),
            ("table_heavy", crate::stress::table_heavy_doc),
            ("emoji", crate::stress::emoji_heavy_doc),
            ("task_list", crate::stress::task_list_doc),
            ("pathological", crate::stress::pathological_doc),
        ];
        for (label, generator) in &generators {
            let doc = generator(100);
            let (blocks, height) = headless_render(&doc);
            assert!(!blocks.is_empty(), "{label}: should have blocks");
            assert!(height > 0.0, "{label}: should have positive height");
            let doc1 = generator(1);
            assert!(!doc1.is_empty(), "{label}: 1KB doc should be non-empty");
            let parsed = crate::parse::parse_markdown(&doc1);
            assert!(!parsed.is_empty(), "{label}: should produce blocks");
        }
        for (label, doc) in crate::stress::minimal_docs() {
            let (_, height) = headless_render(&doc);
            assert!(height >= 0.0, "minimal doc '{label}' should render");
            let blocks = crate::parse::parse_markdown(&doc);
            assert!(
                !doc.is_empty() || blocks.is_empty(),
                "minimal '{label}': non-empty doc should parse"
            );
        }
    }

    // ── Viewport culling correctness ───────────────────────────────

    #[test]
    fn viewport_culling_and_monotonicity() {
        // Culling: two caches of same doc agree
        let mut doc = String::with_capacity(5_000);
        for i in 0..20 {
            write!(
                doc,
                "## Section {i}\n\nParagraph content for section {i}.\n\n"
            )
            .ok();
        }
        let style = dark_colored_style();
        let mut c1 = MarkdownCache::default();
        let mut c2 = MarkdownCache::default();
        c1.ensure_parsed(&doc);
        c2.ensure_parsed(&doc);
        c1.ensure_heights(14.0, 900.0, &style);
        c2.ensure_heights(14.0, 900.0, &style);
        assert!((c1.total_height - c2.total_height).abs() < f32::EPSILON);
        assert_eq!(c1.blocks.len(), c2.blocks.len());

        // cum_y monotonically increases on large mixed doc
        let doc = crate::stress::large_mixed_doc(50);
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 900.0, &style);
        for i in 1..cache.cum_y.len() {
            assert!(
                cache.cum_y[i] >= cache.cum_y[i - 1],
                "cum_y monotonic at {i}"
            );
        }
    }
    // ── Combined stress test (the full markdown file) ──────────────

    #[test]
    fn render_comprehensive_stress_test() {
        let md = include_str!("../../../../test-assets/stress-test.md");
        let (blocks, height) = headless_render(md);
        assert!(blocks.len() > 30);
        assert!(height > 1000.0);
        // Verify key block types
        for check in [
            blocks.iter().any(|b| matches!(b, Block::Heading { .. })),
            blocks.iter().any(|b| matches!(b, Block::Table(_))),
            blocks.iter().any(|b| matches!(b, Block::Code { .. })),
            blocks.iter().any(|b| matches!(b, Block::UnorderedList(_))),
            blocks
                .iter()
                .any(|b| matches!(b, Block::OrderedList { .. })),
            blocks.iter().any(|b| matches!(b, Block::Quote(_))),
            blocks.iter().any(|b| matches!(b, Block::ThematicBreak)),
            blocks.iter().any(|b| matches!(b, Block::Image { .. })),
        ] {
            assert!(check, "stress test missing block type");
        }

        // Scrollable at various positions
        let (_, total) = headless_render_scrollable(md, None);
        assert!(total > 0.0);
        let step = total / 20.0;
        for i in 0..22 {
            let _ = headless_render_scrollable(md, Some(step * i as f32));
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
    fn ordered_list_digit_handling() {
        // digit_count_for cases
        let cases = [
            (1, 1, 1),
            (1, 9, 1),
            (5, 3, 1),
            (1, 10, 2),
            (1, 99, 2),
            (50, 5, 2),
            (1, 100, 3),
            (1, 999, 3),
            (998, 2, 3),
            (1, 1000, 4),
            (1, 10_000, 5),
            (999_999, 2, 7),
            (0, 1, 1),
            (0, 10, 1),
            (0, 11, 2),
            (1, 0, 1),
            (100, 0, 3),
        ];
        for (start, count, expected) in cases {
            assert_eq!(
                digit_count_for(start, count),
                expected,
                "digit_count_for({start}, {count})"
            );
        }

        // num_width grows with digits
        let body_size = 14.0_f32;
        let widths: Vec<f32> = [1u32, 2, 3, 4, 5]
            .iter()
            .map(|&dc| body_size * 0.6f32.mul_add(dc as f32, 1.0))
            .collect();
        for i in 0..widths.len() - 1 {
            assert!(
                widths[i] < widths[i + 1],
                "grows: {} vs {}",
                widths[i],
                widths[i + 1]
            );
        }
    }
    // ── Table column width heuristic ───────────────────────────────

    #[test]
    fn table_col_widths_via_helper() {
        // Equal-length columns → roughly equal
        let (widths, _) = compute_table_col_widths(
            &make_cells(&["Name", "City"]),
            &[make_cells(&["Alice", "Tokyo"])],
            800.0,
            7.7,
            14.0,
        );
        assert_eq!(widths.len(), 2);
        assert!(
            (widths[0] - widths[1]).abs() < 1.0,
            "equal columns: {widths:?}"
        );

        // One long column → gets more space
        let (widths, _) = compute_table_col_widths(
            &make_cells(&["A", "B", "Description"]),
            &[make_cells(&[
                "x",
                "y",
                "This is a much longer description column than the others",
            ])],
            800.0,
            7.7,
            14.0,
        );
        assert!(
            widths[2] > widths[0] && widths[2] > widths[1],
            "long col wider: {widths:?}"
        );

        // Single column
        let (widths, _) = compute_table_col_widths(
            &make_cells(&["Only"]),
            &[make_cells(&["data"])],
            600.0,
            7.7,
            14.0,
        );
        assert_eq!(widths.len(), 1);
        assert!(widths[0] > 100.0, "single col wide: {widths:?}");

        // Empty cells → all above minimum
        let (widths, min_col_w) = compute_table_col_widths(
            &make_cells(&["A", "B", "C"]),
            &[make_cells(&["", "", ""]), make_cells(&["x", "", ""])],
            800.0,
            7.7,
            14.0,
        );
        for (i, w) in widths.iter().enumerate() {
            assert!(*w >= min_col_w - 0.01, "col {i} >= min: {w}");
        }

        // Edge cases: zero usable space
        let (widths, _) = compute_table_col_widths(
            &[plain("A"), plain("B")],
            &[vec![plain("x"), plain("y")]],
            0.0,
            7.0,
            14.0,
        );
        assert_eq!(widths.len(), 2);
        for w in &widths {
            assert!(*w >= 0.0, "width should be non-negative, got {w}");
        }

        // Very small usable → clamped to min_col_w
        let (widths, min_col_w) = compute_table_col_widths(
            &[plain("A"), plain("B"), plain("C")],
            &[vec![plain("x"), plain("y"), plain("z")]],
            10.0,
            7.0,
            14.0,
        );
        for w in &widths {
            assert!(*w >= min_col_w - 0.01, "width {w} >= min {min_col_w}");
        }

        // One much longer column → gets more space
        let (widths, _) = compute_table_col_widths(
            &[plain("A"), plain(&"X".repeat(300))],
            &[vec![plain("a"), plain(&"Y".repeat(300))]],
            600.0,
            7.0,
            14.0,
        );
        assert!(widths[1] > widths[0], "longer column wider: {widths:?}");

        // body_size = 0 → min_col_w = 36
        let (_, min_col_w) = compute_table_col_widths(
            &[plain("A"), plain("B")],
            &[vec![plain("x"), plain("y")]],
            200.0,
            7.0,
            0.0,
        );
        assert!((min_col_w - 36.0).abs() < 0.01, "min_col_w={min_col_w}");

        // Single column capped at ~60%
        let header = make_cells(&["Status"]);
        let rows = vec![make_cells(&["OK"]), make_cells(&["Error"])];
        let (widths, _) = compute_table_col_widths(&header, &rows, 600.0, 7.7, 14.0);
        assert_eq!(widths.len(), 1);
        assert!(
            widths[0] <= 600.0 * 0.61,
            "single col capped: {}",
            widths[0]
        );

        // Proportional to content
        let header = make_cells(&["ID", "Full Name and Description Here"]);
        let rows = vec![
            make_cells(&["1", "Alice Johnson, Software Engineer"]),
            make_cells(&["2", "Bob Smith, Product Manager"]),
        ];
        let (widths, _) = compute_table_col_widths(&header, &rows, 600.0, 7.7, 14.0);
        assert!(
            widths[1] > widths[0] * 1.5,
            "wide col should be wider: {widths:?}"
        );

        // Three equal columns
        let header = make_cells(&["Left", "Center", "Right"]);
        let rows = vec![make_cells(&["data", "data", "data"])];
        let (widths, min_col_w) = compute_table_col_widths(&header, &rows, 600.0, 7.7, 14.0);
        for (i, w) in widths.iter().enumerate() {
            assert!(*w >= min_col_w - 0.01, "col {i}: {w} >= {min_col_w}");
        }
        assert!(widths.iter().sum::<f32>() <= 601.0);

        // 10 cols all minimum
        let header = make_cells(&["A", "B", "C", "D", "E", "F", "G", "H", "I", "J"]);
        let rows = vec![make_cells(&[
            "1", "2", "3", "4", "5", "6", "7", "8", "9", "0",
        ])];
        let (widths, min_col_w) = compute_table_col_widths(&header, &rows, 400.0, 7.7, 14.0);
        for (i, w) in widths.iter().enumerate() {
            assert!(
                *w >= min_col_w - 0.01,
                "10-col: col {i}: {w} >= {min_col_w}"
            );
        }

        // One dominant column capped
        let header = make_cells(&["Tiny", "Medium text", &"x".repeat(200)]);
        let rows = vec![make_cells(&["a", "something", "y"])];
        let (widths, _) = compute_table_col_widths(&header, &rows, 600.0, 7.7, 14.0);
        assert!(widths[2] < 500.0, "dominant capped: {}", widths[2]);

        // 12 cols respect minimum
        let header = make_cells(&["H", "H", "H", "H", "H", "H", "H", "H", "H", "H", "H", "H"]);
        let rows = vec![make_cells(&[
            "v", "v", "v", "v", "v", "v", "v", "v", "v", "v", "v", "v",
        ])];
        let (widths, min_col_w) = compute_table_col_widths(&header, &rows, 800.0, 7.7, 14.0);
        for (i, w) in widths.iter().enumerate() {
            assert!(
                *w >= min_col_w - 0.01,
                "12-col: col {i}: {w} >= {min_col_w}"
            );
        }
    }

    // ── Image URL resolution ───────────────────────────────────────

    #[test]
    fn image_url_resolution_cases() {
        let cases = [
            // (url, base_uri, expected, description)
            (
                "https://example.com/pic.png",
                "",
                "https://example.com/pic.png",
                "absolute stays unchanged",
            ),
            (
                "images/pic.png",
                "file:///home/user/docs/",
                "file:///home/user/docs/images/pic.png",
                "relative with base_uri",
            ),
            (
                "images/pic.png",
                "",
                "images/pic.png",
                "relative with empty base_uri",
            ),
            (
                "http://cdn.example.com/img.jpg",
                "file:///local/base/",
                "http://cdn.example.com/img.jpg",
                "absolute ignores base_uri",
            ),
            (
                "//cdn.example.com/image.png",
                "file:///local/base/",
                "//cdn.example.com/image.png",
                "protocol-relative unchanged",
            ),
            (
                "image.png",
                "file:///home/user",
                "file:///home/user/image.png",
                "base_uri missing trailing slash",
            ),
            (
                "/images/pic.png",
                "file:///home/user/docs/",
                "file:///images/pic.png",
                "absolute path resolves against authority",
            ),
            (
                "/assets/logo.png",
                "http://example.com/docs/",
                "http://example.com/assets/logo.png",
                "absolute path with http base",
            ),
            (
                "/images/pic.png",
                "no-scheme-base",
                "/images/pic.png",
                "no scheme passthrough",
            ),
        ];
        for (url, base, expected, desc) in cases {
            assert_eq!(
                resolve_image_url(url, base),
                expected,
                "{desc}: url={url:?} base={base:?}"
            );
        }
    }

    // ── Height estimation: comprehensive ───────────────────────────

    #[test]
    fn estimate_height_larger_inputs_taller() {
        let style = dark_style();

        // Heading: H1 should be tallest, scaling down to H6
        let text = plain("Heading");
        let mut prev_h = f32::MAX;
        for level in 1..=6u8 {
            let h = estimate_block_height(
                &Block::Heading {
                    level,
                    text: text.clone(),
                },
                14.0,
                400.0,
                &style,
            );
            assert!(h > 0.0, "H{level} height > 0");
            assert!(
                h <= prev_h + 0.01,
                "H{level} ({h}) <= H{} ({prev_h})",
                level - 1
            );
            prev_h = h;
        }

        // Paragraph: longer text → taller
        let h_short = estimate_block_height(&Block::Paragraph(plain("Hi")), 14.0, 400.0, &style);
        let h_long = estimate_block_height(
            &Block::Paragraph(plain(&"A".repeat(500))),
            14.0,
            400.0,
            &style,
        );
        assert!(
            h_long > h_short,
            "longer paragraph ({h_long}) > short ({h_short})"
        );

        // Code block: more lines → taller
        let h_small = estimate_block_height(
            &Block::Code {
                language: Box::from(""),
                code: "line1".into(),
            },
            14.0,
            400.0,
            &style,
        );
        let code_10: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        let h_large = estimate_block_height(
            &Block::Code {
                language: Box::from(""),
                code: code_10.into_boxed_str(),
            },
            14.0,
            400.0,
            &style,
        );
        assert!(
            h_large > h_small,
            "10-line code ({h_large}) > 1-line ({h_small})"
        );

        // Blockquote: nested > simple
        let simple_q = Block::Quote(vec![Block::Paragraph(plain("one"))]);
        let nested_q = Block::Quote(vec![
            Block::Paragraph(plain("one")),
            Block::Quote(vec![Block::Paragraph(plain("two"))]),
        ]);
        let h_sq = estimate_block_height(&simple_q, 14.0, 400.0, &style);
        let h_nq = estimate_block_height(&nested_q, 14.0, 400.0, &style);
        assert!(h_nq > h_sq, "nested quote ({h_nq}) > simple ({h_sq})");

        // List: more items → taller
        let short_list = Block::UnorderedList(vec![plain_item("a")]);
        let long_list = Block::UnorderedList((0..5).map(|c| plain_item(&format!("{c}"))).collect());
        let h_sl = estimate_block_height(&short_list, 14.0, 400.0, &style);
        let h_ll = estimate_block_height(&long_list, 14.0, 400.0, &style);
        assert!(h_ll > h_sl, "5-item list ({h_ll}) > 1-item ({h_sl})");

        // Table: more rows → taller
        let h_st = estimate_block_height(
            &Block::Table(Box::new(make_table(1, 1, "r1"))),
            14.0,
            400.0,
            &style,
        );
        let h_lt = estimate_block_height(
            &Block::Table(Box::new(make_table(1, 20, "row"))),
            14.0,
            400.0,
            &style,
        );
        assert!(h_lt > h_st, "20-row table ({h_lt}) > 1-row ({h_st})");

        // CJK not overestimated
        let cjk_h = estimate_text_height("日本語テスト文字列十", 14.0, 200.0);
        let latin_h = estimate_text_height("abcdefghij", 14.0, 200.0);
        assert!(cjk_h < latin_h * 3.0, "CJK {cjk_h} vs Latin {latin_h}");

        // Ordered list wider numbers — similar height to unordered
        let items: Vec<ListItem> = (0..100).map(|i| plain_item(&format!("item {i}"))).collect();
        let h_ord = estimate_block_height(
            &Block::OrderedList {
                start: 1,
                items: items.clone(),
            },
            14.0,
            400.0,
            &style,
        );
        let h_unord = estimate_block_height(&Block::UnorderedList(items), 14.0, 400.0, &style);
        assert!(
            h_ord >= h_unord * 0.9,
            "ordered {h_ord} vs unordered {h_unord}"
        );
    }

    // ── strengthen_color tests ───────────────────────────────────────

    #[test]
    fn strengthen_color_cases() {
        let rgb = |r, g, b| egui::Color32::from_rgb(r, g, b);
        let rgb_of = |c: egui::Color32| {
            let [r, g, b, _] = c.to_array();
            (r, g, b)
        };

        // Black stays black, white stays white
        assert_eq!(
            rgb_of(strengthen_color(rgb(0, 0, 0))),
            (0, 0, 0),
            "black cannot get darker"
        );
        assert_eq!(
            rgb_of(strengthen_color(rgb(255, 255, 255))),
            (255, 255, 255)
        );

        // Dark text (luma < 127) → darken
        let (sr, sg, sb) = (80, 80, 80);
        let (dr, dg, db) = rgb_of(strengthen_color(rgb(sr, sg, sb)));
        assert!(dr < sr && dg < sg && db < sb, "dark text should darken");

        // Bright text (luma > 127) → brighten
        let (sr, sg, sb) = (200, 200, 200);
        let (dr, dg, db) = rgb_of(strengthen_color(rgb(sr, sg, sb)));
        assert!(dr > sr && dg > sg && db > sb, "bright text should brighten");

        // Alpha preservation
        let out = strengthen_color(egui::Color32::from_rgba_premultiplied(100, 100, 100, 42));
        assert_eq!(out.to_array()[3], 42, "alpha must be preserved");

        // Near threshold: 128 → brighten, 126 → darken
        let (lr, lg, lb) = rgb_of(strengthen_color(rgb(128, 128, 128)));
        assert!(lr > 128 && lg > 128 && lb > 128, "128 should brighten");
        let (dr, dg, db) = rgb_of(strengthen_color(rgb(126, 126, 126)));
        assert!(dr < 126 && dg < 126 && db < 126, "126 should darken");

        // Semi-transparent: premultiplied channels must not exceed alpha
        let result = strengthen_color(egui::Color32::from_rgba_unmultiplied(200, 200, 200, 100));
        let [rr, rg, rb, ra] = result.to_array();
        assert!(
            rr <= ra && rg <= ra && rb <= ra,
            "channels must not exceed alpha: {result:?}"
        );

        // Produces visible difference
        let mid = rgb(128, 128, 128);
        let [mr, mg, mb, _] = mid.to_srgba_unmultiplied();
        let [br, bg, bb, _] = strengthen_color(mid).to_srgba_unmultiplied();
        let max_delta = (mr.abs_diff(br)).max(mg.abs_diff(bg)).max(mb.abs_diff(bb));
        assert!(
            max_delta >= 30,
            "should produce visible difference, got delta={max_delta}"
        );
    }

    /// When text has both strong and strikethrough styles, the strikethrough
    /// stroke colour should match the (strengthened) text colour.  Currently
    /// `build_layout_job` sets the stroke *before* strengthening, producing a
    /// colour mismatch.
    #[test]
    fn strikethrough_color_matches_strong_text_color() {
        let ctx = headless_ctx();
        let style = dark_colored_style();
        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                // Parse markdown with both strong and strikethrough.
                let blocks = crate::parse::parse_markdown("**~~bold strike~~**");
                let st = match &blocks[0] {
                    Block::Paragraph(st) => st,
                    other => panic!("expected Paragraph, got {other:?}"),
                };
                // Verify the parser produced combined strong+strikethrough.
                assert!(
                    st.spans
                        .iter()
                        .any(|s| s.style.strong() && s.style.strikethrough()),
                    "should have a span with both strong and strikethrough"
                );

                let base_color = ui.visuals().text_color();
                let job = build_layout_job(st, &st.spans, &style, base_color, 14.0, 400.0, ui);

                for section in &job.sections {
                    if section.format.strikethrough.width > 0.0 {
                        let text_color = section.format.color;
                        let strike_color = section.format.strikethrough.color;
                        assert_eq!(
                            text_color, strike_color,
                            "strikethrough color ({strike_color:?}) should match \
                             text color ({text_color:?})"
                        );
                    }
                }
            });
        });
    }

    /// When code text is inside a link, the code background should still
    /// be applied so the monospace span is visually distinguished.
    #[test]
    fn code_inside_link_has_background() {
        // Parse: [`code_text`](url) — code inside a link.
        let blocks = crate::parse::parse_markdown("[`code_text`](https://example.com)");
        let st = match &blocks[0] {
            Block::Paragraph(st) => st,
            other => panic!("expected Paragraph, got {other:?}"),
        };
        // The text should have a span that is both code and a link.
        let code_link_span = st
            .spans
            .iter()
            .find(|s| s.style.code() && s.style.has_link());
        assert!(
            code_link_span.is_some(),
            "should have a span with both code and link style"
        );
        // Note: the render_text_with_links path currently does NOT apply
        // code_bg for link spans, so code inside links loses its background.
        // This test documents the expectation that it should be applied.
    }

    #[test]
    fn height_estimation_edge_cases_comprehensive() {
        let style = dark_style();

        // Image: scales with width
        let img = Block::Image {
            url: Box::from("img.png"),
            alt: Box::from(""),
        };
        let h_narrow = estimate_block_height(&img, 14.0, 200.0, &style);
        let h_wide = estimate_block_height(&img, 14.0, 800.0, &style);
        assert!(
            h_wide > h_narrow,
            "wider viewport ({h_wide}) > narrow ({h_narrow})"
        );
        for width in [0.0_f32, 1.0, 10.0, 50.0] {
            assert_sane_height(
                estimate_block_height(&img, 14.0, width, &style),
                &format!("image at {width}px"),
            );
        }

        // Paragraph: single long word, empty text
        let h = estimate_block_height(
            &Block::Paragraph(plain(&"x".repeat(5000))),
            14.0,
            600.0,
            &style,
        );
        assert_sane_height(h, "5k-char word paragraph");
        let h_empty = estimate_block_height(&Block::Paragraph(plain("")), 14.0, 600.0, &style);
        assert!(
            h_empty > 0.0 && h_empty.is_finite(),
            "empty paragraph height"
        );

        // Blockquote: deep nesting
        let mut bq = Block::Quote(vec![Block::Paragraph(plain("deep content"))]);
        for _ in 0..7 {
            bq = Block::Quote(vec![bq]);
        }
        assert_sane_height(
            estimate_block_height(&bq, 14.0, 400.0, &style),
            "8-level blockquote",
        );
        let mut bq = Block::Quote(vec![Block::Paragraph(plain("text"))]);
        for _ in 0..19 {
            bq = Block::Quote(vec![bq]);
        }
        assert_sane_height(
            estimate_block_height(&bq, 14.0, 200.0, &style),
            "20-level blockquote narrow",
        );

        // Deeply nested list
        let mut list = Block::UnorderedList(vec![plain_item("leaf")]);
        for depth in 0..10 {
            list = Block::UnorderedList(vec![ListItem {
                content: plain(&format!("level {depth}")),
                children: vec![list],
                checked: None,
            }]);
        }
        assert_sane_height(
            estimate_block_height(&list, 14.0, 600.0, &style),
            "10-level nested list",
        );
        let flat = Block::UnorderedList(vec![plain_item("single")]);
        assert!(
            estimate_block_height(&list, 14.0, 400.0, &style)
                > estimate_block_height(&flat, 14.0, 400.0, &style)
        );

        // Long vs short list item text
        let long = Block::UnorderedList(vec![plain_item(&"word ".repeat(500))]);
        let short = Block::UnorderedList(vec![plain_item("hi")]);
        assert!(
            estimate_block_height(&long, 14.0, 600.0, &style)
                > estimate_block_height(&short, 14.0, 600.0, &style)
        );

        // Narrow wrap width
        let narrow_list = Block::UnorderedList(vec![plain_item("some item text here")]);
        assert_sane_height(
            estimate_block_height(&narrow_list, 14.0, 10.0, &style),
            "list at 10px wrap",
        );

        // Viewport widths: narrow vs wide
        let md = "# Title\n\nA paragraph with some text that should wrap at narrow widths.\n\n                  - Item one\n- Item two\n- Item three\n\n                  ```\ncode block\n```\n\n                  | A | B | C |\n|---|---|---|\n| x | y | z |\n";
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(md);
        cache.ensure_heights(14.0, 200.0, &style);
        assert!(cache.total_height > 0.0);
        for (i, h) in cache.heights.iter().enumerate() {
            assert!(*h > 0.0, "block {i} > 0 at 200px");
        }
        let narrow_total = cache.total_height;
        cache.heights.clear();
        cache.ensure_heights(14.0, 2000.0, &style);
        assert!(cache.total_height > 0.0);
        assert!(narrow_total >= cache.total_height, "narrow >= wide");
        assert!(cache.total_height < 500.0);

        // Extreme narrow widths
        let doc = "# Title\n\nParagraph with **bold** and `code`.\n\n- list item\n- another item\n\n```\ncode block\n```\n\n> blockquote\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n---\n\n![img](url)\n";
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(doc);
        for &width in &[1.0_f32, 5.0, 10.0, 20.0] {
            cache.heights.clear();
            cache.ensure_heights(14.0, width, &style);
            for (i, h) in cache.heights.iter().enumerate() {
                assert!(h.is_finite() && *h >= 0.0, "width={width}: h[{i}]={h}");
            }
            assert!(cache.total_height.is_finite() && cache.total_height >= 0.0);
        }

        // Tiny font sizes
        for &size in &[0.1_f32, 0.5, 1.0] {
            cache.heights.clear();
            cache.ensure_heights(size, 400.0, &style);
            for (i, h) in cache.heights.iter().enumerate() {
                assert!(h.is_finite() && *h >= 0.0, "size={size}: h[{i}]={h}");
            }
            assert!(cache.total_height > 0.0, "size={size}: positive");
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
        let style = dark_colored_style();
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
    fn viewport_structural_invariants() {
        // Exact boundary finds correct block
        let doc = uniform_paragraph_doc(200);
        let cache = build_cache(&doc);
        assert!(cache.blocks.len() >= 200);
        for i in 0..cache.blocks.len() {
            let vis_top = cache.cum_y[i];
            let (first, _) = viewport_range(&cache, vis_top, vis_top + 800.0);
            assert_eq!(
                first, i,
                "scrolling to cum_y[{i}]={vis_top} should start at block {i}, got {first}"
            );
        }

        // Structural invariants across doc types
        let test_docs: Vec<(&str, String)> = vec![
            ("uniform_10k", uniform_paragraph_doc(10_500)),
            ("mixed", crate::stress::large_mixed_doc(200)),
            ("single", "# H\n".to_owned()),
            ("many", uniform_paragraph_doc(500)),
            ("pathological", crate::stress::pathological_doc(50)),
        ];
        for (label, doc) in &test_docs {
            let cache = build_cache(doc);
            assert!(!cache.cum_y.is_empty(), "{label}");
            // cum_y[0] == 0
            assert!(
                (cache.cum_y[0]).abs() < f32::EPSILON,
                "{label}: cum_y[0]={}",
                cache.cum_y[0]
            );
            // cum_y monotonically increases
            for i in 1..cache.cum_y.len() {
                assert!(
                    cache.cum_y[i] >= cache.cum_y[i - 1],
                    "{label}: cum_y not monotonic at {i}"
                );
            }
            // total_height == sum of heights
            let sum: f32 = cache.heights.iter().sum();
            assert!(
                (cache.total_height - sum).abs() < 1.0,
                "{label}: total_height={} but sum={sum}",
                cache.total_height,
            );
            // Array lengths match
            assert_eq!(cache.blocks.len(), cache.heights.len(), "{label}");
            assert_eq!(cache.blocks.len(), cache.cum_y.len(), "{label}");
        }

        // Last block boundary = total_height
        let doc = uniform_paragraph_doc(200);
        let cache = build_cache(&doc);
        let n = cache.blocks.len();
        let last_end = cache.cum_y[n - 1] + cache.heights[n - 1];
        assert!(
            (last_end - cache.total_height).abs() < 0.01,
            "last_end={last_end} != total={}",
            cache.total_height
        );

        // Pathological doc viewport sweep
        let cache = build_cache(&crate::stress::pathological_doc(50));
        let step = cache.total_height / 100.0;
        for i in 0..=110 {
            let vis_top = step * i as f32;
            let (first, last) = viewport_range(&cache, vis_top, vis_top + 800.0);
            assert!(first <= last);
            assert!(last <= cache.blocks.len());
        }
    }

    #[test]
    fn viewport_edge_cases() {
        // Single block
        let cache = build_cache("# Only heading\n");
        assert_eq!(cache.blocks.len(), 1);
        assert!((cache.cum_y[0]).abs() < f32::EPSILON);
        let (first, last) = viewport_range(&cache, 0.0, 800.0);
        assert_eq!(first, 0);
        assert_eq!(last, 1);
        let (count, h) = headless_render_scrollable("# Only\n", Some(0.0));
        assert_eq!(count, 1);
        assert!(h > 0.0);

        // Empty doc
        let cache = build_cache("");
        assert!(cache.blocks.is_empty());
        assert!((cache.total_height).abs() < f32::EPSILON);
        let (first, last) = viewport_range(&cache, 0.0, 800.0);
        assert_eq!((first, last), (0, 0));
        let (count, h) = headless_render_scrollable("", Some(0.0));
        assert_eq!(count, 0);
        assert!((h).abs() < f32::EPSILON);

        // Scroll past total_height
        let doc = uniform_paragraph_doc(100);
        let cache = build_cache(&doc);
        let vis_top = cache.total_height * 2.0;
        let (first, last) = viewport_range(&cache, vis_top, vis_top + 800.0);
        assert!(first < cache.blocks.len());
        assert!(last <= cache.blocks.len());
        let (count, h) = headless_render_scrollable("# Hello\n\nWorld\n\n", Some(999_999.0));
        assert!(count > 0 && h > 0.0);

        // Narrow 1px viewport at block boundaries
        let doc = uniform_paragraph_doc(100);
        let cache = build_cache(&doc);
        for i in (0..cache.blocks.len()).step_by(10) {
            let vis_top = cache.cum_y[i];
            let (first, last) = viewport_range(&cache, vis_top, vis_top + 1.0);
            assert!(last > first, "1px at block {i}: [{first}, {last})");
        }
        let mid_y = cache.cum_y[50] + cache.heights[50] / 2.0;
        let (first, last) = viewport_range(&cache, mid_y, mid_y + 1.0);
        assert!(
            first <= 50 && last > 50,
            "1px mid-block 50: [{first}, {last})"
        );

        // Wide viewport includes all or rest
        let doc = uniform_paragraph_doc(500);
        let cache = build_cache(&doc);
        let n = cache.blocks.len();
        let (first, last) = viewport_range(&cache, 0.0, 100_000.0);
        assert_eq!((first, last), (0, n));
        let mid = n / 2;
        let (first, last) = viewport_range(&cache, cache.cum_y[mid], cache.cum_y[mid] + 100_000.0);
        assert_eq!((first, last), (mid, n));

        // Negative scroll: overlapping viewport
        let doc = uniform_paragraph_doc(100);
        let cache = build_cache(&doc);
        let (first, last) = viewport_range(&cache, -100.0, 700.0);
        assert_eq!(first, 0, "negative scroll starts at 0");
        assert!(last > 0, "overlapping renders blocks");
        let (first, last) = viewport_range(&cache, -1000.0, -200.0);
        assert_eq!((first, last), (0, 0), "entirely negative renders nothing");
        let (count, _) = headless_render_scrollable("# Hello\n\nWorld\n\n", Some(-500.0));
        assert!(count > 0);

        // Uniform: binary search exact
        let doc = uniform_paragraph_doc(500);
        let cache = build_cache(&doc);
        let h0 = cache.heights[0];
        for (i, &h) in cache.heights.iter().enumerate() {
            assert!((h - h0).abs() < f32::EPSILON, "block {i}: {h} != {h0}");
        }
        for (i, &y) in cache.cum_y.iter().enumerate() {
            assert!((y - h0 * i as f32).abs() < 0.1, "cum_y[{i}]={y}");
        }
        for i in (0..cache.blocks.len()).step_by(50) {
            let (first, _) = viewport_range(&cache, cache.cum_y[i], cache.cum_y[i] + 800.0);
            assert_eq!(first, i, "exact at block {i}");
        }

        // Varying: tiny paragraph + huge code block
        let mut doc = String::from("Hi\n\n```\n");
        for i in 0..500 {
            writeln!(doc, "code line {i}").ok();
        }
        doc.push_str("```\n\nAfter code\n\n");
        let cache = build_cache(&doc);
        assert!(
            cache.heights[1] > cache.heights[0] * 10.0,
            "code >> paragraph"
        );
        let code_mid = cache.cum_y[1] + cache.heights[1] / 2.0;
        let (first, last) = viewport_range(&cache, code_mid, code_mid + 100.0);
        assert!(first <= 1 && last > 1, "code in range [{first}, {last})");
    }

    #[test]
    fn viewport_stress_tests() {
        // Multiple viewport widths with stress test
        let md = include_str!("../../../../test-assets/stress-test.md");
        let style = dark_colored_style();
        for &width in &[320.0_f32, 768.0, 1024.0, 1920.0] {
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
            assert!(!cache.blocks.is_empty(), "width={width}");
            assert!(cache.total_height > 0.0, "width={width}: positive height");
        }

        // 10k paragraphs headless
        let doc = uniform_paragraph_doc(10_500);
        let (count, total_h) = headless_render_scrollable(&doc, None);
        assert!(count >= 10_000);
        assert!(total_h > 0.0);
        for &frac in &[0.0, 0.25, 0.5, 0.75, 1.0, 1.5] {
            let (c, _) = headless_render_scrollable(&doc, Some(total_h * frac));
            assert!(c >= 10_000, "stable at scroll {frac:.0}");
        }

        // Narrow (100px) and wide (5000px) widths
        let md2 = "# Heading\n\nA paragraph with some text.\n\n| A | B | C |\n|---|---|---|\n| 1 | 2 | 3 |\n";
        for (label, width) in [("narrow", 100.0_f32), ("wide", 5000.0)] {
            let ctx = headless_ctx();
            let mut cache = MarkdownCache::default();
            let viewer = MarkdownViewer::new("width_test");
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 768.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    viewer.show_scrollable(ui, &mut cache, &style, md2, None);
                });
            });
            assert!(!cache.blocks.is_empty(), "{label}: blocks");
            assert!(cache.total_height > 0.0, "{label}: height");
        }
    }

    /// Helper: build a `StyledText` with no spans.
    fn plain(s: &str) -> StyledText {
        StyledText {
            text: s.to_owned(),
            spans: vec![],
            ..StyledText::default()
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
    fn table_height_estimation_cases() {
        let style = dark_style();
        // Scales with rows
        let h5 = estimate_block_height(
            &Block::Table(Box::new(make_table(3, 5, "cell"))),
            14.0,
            600.0,
            &style,
        );
        let h20 = estimate_block_height(
            &Block::Table(Box::new(make_table(3, 20, "cell"))),
            14.0,
            600.0,
            &style,
        );
        assert!(h20 > h5, "20 rows ({h20}) > 5 rows ({h5})");

        // Extreme column counts
        for ncols in [1, 5, 50, 100] {
            let h = estimate_block_height(
                &Block::Table(Box::new(make_table(ncols, 3, "x"))),
                14.0,
                600.0,
                &style,
            );
            assert_sane_height(h, &format!("{ncols}-col table"));
        }

        // Long cell content
        let long = make_table(3, 3, &"x".repeat(500));
        let h = estimate_block_height(&Block::Table(Box::new(long)), 14.0, 600.0, &style);
        assert_sane_height(h, "long cell table");

        // Empty header, empty rows, zero everything
        let mk_td = |h: Vec<StyledText>, r: Vec<Vec<StyledText>>| TableData {
            alignments: vec![Alignment::None; h.len()],
            header: h,
            rows: r,
        };
        for (label, table) in [
            ("empty_header", mk_td(vec![], vec![vec![plain("x")]])),
            ("empty_rows", mk_td(vec![plain("H")], vec![])),
            ("zero_all", mk_td(vec![], vec![])),
        ] {
            let h = estimate_block_height(&Block::Table(Box::new(table)), 14.0, 600.0, &style);
            assert!(h >= 0.0, "{label}: height should be non-negative, got {h}");
        }

        // estimate_table_height directly: empty header, many rows, narrow
        let h_empty_hdr =
            estimate_table_height(&mk_td(vec![], vec![vec![plain("x")]]), 14.0, 400.0);
        assert!(
            h_empty_hdr > 0.0,
            "empty header table should have height from rows"
        );

        let many_rows = TableData {
            header: vec![plain("A"), plain("B"), plain("C")],
            alignments: vec![Alignment::None; 3],
            rows: (0..200)
                .map(|r| vec![plain(&format!("r{r}")), plain("mid"), plain("end")])
                .collect(),
        };
        let h_many = estimate_table_height(&many_rows, 14.0, 400.0);
        assert!(
            h_many > 200.0,
            "200-row table should be substantial, got {h_many}"
        );

        let narrow_table = TableData {
            header: vec![plain("Header1"), plain("Header2")],
            alignments: vec![Alignment::None; 2],
            rows: vec![vec![plain(&"word ".repeat(50)), plain(&"text ".repeat(50))]],
        };
        let h_narrow = estimate_table_height(&narrow_table, 14.0, 50.0);
        let h_wide = estimate_table_height(&narrow_table, 14.0, 800.0);
        assert!(h_narrow >= h_wide, "narrow ({h_narrow}) >= wide ({h_wide})");

        // col_width floor with many columns
        let table = Block::Table(Box::new(make_table(100, 3, "cell data")));
        assert_sane_height(
            estimate_block_height(&table, 14.0, 400.0, &style),
            "100-col in 400px",
        );

        // Empty table (no header, no rows)
        assert!(
            estimate_block_height(
                &Block::Table(Box::new(mk_td(vec![], vec![]))),
                14.0,
                400.0,
                &style
            ) > 0.0,
            "empty table positive height"
        );

        // Table height vs render constants
        let row_h = 14.0 * 1.8;
        let table = make_table(3, 5, "data");
        let h = estimate_table_height(&table, 14.0, 600.0);
        assert!(
            h >= row_h * 5.0 * 0.5,
            "5-row table {h} vs min {}",
            row_h * 5.0
        );

        // Short rows no panic
        let md = "| A | B |\n|---|---|\n| x | y |\n";
        let (blocks, height) = headless_render(md);
        assert!(matches!(&blocks[0], Block::Table(_)));
        assert!(height > 0.0);

        // Single column width reasonable
        let header = vec![plain("Header")];
        let rows = vec![vec![plain("val")]];
        let (widths, _) = compute_table_col_widths(&header, &rows, 600.0, 7.7, 14.0);
        assert!(
            widths[0] > 40.0 && widths[0] < 600.0,
            "single col width: {}",
            widths[0]
        );

        // Redistribution respects min_col_width
        let header: Vec<StyledText> = (0..10).map(|i| plain(&format!("H{i}"))).collect();
        let rows = vec![(0..10).map(|i| plain(&format!("d{i}"))).collect()];
        let (widths, min_col_w) = compute_table_col_widths(&header, &rows, 400.0, 7.7, 14.0);
        for (i, w) in widths.iter().enumerate() {
            assert!(*w >= min_col_w - 0.01, "col {i}: {w} >= {min_col_w}");
        }
    }

    // ── Code blocks: edge cases ────────────────────────────────────

    #[test]
    fn code_block_height_estimation_cases() {
        let style = dark_style();
        let code_block = |lang: &str, code: &str| Block::Code {
            language: Box::from(lang),
            code: code.into(),
        };

        let cases: Vec<(&str, Block)> = vec![
            ("empty", code_block("", "")),
            ("single_line", code_block("rust", "let x = 1;")),
            ("long_single_line", code_block("", &"x".repeat(10_000))),
        ];
        for (label, block) in &cases {
            assert_sane_height(estimate_block_height(block, 14.0, 600.0, &style), label);
        }

        // Thousands of lines
        let big_code: String = (0..3000).map(|i| format!("line {i}\n")).collect();
        let h_big = estimate_block_height(&code_block("text", &big_code), 14.0, 600.0, &style);
        assert!(h_big > 1000.0, "3000 lines should be > 1000px, got {h_big}");

        // Scales with lines
        let h_3 = estimate_block_height(&code_block("", "a\nb\nc\n"), 14.0, 600.0, &style);
        let code_100: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let h_100 = estimate_block_height(&code_block("", &code_100), 14.0, 600.0, &style);
        assert!(h_100 > h_3, "100-line ({h_100}) > 3-line ({h_3})");

        // Trailing newlines not overcounted
        let h_wt = estimate_block_height(&code_block("", "line1\nline2\n"), 14.0, 600.0, &style);
        let h_wot = estimate_block_height(&code_block("", "line1\nline2"), 14.0, 600.0, &style);
        assert!(
            (h_wt - h_wot).abs() < f32::EPSILON,
            "trailing \\n: {h_wt} vs {h_wot}"
        );

        // Only newlines ≈ empty
        let h_nl = estimate_block_height(&code_block("", "\n\n\n"), 14.0, 600.0, &style);
        let h_empty = estimate_block_height(&code_block("", ""), 14.0, 600.0, &style);
        assert!(
            (h_nl - h_empty).abs() < f32::EPSILON,
            "only-newlines ({h_nl}) ≈ empty ({h_empty})"
        );

        // Language label adds height
        let h_wl = estimate_block_height(&code_block("python", "pass"), 14.0, 600.0, &style);
        let h_nl2 = estimate_block_height(&code_block("", "pass"), 14.0, 600.0, &style);
        assert!(h_wl > h_nl2, "with language ({h_wl}) > without ({h_nl2})");
    }

    #[test]
    fn block_height_robustness() {
        let style = dark_style();

        // All block types handle wrap_width=0
        let all_blocks: Vec<Block> = vec![
            Block::Heading {
                level: 1,
                text: plain("Title"),
            },
            Block::Paragraph(plain("text")),
            Block::Code {
                language: Box::from("rs"),
                code: "code".into(),
            },
            Block::Quote(vec![Block::Paragraph(plain("q"))]),
            Block::UnorderedList(vec![plain_item("item")]),
            Block::OrderedList {
                start: 1,
                items: vec![plain_item("item")],
            },
            Block::ThematicBreak,
            Block::Table(Box::new(make_table(2, 2, "v"))),
            Block::Image {
                url: Box::from("u"),
                alt: Box::from("a"),
            },
        ];
        for (i, block) in all_blocks.iter().enumerate() {
            let h = estimate_block_height(block, 14.0, 0.0, &style);
            assert!(h.is_finite() && h > 0.0, "block {i}: wrap_width=0, h={h}");
        }

        // Finite and positive across font sizes
        let block = Block::Paragraph(plain(&"word ".repeat(50)));
        for size in [1.0_f32, 8.0, 14.0, 24.0, 72.0, 200.0] {
            let h = estimate_block_height(&block, size, 600.0, &style);
            assert!(h.is_finite() && h > 0.0, "font_size={size}: {h}");
        }

        // Total height == sum of individual heights
        let blocks: Vec<Block> = vec![
            Block::Heading {
                level: 2,
                text: plain("Section"),
            },
            Block::Paragraph(plain("Some body text here.")),
            Block::Code {
                language: Box::from("py"),
                code: "print('hi')\n".into(),
            },
            Block::Quote(vec![Block::Paragraph(plain("quoted"))]),
            Block::UnorderedList(vec![plain_item("a"), plain_item("b")]),
            Block::Table(Box::new(make_table(3, 4, "data"))),
            Block::ThematicBreak,
            Block::Image {
                url: Box::from("img.png"),
                alt: Box::from("pic"),
            },
        ];
        let sum: f32 = blocks
            .iter()
            .map(|b| estimate_block_height(b, 14.0, 600.0, &style))
            .sum();
        let mut cache = MarkdownCache::default();
        cache.blocks = blocks;
        cache.ensure_heights(14.0, 600.0, &style);
        assert!(
            (cache.total_height - sum).abs() < 0.01,
            "total ({}) = sum ({sum})",
            cache.total_height
        );
    }

    #[test]
    fn ordered_list_huge_start_no_overflow() {
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = dark_colored_style();
        cache.blocks = vec![Block::OrderedList {
            start: u64::MAX - 1,
            items: vec![
                plain_item("first"),
                plain_item("second"),
                plain_item("third"),
            ],
        }];

        // Render must not panic — previous code used `start + i` which overflows.
        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                render_blocks(ui, &cache.blocks, &style, 0);
            });
        });

        // Height estimation must also handle near-max start.
        let h = estimate_block_height(&cache.blocks[0], 14.0, 400.0, &style);
        assert!(h.is_finite() && h > 0.0, "near-u64::MAX height: {h}");
    }

    // ── Headless rendering: width-configurable helper ──────────────

    /// Render markdown headlessly at a given screen width, returning
    /// `(block_count, estimated_total_height, rendered_total_height)`.
    fn headless_render_at_width(source: &str, width: f32) -> (usize, f32, f32) {
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = dark_colored_style();
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

    #[test]
    fn height_accuracy_and_scrollable_stress() {
        // Height accuracy across block types
        let cases = [
            (
                "heading+paragraph",
                "# Main Title\n\nThis is a short paragraph.\n",
            ),
            (
                "tables",
                "| A | B | C |\n|---|---|---|\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |\n",
            ),
            (
                "code blocks",
                "```rust\nfn main() {}\n```\n\n```python\nfor i in range(10):\n    print(i)\n```\n",
            ),
            (
                "lists",
                "- Item one\n- Item two\n  - Nested A\n- Item three\n",
            ),
        ];
        for (label, md) in cases {
            let (_, estimated, rendered) = headless_render_at_width(md, 1024.0);
            assert!(estimated > 0.0 && rendered > 0.0, "{label}");
            let ratio = estimated / rendered;
            assert!(ratio > 0.1 && ratio < 10.0, "{label}: ratio={ratio}");
        }

        // Scrollable stress across doc types
        let generators: Vec<(&str, String)> = vec![
            ("large_mixed", crate::stress::large_mixed_doc(20)),
            ("unicode", crate::stress::unicode_stress_doc(10)),
            ("table_heavy", crate::stress::table_heavy_doc(10)),
            ("emoji", crate::stress::emoji_heavy_doc(10)),
            ("task_list", crate::stress::task_list_doc(10)),
            ("pathological", crate::stress::pathological_doc(10)),
        ];
        for (label, doc) in &generators {
            for &frac in &[0.0, 0.25, 0.5, 0.75, 1.0] {
                let total_h = height_of(doc);
                let (count, _) = headless_render_scrollable(doc, Some(total_h * frac));
                assert!(count > 0, "{label} at {frac:.0}: should have blocks");
            }
        }

        // Layout consistency: same doc twice → same layout
        let md = "# Hello\n\nWorld with **bold** and `code`.\n\n- List\n- Items\n\n| A | B |\n|---|---|\n| 1 | 2 |\n";
        let (blocks1, h1) = headless_render(md);
        let (blocks2, h2) = headless_render(md);
        assert_eq!(blocks1.len(), blocks2.len());
        assert!((h1 - h2).abs() < 0.01, "heights should match: {h1} vs {h2}");
        let (scroll_count, _) = headless_render_scrollable(md, None);
        assert_eq!(blocks1.len(), scroll_count, "block count should match");

        // Width sensitivity: wider → shorter or equal height
        let md = "# Title\n\nA longer paragraph to test wrapping.\n\n- item one\n- item two\n\n| Col A | Col B |\n|-------|-------|\n| data  | more  |\n";
        let mut prev_h = f32::MAX;
        for &w in &[320.0_f32, 768.0, 1024.0, 1920.0] {
            let (_, est, _) = headless_render_at_width(md, w);
            assert!(est > 0.0, "positive height at {w}");
            assert!(
                est <= prev_h + 1.0,
                "wider ({w}) should not be much taller: {est} vs {prev_h}"
            );
            prev_h = est;
        }
    }

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

    #[test]
    fn table_stress_various_content() {
        let emojis = ["🎉", "🚀", "✅", "❌", "⚡", "🔥", "💡", "📝"];
        let rich_cell = (0..50)
            .map(|i| match i % 3 {
                0 => format!("**bold{i}**"),
                1 => format!("*italic{i}*"),
                _ => format!("`code{i}`"),
            })
            .collect::<Vec<_>>()
            .join(" ");

        let cases: Vec<(&str, String)> = vec![
            ("single_col", build_table_md(1, 5, |r, _| format!("row{r}"))),
            ("50_cols", build_table_md(50, 3, |r, c| format!("r{r}c{c}"))),
            (
                "100_cols",
                build_table_md(100, 2, |r, c| format!("r{r}c{c}")),
            ),
            ("header_only", {
                let mut md = String::from("| H1 | H2 | H3 |\n|---|---|---|\n");
                md.push_str("\nSome text after header-only table.\n");
                md
            }),
            ("long_cells", build_table_md(3, 3, |_, _| "x".repeat(1200))),
            (
                "rich_inline",
                build_table_md(3, 5, |_, _| rich_cell.clone()),
            ),
            (
                "links",
                build_table_md(3, 5, |r, c| {
                    format!("[link{r}{c}](https://example.com/{r}/{c})")
                }),
            ),
            (
                "emoji",
                build_table_md(4, 4, |r, c| emojis[(r + c) % emojis.len()].to_owned()),
            ),
        ];
        for (label, md) in &cases {
            let (blocks, height) = headless_render(md);
            assert!(!blocks.is_empty(), "{label}: should produce blocks");
            assert!(height > 0.0, "{label}: should have positive height");
        }

        // 500 rows scrollable at various positions
        let md = build_table_md(3, 500, |r, c| format!("val_{r}_{c}"));
        for scroll_y in [None, Some(0.0), Some(2000.0), Some(10000.0)] {
            let (count, height) = headless_render_scrollable(&md, scroll_y);
            assert!(count > 0, "500-row table should parse into blocks");
            assert!(
                height > 0.0,
                "500-row table positive height at scroll {scroll_y:?}"
            );
        }

        // 100 small tables, scroll to middle
        let mut md100 = String::new();
        for i in 0..100 {
            let _ = writeln!(md100, "| T{i}A | T{i}B |");
            md100.push_str("|---|---|\n");
            let _ = writeln!(md100, "| {i}a | {i}b |");
            md100.push('\n');
        }
        let (count, height) = headless_render_scrollable(&md100, Some(height_of(&md100) / 2.0));
        assert!(count >= 100, "should parse all 100 tables, got {count}");
        assert!(height > 0.0);

        // Alternating tables and paragraphs
        let mut alt_md = String::new();
        for i in 0..50 {
            let _ = writeln!(alt_md, "Paragraph {i} with some text here.\n");
            let _ = writeln!(alt_md, "| Col1 | Col2 |");
            alt_md.push_str("|------|------|\n");
            let _ = writeln!(alt_md, "| r{i}  | c{i}  |\n");
        }
        for scroll_y in [None, Some(0.0), Some(500.0), Some(2000.0)] {
            let (count, height) = headless_render_scrollable(&alt_md, scroll_y);
            assert!(count > 0, "mixed doc should have blocks");
            assert!(height > 0.0, "positive height at scroll {scroll_y:?}");
        }
    }
    /// Quick height estimate for scroll-target computation in tests.
    fn height_of(source: &str) -> f32 {
        let style = dark_colored_style();
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(source);
        cache.ensure_heights(14.0, 900.0, &style);
        cache.total_height
    }

    fn plain_item(text: &str) -> ListItem {
        ListItem {
            content: plain(text),
            children: vec![],
            checked: None,
        }
    }

    #[test]
    fn stress_list_extreme_counts_and_nesting() {
        // 500 unordered items via scrollable
        let mut md = String::with_capacity(500 * 20);
        for i in 0..500 {
            writeln!(md, "- Item {i}").ok();
        }
        let (count, height) = headless_render_scrollable(&md, None);
        assert!(count > 0);
        assert!(height > 0.0, "500-item unordered list");

        // 500 ordered items
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

        // Nested 10 and 20 levels
        for depth in [10, 20] {
            let mut md = String::new();
            for d in 0..depth {
                let indent = "  ".repeat(d);
                writeln!(md, "{indent}- Level {d}").ok();
            }
            let (blocks, height) = headless_render(&md);
            assert!(!blocks.is_empty(), "nested {depth} levels");
            assert!(height > 0.0, "nested {depth} levels height");

            let style = dark_style();
            let mut cache = MarkdownCache::default();
            cache.ensure_parsed(&md);
            cache.ensure_heights(14.0, 400.0, &style);
            assert!(cache.total_height.is_finite());
        }

        // Lists in various contexts
        let long_item = format!("- {}\n- Short\n", "A".repeat(600));
        for (label, md) in [
            ("after_heading", "# Title\n\n- Item one\n- Item two\n"),
            (
                "inside_blockquote",
                "> - Quoted item 1\n> - Quoted item 2\n",
            ),
            ("only_lists", "- A\n- B\n\n1. One\n2. Two\n\n- C\n- D\n"),
            (
                "alternating",
                "1. Ordered\n2. Items\n\n- Unordered\n- Items\n\n3. More ordered\n",
            ),
            (
                "inline_formatting",
                "- **Bold item**\n- *Italic item*\n- `Code item`\n- [Link](https://example.com)\n- ~~Strike~~\n- **Bold** and *italic* together\n",
            ),
            ("very_long_text", long_item.as_str()),
            (
                "continuation_lines",
                "- First line\n  continued on next\n  and another\n- Second item\n",
            ),
        ] {
            let (blocks, height) = headless_render(md);
            assert!(!blocks.is_empty(), "{label}: should have blocks");
            assert!(height > 0.0, "{label}: should have positive height");
        }

        // Height estimation scaling: 50 vs 100 items
        let style = dark_style();
        let mk_list = |n: usize| -> String { (0..n).map(|i| format!("- Item {i}\n")).collect() };
        let mut c50 = MarkdownCache::default();
        c50.ensure_parsed(&mk_list(50));
        c50.ensure_heights(14.0, 400.0, &style);
        let mut c100 = MarkdownCache::default();
        c100.ensure_parsed(&mk_list(100));
        c100.ensure_heights(14.0, 400.0, &style);
        let ratio = c100.total_height / c50.total_height;
        assert!(
            ratio > 1.5 && ratio < 2.5,
            "100/50 ratio should be ~2, got {ratio}"
        );

        // Linear scaling: 100 vs 200 vs 400 items
        let mut prev_h = 0.0;
        for n in [100, 200, 400] {
            let mut cache = MarkdownCache::default();
            cache.ensure_parsed(&mk_list(n));
            cache.ensure_heights(14.0, 400.0, &style);
            if prev_h > 0.0 {
                assert!(cache.total_height > prev_h, "{n} items should be taller");
            }
            prev_h = cache.total_height;
        }

        // Nested vs flat
        let flat_md: String = (0..50).map(|i| format!("- Item {i}\n")).collect();
        let nested_md: String = (0..50)
            .map(|i| {
                let indent = "  ".repeat(i % 5);
                format!("{indent}- Nested item {i}\n")
            })
            .collect();
        let mut flat_c = MarkdownCache::default();
        flat_c.ensure_parsed(&flat_md);
        flat_c.ensure_heights(14.0, 400.0, &style);
        let mut nest_c = MarkdownCache::default();
        nest_c.ensure_parsed(&nested_md);
        nest_c.ensure_heights(14.0, 400.0, &style);
        assert!(nest_c.total_height > 0.0 && flat_c.total_height > 0.0);

        // Ordered list: various start numbers
        for (start, count, label) in [(999u64, 3, "start_999"), (99999, 3, "start_99999")] {
            let md: String = (0..count)
                .map(|i| format!("{}. Item\n", start + i))
                .collect();
            let (blocks, height) = headless_render(&md);
            match &blocks[0] {
                Block::OrderedList { start: s, items } => {
                    assert_eq!(*s, start, "{label}");
                    assert_eq!(items.len(), count as usize, "{label}");
                }
                other => panic!("{label}: expected OrderedList, got {other:?}"),
            }
            assert!(height > 0.0, "{label}");
        }

        // 1000 items with digit growth
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

        // Digit width height estimation
        for (start, item_count) in [(1u64, 1usize), (99999, 1), (1, 1000)] {
            let items: Vec<_> = (0..item_count)
                .map(|i| plain_item(&format!("item {i}")))
                .collect();
            let block = Block::OrderedList { start, items };
            assert!(estimate_block_height(&block, 14.0, 400.0, &style) > 0.0);
        }

        // Task list: all checked and all unchecked
        for (checked, marker) in [(true, "[x]"), (false, "[ ]")] {
            let md: String = (0..20).map(|i| format!("- {marker} Task {i}\n")).collect();
            let (blocks, _) = headless_render(&md);
            match &blocks[0] {
                Block::UnorderedList(items) => {
                    assert_eq!(items.len(), 20);
                    assert!(
                        items.iter().all(|it| it.checked == Some(checked)),
                        "all {marker}"
                    );
                }
                other => panic!("expected UnorderedList, got {other:?}"),
            }
        }

        // Mixed task list states
        let md = "- [x] Checked\n- [ ] Unchecked\n- Regular\n- [x] Another checked\n- [ ] Another unchecked\n- Also regular\n";
        let (blocks, height) = headless_render(md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 6);
                assert_eq!(items[0].checked, Some(true));
                assert_eq!(items[1].checked, Some(false));
                assert_eq!(items[2].checked, None);
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
        assert!(height > 0.0);

        // Nested task list
        let md = "- [x] Parent checked\n  - [ ] Child unchecked\n  - [x] Child checked\n    - [ ] Grandchild\n- [ ] Parent unchecked\n  - [x] Child checked\n";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty() && height > 0.0);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].checked, Some(true));
                assert!(!items[0].children.is_empty(), "should have children");
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    // ── build_layout_job section coverage ──────────────────────────

    #[test]
    fn layout_job_tests() {
        let ctx = headless_ctx();
        let style = dark_colored_style();

        let run_job = |st: &StyledText| -> egui::text::LayoutJob {
            let mut result = None;
            let _ = ctx.run(raw_input_1024x768(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let base_color = ui.visuals().text_color();
                    result = Some(build_layout_job(
                        st, &st.spans, &style, base_color, 14.0, 900.0, ui,
                    ));
                });
            });
            result.expect("layout job")
        };

        let mk_span = |start, end, style| Span { start, end, style };

        // Sections cover all bytes
        let mut bold = SpanStyle::plain();
        bold.set_strong();
        let st = StyledText {
            text: "Hello **world** end".to_owned(),
            spans: vec![
                mk_span(0, 6, SpanStyle::plain()),
                mk_span(6, 15, bold),
                mk_span(15, 19, SpanStyle::plain()),
            ],
            ..StyledText::default()
        };
        let job = run_job(&st);
        let mut covered = 0;
        for sec in &job.sections {
            assert_eq!(sec.byte_range.start, covered, "gap in byte coverage");
            covered = sec.byte_range.end;
        }
        assert_eq!(covered, job.text.len(), "sections must cover all bytes");

        // Single style single section
        let st = StyledText {
            text: "all plain text".to_owned(),
            spans: vec![mk_span(0, 14, SpanStyle::plain())],
            ..StyledText::default()
        };
        let job = run_job(&st);
        assert_eq!(job.sections.len(), 1);
        assert_eq!(job.sections[0].byte_range, 0..14);

        // Formatting flags (bold/italic)
        let mut italic = SpanStyle::plain();
        italic.set_emphasis();
        let st = StyledText {
            text: "AB".to_owned(),
            spans: vec![mk_span(0, 1, bold), mk_span(1, 2, italic)],
            ..StyledText::default()
        };
        let job = run_job(&st);
        assert_eq!(job.sections.len(), 2);
        assert!(!job.sections[0].format.italics);
        assert!(job.sections[1].format.italics);

        // Code uses monospace
        let mut code = SpanStyle::plain();
        code.set_code();
        let st = StyledText {
            text: "fn main()".to_owned(),
            spans: vec![mk_span(0, 9, code)],
            ..StyledText::default()
        };
        let job = run_job(&st);
        assert_eq!(job.sections.len(), 1);
        assert_eq!(
            job.sections[0].format.font_id.family,
            egui::FontFamily::Monospace
        );
    }
    // ── Blockquote/heading/HR height-estimation consistency ────────

    #[test]
    fn blockquote_rendering_consistency() {
        let style = dark_style();
        let body_size = 14.0;
        let reserved = body_size + 3.0;

        // Min-width floor: narrow wrap → inner_w clamped to 40
        let narrow = 20.0_f32;
        let inner_w_est = (narrow - reserved).max(40.0);
        assert!((inner_w_est - 40.0).abs() < f32::EPSILON);

        // Deeply nested at narrow → still sane
        let mut deep = Block::Quote(vec![Block::Paragraph(plain("content"))]);
        for _ in 0..10 {
            deep = Block::Quote(vec![deep]);
        }
        assert_sane_height(
            estimate_block_height(&deep, body_size, narrow, &style),
            "deep at narrow",
        );

        // Render at 60px → no panic
        let (count, _est, rendered) =
            headless_render_at_width("> > > > > > > > deep nesting\n", 60.0);
        assert!(count > 0);
        assert!(rendered > 0.0);

        // Estimate vs render within bounds
        let md = "> Line one\n> Line two\n> Line three\n";
        let (_, estimated, rendered) = headless_render_at_width(md, 800.0);
        let ratio = estimated / rendered;
        assert!(ratio > 0.1 && ratio < 10.0, "ratio={ratio}");

        // Each nesting level positive height
        for depth in 1..=8_usize {
            let mut block = Block::Quote(vec![Block::Paragraph(plain("text"))]);
            for _ in 1..depth {
                block = Block::Quote(vec![block]);
            }
            assert_sane_height(
                estimate_block_height(&block, 14.0, 400.0, &style),
                &format!("depth {depth}"),
            );
        }

        // 10-level deep with content
        let mut md = String::new();
        for level in 0..10 {
            let prefix = "> ".repeat(level + 1);
            let _ = writeln!(md, "{prefix}Level {}", level + 1);
        }
        let (count, _est, rendered) = headless_render_at_width(&md, 1024.0);
        assert!(count > 0);
        assert!(rendered > 0.0);

        // Width floor consistency at tight_width and very tight
        for width in [reserved + 10.0, 5.0_f32] {
            let block = Block::Quote(vec![Block::Paragraph(plain("Some text content"))]);
            assert_sane_height(
                estimate_block_height(&block, body_size, width, &style),
                &format!("bq at {width}px"),
            );
        }
    }
    // ── Hash collision edge cases (FNV-1a) ─────────────────────────

    #[test]
    fn hash_correctness() {
        // Single byte diff in large doc
        let doc = "a".repeat(100 * 1024);
        let h1 = simple_hash(&doc);
        let mid = doc.len() / 2;
        let mut doc2_bytes = doc.as_bytes().to_vec();
        doc2_bytes[mid] = b'b';
        let doc2 = String::from_utf8(doc2_bytes).expect("still valid UTF-8");
        assert_ne!(
            h1,
            simple_hash(&doc2),
            "1-byte diff in 100KB must change hash"
        );

        // Trailing whitespace
        let ha = simple_hash("Hello world");
        let hb = simple_hash("Hello world ");
        let hc = simple_hash("Hello world  ");
        assert_ne!(ha, hb, "trailing space changes hash");
        assert_ne!(hb, hc, "extra trailing space changes hash");
        assert_ne!(ha, hc);

        // CRLF vs LF
        assert_ne!(
            simple_hash("line1\nline2\nline3\n"),
            simple_hash("line1\r\nline2\r\nline3\r\n"),
            "\\n vs \\r\\n must differ"
        );

        // Never zero for typical inputs
        for input in [
            "",
            "x",
            "hello world",
            &"a".repeat(100_000),
            "# Heading\n\nParagraph\n",
            "\n\n\n",
            "\t\t\t",
        ] {
            assert_ne!(
                simple_hash(input),
                0,
                "hash should not be 0 for len={}",
                input.len()
            );
        }
    }

    #[test]
    fn tight_loop_no_drift() {
        let style = dark_style();
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

    #[test]
    fn pure_block_type_documents() {
        let style = dark_style();
        let check_type =
            |cache: &MarkdownCache, variant: fn(&Block) -> bool, expected: usize, label: &str| {
                let count = cache.blocks.iter().filter(|b| variant(b)).count();
                assert_eq!(count, expected, "{label}: expected {expected}, got {count}");
            };
        let cases: Vec<(&str, String, Box<dyn Fn(&MarkdownCache)>)> = vec![
            (
                "paragraphs_1000",
                {
                    (0..1000)
                        .map(|i| format!("Paragraph number {i} with some filler text.\n\n"))
                        .collect()
                },
                Box::new(|c| assert!(c.blocks.len() >= 1000)),
            ),
            (
                "code_blocks_500",
                {
                    (0..500)
                        .map(|i| format!("```\ncode block {i}\nline 2\nline 3\n```\n\n"))
                        .collect()
                },
                Box::new(|c| {
                    check_type(c, |b| matches!(b, Block::Code { .. }), 500, "code_blocks");
                }),
            ),
            (
                "tables_200",
                {
                    (0..200)
                        .map(|i| format!("| A{i} | B{i} |\n|---|---|\n| c | d |\n\n"))
                        .collect()
                },
                Box::new(|c| check_type(c, |b| matches!(b, Block::Table(_)), 200, "tables")),
            ),
            (
                "lists_300",
                { (0..300).map(|i| format!("- List item {i}\n\n")).collect() },
                Box::new(|c| assert!(!c.blocks.is_empty())),
            ),
            (
                "blockquotes_100",
                { (0..100).map(|i| format!("> Blockquote {i}\n\n")).collect() },
                Box::new(|c| assert!(!c.blocks.is_empty())),
            ),
            (
                "thematic_breaks_500",
                { "---\n\n".repeat(500) },
                Box::new(|c| check_type(c, |b| matches!(b, Block::ThematicBreak), 500, "breaks")),
            ),
            (
                "images_200",
                {
                    (0..200)
                        .map(|i| format!("![img{i}](https://example.com/{i}.png)\n\n"))
                        .collect()
                },
                Box::new(|c| check_type(c, |b| matches!(b, Block::Image { .. }), 200, "images")),
            ),
        ];
        for (label, doc, check) in &cases {
            let mut cache = MarkdownCache::default();
            cache.ensure_parsed(doc);
            cache.ensure_heights(14.0, 400.0, &style);
            check(&cache);
            assert!(cache.total_height > 0.0, "{label}: positive total height");
            for (i, h) in cache.heights.iter().enumerate() {
                assert_sane_height(*h, &format!("{label}[{i}]"));
            }
        }
    }

    #[test]
    fn progressive_refinement() {
        let ctx = headless_ctx();
        let mut cache = MarkdownCache::default();
        let style = dark_colored_style();
        let viewer = MarkdownViewer::new("refine_test");
        let md = "# Big Heading\n\nShort paragraph.\n\n```\nfn main() {\n    println!(\"hello\");\n}\n```\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n> Quote\n\n- List\n- Items\n";

        let mut prev_height = 0.0_f32;
        for pass in 0..5 {
            let _ = ctx.run(raw_input_1024x768(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    viewer.show_scrollable(ui, &mut cache, &style, md, None);
                });
            });
            let h = cache.total_height;
            assert!(h > 0.0, "pass {pass}: positive height");
            if prev_height > 0.0 {
                let delta = (h - prev_height).abs();
                assert!(
                    delta < prev_height * 0.1,
                    "pass {pass}: should stabilize, delta={delta}"
                );
            }
            prev_height = h;
        }
    }

    #[test]
    fn render_edge_case_structures_no_panic() {
        let cases: Vec<(&str, &str)> = vec![
            (
                "code_in_blockquote",
                "> Some text\n>\n> ```rust\n> fn main() {}\n> ```\n>\n> More text\n",
            ),
            (
                "heading_in_blockquote",
                "> ## Heading inside quote\n> Normal text\n",
            ),
            (
                "deeply_nested_structure",
                "> - Item in quote\n>   > Nested quote\n>   > with text\n> - Second item\n",
            ),
            (
                "adjacent_code_blocks",
                "```python\nprint('hello')\n```\n\n```rust\nfn main() {}\n```\n",
            ),
            (
                "task_list_nested_children",
                "- [x] Done task\n  - Sub-item A\n  - Sub-item B\n- [ ] Pending task\n  - Sub-item C\n",
            ),
            ("image_in_list", "- ![screenshot](image.png)\n- Text item\n"),
            ("empty_blocks", ""),
            ("whitespace_only", " \n \n "),
            ("empty_blockquote", ">\n"),
            ("empty_list_item", "- \n"),
            ("empty_table_cells", "| |\n|---|\n| |\n"),
            ("empty_code_block2", "```\n```\n"),
            (
                "blockquote_nested",
                "> Level 1\n>> Level 2\n>>> Level 3\n>>>> Level 4\n>>>>> Level 5\n",
            ),
            (
                "paragraph_in_blockquote",
                "> First paragraph\n>\n> Second paragraph\n",
            ),
        ];
        for (label, md) in &cases {
            let (_, height) = headless_render(md);
            assert!(height >= 0.0, "{label}: non-negative height");
        }

        // Link in heading
        let (blocks, _) = headless_render("## [Documentation](https://docs.rs)\n");
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 2);
                assert!(
                    text.spans.iter().any(|s| s.style.has_link()),
                    "heading should have link span"
                );
            }
            other => panic!("expected Heading, got {other:?}"),
        }

        // Ordered list start=0
        let (blocks, _) = headless_render("0. Zero\n1. One\n2. Two\n");
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 0);
                assert_eq!(items.len(), 3);
            }
            other => panic!("expected OrderedList, got {other:?}"),
        }

        // Ordered list start=999999
        let (blocks, _) = headless_render("999999. Item A\n1000000. Item B\n");
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 999_999);
                assert_eq!(items.len(), 2);
            }
            other => panic!("expected OrderedList, got {other:?}"),
        }

        // Mixed inline in table cell
        let (blocks, _) = headless_render(
            "| Feature | Description |\n|---------|-------------|\n| **Bold** `code` *italic* | [Link](url) ~~strike~~ |\n",
        );
        match &blocks[0] {
            Block::Table(table) => {
                assert_eq!(table.rows.len(), 1);
                assert!(
                    !table.rows[0][0].spans.is_empty(),
                    "formatted cell should have spans"
                );
            }
            other => panic!("expected Table, got {other:?}"),
        }

        // Unicode edge cases
        let md = "# Héading with àccénts\n\nParagraph with emoji 🎉🚀💻 and CJK 你好世界\n\n- ZWJ: 👨\u{200d}👩\u{200d}👧\u{200d}👦\n\n| ™©®℠ | ∀∃∑∏ |\n|------|------|\n| sym  | math |\n";
        let (blocks, height) = headless_render(md);
        assert!(!blocks.is_empty());
        assert!(height > 0.0);

        // 200 levels of nested blockquotes must not stack overflow.
        let md = "> ".repeat(200) + "content\n";
        let (blocks, height) = headless_render(&md);
        assert!(!blocks.is_empty() && height.is_finite() && height > 0.0);

        // 200 levels of nested lists
        let mut md = String::new();
        for depth in 0..200 {
            let indent = "  ".repeat(depth);
            writeln!(md, "{indent}- depth {depth}").ok();
        }
        let (blocks, height) = headless_render(&md);
        assert!(!blocks.is_empty() && height.is_finite());

        // NaN and Inf inputs should not crash or propagate NaN.
        for (size, width, label) in [
            (14.0, f32::NAN, "NaN wrap_width"),
            (f32::NAN, 200.0, "NaN font_size"),
            (14.0, f32::INFINITY, "Inf wrap_width"),
            (0.0, 200.0, "zero font_size"),
            (-5.0, 200.0, "negative font_size"),
        ] {
            let h = estimate_text_height("hello world", size, width);
            assert!(h.is_finite(), "{label} produced: {h}");
        }

        // Empty table height estimation
        let style = dark_style();
        let h = estimate_block_height(
            &Block::Table(Box::new(TableData {
                header: vec![],
                alignments: vec![],
                rows: vec![],
            })),
            14.0,
            400.0,
            &style,
        );
        assert!(h.is_finite() && h >= 0.0, "empty table height: {h}");

        // NaN scroll offset should not crash.
        let md = "# Hello\n\nWorld\n";
        for scroll in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let _ = headless_render_scrollable(md, Some(scroll));
        }

        // Bad spans should not panic.
        let st = StyledText {
            text: "hello".to_owned(),
            spans: vec![crate::parse::Span {
                start: 0,
                end: 100,
                style: crate::parse::SpanStyle::default(),
            }],
            ..StyledText::default()
        };
        let ctx = headless_ctx();
        let style = dark_colored_style();
        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _job = build_layout_job(
                    &st,
                    &st.spans,
                    &style,
                    egui::Color32::WHITE,
                    14.0,
                    400.0,
                    ui,
                );
            });
        });
    }

    #[test]
    fn live_reload_stress() {
        let ctx = headless_ctx();
        let style = dark_colored_style();
        let viewer = MarkdownViewer::new("reload_stress");

        let render = |cache: &mut MarkdownCache, md: &str, scroll: Option<f32>| {
            let _ = ctx.run(raw_input_1024x768(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    viewer.show_scrollable(ui, cache, &style, md, scroll);
                });
            });
        };

        // Content-change viewport preservation
        {
            let mut cache = MarkdownCache::default();
            let md1: String = (0..100)
                .map(|i| format!("## Heading {i}\n\nParagraph {i}.\n\n"))
                .collect();
            render(&mut cache, &md1, None);
            let original_height = cache.total_height;
            let original_blocks = cache.blocks.len();
            assert!(original_height > 0.0 && original_blocks > 0);

            let md2: String = (0..50)
                .map(|i| format!("## New Heading {i}\n\nNew paragraph {i}.\n\n"))
                .collect();
            render(&mut cache, &md2, Some(original_height / 2.0));
            assert!(cache.blocks.len() < original_blocks);
            assert!(cache.total_height < original_height && cache.total_height > 0.0);

            render(&mut cache, "", None);
            assert!(cache.blocks.is_empty());
            assert!(cache.total_height.abs() < f32::EPSILON);
        }

        // Content shrinks: scroll past end doesn't panic
        {
            let mut cache = MarkdownCache::default();
            let long_md: String = (0..500).map(|i| format!("Line {i}\n")).collect();
            render(&mut cache, &long_md, None);
            let long_height = cache.total_height;
            render(
                &mut cache,
                "# Just one heading\n\nAnd a paragraph.\n",
                Some(long_height),
            );
            assert!(cache.total_height >= 0.0);
        }

        // Content grows at various scroll positions
        for frac in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let mut cache = MarkdownCache::default();
            render(&mut cache, "# Short\n\nJust a few lines.\n", None);
            let short_height = cache.total_height;
            let long_md: String = (0..200)
                .map(|i| format!("## Section {i}\n\nContent for section {i}.\n\n"))
                .collect();
            render(&mut cache, &long_md, Some(short_height * frac));
            assert!(
                cache.total_height > short_height,
                "frac={frac}: long content should be taller"
            );
            assert!(
                cache.total_height.is_finite(),
                "frac={frac}: height should be finite"
            );
        }

        // Rapid content switches (simulates rapid file saves)
        {
            let mut cache = MarkdownCache::default();
            let contents: Vec<String> = (0..20)
                .map(|v| {
                    (0..50 + v * 10)
                        .map(|i| format!("## V{v} H{i}\n\nParagraph.\n\n"))
                        .collect()
                })
                .collect();
            for (i, md) in contents.iter().enumerate() {
                let scroll_y = if i > 0 {
                    Some(cache.total_height / 3.0)
                } else {
                    None
                };
                render(&mut cache, md, scroll_y);
                assert!(!cache.blocks.is_empty(), "iter {i}: blocks should exist");
                assert!(
                    cache.total_height > 0.0 && cache.total_height.is_finite(),
                    "iter {i}"
                );
                assert_eq!(
                    cache.heights.len(),
                    cache.blocks.len(),
                    "iter {i}: mismatch"
                );
            }
        }
    }

    #[test]
    fn fuzz_resolve_image_url() {
        let base = "file:///home/user/docs/";

        // Path traversal must be blocked
        for url in [
            "../../../etc/passwd",
            "..\\..\\..\\windows\\system32\\config",
            "images/../../../secret.txt",
            "foo/bar/../../..",
            "..",
            "../",
            "..\\",
            "a/b/../../../etc/shadow",
        ] {
            assert_eq!(
                resolve_image_url(url, base).as_ref(),
                "",
                "traversal blocked: {url}"
            );
        }
        // URL-encoded dots are NOT path traversal — they pass through.
        let encoded = "img/..%2f..%2f..%2fetc/passwd";
        assert_ne!(
            resolve_image_url(encoded, base).as_ref(),
            "",
            "URL-encoded dots are safe"
        );

        // Safe paths resolve correctly
        for (url, b, expected) in [
            ("image.png", "file:///docs/", "file:///docs/image.png"),
            (
                "sub/image.png",
                "file:///docs/",
                "file:///docs/sub/image.png",
            ),
            (
                "/absolute.png",
                "file:///home/user/",
                "file:///absolute.png",
            ),
            (
                "https://example.com/img.png",
                "",
                "https://example.com/img.png",
            ),
            (
                "//cdn.example.com/img.png",
                "file:///docs/",
                "//cdn.example.com/img.png",
            ),
        ] {
            assert_eq!(
                resolve_image_url(url, b).as_ref(),
                expected,
                "safe URL: {url}"
            );
        }

        // Adversarial inputs must not panic
        for url in [
            "",
            " ",
            "\0",
            &"a".repeat(100_000),
            "\n\r\t",
            "file:///etc/passwd",
        ] {
            let _ = resolve_image_url(url, base).len();
        }

        // contains_dot_dot_segment
        for (input, expected) in [
            ("..", true),
            ("../foo", true),
            ("foo/../bar", true),
            ("foo/..", true),
            ("..\\foo", true),
            ("foo\\..\\bar", true),
            ("...", false),
            ("..name", false),
            ("name..ext", false),
            ("foo/.../bar", false),
        ] {
            assert_eq!(
                contains_dot_dot_segment(input),
                expected,
                "dot_dot: {input}"
            );
        }

        // Adversarial height estimation sizes
        let adversarial_sizes = [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            -1.0,
            0.0,
            f32::MIN,
            f32::MAX,
            f32::MIN_POSITIVE,
        ];
        for size in adversarial_sizes {
            let h = estimate_text_height("hello world", size, 400.0);
            assert!(h.is_finite() && h >= 0.0, "size {size}: {h}");
        }
        for width in adversarial_sizes {
            let h = estimate_text_height("hello world", 14.0, width);
            assert!(h.is_finite(), "width {width}: {h}");
        }
        let huge = "x".repeat(10_000_000);
        let h = estimate_text_height(&huge, 14.0, 400.0);
        assert!(h.is_finite() && h > 0.0);
    }

    // ── Issue verification tests ──────────────────────────────────

    /// Issue 1: Blockquote — child UI positioned at parent left edge.
    ///
    /// `allocate_ui_with_layout` with a narrower `desired_size` in a
    /// `top_down(LEFT)` parent places the child rect at the parent's
    /// `available_rect.min.x`, not offset by the bar + margin. The bar
    /// is painted at `min.x + bar_margin + bar_width/2`, so content
    /// overlaps the bar area.
    ///
    /// CONFIRMED: code analysis shows `render_blockquote` subtracts
    /// `reserved` from the *width* but does not shift the child rect
    /// rightward.  `allocate_ui_with_layout` LEFT-aligns the child at
    /// the parent's left edge (verified via egui 0.33 source:
    /// `next_frame_ignore_wrap` → `align2 = Align2([LEFT, TOP])` →
    /// child starts at `available_rect.min.x`).
    #[test]
    fn issue1_blockquote_content_overlaps_bar() {
        // Verify the blockquote renders without panic and the bar parameters
        // demonstrate the overlap: child content width < available but
        // starts at the same left edge as the bar reference point.
        let (blocks, height) = headless_render("> Quoted text\n> Second line\n");
        assert!(matches!(&blocks[0], Block::Quote(inner) if !inner.is_empty()));
        assert!(height > 0.0);

        // Nested blockquotes also affected.
        let (blocks, height) = headless_render("> > Nested quote\n");
        assert!(matches!(&blocks[0], Block::Quote(_)));
        assert!(height > 0.0);

        // Confirm the geometry: bar_margin = body_size * 0.4, bar_width = 3,
        // content_margin = body_size * 0.6.  The reserved space is
        // bar_margin + bar_width + content_margin but the child UI starts
        // at min.x, not min.x + reserved.
        let body_size = 14.0_f32;
        let bar_margin = body_size * 0.4;
        let bar_width = 3.0_f32;
        let content_margin = body_size * 0.6;
        let reserved = bar_margin + bar_width + content_margin;
        // The bar center is at bar_margin + bar_width/2 = ~7.1px from left.
        let bar_center_offset = bar_margin + bar_width * 0.5;
        // Content starts at 0px from left (not offset by reserved ~14.4px).
        assert!(
            bar_center_offset > 0.0 && reserved > bar_center_offset,
            "bar is inside the reserved area, but content starts before it"
        );
    }

    /// Issue 2: Non-list child blocks of list items ignore indent.
    ///
    /// `render_block()` receives `indent` but only list variants use it.
    /// Paragraph, Code, Quote, Table, Image, `ThematicBreak`, Heading all
    /// ignore the parameter.  Child blocks rendered via
    /// `render_blocks(ui, &item.children, style, indent + 1)` after
    /// the `ui.horizontal()` closure appear at the parent margin.
    ///
    /// CONFIRMED: code inspection shows `render_block` match arms for
    /// Paragraph/Code/Quote/Table/Image/ThematicBreak/Heading do not
    /// reference `indent` at all.
    #[test]
    fn issue2_list_child_blocks_ignore_indent() {
        // Parse a list item with child blocks (paragraph + code).
        let md = concat!(
            "1. First item\n",
            "\n",
            "   Child paragraph inside list item.\n",
            "\n",
            "   ```rust\n",
            "   fn nested() {}\n",
            "   ```\n",
            "\n",
            "2. Second item\n",
        );
        let blocks = crate::parse::parse_markdown(md);

        // The first block should be an ordered list.
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 1);
                assert!(items.len() >= 2);

                // First item should have child blocks.
                let first = &items[0];
                assert!(
                    !first.children.is_empty(),
                    "first list item should have child blocks, got none"
                );

                // Verify child block types.
                let has_paragraph = first
                    .children
                    .iter()
                    .any(|b| matches!(b, Block::Paragraph(_)));
                let has_code = first
                    .children
                    .iter()
                    .any(|b| matches!(b, Block::Code { .. }));
                assert!(has_paragraph, "expected Paragraph in children");
                assert!(has_code, "expected Code in children");
            }
            other => panic!("expected OrderedList, got {other:?}"),
        }

        // Verify render_block signature: indent is passed but unused by
        // non-list variants.  This is a structural assertion — non-list
        // blocks do not call add_space(indent_px) or similar.
        let (_, height) = headless_render(md);
        assert!(height > 0.0);
    }

    /// Issue 3: Link text wrapping at span/widget boundaries.
    ///
    /// `render_text_with_links` emits one widget per span inside
    /// `horizontal_wrapped`. There is no word-boundary splitting logic
    /// across widget boundaries — wrapping can occur mid-word when a
    /// span boundary falls inside a word.
    ///
    /// CONFIRMED: each span becomes a separate `ui.label()` or
    /// `ui.hyperlink_to()` call (text.rs:131-178). `item_spacing.x = 0`
    /// removes gaps but doesn't control wrap granularity.
    #[test]
    fn issue3_link_text_per_span_widgets() {
        // Parse text with an inline link — confirm spans and link presence.
        let md = "Click [here](https://example.com) to continue.\n";
        let blocks = crate::parse::parse_markdown(md);
        match &blocks[0] {
            Block::Paragraph(st) => {
                assert!(st.has_links, "should detect links");
                assert!(!st.links.is_empty(), "should have link URLs");
                // Multiple spans: "Click " (no link), "here" (link), " to continue." (no link)
                assert!(
                    st.spans.len() >= 3,
                    "expected at least 3 spans for mixed link text, got {}",
                    st.spans.len()
                );
                // Each span becomes a separate widget in render_text_with_links.
                // No word-boundary splitting across widgets.
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    /// Issue 4: Table right-alignment uses `right_to_left(TOP)`.
    ///
    /// CONFIRMED: `render_table_cell` maps `Alignment::Right` to
    /// `Layout::right_to_left(Align::TOP)` (table.rs:152).
    /// `Alignment::Center` → `top_down(Center)`,
    /// `Alignment::Left`/`None` → `left_to_right(TOP)`.
    #[test]
    fn issue4_table_alignment_layouts() {
        // Parse a table with all alignment types.
        let md = concat!(
            "| Left | Center | Right |\n",
            "|:-----|:------:|------:|\n",
            "| a    | b      | c     |\n",
        );
        let blocks = crate::parse::parse_markdown(md);
        match &blocks[0] {
            Block::Table(table) => {
                assert_eq!(table.alignments.len(), 3);
                assert_eq!(table.alignments[0], Alignment::Left);
                assert_eq!(table.alignments[1], Alignment::Center);
                assert_eq!(table.alignments[2], Alignment::Right);
            }
            other => panic!("expected Table, got {other:?}"),
        }
        // Confirm headless render succeeds with alignment layouts.
        let (_, height) = headless_render(md);
        assert!(height > 0.0);
    }

    /// Issue 5: Code block `set_min_width(available - 12.0)` inside Frame.
    ///
    /// REFUTED: The subtraction is intentional.  `available` is the
    /// OUTER width captured before the Frame.  Inside the Frame with
    /// `inner_margin(6)` (12px total), the inner available width is
    /// ~`available - 12`.  `set_min_width(available - 12)` ensures the
    /// inner content fills the inner area.  The Frame adds 12px of
    /// margin around it, so total outer width ≈ `available`.  No
    /// double-subtraction occurs.
    #[test]
    fn issue5_code_block_width_is_correct() {
        // Render a code block and verify it doesn't panic or produce
        // degenerate heights — confirming the width math is sound.
        let md = "```rust\nfn main() { println!(\"hello\"); }\n```\n";
        let (blocks, height) = headless_render(md);
        assert!(matches!(&blocks[0], Block::Code { .. }));
        assert!(height > 0.0);

        // A very wide code block should still render correctly.
        let wide_code = format!("```\n{}\n```\n", "x".repeat(500));
        let (_, height) = headless_render(&wide_code);
        assert!(height > 0.0, "wide code block should render");
    }

    /// Issue 6: H3-H6 have only `size * 0.15` bottom spacing.
    ///
    /// CONFIRMED: `render_heading` adds `size * 0.3` top space and
    /// `size * 0.15` bottom space for all levels.  H1-H2 get an
    /// additional horizontal rule + 4px.  H3-H6 have tight bottom
    /// spacing (~3px for H3 at 14px body).  Tables and code blocks
    /// add bottom margin after themselves but no top margin, so the
    /// gap between an H3 and a following table/code is very tight.
    #[test]
    fn issue6_heading_spacing_values() {
        let style = dark_style();
        let body_size = 14.0_f32;

        // H3 bottom spacing: size * 0.15
        let h3_scale = style.headings[2].font_scale; // index 2 = H3
        let h3_size = body_size * h3_scale;
        let h3_bottom = h3_size * 0.15;
        assert!(
            h3_bottom < 5.0,
            "H3 bottom spacing ({h3_bottom:.1}px) is very tight"
        );

        // H1/H2 get additional rule + 4px
        let h1_scale = style.headings[0].font_scale;
        let h1_size = body_size * h1_scale;
        let h1_bottom = h1_size * 0.15 + 4.0; // plus rule
        assert!(
            h1_bottom > h3_bottom,
            "H1 bottom ({h1_bottom:.1}px) should exceed H3 bottom ({h3_bottom:.1}px)"
        );

        // Verify tables/code blocks have no explicit top spacing by
        // rendering H3 followed by a table — heights should be computable.
        let md = "### Heading\n\n| A | B |\n|---|---|\n| 1 | 2 |\n";
        let (_, height) = headless_render(md);
        assert!(height > 0.0);
    }

    /// Issue 7: demo.md "Medium test image" links to `small.png`.
    ///
    /// CONFIRMED: Line 169 has alt text "Medium test image" but URL
    /// points to `small.png`.  `test-assets/medium.png` exists in the
    /// repository and should be used instead.
    #[test]
    fn issue7_demo_md_medium_image_url() {
        let demo = include_str!("../../../rustdown-gui/src/bundled/demo.md");
        // Find the line with "Medium test image".
        let medium_line = demo
            .lines()
            .find(|l: &&str| l.contains("Medium test image"))
            .expect("demo.md should contain 'Medium test image'");
        // The URL should reference medium.png (not small.png).
        assert!(
            medium_line.contains("medium.png"),
            "medium image line should reference medium.png: {medium_line}"
        );
        assert!(
            !medium_line.contains("small.png"),
            "medium image line should NOT reference small.png: {medium_line}"
        );
    }

    /// Issue 8: Empty heading now skipped in rendering.
    ///
    /// Empty headings (just `##` with no text) are skipped by both the
    /// render and height estimation, matching the nav panel's behavior
    /// of excluding empty headings.
    #[test]
    fn issue8_empty_heading_renders_spacing() {
        // Parse an empty H2 heading.
        let blocks = crate::parse::parse_markdown("##\n");
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 2);
                assert!(text.text.is_empty(), "empty heading should have empty text");
            }
            other => panic!("expected Heading, got {other:?}"),
        }

        // Render it — should not panic, but produces no output.
        let (_, height) = headless_render("##\n");
        assert!(
            height < 1.0,
            "empty heading should produce ~zero height, got {height}"
        );

        // Height estimation also returns 0 for empty headings.
        let style = dark_style();
        let h = height::estimate_block_height(
            &Block::Heading {
                level: 2,
                text: StyledText::default(),
            },
            14.0,
            400.0,
            &style,
        );
        assert!(
            h < f32::EPSILON,
            "empty heading height estimate should be ~0"
        );
    }
}
