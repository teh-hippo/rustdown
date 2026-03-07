//! List rendering — unordered and ordered lists with checkboxes.

#![allow(clippy::cast_precision_loss)] // UI math — indent values are small

use super::blocks::render_blocks;
use super::layout::RenderContext;
use super::layout::RenderMetrics;
use super::text::render_styled_text;
use crate::parse::ListItem;
use crate::style::MarkdownStyle;

/// Compute the number column width for an ordered list.
///
/// Returns the width in pixels for the number column, based on the
/// widest number that will appear.  Shared between the renderer and
/// the height estimator for consistency.
#[inline]
pub(super) fn ordered_num_width(start: u64, item_count: usize, body_size: f32) -> f32 {
    let max_num = start.saturating_add(item_count.saturating_sub(1) as u64);
    let digit_count = if max_num == 0 {
        1u32
    } else {
        (max_num as f64).log10().floor() as u32 + 1
    };
    body_size * 0.6f32.mul_add(digit_count as f32, 1.0)
}

pub(super) fn render_unordered_list(
    ui: &mut egui::Ui,
    items: &[ListItem],
    style: &MarkdownStyle,
    ctx: RenderContext,
) {
    let metrics = ctx.metrics();
    let bullet = metrics.bullet_text();
    let indent_px = metrics.list_indent_px();
    let body_size = metrics.body_size();

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
                egui::vec2(metrics.unordered_bullet_column_width(), body_size),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.label(bullet_text);
                },
            );
            ui.add_space(RenderMetrics::unordered_gap_px());
            ui.vertical(|ui| {
                render_styled_text(ui, &item.content, style);
                if !item.children.is_empty() {
                    render_blocks(ui, &item.children, style, ctx.nested_list());
                }
            });
        });
    }
}

pub(super) fn render_ordered_list(
    ui: &mut egui::Ui,
    start: u64,
    items: &[ListItem],
    style: &MarkdownStyle,
    ctx: RenderContext,
) {
    let metrics = ctx.metrics();
    let indent_px = metrics.list_indent_px();
    let body_size = metrics.body_size();
    let mut num_buf = String::with_capacity(8);
    let num_width = ordered_num_width(start, items.len(), body_size);

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
            ui.add_space(RenderMetrics::ordered_gap_px());
            ui.vertical(|ui| {
                render_styled_text(ui, &item.content, style);
                if !item.children.is_empty() {
                    render_blocks(ui, &item.children, style, ctx.nested_list());
                }
            });
        });
    }
}
