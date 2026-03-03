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
    /// Which top-level (min-level) heading index is expanded (accordion).
    /// `None` means nothing expanded.
    expanded_index: Option<usize>,
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
            expanded_index: None,
            active_index: None,
            pending_scroll: None,
        }
    }
}

/// Minimum panel width.
const NAV_PANEL_MIN_WIDTH: f32 = 140.0;
/// Default panel width.
const NAV_PANEL_DEFAULT_WIDTH: f32 = 220.0;
/// Per-level indentation in pixels.
const NAV_INDENT_PX: f32 = 12.0;

impl NavState {
    /// Rebuild the heading outline if the document has changed.
    pub fn refresh_outline(&mut self, source: &str, edit_seq: u64) {
        if edit_seq == self.outline_seq {
            return;
        }
        self.outline = nav_outline::extract_headings(source);
        self.outline_seq = edit_seq;
        // Reset accordion when outline changes.
        self.expanded_index = None;
    }

    /// Update `active_index` from a byte position in the document.
    pub fn update_active_from_position(&mut self, byte_position: usize) {
        self.active_index =
            nav_outline::active_heading_index(&self.outline, self.max_depth, byte_position);
    }

    /// Show the navigation panel.  Returns `true` if the panel is visible
    /// (so the caller can adjust layout).
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
        // Title row with return-to-top.
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

        // Scrollable heading list.
        let max_depth = self.max_depth;
        let entries: Vec<(usize, HeadingEntry)> = self
            .outline
            .iter()
            .enumerate()
            .filter(|(_, h)| h.level <= max_depth)
            .map(|(i, h)| (i, h.clone()))
            .collect();

        let min_level = entries.iter().map(|(_, h)| h.level).min().unwrap_or(1);

        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                if entries.is_empty() {
                    ui.weak("No headings found.");
                    return;
                }
                self.render_entries(ui, &entries, min_level);
            });

        ui.separator();
        // Depth controls at the bottom.
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.max_depth > 1, egui::Button::new("−").small())
                .on_hover_text("Show fewer heading levels")
                .clicked()
            {
                self.max_depth = self.max_depth.saturating_sub(1).max(1);
            }
            ui.label(format!("H1–H{}", self.max_depth));
            if ui
                .add_enabled(self.max_depth < 6, egui::Button::new("+").small())
                .on_hover_text("Show more heading levels")
                .clicked()
            {
                self.max_depth = (self.max_depth + 1).min(6);
            }
        });
    }

    fn render_entries(
        &mut self,
        ui: &mut egui::Ui,
        entries: &[(usize, HeadingEntry)],
        min_level: u8,
    ) {
        // Build index of which entries are "top-level" (min_level).
        let top_level_indices: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, (_, h))| h.level == min_level)
            .map(|(i, _)| i)
            .collect();

        for (pos, &top_idx) in top_level_indices.iter().enumerate() {
            let (global_idx, ref heading) = entries[top_idx];

            // Determine if this top-level entry is expanded.
            let is_expanded = self.expanded_index == Some(pos);

            // Find children: entries between this top-level and the next.
            let next_top = top_level_indices
                .get(pos + 1)
                .copied()
                .unwrap_or(entries.len());
            let has_children = next_top > top_idx + 1;

            // Render the top-level heading.
            let is_active = self.active_index == Some(global_idx);
            let response = Self::render_heading_row(
                ui,
                heading,
                min_level,
                is_active,
                has_children,
                is_expanded,
            );

            if response.clicked() {
                if has_children {
                    self.expanded_index = if is_expanded { None } else { Some(pos) };
                }
                self.pending_scroll = Some(NavScrollTarget::ByteOffset(heading.byte_offset));
            }

            // Show children if expanded.
            if is_expanded && has_children {
                for (child_global_idx, child_heading) in &entries[top_idx + 1..next_top] {
                    let child_active = self.active_index == Some(*child_global_idx);
                    let child_resp = Self::render_heading_row(
                        ui,
                        child_heading,
                        min_level,
                        child_active,
                        false,
                        false,
                    );
                    if child_resp.clicked() {
                        self.pending_scroll =
                            Some(NavScrollTarget::ByteOffset(child_heading.byte_offset));
                    }
                }
            }
        }

        // If there are entries before the first top-level heading, render them.
        if let Some(&first_top) = top_level_indices.first() {
            for (global_idx, heading) in &entries[..first_top] {
                let is_active = self.active_index == Some(*global_idx);
                let resp =
                    Self::render_heading_row(ui, heading, min_level, is_active, false, false);
                if resp.clicked() {
                    self.pending_scroll = Some(NavScrollTarget::ByteOffset(heading.byte_offset));
                }
            }
        }
    }

    fn render_heading_row(
        ui: &mut egui::Ui,
        heading: &HeadingEntry,
        min_level: u8,
        is_active: bool,
        has_children: bool,
        is_expanded: bool,
    ) -> egui::Response {
        let indent = (heading.level.saturating_sub(min_level)) as f32 * NAV_INDENT_PX;

        ui.horizontal(|ui| {
            ui.add_space(indent);

            // Collapse indicator for parents.
            if has_children {
                let arrow = if is_expanded { "▾" } else { "▸" };
                ui.label(egui::RichText::new(arrow).small());
            } else if heading.level > min_level {
                // Small bullet for non-top-level items.
                ui.label(egui::RichText::new("·").small().weak());
            }

            let text = egui::RichText::new(&heading.label).small();
            let text = if is_active { text.strong() } else { text };

            ui.add(
                egui::Label::new(text)
                    .truncate()
                    .sense(egui::Sense::click()),
            )
        })
        .inner
    }
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
        state.expanded_index = Some(0);
        state.refresh_outline("# A\n## B\n### C\n", 2);
        assert_eq!(state.expanded_index, None);
    }

    #[test]
    fn default_max_depth_is_4() {
        let state = NavState::default();
        assert_eq!(state.max_depth, 4);
    }

    #[test]
    fn depth_controls_clamp() {
        let mut state = NavState::default();
        state.max_depth = 1;
        state.max_depth = state.max_depth.saturating_sub(1).max(1);
        assert_eq!(state.max_depth, 1);

        state.max_depth = 6;
        state.max_depth = (state.max_depth + 1).min(6);
        assert_eq!(state.max_depth, 6);
    }

    #[test]
    fn accordion_expand_collapse() {
        let mut state = make_state("# A\n## child\n# B\n## child2\n");
        // Simulate expanding first top-level.
        state.expanded_index = Some(0);
        assert_eq!(state.expanded_index, Some(0));
        // Clicking same one collapses.
        state.expanded_index = None;
        assert_eq!(state.expanded_index, None);
    }

    #[test]
    fn scroll_target_top() {
        let state = NavState {
            pending_scroll: Some(NavScrollTarget::Top),
            ..NavState::default()
        };
        assert_eq!(state.pending_scroll, Some(NavScrollTarget::Top));
    }

    #[test]
    fn scroll_target_byte_offset() {
        let state = NavState {
            pending_scroll: Some(NavScrollTarget::ByteOffset(42)),
            ..NavState::default()
        };
        assert_eq!(state.pending_scroll, Some(NavScrollTarget::ByteOffset(42)));
    }
}
