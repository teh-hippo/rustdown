//! Height estimation for viewport culling — pure math, no UI dependency.

use crate::parse::{Block, ListItem, StyledText, TableData};
use crate::style::MarkdownStyle;

/// Estimate pixel height for a top-level block without actually laying it out.
/// Errs on the side of *over*-estimating so that blocks are never clipped.
#[allow(clippy::cast_precision_loss)] // UI math — counts are small
pub(super) fn estimate_block_height(
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

pub(super) fn estimate_quote_height(
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
pub(super) fn estimate_list_height(
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
pub(super) fn estimate_table_height(table: &TableData, body_size: f32, wrap_width: f32) -> f32 {
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
pub(super) fn estimate_text_height(text: &str, font_size: f32, wrap_width: f32) -> f32 {
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
