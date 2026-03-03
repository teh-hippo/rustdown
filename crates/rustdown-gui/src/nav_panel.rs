use std::sync::Arc;

use eframe::egui;

use crate::nav_outline::{self, HeadingEntry};

/// What the nav panel wants the host to scroll to.
#[derive(Debug, PartialEq, Eq)]
pub enum NavScrollTarget {
    /// Jump to a heading at this byte offset in the source text.
    ByteOffset(usize),
    /// Scroll to the top of the active view.
    Top,
}

/// Persistent state for the Navigation (table-of-contents) panel.
pub struct NavState {
    /// Whether the panel is visible.
    pub visible: bool,
    /// Cached heading outline.
    pub outline: Vec<HeadingEntry>,
    /// The source text the outline was extracted from (for label resolution).
    outline_source: Arc<String>,
    /// The `edit_seq` that produced the current outline.
    outline_seq: u64,
    /// Maximum heading depth to display (1..=6, default 4).
    pub max_depth: u8,
    /// Cached visible-index list, rebuilt when `max_depth` or outline changes.
    visible_indices: Vec<usize>,
    /// Cached top-level positions within `visible_indices`.
    top_positions: Vec<usize>,
    /// The `(outline_seq, max_depth)` that produced the cached indices.
    vis_cache_key: (u64, u8),
    /// Which top-level (min-level) heading *position* is expanded (accordion).
    expanded_pos: Option<usize>,
    /// The heading index the user is currently scrolled to.
    active_index: Option<usize>,
    /// Pending scroll request for the host to execute.
    pub pending_scroll: Option<NavScrollTarget>,
    /// Resolved scroll-y target (pixels) to be consumed inside the scroll
    /// area closure on the next frame for smooth animation.
    pub pending_scroll_y: Option<f32>,
}

impl Default for NavState {
    fn default() -> Self {
        Self {
            visible: false,
            outline: Vec::new(),
            outline_source: Arc::new(String::new()),
            outline_seq: u64::MAX,
            max_depth: 4,
            visible_indices: Vec::new(),
            top_positions: Vec::new(),
            vis_cache_key: (u64::MAX, 0),
            expanded_pos: None,
            active_index: None,
            pending_scroll: None,
            pending_scroll_y: None,
        }
    }
}

const NAV_PANEL_MIN_WIDTH: f32 = 140.0;
const NAV_PANEL_DEFAULT_WIDTH: f32 = 220.0;
const NAV_INDENT_PX: f32 = 12.0;

/// Empirical scale factor mapping scroll-y pixels to source bytes in preview
/// mode.  The exact value does not matter - the mapping only needs to be
/// monotonically increasing so the highlighted heading advances as the user
/// scrolls.  Both `preview_byte_to_scroll_y` and `preview_scroll_y_to_byte`
/// use this constant to stay consistent.
const PREVIEW_SCROLL_SCALE_PX: f32 = 800.0;

/// Compute the scroll-area [`egui::Id`] used by the code editor.
pub fn editor_scroll_id() -> egui::Id {
    egui::Id::new("editor").with("editor_scroll")
}

/// Compute the scroll-area [`egui::Id`] used by the preview pane.
pub fn preview_scroll_id() -> egui::Id {
    egui::Id::new("preview_markdown").with("_scroll_area")
}

/// Convert `byte_offset` to an estimated preview scroll-y value.
/// Returns `0.0` when the outline is empty or all headings are at offset 0.
pub fn preview_byte_to_scroll_y(outline: &[HeadingEntry], byte_offset: usize) -> f32 {
    let max_offset = match outline.last() {
        Some(h) if h.byte_offset > 0 => h.byte_offset as f32,
        _ => return 0.0,
    };
    (byte_offset as f32 * (PREVIEW_SCROLL_SCALE_PX / max_offset)).max(0.0)
}

/// Convert a preview scroll-y value to an estimated byte offset.
/// Returns `0` when the outline is empty.
fn preview_scroll_y_to_byte(outline: &[HeadingEntry], scroll_y: f32) -> usize {
    let max_offset = match outline.last() {
        Some(h) if h.byte_offset > 0 => h.byte_offset as f32,
        _ => return 0,
    };
    (scroll_y * (max_offset / PREVIEW_SCROLL_SCALE_PX)).max(0.0) as usize
}

impl NavState {
    /// Rebuild the heading outline if the document has changed.
    pub fn refresh_outline(&mut self, source: &Arc<String>, edit_seq: u64) {
        if edit_seq == self.outline_seq {
            return;
        }
        self.outline = nav_outline::extract_headings(source.as_str());
        self.outline_source = Arc::clone(source);
        self.outline_seq = edit_seq;
        self.expanded_pos = None;
        self.vis_cache_key = (u64::MAX, 0); // invalidate vis cache
    }

    /// Update `active_index` from a byte position in the document.
    pub fn update_active_from_position(&mut self, byte_position: usize) {
        self.active_index =
            nav_outline::active_heading_index(&self.outline, self.max_depth, byte_position);
    }

    /// Update `active_index` for preview mode using a scroll-y offset.
    pub fn update_active_from_scroll_y(&mut self, scroll_y: f32) {
        if self.outline.is_empty() {
            self.active_index = None;
            return;
        }
        let estimated_byte = preview_scroll_y_to_byte(&self.outline, scroll_y);
        self.active_index =
            nav_outline::active_heading_index(&self.outline, self.max_depth, estimated_byte);
    }

    /// Decrease `max_depth` by one (clamped to 1).
    pub fn decrease_depth(&mut self) {
        self.max_depth = self.max_depth.saturating_sub(1).max(1);
    }

    /// Increase `max_depth` by one (clamped to 6).
    pub fn increase_depth(&mut self) {
        self.max_depth = (self.max_depth + 1).min(6);
    }

    /// Show the navigation panel.
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.visible {
            return;
        }

        let panel_frame = egui::Frame::new()
            .fill(ctx.style().visuals.panel_fill)
            .inner_margin(6);

        egui::SidePanel::right("navigation")
            .resizable(true)
            .min_width(NAV_PANEL_MIN_WIDTH)
            .default_width(NAV_PANEL_DEFAULT_WIDTH)
            .frame(panel_frame)
            .show(ctx, |ui| self.panel_contents(ui));
    }

    fn panel_contents(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Navigation");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("⬆")
                    .on_hover_text("Return to top")
                    .clicked()
                {
                    self.pending_scroll = Some(NavScrollTarget::Top);
                }
            });
        });
        ui.separator();

        // Rebuild cached indices only when outline or depth changes.
        let cache_key = (self.outline_seq, self.max_depth);
        if self.vis_cache_key != cache_key {
            let max_depth = self.max_depth;
            self.visible_indices.clear();
            self.visible_indices.extend(
                self.outline
                    .iter()
                    .enumerate()
                    .filter(|(_, h)| h.level <= max_depth)
                    .map(|(i, _)| i),
            );
            let min_level = self
                .visible_indices
                .iter()
                .map(|&i| self.outline[i].level)
                .min()
                .unwrap_or(1);
            self.top_positions.clear();
            self.top_positions.extend(
                self.visible_indices
                    .iter()
                    .enumerate()
                    .filter(|&(_, &gi)| self.outline[gi].level == min_level)
                    .map(|(pos, _)| pos),
            );
            self.vis_cache_key = cache_key;
        }

        let min_level = self
            .visible_indices
            .iter()
            .map(|&i| self.outline[i].level)
            .min()
            .unwrap_or(1);

        let source = Arc::clone(&self.outline_source);
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                if self.visible_indices.is_empty() {
                    ui.weak("No headings found.");
                    return;
                }
                let result = render_entries(
                    ui,
                    &RenderContext {
                        outline: &self.outline,
                        visible: &self.visible_indices,
                        top_positions: &self.top_positions,
                        min_level,
                        expanded_pos: self.expanded_pos,
                        active_index: self.active_index,
                        source: &source,
                    },
                );
                if let Some(action) = result {
                    self.pending_scroll = Some(NavScrollTarget::ByteOffset(action.byte_offset));
                    if let Some(toggle) = action.toggle_pos {
                        self.expanded_pos = if self.expanded_pos == Some(toggle) {
                            None
                        } else {
                            Some(toggle)
                        };
                    }
                }
            });

        ui.separator();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.max_depth > 1, egui::Button::new("−").small())
                .on_hover_text("Show fewer heading levels")
                .clicked()
            {
                self.decrease_depth();
            }
            ui.label(format!("H1–H{}", self.max_depth));
            if ui
                .add_enabled(self.max_depth < 6, egui::Button::new("+").small())
                .on_hover_text("Show more heading levels")
                .clicked()
            {
                self.increase_depth();
            }
        });
    }
}

// --- Pure rendering functions (no &mut self) ---

struct ClickAction {
    byte_offset: usize,
    toggle_pos: Option<usize>,
}

struct RowStyle {
    indent: f32,
    is_active: bool,
    has_children: bool,
    is_expanded: bool,
}

/// All data needed to render the heading list (avoids too-many-arguments).
struct RenderContext<'a> {
    outline: &'a [HeadingEntry],
    visible: &'a [usize],
    top_positions: &'a [usize],
    min_level: u8,
    expanded_pos: Option<usize>,
    active_index: Option<usize>,
    source: &'a str,
}

fn render_entries(ui: &mut egui::Ui, cx: &RenderContext<'_>) -> Option<ClickAction> {
    let mut result = None;

    // Render orphan entries before the first top-level heading (if any).
    let first_top_vis = cx
        .top_positions
        .first()
        .copied()
        .unwrap_or(cx.visible.len());
    for &gi in &cx.visible[..first_top_vis] {
        let h = &cx.outline[gi];
        let style = RowStyle {
            indent: f32::from(h.level.saturating_sub(cx.min_level)) * NAV_INDENT_PX,
            is_active: cx.active_index == Some(gi),
            has_children: false,
            is_expanded: false,
        };
        if render_heading_row(ui, h, &style, cx.source).clicked() {
            result = Some(ClickAction {
                byte_offset: h.byte_offset,
                toggle_pos: None,
            });
        }
    }

    for (rank, &vis_pos) in cx.top_positions.iter().enumerate() {
        let gi = cx.visible[vis_pos];
        let heading = &cx.outline[gi];
        let is_expanded = cx.expanded_pos == Some(rank);

        let next_vis = cx
            .top_positions
            .get(rank + 1)
            .copied()
            .unwrap_or(cx.visible.len());
        let has_children = next_vis > vis_pos + 1;

        let style = RowStyle {
            indent: f32::from(heading.level.saturating_sub(cx.min_level)) * NAV_INDENT_PX,
            is_active: cx.active_index == Some(gi),
            has_children,
            is_expanded,
        };

        if render_heading_row(ui, heading, &style, cx.source).clicked() {
            result = Some(ClickAction {
                byte_offset: heading.byte_offset,
                toggle_pos: if has_children { Some(rank) } else { None },
            });
        }

        if is_expanded && has_children {
            for &child_vis in &cx.visible[vis_pos + 1..next_vis] {
                let child = &cx.outline[child_vis];
                let child_style = RowStyle {
                    indent: f32::from(child.level.saturating_sub(cx.min_level)) * NAV_INDENT_PX,
                    is_active: cx.active_index == Some(child_vis),
                    has_children: false,
                    is_expanded: false,
                };
                if render_heading_row(ui, child, &child_style, cx.source).clicked() {
                    result = Some(ClickAction {
                        byte_offset: child.byte_offset,
                        toggle_pos: None,
                    });
                }
            }
        }
    }

    result
}

fn render_heading_row(
    ui: &mut egui::Ui,
    heading: &HeadingEntry,
    style: &RowStyle,
    source: &str,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_space(style.indent);

        if style.has_children {
            let arrow = if style.is_expanded { "▾" } else { "▸" };
            ui.label(egui::RichText::new(arrow).small());
        } else if style.indent > 0.0 {
            ui.label(egui::RichText::new("·").small().weak());
        }

        let label = heading.label(source);
        let text = egui::RichText::new(label).small();
        let text = if style.is_active { text.strong() } else { text };

        ui.add(
            egui::Label::new(text)
                .truncate()
                .sense(egui::Sense::click()),
        )
    })
    .inner
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn make_state(md: &str) -> NavState {
        let mut state = NavState {
            visible: true,
            ..NavState::default()
        };
        let source = Arc::new(md.to_owned());
        state.refresh_outline(&source, 1);
        state
    }

    #[test]
    fn refresh_outline_caches_by_edit_seq() {
        let mut state = NavState::default();
        let s1 = Arc::new("# A\n## B\n".to_owned());
        state.refresh_outline(&s1, 1);
        assert_eq!(state.outline.len(), 2);

        state.outline.clear();
        let s2 = Arc::new("# A\n## B\n### C\n".to_owned());
        state.refresh_outline(&s2, 1);
        assert!(state.outline.is_empty(), "should not have rebuilt");

        state.refresh_outline(&s2, 2);
        assert_eq!(state.outline.len(), 3);
    }

    #[test]
    fn refresh_outline_resets_accordion() {
        let mut state = make_state("# A\n## B\n");
        state.expanded_pos = Some(0);
        let s = Arc::new("# A\n## B\n### C\n".to_owned());
        state.refresh_outline(&s, 2);
        assert_eq!(state.expanded_pos, None);
    }

    #[test]
    fn default_max_depth_is_4() {
        let state = NavState::default();
        assert_eq!(state.max_depth, 4);
    }

    #[test]
    fn decrease_depth_clamps_at_one() {
        let mut state = NavState {
            max_depth: 1,
            ..NavState::default()
        };
        state.decrease_depth();
        assert_eq!(state.max_depth, 1);
    }

    #[test]
    fn increase_depth_clamps_at_six() {
        let mut state = NavState {
            max_depth: 6,
            ..NavState::default()
        };
        state.increase_depth();
        assert_eq!(state.max_depth, 6);
    }

    #[test]
    fn depth_round_trip() {
        let mut state = NavState::default();
        assert_eq!(state.max_depth, 4);
        state.decrease_depth();
        assert_eq!(state.max_depth, 3);
        state.increase_depth();
        assert_eq!(state.max_depth, 4);
    }

    #[test]
    fn update_active_tracks_position() {
        let mut state = make_state("# A\n\ntext\n\n## B\n\nmore\n\n### C\n");
        let b_offset = state.outline[1].byte_offset;
        state.update_active_from_position(b_offset);
        assert_eq!(state.active_index, Some(1));
    }

    #[test]
    fn update_active_respects_max_depth() {
        let mut state = make_state("# A\n\n#### D\n\n## B\n");
        let d_offset = state.outline[1].byte_offset;
        state.max_depth = 2;
        state.update_active_from_position(d_offset);
        assert_eq!(state.active_index, Some(0));
    }

    #[test]
    fn scroll_target_variants() {
        let state = NavState {
            pending_scroll: Some(NavScrollTarget::Top),
            ..NavState::default()
        };
        assert_eq!(state.pending_scroll, Some(NavScrollTarget::Top));

        let state = NavState {
            pending_scroll: Some(NavScrollTarget::ByteOffset(42)),
            ..NavState::default()
        };
        assert_eq!(state.pending_scroll, Some(NavScrollTarget::ByteOffset(42)));
    }

    #[test]
    fn preview_scroll_round_trip() {
        // Use extract_headings to build entries with valid byte ranges.
        let md = "x".repeat(500)
            + "\n# A\n"
            + &"y".repeat(500)
            + "\n## B\n"
            + &"z".repeat(500)
            + "\n## C\n";
        let outline = nav_outline::extract_headings(&md);
        assert!(outline.len() >= 2);
        let mid_offset = outline[1].byte_offset;
        let y = preview_byte_to_scroll_y(&outline, mid_offset);
        let byte = preview_scroll_y_to_byte(&outline, y);
        assert_eq!(byte, mid_offset);
    }

    #[test]
    fn preview_scroll_empty_outline() {
        assert_eq!(preview_byte_to_scroll_y(&[], 100), 0.0);
        assert_eq!(preview_scroll_y_to_byte(&[], 100.0), 0);
    }

    #[test]
    fn update_active_from_scroll_y_advances() {
        let mut state = make_state("# A\n\ntext\n\n## B\n\nmore\n\n### C\n");
        state.update_active_from_scroll_y(0.0);
        let first = state.active_index;
        let last_offset = state.outline.last().map_or(0, |h| h.byte_offset);
        let big_y = preview_byte_to_scroll_y(&state.outline, last_offset);
        state.update_active_from_scroll_y(big_y);
        let last = state.active_index;
        // Scrolling further should advance to a later (or equal) heading.
        assert!(last >= first);
    }
}
