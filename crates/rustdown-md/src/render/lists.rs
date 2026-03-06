//! List rendering — unordered and ordered lists with checkboxes.

#![allow(clippy::cast_precision_loss)] // UI math — indent values are small

use super::{render_blocks, text::render_styled_text};
use crate::parse::ListItem;
use crate::style::MarkdownStyle;

pub(super) fn render_unordered_list(
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
                if !item.children.is_empty() {
                    render_blocks(ui, &item.children, style, indent + 1);
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
                if !item.children.is_empty() {
                    render_blocks(ui, &item.children, style, indent + 1);
                }
            });
        });
    }
}
