#![forbid(unsafe_code)]
//! Render parsed Markdown blocks into egui widgets.
//!
//! Key feature: viewport culling in `show_scrollable` — only blocks
//! overlapping the visible region are laid out, giving O(visible) cost.

mod blocks;
mod height;
mod lists;
mod table;
mod text;

#[cfg(test)]
mod tests;

use crate::parse::{Block, parse_markdown_into};
use crate::style::MarkdownStyle;

use blocks::{render_block, render_blocks};
pub use height::bytecount_newlines;
use height::estimate_block_height;

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
                render_block(ui, &cache.blocks[idx], style, 0, 0);
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
        render_blocks(ui, &cache.blocks, style, 0, 0);
    }
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
