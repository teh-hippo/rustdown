pub(super) use crate::parse::{Alignment, Block, ListItem, Span, SpanStyle, StyledText, TableData};
pub(super) use crate::render::blocks::{
    contains_dot_dot_segment, render_blocks, resolve_image_url,
};
pub(super) use crate::render::height::{
    self, estimate_block_height, estimate_table_height, estimate_text_height,
};
pub(super) use crate::render::table::compute_table_col_widths;
pub(super) use crate::render::text::{build_layout_job, strengthen_color};
pub(super) use crate::render::*;
pub(super) use std::fmt::Write;

pub(super) fn dark_style() -> MarkdownStyle {
    MarkdownStyle::from_visuals(&egui::Visuals::dark())
}

pub(super) fn dark_colored_style() -> MarkdownStyle {
    MarkdownStyle::colored(&egui::Visuals::dark())
}

pub(super) fn headless_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    // Warm up so fonts are available.
    let _ = ctx.run(egui::RawInput::default(), |_| {});
    ctx
}

pub(super) fn raw_input_1024x768() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1024.0, 768.0),
        )),
        ..Default::default()
    }
}

pub(super) fn headless_render(source: &str) -> (Vec<Block>, f32) {
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

pub(super) fn headless_render_scrollable(source: &str, scroll_to_y: Option<f32>) -> (usize, f32) {
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

pub(super) fn make_cells(texts: &[&str]) -> Vec<StyledText> {
    texts.iter().map(|t| plain(t)).collect()
}

pub(super) fn digit_count_for(start: u64, item_count: usize) -> u32 {
    let max_num = start.saturating_add(item_count.saturating_sub(1) as u64);
    if max_num == 0 {
        1
    } else {
        (max_num as f64).log10().floor() as u32 + 1
    }
}

pub(super) fn viewport_range(
    cache: &MarkdownCache,
    vis_top: f32,
    vis_bottom: f32,
) -> (usize, usize) {
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

pub(super) fn build_cache(source: &str) -> MarkdownCache {
    let style = dark_colored_style();
    let mut cache = MarkdownCache::default();
    cache.ensure_parsed(source);
    cache.ensure_heights(14.0, 900.0, &style);
    cache
}

pub(super) fn uniform_paragraph_doc(n: usize) -> String {
    let mut doc = String::with_capacity(n * 20);
    for i in 0..n {
        write!(doc, "Paragraph {i}\n\n").ok();
    }
    doc
}

pub(super) fn plain(s: &str) -> StyledText {
    StyledText {
        text: s.to_owned(),
        spans: vec![],
        ..StyledText::default()
    }
}

pub(super) fn make_table(ncols: usize, nrows: usize, cell: &str) -> TableData {
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

pub(super) fn assert_sane_height(h: f32, label: &str) {
    assert!(h.is_finite(), "{label}: height is not finite ({h})");
    assert!(h > 0.0, "{label}: height should be > 0, got {h}");
}

pub(super) fn headless_render_at_width(source: &str, width: f32) -> (usize, f32, f32) {
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

pub(super) fn build_table_md(
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

pub(super) fn height_of(source: &str) -> f32 {
    let style = dark_colored_style();
    let mut cache = MarkdownCache::default();
    cache.ensure_parsed(source);
    cache.ensure_heights(14.0, 900.0, &style);
    cache.total_height
}

pub(super) fn plain_item(text: &str) -> ListItem {
    ListItem {
        content: plain(text),
        children: vec![],
        checked: None,
    }
}
