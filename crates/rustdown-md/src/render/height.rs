//! Height estimation for viewport culling — pure math, no UI dependency.

#![allow(clippy::cast_precision_loss)] // UI math — counts/dimensions are small

use crate::parse::{Block, ListItem, StyledText, TableData};
use crate::style::MarkdownStyle;

use super::layout::RenderMetrics;

/// Estimate pixel height for a top-level block without actually laying it out.
/// Errs on the side of *over*-estimating so that blocks are never clipped.
pub(super) fn estimate_block_height(
    block: &Block,
    body_size: f32,
    wrap_width: f32,
    style: &MarkdownStyle,
) -> f32 {
    estimate_block_height_with_metrics(block, RenderMetrics::new(body_size), wrap_width, style)
}

fn estimate_block_height_with_metrics(
    block: &Block,
    metrics: RenderMetrics,
    wrap_width: f32,
    style: &MarkdownStyle,
) -> f32 {
    match block {
        Block::Heading { level, text } => {
            if text.text.is_empty() {
                return 0.0;
            }
            let idx = (*level as usize).saturating_sub(1).min(5);
            let size = metrics.body_size() * style.headings[idx].font_scale;
            let text_h = estimate_styled_height(text, size, wrap_width);
            RenderMetrics::heading_top_spacing(size) + metrics.heading_bottom_spacing(size) + text_h
        }
        Block::Paragraph(text) => {
            metrics.paragraph_spacing()
                + estimate_styled_height(text, metrics.body_size(), wrap_width)
        }
        Block::Code { language, code, .. } => {
            let mono_size = metrics.code_font_size();
            // Match render_code_block: trailing newlines are stripped before display.
            let trimmed = code.trim_end_matches('\n');
            let lines = (bytecount_newlines(trimmed.as_bytes()) + 1).max(1) as f32;
            let lang_h = if language.is_empty() {
                0.0
            } else {
                metrics.body_size()
            };
            metrics.paragraph_spacing()
                + (lines * mono_size).mul_add(1.4, RenderMetrics::code_block_horizontal_padding())
                + lang_h
        }
        Block::Quote(inner) => estimate_quote_height(inner, metrics, wrap_width, style),
        Block::UnorderedList(items) => {
            estimate_list_height_with_metrics(items, metrics, wrap_width, style, None)
        }
        Block::OrderedList { start, items } => {
            estimate_list_height_with_metrics(items, metrics, wrap_width, style, Some(*start))
        }
        Block::ThematicBreak => metrics.thematic_break_height(),
        Block::Table(table) => estimate_table_height(table, metrics.body_size(), wrap_width),
        Block::Image { .. } => {
            metrics.paragraph_spacing()
                + RenderMetrics::image_max_height(wrap_width).max(metrics.image_fallback_height())
        }
    }
}

fn estimate_quote_height(
    inner: &[Block],
    metrics: RenderMetrics,
    wrap_width: f32,
    style: &MarkdownStyle,
) -> f32 {
    let inner_w = metrics.blockquote_content_width(wrap_width);
    let inner_h: f32 = inner
        .iter()
        .map(|b| estimate_block_height_with_metrics(b, metrics.with_list_depth(0), inner_w, style))
        .sum();
    metrics.paragraph_spacing() + inner_h
}

fn estimate_list_height_with_metrics(
    items: &[ListItem],
    metrics: RenderMetrics,
    wrap_width: f32,
    style: &MarkdownStyle,
    ordered_start: Option<u64>,
) -> f32 {
    let bullet_col = match ordered_start {
        Some(start) => {
            super::lists::ordered_num_width(start, items.len(), metrics.body_size())
                + RenderMetrics::ordered_gap_px()
        }
        None => metrics.unordered_bullet_column_width() + RenderMetrics::unordered_gap_px(),
    };
    let content_w = (wrap_width - bullet_col - metrics.list_indent_px()).max(40.0);
    let item_h: f32 = items
        .iter()
        .map(|item| {
            let text_h = estimate_styled_height(&item.content, metrics.body_size(), content_w);
            let child_h: f32 = item
                .children
                .iter()
                .map(|b| {
                    estimate_block_height_with_metrics(b, metrics.nested_list(), content_w, style)
                })
                .sum();
            metrics.list_item_overhead() + text_h + child_h
        })
        .sum();
    metrics.list_spacing() + item_h
}

pub(super) fn estimate_table_height(table: &TableData, body_size: f32, wrap_width: f32) -> f32 {
    let metrics = RenderMetrics::new(body_size);
    let num_cols = table.header.len().max(1);
    let min_col_w = metrics.table_min_col_width();
    let col_width = (wrap_width / num_cols as f32).max(40.0);
    let base_row_h = metrics.table_base_row_height();
    let row_spacing = RenderMetrics::table_row_spacing();

    let row_height = |cells: &[StyledText]| -> f32 {
        cells.iter().fold(base_row_h, |max, c| {
            estimate_styled_height(c, body_size, col_width).max(max)
        }) + row_spacing
    };

    let hdr = if table.header.is_empty() {
        0.0
    } else {
        row_height(&table.header)
    };
    let rows_h: f32 = table.rows.iter().map(|r| row_height(r)).sum();
    // Account for horizontal scrollbar when columns exceed available width.
    // Include inter-column spacing (~8px per gap) in the overflow check.
    let spacing = 8.0 * num_cols.saturating_sub(1) as f32;
    let scrollbar_h = if min_col_w.mul_add(num_cols as f32, spacing) > wrap_width {
        RenderMetrics::table_scrollbar_height()
    } else {
        0.0
    };
    metrics.paragraph_spacing() + hdr + rows_h + scrollbar_h
}

/// Rough text height estimate using byte-level newline counting.
/// Avoids `.lines()` iteration for better throughput on large texts.
#[cfg(test)]
pub(super) fn estimate_text_height(text: &str, font_size: f32, wrap_width: f32) -> f32 {
    estimate_text_height_inner(text, font_size, wrap_width, None, None)
}

/// Like [`estimate_text_height`], but uses a pre-computed character count
/// from [`StyledText::char_count`] to skip the O(n) UTF-8 scan for non-ASCII text.
#[inline]
pub(super) fn estimate_styled_height(st: &StyledText, font_size: f32, wrap_width: f32) -> f32 {
    // Only use cached count when it was actually populated (> 0 for non-empty text).
    let hint = if st.char_count > 0 {
        Some(st.char_count as usize)
    } else {
        None
    };
    estimate_text_height_inner(&st.text, font_size, wrap_width, hint, Some(st.is_ascii))
}

fn estimate_text_height_inner(
    text: &str,
    font_size: f32,
    wrap_width: f32,
    char_count_hint: Option<usize>,
    is_ascii_hint: Option<bool>,
) -> f32 {
    // Guard against NaN / Inf / non-positive / absurdly large font_size.
    // Clamp to a sane range: no real font exceeds ~1000px.
    let font_size = if font_size.is_finite() && font_size > 0.0 {
        font_size.min(1000.0)
    } else {
        14.0
    };
    if text.is_empty() {
        return font_size;
    }
    // Guard against NaN / Inf / non-positive / absurdly large wrap_width.
    let wrap_width = if wrap_width.is_finite() && wrap_width > 0.0 {
        wrap_width.min(100_000.0)
    } else {
        400.0
    };
    // Derive char count and ASCII-ness simultaneously to avoid redundant scans.
    let (char_count, is_ascii) = match (char_count_hint, is_ascii_hint) {
        (Some(hint), Some(is_ascii)) => (hint, is_ascii),
        (Some(hint), None) => (hint, hint == text.len()),
        (None, Some(is_ascii)) if is_ascii => (text.len(), true),
        (None, _) => {
            if text.is_ascii() {
                (text.len(), true)
            } else {
                (text.chars().count(), false)
            }
        }
    };
    // Use wider average char width for non-ASCII text (CJK glyphs are roughly
    // square, so ≈0.7 em is a better estimate than the 0.55 em used for Latin).
    let avg_char_width = if is_ascii {
        font_size * 0.55
    } else {
        font_size * 0.7
    };
    let chars_per_line = (wrap_width / avg_char_width).max(1.0);
    // Count newlines by scanning bytes (much faster than .lines() for large text).
    let newline_count = bytecount_newlines(text.as_bytes());
    let hard_lines = (newline_count + 1).max(1);
    let avg_line_len = char_count as f32 / hard_lines as f32;
    let wraps_per_line = (avg_line_len / chars_per_line).ceil().max(1.0);
    let total = hard_lines as f32 * wraps_per_line;
    total * font_size * 1.3
}

/// Fast newline counting via memchr.
#[inline]
#[must_use]
pub fn bytecount_newlines(bytes: &[u8]) -> usize {
    memchr::memchr_iter(b'\n', bytes).count()
}
