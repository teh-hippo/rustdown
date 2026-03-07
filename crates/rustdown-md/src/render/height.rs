//! Height estimation for viewport culling — pure math, no UI dependency.

#![allow(clippy::cast_precision_loss)] // UI math — counts/dimensions are small

use crate::parse::{Block, ListItem, StyledText, TableData};
use crate::style::MarkdownStyle;

/// Estimate pixel height for a top-level block without actually laying it out.
/// Errs on the side of *over*-estimating so that blocks are never clipped.
pub(super) fn estimate_block_height(
    block: &Block,
    body_size: f32,
    wrap_width: f32,
    style: &MarkdownStyle,
) -> f32 {
    estimate_block_height_at_depth(block, body_size, wrap_width, style, 0)
}

/// Inner height estimation that tracks list nesting depth.
///
/// `list_depth` counts how many list levels deep we are (reset to 0
/// inside blockquotes, since blockquotes handle their own visual offset).
fn estimate_block_height_at_depth(
    block: &Block,
    body_size: f32,
    wrap_width: f32,
    style: &MarkdownStyle,
    list_depth: usize,
) -> f32 {
    match block {
        Block::Heading { level, text } => {
            if text.text.is_empty() {
                return 0.0;
            }
            let idx = (*level as usize).saturating_sub(1).min(5);
            let size = body_size * style.headings[idx].font_scale;
            let text_h = estimate_styled_height(text, size, wrap_width);
            // Render adds top_space (0.3) + bottom_space max(0.15*size, 0.3*body).
            let bottom = (size * 0.15).max(body_size * 0.3);
            size.mul_add(0.3, bottom + text_h)
        }
        Block::Paragraph(text) => {
            body_size.mul_add(0.4, estimate_styled_height(text, body_size, wrap_width))
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
            estimate_list_height_at_depth(items, body_size, wrap_width, style, None, list_depth)
        }
        Block::OrderedList { start, items } => estimate_list_height_at_depth(
            items,
            body_size,
            wrap_width,
            style,
            Some(*start),
            list_depth,
        ),
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

pub(super) fn estimate_quote_height(
    inner: &[Block],
    body_size: f32,
    wrap_width: f32,
    style: &MarkdownStyle,
) -> f32 {
    // Reserve: bar_margin (0.4em) + bar_width (3px) + content_margin (0.6em) ≈ 1em + 3px.
    let reserved = body_size + 3.0;
    let inner_w = (wrap_width - reserved).max(40.0);
    // Blockquotes reset list depth — they handle their own visual offset
    // via scope_builder, so lists inside don't need extra indent.
    let inner_h: f32 = inner
        .iter()
        .map(|b| estimate_block_height_at_depth(b, body_size, inner_w, style, 0))
        .sum();
    body_size.mul_add(0.4, inner_h)
}

/// Inner list height estimation that tracks list nesting depth.
///
/// `list_depth` mirrors the renderer's indent calculation:
/// `indent_px = 16.0 * list_depth` is deducted from the available width.
fn estimate_list_height_at_depth(
    items: &[ListItem],
    body_size: f32,
    wrap_width: f32,
    style: &MarkdownStyle,
    ordered_start: Option<u64>,
    list_depth: usize,
) -> f32 {
    // Match the bullet/number column width used in the actual renderers.
    let bullet_col = match ordered_start {
        Some(start) => {
            // Mirrors render_ordered_list: num_width + 4px gap.
            super::lists::ordered_num_width(start, items.len(), body_size) + 4.0
        }
        None => body_size.mul_add(1.5, 2.0),
    };
    // Deduct indent_px — mirrors the renderer's `16.0 * list_depth` indent.
    let indent_px = 16.0 * list_depth as f32;
    let content_w = (wrap_width - bullet_col - indent_px).max(40.0);
    let item_h: f32 = items
        .iter()
        .map(|item| {
            let text_h = estimate_styled_height(&item.content, body_size, content_w);
            let child_h: f32 = item
                .children
                .iter()
                .map(|b| {
                    estimate_block_height_at_depth(b, body_size, content_w, style, list_depth + 1)
                })
                .sum();
            // ui.horizontal adds ~body_size vertical per item.
            body_size.mul_add(0.3, text_h + child_h)
        })
        .sum();
    body_size.mul_add(0.2, item_h)
}

pub(super) fn estimate_table_height(table: &TableData, body_size: f32, wrap_width: f32) -> f32 {
    let num_cols = table.header.len().max(1);
    let min_col_w = (body_size * 2.5).max(36.0);
    let col_width = (wrap_width / num_cols as f32).max(40.0);
    let base_row_h = body_size * 1.4;
    let row_spacing = 3.0;

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
    let scrollbar_h = if min_col_w * num_cols as f32 + spacing > wrap_width {
        14.0
    } else {
        0.0
    };
    body_size.mul_add(0.4, hdr + rows_h + scrollbar_h)
}

/// Rough text height estimate using byte-level newline counting.
/// Avoids `.lines()` iteration for better throughput on large texts.
#[cfg(test)]
pub(super) fn estimate_text_height(text: &str, font_size: f32, wrap_width: f32) -> f32 {
    estimate_text_height_inner(text, font_size, wrap_width, None)
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
    estimate_text_height_inner(&st.text, font_size, wrap_width, hint)
}

fn estimate_text_height_inner(
    text: &str,
    font_size: f32,
    wrap_width: f32,
    char_count_hint: Option<usize>,
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
    // When char_count_hint is provided (from StyledText.char_count), we can
    // infer ASCII-ness: if char_count == byte_count, the text is ASCII.
    let (char_count, is_ascii) = match char_count_hint {
        Some(hint) => (hint, hint == text.len()),
        None => {
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
