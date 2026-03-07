//! Table rendering — column width computation, grid layout, cell formatting.

#![allow(clippy::cast_precision_loss)] // UI math — column count is small

use super::text::{render_styled_text, render_styled_text_ex, strengthen_color};
use crate::parse::{Alignment, StyledText};
use crate::style::MarkdownStyle;

/// Compute column widths for a table from content, with optional
/// normalisation to fill available space.
///
/// Returns `(col_widths, min_col_w, needs_scroll)`.
///
/// **Algorithm:** First compute natural content-proportional widths.
/// If the total exceeds `usable`, keep the natural widths and signal
/// horizontal scroll.  Otherwise, scale up to fill the available space.
pub(super) fn compute_table_col_widths(
    header: &[StyledText],
    rows: &[Vec<StyledText>],
    usable: f32,
    avg_char_w: f32,
    body_size: f32,
) -> (Vec<f32>, f32, bool) {
    let num_cols = header.len().max(1);
    let min_col_w = (body_size * 2.5).max(36.0);

    // Estimate each column's natural width from content length.
    let mut widths = Vec::with_capacity(num_cols);
    let mut total_est = 0.0_f32;
    for ci in 0..num_cols {
        let hdr_len = header.get(ci).map_or(0, |c| c.text.chars().count());
        let max_row_len = rows
            .iter()
            .map(|r| r.get(ci).map_or(0, |c| c.text.chars().count()))
            .max()
            .unwrap_or(0);
        let char_len = hdr_len.max(max_row_len).max(3) as f32;
        let w = avg_char_w.mul_add(char_len, 12.0).max(min_col_w);
        total_est += w;
        widths.push(w);
    }

    // If natural widths exceed the budget, use them as-is and scroll.
    if total_est > usable {
        return (widths, min_col_w, true);
    }

    // Content fits — scale up proportionally to fill the usable width.
    if total_est > 0.0 {
        let scale = usable / total_est;
        for w in &mut widths {
            *w = (*w * scale).max(min_col_w);
        }
    }

    // Cap single-column tables to 60% of usable width.
    if num_cols == 1 {
        cap_single_column(&mut widths, header, rows, avg_char_w, usable);
    }

    (widths, min_col_w, false)
}

/// Limit a single-column table's width to 60% of usable, unless
/// content requires more.
fn cap_single_column(
    widths: &mut [f32],
    header: &[StyledText],
    rows: &[Vec<StyledText>],
    avg_char_w: f32,
    usable: f32,
) {
    if let Some(w) = widths.first_mut() {
        let hdr_chars = header.first().map_or(0, |c| c.text.chars().count());
        let max_row_chars = rows
            .iter()
            .map(|r| r.first().map_or(0, |c| c.text.chars().count()))
            .max()
            .unwrap_or(0);
        let max_chars = hdr_chars.max(max_row_chars);
        let content_est = (max_chars as f32).mul_add(avg_char_w, 24.0);
        let max_single = (usable * 0.6).max(content_est.min(usable));
        *w = (*w).min(max_single);
    }
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

    let (col_widths, min_col_w, needs_scroll) =
        compute_table_col_widths(header, rows, usable, avg_char_w, body_size);

    let render_grid = |ui: &mut egui::Ui| {
        egui::Grid::new(ui.next_auto_id())
            .striped(true)
            .min_row_height(body_size * 1.4)
            .show(ui, |ui| {
                for (i, cell) in header.iter().enumerate() {
                    let align = alignments.get(i).copied().unwrap_or(Alignment::None);
                    let w = col_widths.get(i).copied().unwrap_or(min_col_w);
                    render_table_cell(ui, cell, style, align, true, w);
                }
                ui.end_row();

                for row in rows {
                    for (i, cell) in row.iter().take(num_cols).enumerate() {
                        let align = alignments.get(i).copied().unwrap_or(Alignment::None);
                        let w = col_widths.get(i).copied().unwrap_or(min_col_w);
                        render_table_cell(ui, cell, style, align, false, w);
                    }
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
    width: f32,
) {
    let layout = match align {
        Alignment::Right => egui::Layout::top_down(egui::Align::Max),
        Alignment::Center => egui::Layout::top_down(egui::Align::Center),
        Alignment::Left | Alignment::None => egui::Layout::top_down(egui::Align::Min),
    };
    let width = width.max(1.0);
    ui.allocate_ui_with_layout(egui::vec2(width, 0.0), layout, |ui| {
        ui.set_width(width);
        ui.set_min_width(width);
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
    use crate::style::MarkdownStyle;

    fn empty_styled() -> StyledText {
        StyledText::default()
    }

    #[test]
    fn compute_col_widths_empty_header() {
        // 0 columns → treated as max(0, 1) = 1 column
        let (widths, _, _) = compute_table_col_widths(&[], &[], 500.0, 8.0, 16.0);
        assert_eq!(widths.len(), 1);
        assert!(widths[0].is_finite());
    }

    #[test]
    fn compute_col_widths_single_col() {
        let header = vec![empty_styled()];
        let (widths, _, _) = compute_table_col_widths(&header, &[], 500.0, 8.0, 16.0);
        assert_eq!(widths.len(), 1);
        assert!(widths[0].is_finite());
        assert!(widths[0] > 0.0);
    }

    #[test]
    fn compute_col_widths_tiny_usable() {
        // Very small usable width should not produce Inf/NaN
        let header = vec![empty_styled(), empty_styled(), empty_styled()];
        let (widths, min_w, _) = compute_table_col_widths(&header, &[], 1.0, 8.0, 16.0);
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
        let (widths, _, _) = compute_table_col_widths(&header, &[], 500.0, 0.0, 16.0);
        assert_eq!(widths.len(), 1);
        assert!(widths[0].is_finite());
    }

    #[test]
    fn compute_col_widths_many_columns() {
        // 100 columns in tight space
        let header: Vec<StyledText> = (0..100).map(|_| empty_styled()).collect();
        let (widths, min_w, _) = compute_table_col_widths(&header, &[], 200.0, 8.0, 16.0);
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
    fn diag_single_col_cap_includes_row_content() {
        let short_header = vec![styled("ID")];
        let long_body = vec![vec![styled(
            "This is a very long description that should influence the width cap",
        )]];
        let short_body = vec![vec![styled("x")]];

        let (w_long, _, _) = compute_table_col_widths(&short_header, &long_body, 600.0, 7.7, 14.0);
        let (w_short, _, _) =
            compute_table_col_widths(&short_header, &short_body, 600.0, 7.7, 14.0);

        assert!(w_long[0].is_finite() && w_long[0] > 0.0);
        assert!(w_short[0].is_finite() && w_short[0] > 0.0);

        // FIX VERIFIED: The cap now considers row content, so the
        // long-body column should be wider than the short-body column.
        assert!(
            w_long[0] > w_short[0] + 1.0,
            "long body ({:.1}) should be wider than short body ({:.1})",
            w_long[0],
            w_short[0],
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
    fn diag_col_width_char_count_for_non_ascii() {
        let ascii_header = vec![styled("AAAAAAAAAA"), styled("BBBBBBBBBB")];
        let ascii_rows = vec![vec![styled("aaaaaaaaaa"), styled("bbbbbbbbbb")]];

        let cjk_header = vec![styled("AAAAAAAAAA"), styled("你好世界你好世界你好")];
        let cjk_rows = vec![vec![styled("aaaaaaaaaa"), styled("你好世界你好世界你好")]];

        let (w_ascii, _, _) =
            compute_table_col_widths(&ascii_header, &ascii_rows, 800.0, 7.7, 14.0);
        let (w_cjk, _, _) = compute_table_col_widths(&cjk_header, &cjk_rows, 800.0, 7.7, 14.0);

        let ascii_ratio = w_ascii[1] / w_ascii[0];
        let cjk_ratio = w_cjk[1] / w_cjk[0];

        // FIX VERIFIED: Using chars().count() instead of byte length,
        // CJK and ASCII columns with equal char counts get equal widths.
        assert!(
            (cjk_ratio - ascii_ratio).abs() < 0.1,
            "CJK col ratio ({cjk_ratio:.2}) should match \
             ASCII col ratio ({ascii_ratio:.2}) using char count",
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
    fn zero_usable_still_respects_min_col_w() {
        let header = vec![styled("A"), styled("B")];
        let rows = vec![vec![styled("x"), styled("y")]];
        let (widths, min_col_w, needs_scroll) =
            compute_table_col_widths(&header, &rows, 0.0, 7.7, 14.0);

        // With zero usable, natural widths exceed budget → scroll mode.
        assert!(needs_scroll, "zero usable should trigger scroll");
        for (i, w) in widths.iter().enumerate() {
            assert!(
                *w >= min_col_w,
                "col {i} width ({w}) should be >= min_col_w ({min_col_w})"
            );
        }
    }

    #[test]
    fn render_table_cell_reserves_requested_width_for_64_example() {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        let mut allocated_width = 0.0_f32;
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let cell = styled("***Bold and italic***");

        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let rendered = ui.scope(|ui| {
                    render_table_cell(ui, &cell, &style, Alignment::None, false, 180.0);
                });
                allocated_width = rendered.response.rect.width();
            });
        });

        assert!(
            allocated_width >= 170.0,
            "table cells should reserve nearly all of the requested width, got {allocated_width:.1}"
        );
    }
}
