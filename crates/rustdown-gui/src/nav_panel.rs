#![forbid(unsafe_code)]

use eframe::egui;

use crate::nav_outline::{self, HeadingEntry};

/// What the nav panel wants the host to scroll to.
#[derive(Debug, Clone, PartialEq)]
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
    /// The `edit_seq` that produced the current outline.
    outline_seq: u64,
    /// Maximum heading depth to display (1..=6, default 4).
    pub max_depth: u8,
    /// Which top-level (min-level) heading *position* is expanded (accordion).
    /// `None` means nothing expanded.
    expanded_pos: Option<usize>,
    /// The heading index the user is currently scrolled to.
    pub active_index: Option<usize>,
    /// Pending scroll request for the host to execute.
    pub pending_scroll: Option<NavScrollTarget>,
}

impl Default for NavState {
    fn default() -> Self {
        Self {
            visible: false,
            outline: Vec::new(),
            outline_seq: u64::MAX,
            max_depth: 4,
            expanded_pos: None,
            active_index: None,
            pending_scroll: None,
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

/// Compute the scroll-area [`egui::Id`] used by the preview pane.
pub fn preview_scroll_id() -> egui::Id {
    egui::Id::new("preview_markdown").with("_scroll_area")
}

impl NavState {
    /// Rebuild the heading outline if the document has changed.
    pub fn refresh_outline(&mut self, source: &str, edit_seq: u64) {
        if edit_seq == self.outline_seq {
            return;
        }
        self.outline = nav_outline::extract_headings(source);
        self.outline_seq = edit_seq;
        self.expanded_pos = None;
    }

    /// Update `active_index` from a byte position in the document.
    pub fn update_active_from_position(&mut self, byte_position: usize) {
        self.active_index =
            nav_outline::active_heading_index(&self.outline, self.max_depth, byte_position);
    }

    /// Update `active_index` for preview mode where we only have a scroll-y
    /// offset and no galley.  We keep the last heading whose offset is ≤ a
    /// position proportional to `scroll_y`.  When `scroll_y` is 0 the first
    /// heading wins; larger values advance through the list.
    pub fn update_active_from_scroll_ratio(&mut self, scroll_y: f32) {
        if self.outline.is_empty() {
            self.active_index = None;
            return;
        }
        let max_offset = self
            .outline
            .last()
            .map(|h| h.byte_offset)
            .unwrap_or(1)
            .max(1) as f32;
        // Map scroll_y linearly into the heading byte-offset space.
        // The constant is an empirical scale factor (pixels per source-byte
        // in typical rendered markdown).  It does not need to be precise —
        // the mapping only needs to be monotonically increasing so the
        // active heading advances as the user scrolls.
        let estimated_byte = (scroll_y * (max_offset / 800.0)).max(0.0) as usize;
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
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        if !self.visible {
            return false;
        }

        let panel_frame = egui::Frame::none()
            .fill(ctx.style().visuals.panel_fill)
            .inner_margin(egui::Margin::same(6.0));

        egui::SidePanel::right("navigation")
            .resizable(true)
            .min_width(NAV_PANEL_MIN_WIDTH)
            .default_width(NAV_PANEL_DEFAULT_WIDTH)
            .frame(panel_frame)
            .show(ctx, |ui| self.panel_contents(ui));

        true
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

        // Build a lightweight index of visible entries (indices only, no cloning).
        let max_depth = self.max_depth;
        let visible_indices: Vec<usize> = self
            .outline
            .iter()
            .enumerate()
            .filter(|(_, h)| h.level <= max_depth)
            .map(|(i, _)| i)
            .collect();

        let min_level = visible_indices
            .iter()
            .map(|&i| self.outline[i].level)
            .min()
            .unwrap_or(1);

        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                if visible_indices.is_empty() {
                    ui.weak("No headings found.");
                    return;
                }
                let result = render_entries(
                    ui,
                    &self.outline,
                    &visible_indices,
                    min_level,
                    self.expanded_pos,
                    self.active_index,
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

/// Returned when a heading row is clicked.
struct ClickAction {
    byte_offset: usize,
    /// If set, toggle the accordion at this top-level position.
    toggle_pos: Option<usize>,
}

/// Visual state for a single heading row.
struct RowStyle {
    indent: f32,
    is_active: bool,
    has_children: bool,
    is_expanded: bool,
}

/// Render the heading list.  Returns a [`ClickAction`] if the user clicked a
/// heading, otherwise `None`.
fn render_entries(
    ui: &mut egui::Ui,
    outline: &[HeadingEntry],
    visible: &[usize],
    min_level: u8,
    expanded_pos: Option<usize>,
    active_index: Option<usize>,
) -> Option<ClickAction> {
    // Identify which visible-index positions are top-level.
    let top_positions: Vec<usize> = visible
        .iter()
        .enumerate()
        .filter(|&(_, &gi)| outline[gi].level == min_level)
        .map(|(pos, _)| pos)
        .collect();

    let mut result = None;

    for (rank, &vis_pos) in top_positions.iter().enumerate() {
        let gi = visible[vis_pos];
        let heading = &outline[gi];
        let is_expanded = expanded_pos == Some(rank);

        // Children span from vis_pos+1 to the next top-level entry.
        let next_vis = top_positions
            .get(rank + 1)
            .copied()
            .unwrap_or(visible.len());
        let has_children = next_vis > vis_pos + 1;

        let style = RowStyle {
            indent: (heading.level.saturating_sub(min_level)) as f32 * NAV_INDENT_PX,
            is_active: active_index == Some(gi),
            has_children,
            is_expanded,
        };

        if render_heading_row(ui, heading, &style).clicked() {
            result = Some(ClickAction {
                byte_offset: heading.byte_offset,
                toggle_pos: if has_children { Some(rank) } else { None },
            });
        }

        if is_expanded && has_children {
            for &child_vis in &visible[vis_pos + 1..next_vis] {
                let child = &outline[child_vis];
                let child_style = RowStyle {
                    indent: (child.level.saturating_sub(min_level)) as f32 * NAV_INDENT_PX,
                    is_active: active_index == Some(child_vis),
                    has_children: false,
                    is_expanded: false,
                };
                if render_heading_row(ui, child, &child_style).clicked() {
                    result = Some(ClickAction {
                        byte_offset: child.byte_offset,
                        toggle_pos: None,
                    });
                }
            }
        }
    }

    // Orphan entries before the first top-level heading.
    if let Some(&first_top_vis) = top_positions.first() {
        for &gi in &visible[..first_top_vis] {
            let h = &outline[gi];
            let style = RowStyle {
                indent: (h.level.saturating_sub(min_level)) as f32 * NAV_INDENT_PX,
                is_active: active_index == Some(gi),
                has_children: false,
                is_expanded: false,
            };
            if render_heading_row(ui, h, &style).clicked() {
                result = Some(ClickAction {
                    byte_offset: h.byte_offset,
                    toggle_pos: None,
                });
            }
        }
    }

    result
}

fn render_heading_row(
    ui: &mut egui::Ui,
    heading: &HeadingEntry,
    style: &RowStyle,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_space(style.indent);

        if style.has_children {
            let arrow = if style.is_expanded { "▾" } else { "▸" };
            ui.label(egui::RichText::new(arrow).small());
        } else if style.indent > 0.0 {
            ui.label(egui::RichText::new("·").small().weak());
        }

        let text = egui::RichText::new(&heading.label).small();
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
mod tests {
    use super::*;

    fn make_state(md: &str) -> NavState {
        let mut state = NavState {
            visible: true,
            ..NavState::default()
        };
        state.refresh_outline(md, 1);
        state
    }

    #[test]
    fn refresh_outline_caches_by_edit_seq() {
        let mut state = NavState::default();
        state.refresh_outline("# A\n## B\n", 1);
        assert_eq!(state.outline.len(), 2);

        // Same seq → no rebuild.
        state.outline.clear();
        state.refresh_outline("# A\n## B\n### C\n", 1);
        assert!(state.outline.is_empty(), "should not have rebuilt");

        // New seq → rebuild.
        state.refresh_outline("# A\n## B\n### C\n", 2);
        assert_eq!(state.outline.len(), 3);
    }

    #[test]
    fn refresh_outline_resets_accordion() {
        let mut state = make_state("# A\n## B\n");
        state.expanded_pos = Some(0);
        state.refresh_outline("# A\n## B\n### C\n", 2);
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
        // #### D is at index 1
        let d_offset = state.outline[1].byte_offset;
        state.max_depth = 2;
        state.update_active_from_position(d_offset);
        // Should skip #### D (level 4 > max_depth 2) and return # A
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
}
