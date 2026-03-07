use eframe::egui;

use super::{Mode, RustdownApp, SIDE_BY_SIDE_SCROLL_LERP, SideBySideScrollSource};
use crate::{editor, nav, scroll_math};

impl RustdownApp {
    /// Convert a heading byte offset to a preview scroll-y value.
    ///
    /// Uses exact heading Y positions from the parsed preview cache when
    /// available, and falls back to byte-proportional mapping otherwise.
    pub(crate) fn preview_nav_scroll_y(&self, byte_offset: usize) -> f32 {
        // Binary search on sorted outline for O(log n) lookup.
        if let Ok(ordinal) = self
            .nav
            .outline
            .binary_search_by_key(&byte_offset, |h| h.byte_offset)
            && let Some(y) = self.doc.preview_cache.heading_y(ordinal)
        {
            return y;
        }
        scroll_math::preview_byte_to_scroll_y(
            &self.nav.outline,
            byte_offset,
            self.doc.preview_cache.total_height,
        )
    }

    /// Determine the current scroll position as a byte offset in the source
    /// text.  Works in both editor and preview modes.
    pub(crate) fn current_scroll_byte_offset(&mut self, ctx: &egui::Context) -> Option<usize> {
        if self.uses_editor() {
            self.ensure_row_byte_offsets();
            let state = egui::scroll_area::State::load(ctx, scroll_math::editor_scroll_id())?;
            self.editor_y_to_byte(state.offset.y)
        } else {
            Some(scroll_math::preview_scroll_y_to_byte(
                &self.nav.outline,
                self.doc.preview_cache.last_scroll_y,
                self.doc.preview_cache.total_height,
            ))
        }
    }

    pub(crate) fn current_editor_scroll_y(&mut self, ctx: &egui::Context) -> Option<f32> {
        self.ensure_row_byte_offsets();
        let state = egui::scroll_area::State::load(ctx, scroll_math::editor_scroll_id())?;
        Some(state.offset.y)
    }

    pub(crate) fn current_preview_scroll_byte(&self) -> usize {
        scroll_math::preview_scroll_y_to_byte(
            &self.nav.outline,
            self.doc.preview_cache.last_scroll_y,
            self.doc.preview_cache.total_height,
        )
    }

    /// Resolve a pending [`NavScrollTarget`] into per-pane y-pixel targets.
    /// Must run *before* the scroll areas render so targets are consumed
    /// on the same frame.
    pub(crate) fn resolve_nav_scroll_target(&mut self, ctx: &egui::Context) {
        use nav::panel::NavScrollTarget;
        self.nav_scroll_applied_this_frame = false;
        let Some(target) = self.nav.pending_scroll.take() else {
            return;
        };
        if self.uses_editor() {
            self.ensure_row_byte_offsets();
        }

        // If the mode needs the editor galley for a byte-offset target but
        // it hasn't been built yet (e.g. switching from Preview →
        // Edit/SideBySide just dropped the cache), re-queue the target.
        // The galley will be built during show_editor() this frame; next
        // frame it will resolve successfully.
        // `Top` targets always resolve to 0.0 and don't need the galley.
        if self.uses_editor()
            && self.doc.editor_galley_cache.is_none()
            && matches!(target, NavScrollTarget::ByteOffset(_))
        {
            self.nav.pending_scroll = Some(target);
            self.nav_scroll_applied_this_frame = true;
            ctx.request_repaint();
            return;
        }

        let (editor_target_y, preview_target_y) = match target {
            NavScrollTarget::Top => (Some(0.0_f32), Some(0.0_f32)),
            NavScrollTarget::ByteOffset(byte_offset) => (
                self.editor_byte_to_y(byte_offset),
                Some(self.preview_nav_scroll_y(byte_offset)),
            ),
        };
        match self.mode {
            Mode::Edit => {
                self.nav.pending_editor_scroll_y = editor_target_y;
                self.nav.pending_preview_scroll_y = None;
            }
            Mode::Preview => {
                self.nav.pending_editor_scroll_y = None;
                self.nav.pending_preview_scroll_y = preview_target_y;
            }
            Mode::SideBySide => {
                self.nav.pending_editor_scroll_y = editor_target_y;
                self.nav.pending_preview_scroll_y = preview_target_y;
                // Cancel any in-flight animation so it doesn't override the
                // precise nav-driven preview position on the next frame.
                self.clear_side_by_side_scroll_state();
                // Pre-seed the sync bytes so side-by-side sync does not
                // override the precise nav-driven positions on the next frame.
                self.last_sync_editor_byte = editor_target_y.and_then(|y| self.editor_y_to_byte(y));
                self.last_sync_preview_byte = preview_target_y.map(|y| {
                    scroll_math::preview_scroll_y_to_byte(
                        &self.nav.outline,
                        y,
                        self.doc.preview_cache.total_height,
                    )
                });
            }
        }
        self.nav_scroll_applied_this_frame = true;
        ctx.request_repaint();
    }

    /// Read the current scroll offset and update the active heading in the
    /// nav panel.  Must run *after* the scroll areas render.
    pub(crate) fn sync_nav_active_heading(&mut self, ctx: &egui::Context) {
        if self.uses_editor() {
            self.ensure_row_byte_offsets();
            if let Some(state) =
                egui::scroll_area::State::load(ctx, scroll_math::editor_scroll_id())
                && let Some(byte_pos) = self.editor_y_to_byte(state.offset.y)
            {
                self.nav.update_active_from_position(byte_pos);
            }
        } else {
            // Preview mode: convert cached scroll-y to byte offset via outline.
            let byte_pos = scroll_math::preview_scroll_y_to_byte(
                &self.nav.outline,
                self.doc.preview_cache.last_scroll_y,
                self.doc.preview_cache.total_height,
            );
            self.nav.update_active_from_position(byte_pos);
        }
    }

    /// In Side-by-Side mode, sync the preview scroll position to track the
    /// active pane scroll position. Uses byte offsets as an intermediate
    /// representation so both panes show the same content region.
    pub(crate) fn sync_side_by_side_scroll(&mut self, ctx: &egui::Context) {
        // Skip sync if a nav-panel scroll target was already applied this frame;
        // re-syncing would override it and cause a visible snap.
        if self.nav_scroll_applied_this_frame || !self.side_by_side_scroll_sync {
            return;
        }

        self.ensure_row_byte_offsets();
        self.nav.refresh_outline(&self.doc.text, self.doc.edit_seq);
        let Some(editor_y) = self.current_editor_scroll_y(ctx) else {
            return;
        };
        let Some(editor_byte) = self.editor_y_to_byte(editor_y) else {
            return;
        };
        let preview_byte = self.current_preview_scroll_byte();

        match self.side_by_side_scroll_source {
            Some(SideBySideScrollSource::Editor) => {
                if self.last_sync_editor_byte != Some(editor_byte) {
                    self.last_sync_editor_byte = Some(editor_byte);
                    self.side_by_side_scroll_target = Some(self.preview_nav_scroll_y(editor_byte));
                    ctx.request_repaint();
                }
                return;
            }
            Some(SideBySideScrollSource::Preview) => {
                if self.last_sync_preview_byte != Some(preview_byte) {
                    self.last_sync_preview_byte = Some(preview_byte);
                    self.side_by_side_scroll_target = self.editor_byte_to_y(preview_byte);
                    if self.side_by_side_scroll_target.is_some() {
                        ctx.request_repaint();
                    }
                }
                return;
            }
            None => {}
        }

        let editor_changed = self.last_sync_editor_byte != Some(editor_byte);
        let preview_changed = self.last_sync_preview_byte != Some(preview_byte);
        match (editor_changed, preview_changed) {
            (false, false) => {}
            (true, false) => {
                self.last_sync_editor_byte = Some(editor_byte);
                self.side_by_side_scroll_source = Some(SideBySideScrollSource::Editor);
                self.side_by_side_scroll_target = Some(self.preview_nav_scroll_y(editor_byte));
                ctx.request_repaint();
            }
            (false, true) => {
                self.last_sync_preview_byte = Some(preview_byte);
                self.side_by_side_scroll_source = Some(SideBySideScrollSource::Preview);
                self.side_by_side_scroll_target = self.editor_byte_to_y(preview_byte);
                if self.side_by_side_scroll_target.is_some() {
                    ctx.request_repaint();
                }
            }
            (true, true) => {
                self.last_sync_editor_byte = Some(editor_byte);
                self.last_sync_preview_byte = Some(preview_byte);
            }
        }
    }

    /// Smoothly animate whichever follower pane is tracking the active side.
    pub(crate) fn animate_side_by_side_scroll(&mut self, ctx: &egui::Context) {
        let Some(target) = self.side_by_side_scroll_target else {
            self.side_by_side_scroll_source = None;
            return;
        };
        match self.side_by_side_scroll_source {
            Some(SideBySideScrollSource::Editor) => {
                let current = self.doc.preview_cache.last_scroll_y;
                let diff = target - current;
                if diff.abs() < 1.0 {
                    self.nav.pending_preview_scroll_y = Some(target);
                    self.side_by_side_scroll_target = None;
                    self.side_by_side_scroll_source = None;
                    self.last_sync_preview_byte = Some(scroll_math::preview_scroll_y_to_byte(
                        &self.nav.outline,
                        target,
                        self.doc.preview_cache.total_height,
                    ));
                    return;
                }
                let step = diff.mul_add(SIDE_BY_SIDE_SCROLL_LERP, current);
                self.nav.pending_preview_scroll_y = Some(step);
                ctx.request_repaint();
            }
            Some(SideBySideScrollSource::Preview) => {
                let Some(current) = self.current_editor_scroll_y(ctx) else {
                    return;
                };
                let diff = target - current;
                if diff.abs() < 1.0 {
                    self.nav.pending_editor_scroll_y = Some(target);
                    self.side_by_side_scroll_target = None;
                    self.side_by_side_scroll_source = None;
                    self.last_sync_editor_byte = self.editor_y_to_byte(target);
                    return;
                }
                let step = diff.mul_add(SIDE_BY_SIDE_SCROLL_LERP, current);
                self.nav.pending_editor_scroll_y = Some(step);
                ctx.request_repaint();
            }
            None => {
                self.side_by_side_scroll_target = None;
            }
        }
    }

    /// Map a byte offset to a Y position using the cached row byte offsets.
    /// O(log n) binary search instead of O(n) char scan.
    pub(crate) fn editor_byte_to_y(&self, byte_offset: usize) -> Option<f32> {
        let cache = self.doc.editor_galley_cache.as_ref()?;
        Some(editor::row_byte_offset_to_y(
            &cache.row_byte_offsets,
            byte_offset,
        ))
    }

    /// Map a Y scroll position to a byte offset using the cached row byte
    /// offsets.  O(log n) binary search.
    pub(crate) fn editor_y_to_byte(&self, y: f32) -> Option<usize> {
        let cache = self.doc.editor_galley_cache.as_ref()?;
        Some(editor::row_y_to_byte_offset(&cache.row_byte_offsets, y))
    }

    pub(crate) fn ensure_row_byte_offsets(&mut self) {
        if let Some(cache) = self.doc.editor_galley_cache.as_mut()
            && cache.row_byte_offsets.is_empty()
        {
            cache.row_byte_offsets =
                editor::build_row_byte_offsets(&cache.galley, self.doc.text.as_str());
        }
    }
}
