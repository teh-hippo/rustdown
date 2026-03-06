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

#[cfg(test)]
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
}
