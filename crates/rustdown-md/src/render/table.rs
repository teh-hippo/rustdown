//! Table rendering — column width computation, grid layout, cell formatting.

#![allow(clippy::cast_precision_loss)] // UI math — column count is small

use super::text::{render_styled_text, render_styled_text_ex, strengthen_color};
use crate::parse::{Alignment, StyledText};
use crate::style::MarkdownStyle;

/// Compute normalised column widths for a table, balancing content-proportional
/// sizing with a per-column minimum and a total budget.
///
/// Returns `(col_widths, min_col_w)`.
pub(super) fn compute_table_col_widths(
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
        let scale = (usable / total_est).min(1e6);
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

pub(super) fn render_table(
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

pub(super) fn render_table_cell(
    ui: &mut egui::Ui,
    cell: &StyledText,
    style: &MarkdownStyle,
    align: Alignment,
    is_header: bool,
) {
    let layout = match align {
        Alignment::Right => egui::Layout::top_down(egui::Align::Max),
        Alignment::Center => egui::Layout::top_down(egui::Align::Center),
        Alignment::Left | Alignment::None => egui::Layout::top_down(egui::Align::Min),
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

#[cfg(test)]
#[allow(clippy::doc_markdown)]
mod tests {
    use super::*;
    use crate::parse::StyledText;

    fn empty_styled() -> StyledText {
        StyledText::default()
    }

    #[test]
    fn compute_col_widths_empty_header() {
        // 0 columns → treated as max(0, 1) = 1 column
        let (widths, _) = compute_table_col_widths(&[], &[], 500.0, 8.0, 16.0);
        assert_eq!(widths.len(), 1);
        assert!(widths[0].is_finite());
    }

    #[test]
    fn compute_col_widths_single_col() {
        let header = vec![empty_styled()];
        let (widths, _) = compute_table_col_widths(&header, &[], 500.0, 8.0, 16.0);
        assert_eq!(widths.len(), 1);
        assert!(widths[0].is_finite());
        assert!(widths[0] > 0.0);
    }

    #[test]
    fn compute_col_widths_tiny_usable() {
        // Very small usable width should not produce Inf/NaN
        let header = vec![empty_styled(), empty_styled(), empty_styled()];
        let (widths, min_w) = compute_table_col_widths(&header, &[], 1.0, 8.0, 16.0);
        assert_eq!(widths.len(), 3);
        for w in &widths {
            assert!(w.is_finite(), "width should be finite, got {w}");
            assert!(*w >= min_w);
        }
    }

    #[test]
    fn compute_col_widths_zero_avg_char() {
        // avg_char_w = 0 should not cause issues
        let header = vec![empty_styled()];
        let (widths, _) = compute_table_col_widths(&header, &[], 500.0, 0.0, 16.0);
        assert_eq!(widths.len(), 1);
        assert!(widths[0].is_finite());
    }

    #[test]
    fn compute_col_widths_many_columns() {
        // 100 columns in tight space
        let header: Vec<StyledText> = (0..100).map(|_| empty_styled()).collect();
        let (widths, min_w) = compute_table_col_widths(&header, &[], 200.0, 8.0, 16.0);
        assert_eq!(widths.len(), 100);
        for w in &widths {
            assert!(w.is_finite());
            assert!(*w >= min_w);
        }
    }

    // ── Diagnostic: single-column cap uses header-only content estimate ──

    fn styled(s: &str) -> StyledText {
        StyledText {
            text: s.to_owned(),
            ..StyledText::default()
        }
    }

    /// Diagnostic: single-column table 60% cap ignores row content length.
    ///
    /// Title: Single-column cap content estimate ignores row data
    /// Location: table.rs:76-77
    /// Description: `content_est` in the single-column width cap only
    ///   examines `header.first()` text length.  When row cells are much
    ///   longer than the header, `content_est` remains small and the 60%
    ///   cap cannot be overridden by long body content.  The resulting
    ///   column width is the same whether body cells are short or very
    ///   long, even though a wider column would reduce text wrapping.
    /// Visual impact: Single-column tables with a short header ("ID") and
    ///   long body text get capped at 60% width; body text wraps
    ///   unnecessarily when extra space is available.
    /// Severity: Low — text wraps but is still readable.
    /// Suggested fix: Compute content_est from max(header_len, max_row_len)
    ///   instead of header_len only.
    #[test]
    fn diag_single_col_cap_ignores_row_content() {
        let short_header = vec![styled("ID")];
        let long_body = vec![vec![styled(
            "This is a very long description that should influence the width cap",
        )]];
        let short_body = vec![vec![styled("x")]];

        let (w_long, _) = compute_table_col_widths(&short_header, &long_body, 600.0, 7.7, 14.0);
        let (w_short, _) = compute_table_col_widths(&short_header, &short_body, 600.0, 7.7, 14.0);

        // Both should be finite and positive.
        assert!(w_long[0].is_finite() && w_long[0] > 0.0);
        assert!(w_short[0].is_finite() && w_short[0] > 0.0);

        // BUG EVIDENCE: The cap is based on header only, so both long and
        // short body produce the SAME column width.  A correct
        // implementation would give the long-body column more width.
        let same_width = (w_long[0] - w_short[0]).abs() < 1.0;
        assert!(
            same_width,
            "BUG CONFIRMED: long body ({:.1}) vs short body ({:.1}) \
             yield the same width because the 60% cap only checks the header",
            w_long[0], w_short[0],
        );
    }

    /// Diagnostic: column width computation uses byte length, not char count.
    ///
    /// Title: Column width uses byte-length, not char count, for non-ASCII
    /// Location: table.rs:28-33
    /// Description: `text.len()` returns byte count.  CJK characters are
    ///   3 bytes each, emoji 4 bytes.  This inflates the estimated content
    ///   width for non-ASCII columns relative to ASCII columns, causing
    ///   disproportionate column widths.
    /// Visual impact: In a table with one ASCII column and one CJK column
    ///   of equal visual character count, the CJK column receives ~3×
    ///   more width than it should.
    /// Severity: Low — columns still render; widths are just uneven.
    /// Suggested fix: Use `text.chars().count()` or the cached
    ///   `StyledText::char_count` instead of `text.len()`.
    #[test]
    fn diag_col_width_byte_vs_char_non_ascii() {
        // 10 ASCII chars vs 10 CJK chars (30 bytes).
        let ascii_header = vec![styled("AAAAAAAAAA"), styled("BBBBBBBBBB")];
        let ascii_rows = vec![vec![styled("aaaaaaaaaa"), styled("bbbbbbbbbb")]];

        let cjk_header = vec![styled("AAAAAAAAAA"), styled("你好世界你好世界你好")];
        let cjk_rows = vec![vec![styled("aaaaaaaaaa"), styled("你好世界你好世界你好")]];

        let (w_ascii, _) = compute_table_col_widths(&ascii_header, &ascii_rows, 800.0, 7.7, 14.0);
        let (w_cjk, _) = compute_table_col_widths(&cjk_header, &cjk_rows, 800.0, 7.7, 14.0);

        // Both are 2-column tables with equal visual char count per column.
        // In a correct implementation, widths should be roughly equal.
        let ascii_ratio = w_ascii[1] / w_ascii[0];
        let cjk_ratio = w_cjk[1] / w_cjk[0];

        // BUG EVIDENCE: The CJK column's byte length (30) is 3× the ASCII
        // column's byte length (10), so the CJK column gets proportionally
        // more space than it should.
        assert!(
            (cjk_ratio - ascii_ratio).abs() > 0.1,
            "BUG CONFIRMED: CJK col ratio ({cjk_ratio:.2}) differs from \
             ASCII col ratio ({ascii_ratio:.2}) due to byte-length estimation",
        );
    }

    /// Diagnostic: zero usable width produces zero-width columns (min_col_w
    /// enforcement is inside the `total_est > 0` branch which is skipped).
    ///
    /// Title: Zero usable width bypasses min_col_w enforcement
    /// Location: table.rs:41-70
    /// Description: When `usable = 0`, `col_cap = 0`, all estimated widths
    ///   are 0, `total_est = 0`, and the `if total_est > 0.0` block
    ///   (containing the min_col_w clamp) is skipped entirely.  Columns
    ///   get width 0 instead of min_col_w.
    /// Visual impact: None in practice — the caller floors usable at 0.0
    ///   and a 0-width container is unrealistic.
    /// Severity: Negligible — defensive edge case.
    /// Suggested fix: Move the final min_col_w clamp outside the
    ///   `total_est > 0` guard, or early-return min_col_w widths when
    ///   usable is non-positive.
    #[test]
    fn diag_zero_usable_bypasses_min_col_w() {
        let header = vec![styled("A"), styled("B")];
        let rows = vec![vec![styled("x"), styled("y")]];
        let (widths, min_col_w) = compute_table_col_widths(&header, &rows, 0.0, 7.7, 14.0);

        // When usable = 0, total_est = 0, normalization is skipped, widths stay 0.
        for (i, w) in widths.iter().enumerate() {
            assert!(
                *w < min_col_w,
                "BUG CONFIRMED: col {i} width ({w}) < min_col_w ({min_col_w}) \
                 because zero-usable bypasses the min clamp"
            );
        }
    }
}
