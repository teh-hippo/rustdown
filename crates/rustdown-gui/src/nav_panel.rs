use std::collections::HashSet;
use std::sync::Arc;

use eframe::egui;

use crate::highlight;
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
    /// Whether heading colour mode is active.
    pub heading_color_mode: bool,
    /// Set of outline indices whose children are expanded.
    expanded: HashSet<usize>,
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
            heading_color_mode: false,
            expanded: HashSet::new(),
            active_index: None,
            pending_scroll: None,
            pending_scroll_y: None,
        }
    }
}

const NAV_PANEL_MIN_WIDTH: f32 = 140.0;
const NAV_PANEL_DEFAULT_WIDTH: f32 = 220.0;
const NAV_INDENT_PX: f32 = 12.0;

/// Compute the scroll-area [`egui::Id`] used by the code editor.
pub fn editor_scroll_id() -> egui::Id {
    egui::Id::new("editor").with("editor_scroll")
}

/// Convert `byte_offset` to an estimated preview scroll-y value.
/// Uses the actual total rendered height for accurate mapping.
/// Returns `0.0` when the outline is empty or all headings are at offset 0.
#[allow(clippy::cast_precision_loss)] // byte offsets are small relative to f32 range
pub fn preview_byte_to_scroll_y(
    outline: &[HeadingEntry],
    byte_offset: usize,
    total_height: f32,
) -> f32 {
    let max_offset = match outline.last() {
        Some(h) if h.byte_offset > 0 => h.byte_offset as f32,
        _ => return 0.0,
    };
    if total_height <= 0.0 {
        return 0.0;
    }
    (byte_offset as f32 / max_offset * total_height).max(0.0)
}

/// Convert a preview scroll-y value to an estimated byte offset.
/// Returns `0` when the outline is empty.
#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub fn preview_scroll_y_to_byte(
    outline: &[HeadingEntry],
    scroll_y: f32,
    total_height: f32,
) -> usize {
    let max_offset = match outline.last() {
        Some(h) if h.byte_offset > 0 => h.byte_offset as f32,
        _ => return 0,
    };
    if total_height <= 0.0 {
        return 0;
    }
    (scroll_y / total_height * max_offset).max(0.0) as usize
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
        self.expanded.clear();
    }

    /// Force the next `refresh_outline` call to re-extract headings.
    pub const fn invalidate_outline(&mut self) {
        self.outline_seq = u64::MAX;
    }

    /// Update `active_index` from a byte position in the document.
    pub fn update_active_from_position(&mut self, byte_position: usize) {
        self.active_index =
            nav_outline::active_heading_index(&self.outline, self.max_depth, byte_position);
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

        let min_level = self
            .outline
            .iter()
            .filter(|h| h.level <= self.max_depth)
            .map(|h| h.level)
            .min()
            .unwrap_or(1);

        let source = Arc::clone(&self.outline_source);
        let max_depth = self.max_depth;
        let heading_color_mode = self.heading_color_mode;

        // Compute visible headings: a heading is visible if it's at min_level
        // or all its ancestors are expanded.
        let visible = compute_visible_headings(&self.outline, &self.expanded, max_depth, min_level);

        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                if visible.is_empty() {
                    ui.weak("No headings found.");
                    return;
                }
                let result = render_entries(
                    ui,
                    &RenderContext {
                        outline: &self.outline,
                        visible: &visible,
                        min_level,
                        max_depth,
                        expanded: &self.expanded,
                        active_index: self.active_index,
                        source: &source,
                        heading_color_mode,
                    },
                );
                if let Some(action) = result {
                    self.pending_scroll = Some(NavScrollTarget::ByteOffset(action.byte_offset));
                    if let Some(toggle_idx) = action.toggle_idx {
                        if self.expanded.contains(&toggle_idx) {
                            self.expanded.remove(&toggle_idx);
                        } else {
                            self.expanded.insert(toggle_idx);
                        }
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
    toggle_idx: Option<usize>,
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
    min_level: u8,
    max_depth: u8,
    expanded: &'a HashSet<usize>,
    active_index: Option<usize>,
    source: &'a str,
    heading_color_mode: bool,
}

/// Compute which outline indices are visible given the current expanded set.
/// Uses a parent stack for O(n) traversal instead of O(n²) reverse scans.
fn compute_visible_headings(
    outline: &[HeadingEntry],
    expanded: &HashSet<usize>,
    max_depth: u8,
    min_level: u8,
) -> Vec<usize> {
    let mut visible = Vec::new();
    let mut is_visible = vec![false; outline.len()];
    // Stack of ancestor indices; top has the nearest parent at a lower level.
    let mut parent_stack: Vec<usize> = Vec::new();

    for (idx, h) in outline.iter().enumerate() {
        if h.level > max_depth {
            continue;
        }
        // Pop ancestors at same or deeper level to find the direct parent.
        while parent_stack
            .last()
            .is_some_and(|&p| outline[p].level >= h.level)
        {
            parent_stack.pop();
        }

        let show = if h.level <= min_level {
            true
        } else {
            parent_stack
                .last()
                .is_some_and(|&p| is_visible[p] && expanded.contains(&p))
        };

        if show {
            is_visible[idx] = true;
            visible.push(idx);
        }
        parent_stack.push(idx);
    }
    visible
}

/// Check whether heading at `idx` has any children within `max_depth`.
fn has_visible_children(outline: &[HeadingEntry], max_depth: u8, idx: usize) -> bool {
    let level = outline[idx].level;
    for h in &outline[idx + 1..] {
        if h.level <= level {
            break;
        }
        if h.level <= max_depth {
            return true;
        }
    }
    false
}

fn render_entries(ui: &mut egui::Ui, cx: &RenderContext<'_>) -> Option<ClickAction> {
    let mut result = None;

    for &gi in cx.visible {
        let h = &cx.outline[gi];
        let has_children = has_visible_children(cx.outline, cx.max_depth, gi);
        let is_expanded = cx.expanded.contains(&gi);

        let style = RowStyle {
            indent: f32::from(h.level.saturating_sub(cx.min_level)) * NAV_INDENT_PX,
            is_active: cx.active_index == Some(gi),
            has_children,
            is_expanded,
        };

        if render_heading_row(ui, h, &style, cx.source, cx.heading_color_mode).clicked() {
            result = Some(ClickAction {
                byte_offset: h.byte_offset,
                toggle_idx: if has_children { Some(gi) } else { None },
            });
        }
    }

    result
}

fn render_heading_row(
    ui: &mut egui::Ui,
    heading: &HeadingEntry,
    style: &RowStyle,
    source: &str,
    heading_color_mode: bool,
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
        let mut text = egui::RichText::new(label).small();
        if style.is_active {
            text = text.strong();
        }
        if heading_color_mode {
            let color = highlight::heading_color(ui.visuals(), heading.level as usize, true);
            text = text.color(color);
        }

        let response = ui.add(
            egui::Label::new(text)
                .truncate()
                .sense(egui::Sense::click()),
        );
        // Override the text-select cursor that click-sense produces.
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
        }
        response
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
    fn refresh_outline_resets_expanded() {
        let mut state = make_state("# A\n## B\n");
        state.expanded.insert(0);
        let s = Arc::new("# A\n## B\n### C\n".to_owned());
        state.refresh_outline(&s, 2);
        assert!(state.expanded.is_empty());
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
        let y = preview_byte_to_scroll_y(&outline, mid_offset, 5000.0);
        let byte = preview_scroll_y_to_byte(&outline, y, 5000.0);
        assert_eq!(byte, mid_offset);
    }

    #[test]
    fn preview_scroll_empty_outline() {
        assert_eq!(preview_byte_to_scroll_y(&[], 100, 1000.0), 0.0);
        assert_eq!(preview_scroll_y_to_byte(&[], 100.0, 1000.0), 0);
    }

    #[test]
    fn update_active_advances_through_headings() {
        let mut state = make_state("# A\n\ntext\n\n## B\n\nmore\n\n### C\n");
        state.update_active_from_position(0);
        let first = state.active_index;
        let last_offset = state.outline.last().map_or(0, |h| h.byte_offset);
        state.update_active_from_position(last_offset);
        let last = state.active_index;
        // A later byte position should advance to a later (or equal) heading.
        assert!(last >= first);
    }

    #[test]
    fn compute_visible_only_top_level_when_collapsed() {
        let md = "# A\n## B\n### C\n# D\n## E\n";
        let outline = nav_outline::extract_headings(md);
        let expanded = HashSet::new();
        let visible = compute_visible_headings(&outline, &expanded, 4, 1);
        // Only H1 headings visible when nothing is expanded.
        let levels: Vec<_> = visible.iter().map(|&i| outline[i].level).collect();
        assert_eq!(levels, vec![1, 1]);
    }

    #[test]
    fn compute_visible_expand_shows_direct_children_only() {
        let md = "# A\n## B\n### C\n# D\n";
        let outline = nav_outline::extract_headings(md);
        // Expand A (index 0) — should reveal B but not C.
        let mut expanded = HashSet::new();
        expanded.insert(0);
        let visible = compute_visible_headings(&outline, &expanded, 4, 1);
        let levels: Vec<_> = visible.iter().map(|&i| outline[i].level).collect();
        assert_eq!(levels, vec![1, 2, 1]); // A, B, D

        // Now also expand B — should reveal C.
        expanded.insert(1);
        let visible = compute_visible_headings(&outline, &expanded, 4, 1);
        let levels: Vec<_> = visible.iter().map(|&i| outline[i].level).collect();
        assert_eq!(levels, vec![1, 2, 3, 1]); // A, B, C, D
    }

    #[test]
    fn has_visible_children_detects_children() {
        let md = "# A\n## B\n# C\n";
        let outline = nav_outline::extract_headings(md);
        assert!(has_visible_children(&outline, 4, 0)); // A has child B
        assert!(!has_visible_children(&outline, 4, 1)); // B has no children
        assert!(!has_visible_children(&outline, 4, 2)); // C has no children
    }

    #[test]
    fn has_visible_children_respects_max_depth() {
        let md = "# A\n### C\n";
        let outline = nav_outline::extract_headings(md);
        assert!(has_visible_children(&outline, 4, 0)); // C is within depth
        assert!(!has_visible_children(&outline, 1, 0)); // C exceeds max_depth=1
    }

    #[test]
    fn compute_visible_deeply_nested() {
        // 4 levels deep: expand all ancestors to reveal the deepest.
        let md = "# A\n## B\n### C\n#### D\n";
        let outline = nav_outline::extract_headings(md);
        let mut expanded = HashSet::new();
        // Nothing expanded: only H1 visible.
        let visible = compute_visible_headings(&outline, &expanded, 4, 1);
        assert_eq!(visible, vec![0]);
        // Expand A: B visible.
        expanded.insert(0);
        let visible = compute_visible_headings(&outline, &expanded, 4, 1);
        assert_eq!(visible, vec![0, 1]);
        // Expand B: C visible.
        expanded.insert(1);
        let visible = compute_visible_headings(&outline, &expanded, 4, 1);
        assert_eq!(visible, vec![0, 1, 2]);
        // Expand C: D visible.
        expanded.insert(2);
        let visible = compute_visible_headings(&outline, &expanded, 4, 1);
        assert_eq!(visible, vec![0, 1, 2, 3]);
    }

    #[test]
    fn compute_visible_skipped_levels() {
        // H1 followed directly by H3 (skipping H2).
        let md = "# A\n### C\n## B\n";
        let outline = nav_outline::extract_headings(md);
        let mut expanded = HashSet::new();
        // Expand A: both C and B are direct children (C at level 3, B at level 2).
        expanded.insert(0);
        let visible = compute_visible_headings(&outline, &expanded, 4, 1);
        let levels: Vec<_> = visible.iter().map(|&i| outline[i].level).collect();
        assert_eq!(levels, vec![1, 3, 2]);
    }

    #[test]
    fn compute_visible_collapse_parent_hides_grandchildren() {
        let md = "# A\n## B\n### C\n";
        let outline = nav_outline::extract_headings(md);
        let mut expanded = HashSet::new();
        expanded.insert(0);
        expanded.insert(1);
        // All visible.
        let visible = compute_visible_headings(&outline, &expanded, 4, 1);
        assert_eq!(visible, vec![0, 1, 2]);
        // Collapse A: B and C should be hidden even though B is still "expanded".
        expanded.remove(&0);
        let visible = compute_visible_headings(&outline, &expanded, 4, 1);
        assert_eq!(visible, vec![0]);
    }

    #[test]
    fn compute_visible_empty_outline() {
        let outline: Vec<HeadingEntry> = Vec::new();
        let expanded = HashSet::new();
        let visible = compute_visible_headings(&outline, &expanded, 4, 1);
        assert!(visible.is_empty());
    }

    #[test]
    fn compute_visible_all_same_level() {
        // All headings at the same level: all visible, none expandable.
        let md = "## A\n## B\n## C\n";
        let outline = nav_outline::extract_headings(md);
        let expanded = HashSet::new();
        let visible = compute_visible_headings(&outline, &expanded, 4, 2);
        assert_eq!(visible, vec![0, 1, 2]);
    }

    #[test]
    fn compute_visible_max_depth_filters() {
        let md = "# A\n## B\n### C\n#### D\n";
        let outline = nav_outline::extract_headings(md);
        let mut expanded = HashSet::new();
        expanded.insert(0);
        expanded.insert(1);
        expanded.insert(2);
        // max_depth=2 hides C and D.
        let visible = compute_visible_headings(&outline, &expanded, 2, 1);
        let levels: Vec<_> = visible.iter().map(|&i| outline[i].level).collect();
        assert_eq!(levels, vec![1, 2]);
    }

    #[test]
    fn preview_scroll_byte_to_y_monotonic() {
        let md = "# A\n\ntext\n\n## B\n\nmore\n\n### C\n";
        let outline = nav_outline::extract_headings(md);
        let total_h = 3000.0;
        let mut prev_y = 0.0_f32;
        for h in &outline {
            let y = preview_byte_to_scroll_y(&outline, h.byte_offset, total_h);
            assert!(y >= prev_y, "scroll-y should be monotonically increasing");
            prev_y = y;
        }
    }

    #[test]
    fn preview_scroll_round_trip_all_headings() {
        let md = "x".repeat(200)
            + "\n# A\n"
            + &"y".repeat(200)
            + "\n## B\n"
            + &"z".repeat(200)
            + "\n### C\n";
        let outline = nav_outline::extract_headings(&md);
        let total_h = 5000.0;
        for h in &outline {
            let y = preview_byte_to_scroll_y(&outline, h.byte_offset, total_h);
            let byte = preview_scroll_y_to_byte(&outline, y, total_h);
            assert_eq!(
                byte, h.byte_offset,
                "round-trip failed for offset {}",
                h.byte_offset
            );
        }
    }

    #[test]
    fn preview_scroll_single_heading() {
        let md = "# Only Heading\n";
        let outline = nav_outline::extract_headings(md);
        assert_eq!(preview_byte_to_scroll_y(&outline, 0, 1000.0), 0.0);
        assert_eq!(preview_scroll_y_to_byte(&outline, 0.0, 1000.0), 0);
    }

    #[test]
    fn stress_test_nav_extracts_all_headings() {
        let md = include_str!("../../../test-assets/stress-test.md");
        let state = make_state(md);
        assert!(
            state.outline.len() > 50,
            "stress test has many headings, got {}",
            state.outline.len()
        );

        // All heading levels 1-6 should be present
        for level in 1..=6 {
            assert!(
                state.outline.iter().any(|h| h.level == level),
                "expected heading level {level} in stress test"
            );
        }

        // Byte offsets should be strictly increasing
        for w in state.outline.windows(2) {
            assert!(
                w[1].byte_offset > w[0].byte_offset,
                "heading offsets should increase: {} then {}",
                w[0].byte_offset,
                w[1].byte_offset
            );
        }
    }

    #[test]
    fn stress_test_nav_expand_collapse() {
        let md = include_str!("../../../test-assets/stress-test.md");
        let mut state = make_state(md);

        // Default depth=4 — should show some but not all headings
        let all = compute_visible_headings(&state.outline, &state.expanded, state.max_depth, 1);
        assert!(
            !all.is_empty(),
            "should have visible headings at default depth"
        );
        assert!(
            all.len() <= state.outline.len(),
            "visible should be <= total outline"
        );

        // Expand first heading — should reveal more
        state.expanded.insert(all[0]);
        let expanded =
            compute_visible_headings(&state.outline, &state.expanded, state.max_depth, 1);
        assert!(
            expanded.len() >= all.len(),
            "expanding first heading should show at least as many entries"
        );

        // Collapse — should return to original
        state.expanded.remove(&all[0]);
        let collapsed =
            compute_visible_headings(&state.outline, &state.expanded, state.max_depth, 1);
        assert_eq!(collapsed.len(), all.len());
    }

    #[test]
    fn stress_test_nav_scroll_targets_valid() {
        let md = include_str!("../../../test-assets/stress-test.md");
        let state = make_state(md);
        let total_h = 10_000.0; // reasonable preview height

        for h in &state.outline {
            let y = preview_byte_to_scroll_y(&state.outline, h.byte_offset, total_h);
            assert!(
                y >= 0.0 && y <= total_h,
                "scroll target for offset {} should be in [0, {}], got {}",
                h.byte_offset,
                total_h,
                y,
            );
        }
    }

    #[test]
    fn stress_test_heading_labels_non_empty() {
        let md = include_str!("../../../test-assets/stress-test.md");
        let state = make_state(md);

        for (i, h) in state.outline.iter().enumerate() {
            let label = h.label(md);
            assert!(
                !label.is_empty(),
                "heading {i} at offset {} should have non-empty label",
                h.byte_offset
            );
        }
    }
}
