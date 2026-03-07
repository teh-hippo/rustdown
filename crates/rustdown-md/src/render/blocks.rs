#![forbid(unsafe_code)]

use super::layout::{RenderContext, RenderMetrics};
use super::lists::{render_ordered_list, render_unordered_list};
use super::table::render_table;
use super::text::{render_styled_text, render_styled_text_ex};
use crate::parse::{Block, StyledText};
use crate::style::MarkdownStyle;

// ── Block rendering ────────────────────────────────────────────────

/// Maximum rendering recursion depth to prevent stack overflow from
/// pathologically nested markdown (e.g. 1000 nested blockquotes).
pub(super) const MAX_RENDER_DEPTH: usize = 128;

#[inline]
pub(super) fn render_blocks(
    ui: &mut egui::Ui,
    blocks: &[Block],
    style: &MarkdownStyle,
    ctx: RenderContext,
) {
    if ctx.indent() > MAX_RENDER_DEPTH {
        return;
    }
    for block in blocks {
        render_block(ui, block, style, ctx);
    }
}

#[allow(clippy::cast_precision_loss)] // UI math — indent/count values are small
pub(super) fn render_block(
    ui: &mut egui::Ui,
    block: &Block,
    style: &MarkdownStyle,
    ctx: RenderContext,
) {
    let metrics = ctx.metrics();
    match block {
        Block::Heading { level, text } => {
            render_heading(ui, *level, text, style, metrics);
        }

        Block::Paragraph(text) => {
            render_styled_text(ui, text, style);
            ui.add_space(metrics.paragraph_spacing());
        }

        Block::Code { language, code } => {
            render_code_block(ui, language, code, style, metrics);
        }

        Block::Quote(inner) => {
            render_blockquote(ui, inner, style, ctx);
        }

        Block::UnorderedList(items) => {
            render_unordered_list(ui, items, style, ctx);
            ui.add_space(metrics.list_spacing());
        }

        Block::OrderedList { start, items } => {
            render_ordered_list(ui, *start, items, style, ctx);
            ui.add_space(metrics.list_spacing());
        }

        Block::ThematicBreak => {
            render_hr(ui, style, metrics);
        }

        Block::Table(table) => {
            render_table(
                ui,
                &table.header,
                &table.alignments,
                &table.rows,
                style,
                metrics,
            );
            ui.add_space(metrics.paragraph_spacing());
        }

        Block::Image { url, alt } => {
            render_image(ui, url, alt, style, metrics);
        }
    }
}

/// Resolve a (possibly relative) image URL against a base URI.
///
/// Absolute URLs (containing `://` or starting with `//`) pass through
/// unchanged.  A URL starting with `/` is treated as an absolute path
/// and is resolved against only the scheme+authority of `base_uri`.
/// Otherwise the URL is appended to `base_uri` with exactly one `/`
/// separator.
///
/// **Security:** relative URLs containing `..` path segments are rejected
/// to prevent directory-traversal attacks via malicious markdown images.
pub(super) fn resolve_image_url<'a>(url: &'a str, base_uri: &str) -> std::borrow::Cow<'a, str> {
    if url.starts_with("//") || url.contains("://") || base_uri.is_empty() {
        return std::borrow::Cow::Borrowed(url);
    }

    // Reject path-traversal attempts: any `..` that appears as a full
    // path component (e.g. `../`, `foo/../../bar`, or trailing `..`).
    if contains_dot_dot_segment(url) {
        return std::borrow::Cow::Borrowed("");
    }

    if url.starts_with('/') {
        // Absolute path — combine with the scheme+authority only.
        // e.g. base "file:///home/user/docs/" + "/images/pic.png"
        //   → "file:///images/pic.png"
        if let Some(idx) = base_uri.find("://") {
            let after_scheme = idx + 3; // skip "://"
            // Find the next '/' after the authority (if any).
            let authority_end = base_uri[after_scheme..]
                .find('/')
                .map_or(base_uri.len(), |i| after_scheme + i);
            let mut s = String::with_capacity(authority_end + url.len());
            s.push_str(&base_uri[..authority_end]);
            s.push_str(url);
            return std::borrow::Cow::Owned(s);
        }
        // No scheme — just use url as-is.
        return std::borrow::Cow::Borrowed(url);
    }

    // Relative path — ensure exactly one '/' separator.
    let base_slash = base_uri.ends_with('/');
    let mut s = String::with_capacity(base_uri.len() + url.len() + 1);
    s.push_str(base_uri);
    if !base_slash {
        s.push('/');
    }
    s.push_str(url);
    std::borrow::Cow::Owned(s)
}

/// Returns `true` if `path` contains a `..` path component.
///
/// Matches `..` when it appears as the entire path, at the start
/// (`../foo`), in the middle (`foo/../bar`), or at the end (`foo/..`).
/// Also checks backslash-separated paths for Windows.
pub(super) fn contains_dot_dot_segment(path: &str) -> bool {
    // Quick check: if ".." doesn't appear anywhere, skip the split.
    if memchr::memmem::find(path.as_bytes(), b"..").is_none() {
        return false;
    }
    // Single pass: split on both '/' and '\' simultaneously.
    path.split(['/', '\\']).any(|seg| seg == "..")
}

fn render_image(
    ui: &mut egui::Ui,
    url: &str,
    alt: &str,
    style: &MarkdownStyle,
    metrics: RenderMetrics,
) {
    let resolved = resolve_image_url(url, &style.image_base_uri);

    let max_width = ui.available_width();
    let image = egui::Image::new(resolved.as_ref())
        .max_width(max_width)
        .corner_radius(4.0);

    let response = ui.add(image);

    // Show alt text (or URL) on hover.
    let hover_text = if alt.is_empty() { url } else { alt };
    response.on_hover_text(hover_text);

    ui.add_space(metrics.paragraph_spacing());
}

/// Draw a full-width horizontal rule at the current cursor position.
fn draw_horizontal_rule(ui: &egui::Ui, style: &MarkdownStyle) {
    let rect = ui.available_rect_before_wrap();
    let y = rect.min.y;
    let color = style
        .hr_color
        .unwrap_or_else(|| ui.visuals().weak_text_color());
    ui.painter().line_segment(
        [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
        egui::Stroke::new(1.5, color),
    );
}

fn render_heading(
    ui: &mut egui::Ui,
    level: u8,
    text: &StyledText,
    style: &MarkdownStyle,
    metrics: RenderMetrics,
) {
    // Skip empty headings entirely (matches nav panel which excludes them).
    if text.text.is_empty() {
        return;
    }

    let idx = (level as usize).saturating_sub(1).min(5);
    let hs = &style.headings[idx];
    let body_size = metrics.body_size();
    let size = body_size * hs.font_scale;

    ui.add_space(RenderMetrics::heading_top_spacing(size));
    render_styled_text_ex(ui, text, style, Some(size), Some(hs.color));
    ui.add_space(metrics.heading_bottom_spacing(size));
}

fn render_code_block(
    ui: &mut egui::Ui,
    language: &str,
    code: &str,
    style: &MarkdownStyle,
    metrics: RenderMetrics,
) {
    let bg = style.code_bg.unwrap_or_else(|| ui.visuals().faint_bg_color);
    let available = ui.available_width();
    if !language.is_empty() {
        ui.label(egui::RichText::new(language).small().weak());
    }
    egui::Frame::NONE
        .fill(bg)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::same(RenderMetrics::code_block_inner_margin()))
        .show(ui, |ui| {
            ui.set_min_width(available - RenderMetrics::code_block_horizontal_padding());
            egui::ScrollArea::horizontal().show(ui, |ui| {
                let mono = egui::FontId::new(metrics.code_font_size(), egui::FontFamily::Monospace);
                // Only strip trailing newlines, not whitespace — intentional
                // trailing spaces in code should be preserved.
                let trimmed = code.trim_end_matches('\n');
                // Show a non-breaking space for empty blocks so the frame
                // maintains a visible minimum height.
                let display = if trimmed.is_empty() {
                    "\u{00A0}"
                } else {
                    trimmed
                };
                ui.label(
                    egui::RichText::new(display)
                        .font(mono)
                        .color(ui.visuals().text_color()),
                );
            });
        });
    ui.add_space(metrics.paragraph_spacing());
}

fn render_blockquote(
    ui: &mut egui::Ui,
    inner: &[Block],
    style: &MarkdownStyle,
    ctx: RenderContext,
) {
    let metrics = ctx.metrics();
    let bar_color = style
        .blockquote_bar
        .unwrap_or_else(|| ui.visuals().weak_text_color());

    let bar_width = RenderMetrics::blockquote_bar_width();
    let bar_margin = metrics.blockquote_bar_margin();
    let reserved = metrics.blockquote_reserved_width();

    let available = ui.available_rect_before_wrap();
    let bar_x = bar_width.mul_add(0.5, available.min.x + bar_margin);

    // Use a unique salt per nesting depth so egui doesn't share layout state.
    let salt = ui.next_auto_id().with(ctx.indent());
    let content_width = metrics.blockquote_content_width(available.width());

    // Position the content area to the right of the bar using an
    // explicit child rect.  The child starts at `min.x + reserved`
    // and occupies only `content_width`, so the bar area is clear.
    let content_rect = egui::Rect::from_min_size(
        egui::pos2(available.min.x + reserved, available.min.y),
        egui::vec2(content_width, 0.0),
    );
    let inner_response = ui
        .scope_builder(
            egui::UiBuilder::new()
                .max_rect(content_rect)
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
            |ui| {
                ui.push_id(salt, |ui| {
                    render_blocks(ui, inner, style, ctx.quote_inner());
                });
            },
        )
        .response;

    // Paint the vertical bar spanning the full content height.
    let bar_top = inner_response.rect.min.y;
    let bar_bottom = inner_response.rect.max.y;
    ui.painter().line_segment(
        [egui::pos2(bar_x, bar_top), egui::pos2(bar_x, bar_bottom)],
        egui::Stroke::new(bar_width, bar_color),
    );

    // Advance the parent cursor past the full blockquote height.
    // The scope_builder child rect starts at (min.x + reserved), so its
    // response only covers the content area.  We must ensure the parent
    // cursor advances by the total blockquote height (from available.min.y
    // to bar_bottom) to prevent the next sibling from overlapping.
    let total_h = bar_bottom - available.min.y;
    let already_advanced = ui.cursor().top() - available.min.y;
    let gap = total_h - already_advanced;
    if gap > 0.0 {
        ui.add_space(gap);
    }
    ui.add_space(metrics.paragraph_spacing());
}

fn render_hr(ui: &mut egui::Ui, style: &MarkdownStyle, metrics: RenderMetrics) {
    ui.add_space(metrics.paragraph_spacing());
    draw_horizontal_rule(ui, style);
    ui.add_space(metrics.paragraph_spacing());
}
