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
    /// Bitset of outline indices whose children are expanded (`Vec<bool>` is
    /// faster than `HashSet` for small sets typical of document outlines).
    expanded: Vec<bool>,
    /// The heading index the user is currently scrolled to.
    active_index: Option<usize>,
    /// Pending scroll request for the host to execute.
    pub pending_scroll: Option<NavScrollTarget>,
    /// Pending editor scroll target in pixels.
    pub pending_editor_scroll_y: Option<f32>,
    /// Pending preview scroll target in pixels.
    pub pending_preview_scroll_y: Option<f32>,
    /// Cached visible heading indices with precomputed `has_children` flag.
    cached_visible: Vec<(usize, bool)>,
    /// Sequence counter for visible-heading cache invalidation.
    visible_seq: u64,
    /// The `max_depth` when `cached_visible` was last computed.
    visible_max_depth: u8,
    /// Dirty counter for expanded set — incremented on any expand/collapse.
    expanded_gen: u64,
    /// The generation at which visible headings were last recomputed.
    visible_expanded_gen: u64,
    /// Cached min heading level for the current outline + `max_depth`.
    cached_min_level: u8,
    /// The `outline_seq` and `max_depth` when `cached_min_level` was computed.
    min_level_seq: u64,
    min_level_depth: u8,
}

impl Default for NavState {
    fn default() -> Self {
        Self {
            visible: false,
            outline: Vec::new(),
            outline_source: Arc::new(String::new()),
            outline_seq: u64::MAX,
            max_depth: 4,
            heading_color_mode: true,
            expanded: Vec::new(),
            active_index: None,
            pending_scroll: None,
            pending_editor_scroll_y: None,
            pending_preview_scroll_y: None,
            cached_visible: Vec::new(),
            visible_seq: u64::MAX,
            visible_max_depth: 0,
            expanded_gen: 0,
            visible_expanded_gen: u64::MAX,
            cached_min_level: 1,
            min_level_seq: u64::MAX,
            min_level_depth: 0,
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

/// Convert `byte_offset` to an estimated preview scroll-y value using
/// piecewise-linear interpolation between heading waypoints.
/// Returns `0.0` when the outline is empty or all headings are at offset 0.
#[allow(clippy::cast_precision_loss)]
pub fn preview_byte_to_scroll_y(
    outline: &[HeadingEntry],
    byte_offset: usize,
    total_height: f32,
) -> f32 {
    if total_height <= 0.0 {
        return 0.0;
    }
    let max_byte = match outline.last() {
        Some(h) if h.byte_offset > 0 => h.byte_offset,
        _ => return 0.0,
    };

    // Fewer than 2 headings: fall back to simple linear mapping.
    if outline.len() < 2 {
        return (byte_offset as f32 / max_byte as f32 * total_height).clamp(0.0, total_height);
    }

    let n = outline.len();

    // Find the first heading whose byte_offset is strictly greater than byte_offset.
    let after_idx = outline.partition_point(|h| h.byte_offset <= byte_offset);

    if after_idx == 0 {
        // Before the first heading.
        let first = &outline[0];
        if first.byte_offset == 0 {
            return 0.0;
        }
        return 0.0;
    }

    if after_idx >= n {
        // After the last heading.
        let last = &outline[n - 1];
        let last_y = total_height * ((n - 1) as f32 / n as f32);
        let remaining_bytes = max_byte.saturating_sub(last.byte_offset) as f32;
        if remaining_bytes <= 0.0 {
            return last_y;
        }
        let frac = (byte_offset - last.byte_offset) as f32 / remaining_bytes;
        return last_y + frac * (total_height - last_y);
    }

    // Between two headings — interpolate.
    let before = &outline[after_idx - 1];
    let after = &outline[after_idx];
    let before_y = total_height * ((after_idx - 1) as f32 / n as f32);
    let after_y = total_height * (after_idx as f32 / n as f32);

    let byte_range = (after.byte_offset - before.byte_offset) as f32;
    if byte_range <= 0.0 {
        return before_y;
    }
    let frac = (byte_offset - before.byte_offset) as f32 / byte_range;
    frac.mul_add(after_y - before_y, before_y)
}

/// Convert a preview scroll-y value to an estimated byte offset using
/// the inverse piecewise-linear mapping.
/// Returns `0` when the outline is empty.
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
    if total_height <= 0.0 {
        return 0;
    }
    let max_byte = match outline.last() {
        Some(h) if h.byte_offset > 0 => h.byte_offset,
        _ => return 0,
    };

    // Fewer than 2 headings: fall back to simple linear mapping.
    if outline.len() < 2 {
        return (scroll_y / total_height * max_byte as f32).clamp(0.0, max_byte as f32) as usize;
    }

    let n = outline.len();
    let scroll_y = scroll_y.clamp(0.0, total_height);

    // Find which heading band the scroll_y falls in.
    // Heading i maps to y = total_height * (i / n).
    let slot = scroll_y / total_height * n as f32;
    let before_idx = (slot as usize).min(n - 1);

    if before_idx >= n - 1 {
        // In the last band (after last heading to end).
        let last = &outline[n - 1];
        let last_y = total_height * ((n - 1) as f32 / n as f32);
        let band_height = total_height - last_y;
        if band_height <= 0.0 {
            return last.byte_offset;
        }
        let frac = ((scroll_y - last_y) / band_height).clamp(0.0, 1.0);
        let remaining_bytes = max_byte.saturating_sub(last.byte_offset);
        return last.byte_offset + (frac * remaining_bytes as f32) as usize;
    }

    let before = &outline[before_idx];
    let after = &outline[before_idx + 1];
    let before_y = total_height * (before_idx as f32 / n as f32);
    let after_y = total_height * ((before_idx + 1) as f32 / n as f32);
    let band_height = after_y - before_y;
    if band_height <= 0.0 {
        return before.byte_offset;
    }
    let frac = ((scroll_y - before_y) / band_height).clamp(0.0, 1.0);
    let byte_range = after.byte_offset - before.byte_offset;
    before.byte_offset + (frac * byte_range as f32) as usize
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
        self.expanded.resize(self.outline.len(), false);
        self.expanded_gen = self.expanded_gen.wrapping_add(1);
        let mut h1_indices = self
            .outline
            .iter()
            .enumerate()
            .filter(|(_, h)| h.level == 1)
            .map(|(idx, _)| idx);
        if let Some(sole_h1) = h1_indices.next()
            && h1_indices.next().is_none()
        {
            self.expanded[sole_h1] = true;
        }
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

    /// Cheap hash of the expanded set — used only in tests.
    #[cfg(test)]
    fn expanded_hash(&self) -> u64 {
        let mut h: u64 = self.expanded.len() as u64;
        for (idx, &val) in self.expanded.iter().enumerate() {
            if val {
                h ^= (idx as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
            }
        }
        h
    }

    /// Show the navigation panel.
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.visible {
            return;
        }

        let panel_frame = egui::Frame::new()
            .fill(ctx.style().visuals.panel_fill)
            .inner_margin(6);

        egui::SidePanel::left("navigation")
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

        // Cache min_level — only recompute when outline or max_depth changes.
        if self.min_level_seq != self.outline_seq || self.min_level_depth != self.max_depth {
            self.cached_min_level = self
                .outline
                .iter()
                .filter(|h| h.level <= self.max_depth)
                .map(|h| h.level)
                .min()
                .unwrap_or(1);
            self.min_level_seq = self.outline_seq;
            self.min_level_depth = self.max_depth;
        }
        let min_level = self.cached_min_level;

        let source = Arc::clone(&self.outline_source);
        let max_depth = self.max_depth;
        let heading_color_mode = self.heading_color_mode;

        // Compute visible headings: a heading is visible if it's at min_level
        // or all its ancestors are expanded.  Cache to avoid Vec allocation each frame.
        // Use generation counter instead of per-frame hash computation.
        let expanded_dirty = self.visible_expanded_gen != self.expanded_gen;
        if self.visible_seq != self.outline_seq
            || expanded_dirty
            || self.visible_max_depth != max_depth
        {
            self.cached_visible =
                compute_visible_headings(&self.outline, &self.expanded, max_depth, min_level);
            self.visible_seq = self.outline_seq;
            self.visible_expanded_gen = self.expanded_gen;
            self.visible_max_depth = max_depth;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                if self.cached_visible.is_empty() {
                    ui.weak("No headings found.");
                    return;
                }
                let result = render_entries(
                    ui,
                    &RenderContext {
                        outline: &self.outline,
                        visible: &self.cached_visible,
                        min_level,
                        expanded: &self.expanded,
                        active_index: self.active_index,
                        source: &source,
                        heading_color_mode,
                    },
                );
                if let Some(action) = result {
                    self.pending_scroll = Some(NavScrollTarget::ByteOffset(action.byte_offset));
                    if let Some(toggle_idx) = action.toggle_idx
                        && toggle_idx < self.expanded.len()
                    {
                        self.expanded[toggle_idx] = !self.expanded[toggle_idx];
                        self.expanded_gen = self.expanded_gen.wrapping_add(1);
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
            const DEPTH_LABELS: [&str; 7] = [
                "H1–H0", "H1–H1", "H1–H2", "H1–H3", "H1–H4", "H1–H5", "H1–H6",
            ];
            ui.label(DEPTH_LABELS[(self.max_depth as usize).min(6)]);
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
    visible: &'a [(usize, bool)],
    min_level: u8,
    expanded: &'a [bool],
    active_index: Option<usize>,
    source: &'a str,
    heading_color_mode: bool,
}

/// Compute which outline indices are visible given the current expanded set.
/// Uses a parent stack for O(n) traversal instead of O(n²) reverse scans.
/// Also precomputes which headings have children within `max_depth` in a
/// single backward pass (O(n) total instead of O(n²) per render call).
fn compute_visible_headings(
    outline: &[HeadingEntry],
    expanded: &[bool],
    max_depth: u8,
    min_level: u8,
) -> Vec<(usize, bool)> {
    // Precompute has_children in a backward pass: O(n).
    let mut has_children = vec![false; outline.len()];
    for i in (0..outline.len()).rev() {
        if outline[i].level > max_depth {
            continue;
        }
        let level = outline[i].level;
        for h in &outline[i + 1..] {
            if h.level <= level {
                break;
            }
            if h.level <= max_depth {
                has_children[i] = true;
                break;
            }
        }
    }

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
                .is_some_and(|&p| is_visible[p] && expanded.get(p).copied().unwrap_or(false))
        };

        if show {
            is_visible[idx] = true;
            visible.push((idx, has_children[idx]));
        }
        parent_stack.push(idx);
    }
    visible
}

fn render_entries(ui: &mut egui::Ui, cx: &RenderContext<'_>) -> Option<ClickAction> {
    let mut result = None;

    for &(gi, has_children) in cx.visible {
        let h = &cx.outline[gi];
        let is_expanded = cx.expanded.get(gi).copied().unwrap_or(false);

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

        ui.add(
            egui::Label::new(text)
                .truncate()
                .selectable(false)
                .sense(egui::Sense::click()),
        )
    })
    .inner
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    /// Extract just the outline indices from a visible headings list.
    fn indices(v: &[(usize, bool)]) -> Vec<usize> {
        v.iter().map(|&(i, _)| i).collect()
    }

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
    fn refresh_outline_caching_expansion_and_auto_expand() {
        // Caches by edit_seq.
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

        // Resets expanded on rebuild.
        let mut state = make_state("# A\n## B\n");
        state.expanded[1] = true;
        let s = Arc::new("# A\n## B\n### C\n".to_owned());
        state.refresh_outline(&s, 2);
        assert_eq!(state.expanded, vec![true, false, false]);

        // Auto-expands single H1 only.
        let mut state = NavState::default();
        state.refresh_outline(&Arc::new("# A\n## B\n".to_owned()), 1);
        assert_eq!(state.expanded, vec![true, false]);

        let mut state = NavState::default();
        state.refresh_outline(&Arc::new("# A\n## B\n# C\n".to_owned()), 1);
        assert!(!state.expanded.iter().any(|v| *v));

        let mut state = NavState::default();
        state.refresh_outline(&Arc::new("## A\n## B\n".to_owned()), 1);
        assert!(!state.expanded.iter().any(|v| *v));
    }

    #[test]
    fn depth_default_clamp_and_round_trip() {
        let state = NavState::default();
        assert_eq!(state.max_depth, 4);

        // Clamp at minimum
        let mut state = NavState {
            max_depth: 1,
            ..NavState::default()
        };
        state.decrease_depth();
        assert_eq!(state.max_depth, 1, "clamp at 1");
        for _ in 0..11 {
            state.decrease_depth();
        }
        assert_eq!(state.max_depth, 1, "stays at 1 after repeated decrease");

        // Clamp at maximum
        let mut state = NavState {
            max_depth: 6,
            ..NavState::default()
        };
        state.increase_depth();
        assert_eq!(state.max_depth, 6, "clamp at 6");
        for _ in 0..11 {
            state.increase_depth();
        }
        assert_eq!(state.max_depth, 6, "stays at 6 after repeated increase");

        // Round-trip
        let mut state = NavState::default();
        state.decrease_depth();
        assert_eq!(state.max_depth, 3);
        state.increase_depth();
        assert_eq!(state.max_depth, 4);

        // DEPTH_LABELS index clamp.
        const DEPTH_LABELS: [&str; 7] = [
            "H1–H0", "H1–H1", "H1–H2", "H1–H3", "H1–H4", "H1–H5", "H1–H6",
        ];
        for depth in [0u8, 1, 6, 7, 100, 255] {
            let _ = DEPTH_LABELS[(depth as usize).min(6)];
        }
    }

    #[test]
    fn update_active_from_position_cases() {
        let mut state = make_state("# A\n\ntext\n\n## B\n\nmore\n\n### C\n");
        let b_offset = state.outline[1].byte_offset;
        state.update_active_from_position(b_offset);
        assert_eq!(state.active_index, Some(1));

        // Respects max_depth.
        let mut state = make_state("# A\n\n#### D\n\n## B\n");
        state.max_depth = 2;
        state.update_active_from_position(state.outline[1].byte_offset);
        assert_eq!(state.active_index, Some(0));

        // Advances monotonically.
        let mut state = make_state("# A\n\ntext\n\n## B\n\nmore\n\n### C\n");
        state.update_active_from_position(0);
        let first = state.active_index;
        state.update_active_from_position(state.outline.last().map_or(0, |h| h.byte_offset));
        assert!(state.active_index >= first);

        // Beyond all headings.
        let mut state = make_state("# A\n## B\n");
        state.update_active_from_position(999_999);
        assert!(state.active_index.is_some());
    }

    #[test]
    fn preview_scroll_edge_cases_and_variants() {
        // Scroll target variants.
        let s1 = NavState {
            pending_scroll: Some(NavScrollTarget::Top),
            ..NavState::default()
        };
        assert_eq!(s1.pending_scroll, Some(NavScrollTarget::Top));
        let s2 = NavState {
            pending_scroll: Some(NavScrollTarget::ByteOffset(42)),
            ..NavState::default()
        };
        assert_eq!(s2.pending_scroll, Some(NavScrollTarget::ByteOffset(42)));

        // Empty outline.
        assert_eq!(preview_byte_to_scroll_y(&[], 100, 1000.0), 0.0);
        assert_eq!(preview_scroll_y_to_byte(&[], 100.0, 1000.0), 0);

        // Single heading.
        let md = "# Only Heading\n";
        let outline = nav_outline::extract_headings(md);
        assert_eq!(preview_byte_to_scroll_y(&outline, 0, 1000.0), 0.0);
        assert_eq!(preview_scroll_y_to_byte(&outline, 0.0, 1000.0), 0);

        // Zero and negative total height.
        let md = "# A\n## B\n";
        let outline = nav_outline::extract_headings(md);
        assert_eq!(preview_byte_to_scroll_y(&outline, 10, 0.0), 0.0);
        assert_eq!(preview_scroll_y_to_byte(&outline, 10.0, 0.0), 0);
        assert_eq!(preview_byte_to_scroll_y(&outline, 10, -100.0), 0.0);
        assert_eq!(preview_scroll_y_to_byte(&outline, 10.0, -100.0), 0);
    }

    #[test]
    fn compute_visible_core_behaviors() {
        // Only top-level when collapsed.
        let md = "# A\n## B\n### C\n# D\n## E\n";
        let outline = nav_outline::extract_headings(md);
        let expanded = vec![false; outline.len()];
        let visible = compute_visible_headings(&outline, &expanded, 4, 1);
        let levels: Vec<_> = visible.iter().map(|&(i, _)| outline[i].level).collect();
        assert_eq!(levels, vec![1, 1], "only top-level when collapsed");

        // Expand shows direct children only.
        let md = "# A\n## B\n### C\n# D\n";
        let outline = nav_outline::extract_headings(md);
        let mut expanded = vec![false; outline.len()];
        expanded[0] = true;
        let visible = compute_visible_headings(&outline, &expanded, 4, 1);
        let levels: Vec<_> = visible.iter().map(|&(i, _)| outline[i].level).collect();
        assert_eq!(levels, vec![1, 2, 1], "expand A shows B");
        expanded[1] = true;
        let visible = compute_visible_headings(&outline, &expanded, 4, 1);
        let levels: Vec<_> = visible.iter().map(|&(i, _)| outline[i].level).collect();
        assert_eq!(levels, vec![1, 2, 3, 1], "expand B shows C");

        // Empty outline.
        let empty: Vec<HeadingEntry> = Vec::new();
        assert!(compute_visible_headings(&empty, &Vec::new(), 4, 1).is_empty());

        // All same level.
        let md = "## A\n## B\n## C\n";
        let outline = nav_outline::extract_headings(md);
        let visible = compute_visible_headings(&outline, &[false; 3], 4, 2);
        assert_eq!(indices(&visible), vec![0, 1, 2]);

        // Single heading.
        let md = "# Only\n";
        let outline = nav_outline::extract_headings(md);
        let visible = compute_visible_headings(&outline, &Vec::new(), 4, 1);
        assert_eq!(visible.len(), 1);
        assert!(!visible[0].1);

        // Headings beyond max_depth.
        let md = "##### H5\n###### H6\n";
        let outline = nav_outline::extract_headings(md);
        assert!(compute_visible_headings(&outline, &Vec::new(), 4, 1).is_empty());

        // max_depth=1 hides all subheadings and H1s don't report children.
        let md = "# A\n## B\n### C\n# D\n## E\n";
        let outline = nav_outline::extract_headings(md);
        let visible = compute_visible_headings(&outline, &Vec::new(), 1, 1);
        let levels: Vec<_> = visible.iter().map(|&(i, _)| outline[i].level).collect();
        assert_eq!(levels, vec![1, 1]);
        assert!(!visible[0].1);
        assert!(!visible[1].1);
    }

    #[test]
    fn compute_visible_children_depth_nesting_and_collapse() {
        // has_children flag.
        let md = "# A\n## B\n# C\n";
        let outline = nav_outline::extract_headings(md);
        let expanded = {
            let mut v = vec![false; 3];
            v[0] = true;
            v[2] = true;
            v
        };
        let children: Vec<_> = compute_visible_headings(&outline, &expanded, 4, 1)
            .iter()
            .map(|&(_, hc)| hc)
            .collect();
        assert_eq!(children, vec![true, false, false]);

        // has_children respects max_depth.
        let md = "# A\n### C\n";
        let outline = nav_outline::extract_headings(md);
        assert!(compute_visible_headings(&outline, &[true], 4, 1)[0].1);
        assert!(!compute_visible_headings(&outline, &Vec::new(), 1, 1)[0].1);

        // Deeply nested: expand ancestors to reveal deepest.
        let md = "# A\n## B\n### C\n#### D\n";
        let outline = nav_outline::extract_headings(md);
        let mut exp = vec![false; outline.len()];
        for expected_len in 1..=outline.len() {
            assert_eq!(
                indices(&compute_visible_headings(&outline, &exp, 4, 1)),
                (0..expected_len).collect::<Vec<_>>()
            );
            if expected_len < outline.len() {
                exp[expected_len - 1] = true;
            }
        }

        // Skipped levels: H1 → H3 → H2.
        let md = "# A\n### C\n## B\n";
        let outline = nav_outline::extract_headings(md);
        let levels: Vec<_> = compute_visible_headings(&outline, &[true, false, false], 4, 1)
            .iter()
            .map(|&(i, _)| outline[i].level)
            .collect();
        assert_eq!(levels, vec![1, 3, 2]);

        // Collapse parent hides grandchildren.
        let md = "# A\n## B\n### C\n";
        let outline = nav_outline::extract_headings(md);
        let mut exp = vec![true, true, false];
        assert_eq!(
            indices(&compute_visible_headings(&outline, &exp, 4, 1)),
            vec![0, 1, 2]
        );
        exp[0] = false;
        assert_eq!(
            indices(&compute_visible_headings(&outline, &exp, 4, 1)),
            vec![0]
        );

        // max_depth=2 hides deeper headings.
        let md = "# A\n## B\n### C\n#### D\n";
        let outline = nav_outline::extract_headings(md);
        let levels: Vec<_> = compute_visible_headings(&outline, &[true, true, true, false], 2, 1)
            .iter()
            .map(|&(i, _)| outline[i].level)
            .collect();
        assert_eq!(levels, vec![1, 2]);
    }

    #[test]
    fn preview_scroll_round_trip_and_monotonicity() {
        // Monotonic byte → y.
        let md = "# A\n\ntext\n\n## B\n\nmore\n\n### C\n";
        let outline = nav_outline::extract_headings(md);
        let mut prev_y = 0.0_f32;
        for h in &outline {
            let y = preview_byte_to_scroll_y(&outline, h.byte_offset, 3000.0);
            assert!(y >= prev_y, "monotonic");
            prev_y = y;
        }

        // Round trip with spread-out content.
        let md = "x".repeat(200)
            + "\n# A\n"
            + &"y".repeat(200)
            + "\n## B\n"
            + &"z".repeat(200)
            + "\n### C\n";
        let outline = nav_outline::extract_headings(&md);
        for h in &outline {
            let y = preview_byte_to_scroll_y(&outline, h.byte_offset, 5000.0);
            assert_eq!(
                preview_scroll_y_to_byte(&outline, y, 5000.0),
                h.byte_offset,
                "round-trip offset {}",
                h.byte_offset
            );
        }

        // Round trip with multiple headings and midpoints.
        let md = "x".repeat(500)
            + "\n# A\n"
            + &"y".repeat(500)
            + "\n## B\n"
            + &"z".repeat(500)
            + "\n## C\n";
        let outline = nav_outline::extract_headings(&md);
        let mid_offset = outline[1].byte_offset;
        let y = preview_byte_to_scroll_y(&outline, mid_offset, 5000.0);
        assert_eq!(preview_scroll_y_to_byte(&outline, y, 5000.0), mid_offset);
    }

    #[test]
    fn stress_test_nav_headings_and_expand_collapse() {
        let md = include_str!("../../../test-assets/stress-test.md");
        let mut state = make_state(md);

        // All headings extracted with levels 1-6 and increasing offsets.
        assert!(state.outline.len() > 50);
        for level in 1..=6 {
            assert!(
                state.outline.iter().any(|h| h.level == level),
                "level {level}"
            );
        }
        for w in state.outline.windows(2) {
            assert!(w[1].byte_offset > w[0].byte_offset);
        }

        // Expand/collapse round-trip.
        state.expanded.fill(false);
        let all = compute_visible_headings(&state.outline, &state.expanded, state.max_depth, 1);
        assert!(!all.is_empty() && all.len() <= state.outline.len());
        state.expanded[all[0].0] = true;
        let expanded =
            compute_visible_headings(&state.outline, &state.expanded, state.max_depth, 1);
        assert!(expanded.len() >= all.len());
        state.expanded[all[0].0] = false;
        assert_eq!(
            compute_visible_headings(&state.outline, &state.expanded, state.max_depth, 1).len(),
            all.len()
        );

        // All heading labels non-empty and scroll targets in range
        let total_h = 10_000.0;
        for (i, h) in state.outline.iter().enumerate() {
            let label = h.label(md);
            assert!(!label.is_empty(), "heading {i} should have non-empty label");
            let y = preview_byte_to_scroll_y(&state.outline, h.byte_offset, total_h);
            assert!(y >= 0.0 && y <= total_h, "scroll target out of range: {y}");
        }
    }

    #[test]
    fn expanded_hash_order_independent_and_content_sensitive() {
        let make = |indices: &[usize], len: usize| {
            let mut v = vec![false; len];
            for &i in indices {
                v[i] = true;
            }
            NavState {
                expanded: v,
                ..NavState::default()
            }
            .expanded_hash()
        };
        // Same set, same hash.
        assert_eq!(make(&[1, 5, 10, 42], 43), make(&[42, 10, 5, 1], 43));
        // Different sets, different hashes.
        assert_ne!(make(&[1, 2, 3], 4), make(&[4, 5, 6], 7));
    }

    #[test]
    #[allow(clippy::cast_possible_wrap)]
    fn preview_byte_scroll_round_trip_with_headings() {
        // Build a document with 4 headings and content between them so that
        // byte offsets are spread across a realistic range.
        let md = format!(
            "# A\n{}\n## B\n{}\n## C\n{}\n# D\n",
            "x".repeat(96),
            "y".repeat(396),
            "z".repeat(476),
        );
        let outline = nav_outline::extract_headings(&md);
        assert_eq!(outline.len(), 4, "expected 4 headings");
        let total_height = 2000.0;

        // Collect heading offsets and midpoints between consecutive headings.
        let mut test_offsets: Vec<usize> = outline.iter().map(|h| h.byte_offset).collect();
        for pair in outline.windows(2) {
            test_offsets.push(usize::midpoint(pair[0].byte_offset, pair[1].byte_offset));
        }

        for &byte in &test_offsets {
            let y = preview_byte_to_scroll_y(&outline, byte, total_height);
            let back = preview_scroll_y_to_byte(&outline, y, total_height);
            let delta = (back as i64 - byte as i64).unsigned_abs();
            assert!(
                delta < 2,
                "round-trip drift at byte={byte}: got back={back}, y={y:.2}, delta={delta}"
            );
        }

        // Boundary cases
        let md = "text\n\n# A\n\nmore\n\n## B\n";
        let outline = nav_outline::extract_headings(md);
        let first = outline.first().map_or(0, |h| h.byte_offset);
        let last = outline.last().map_or(0, |h| h.byte_offset);
        for (label, byte, total_h) in [
            ("zero", 0_usize, 1000.0_f32),
            ("at last heading", last, 1000.0),
            ("beyond max", 999_999_usize, 1000.0),
        ] {
            let y = preview_byte_to_scroll_y(&outline, byte, total_h);
            assert!((0.0..=total_h).contains(&y), "{label}: got {y}");
        }
        let outline2 = nav_outline::extract_headings("text\n\n# Only\n");
        assert!(preview_byte_to_scroll_y(&outline2, 999_999, 500.0) <= 500.0);
        assert_eq!(preview_scroll_y_to_byte(&outline, 0.0, 1000.0), first);
        assert!(preview_scroll_y_to_byte(&outline, 1000.0, 1000.0) >= last);
        assert!(preview_scroll_y_to_byte(&outline, 5000.0, 1000.0) <= last + 1000);
        assert_eq!(preview_scroll_y_to_byte(&outline, -100.0, 1000.0), first);
    }

    // ── compute_visible_headings additional tests ───────────────────

    #[test]
    fn compute_visible_advanced_cases() {
        // max_depth=6 fully expanded shows everything.
        let md = "# A\n## B\n### C\n#### D\n##### E\n###### F\n";
        let outline = nav_outline::extract_headings(md);
        let expanded = vec![true, true, true, true, true, false];
        let visible = compute_visible_headings(&outline, &expanded, 6, 1);
        assert_eq!(indices(&visible), vec![0, 1, 2, 3, 4, 5], "full depth");

        // Alternating H1/H6 — collapsed only shows H1s.
        let md = "# A\n###### deep\n# B\n###### deeper\n";
        let outline = nav_outline::extract_headings(md);
        let visible = compute_visible_headings(&outline, &Vec::new(), 6, 1);
        assert_eq!(indices(&visible), vec![0, 2], "alternating collapsed");
        let mut exp = vec![false; 1];
        exp[0] = true;
        let visible = compute_visible_headings(&outline, &exp, 6, 1);
        assert_eq!(indices(&visible), vec![0, 1, 2], "alternating expand first");

        // All H1 — none have children.
        let md = "# A\n# B\n# C\n# D\n";
        let outline = nav_outline::extract_headings(md);
        let visible = compute_visible_headings(&outline, &Vec::new(), 6, 1);
        assert_eq!(visible.len(), 4);
        for &(_, hc) in &visible {
            assert!(!hc, "all H1s — none should have children");
        }
    }
}
