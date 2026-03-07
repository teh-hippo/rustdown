use std::cell::Cell;

use eframe::egui;
use rustdown_md::{MarkdownStyle, MarkdownViewer};

use super::{
    BundledDoc, ConflictChoice, DEBOUNCE, Mode, PANEL_EDGE_PADDING, PendingAction, RustdownApp,
    SCROLL_WHEEL_MULTIPLIER, SaveTrigger, ZOOM_STEP, first_markdown_path,
    save_trigger_from_shortcut,
};
use crate::{
    document::{Document, EditorGalleyCache, TrackedTextBuffer},
    editor, highlight,
};

impl RustdownApp {
    /// Read keyboard/mouse input and dispatch the matching actions (open, save,
    /// zoom, search, format, mode-cycle, etc.).  Skipped when a dialog is open
    /// so that shortcuts do not fire behind modal windows.
    pub(crate) fn handle_keyboard_shortcuts(&mut self, ctx: &egui::Context) {
        let dialog_open = self.pending_action.is_some() || self.disk.conflict.is_some();

        let dropped_path = ctx.input(|i| {
            first_markdown_path(
                i.raw
                    .dropped_files
                    .iter()
                    .filter_map(|file| file.path.as_deref()),
            )
        });
        let (
            open,
            save_trigger,
            new_doc,
            cycle_mode,
            search,
            replace_all,
            format_doc,
            zoom_in,
            zoom_out,
            zoom_delta,
            escape,
            toggle_nav,
            open_demo,
            open_verification,
        ) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            (
                cmd && i.key_pressed(egui::Key::O),
                save_trigger_from_shortcut(cmd, i.modifiers.shift, i.key_pressed(egui::Key::S)),
                cmd && i.key_pressed(egui::Key::N),
                cmd && i.key_pressed(egui::Key::Enter),
                cmd && !i.modifiers.shift && !i.modifiers.alt && i.key_pressed(egui::Key::F),
                cmd && i.modifiers.shift && !i.modifiers.alt && i.key_pressed(egui::Key::F),
                cmd && i.modifiers.alt && !i.modifiers.shift && i.key_pressed(egui::Key::F),
                cmd && i.key_pressed(egui::Key::Equals),
                cmd && i.key_pressed(egui::Key::Minus),
                i.zoom_delta(),
                i.key_pressed(egui::Key::Escape),
                cmd && i.modifiers.shift && i.key_pressed(egui::Key::T),
                cmd && i.modifiers.shift && i.key_pressed(egui::Key::F11),
                cmd && i.modifiers.shift && i.key_pressed(egui::Key::F12),
            )
        });

        if dialog_open {
            return;
        }
        if let Some(path) = dropped_path {
            self.request_action(PendingAction::Open(path));
        }
        if open {
            self.open_file();
        }
        if let Some(trigger) = save_trigger {
            let _ = self.save_doc(matches!(trigger, SaveTrigger::SaveAs));
        }
        if new_doc {
            self.request_action(PendingAction::NewBlank);
        }
        if cycle_mode {
            self.set_mode(self.mode.cycle(), ctx);
        }
        if search {
            self.open_search(false);
        }
        if replace_all {
            self.open_search(true);
        }
        if format_doc {
            self.format_document();
        }
        if zoom_in {
            self.adjust_zoom(ctx, ZOOM_STEP);
        }
        if zoom_out {
            self.adjust_zoom(ctx, -ZOOM_STEP);
        }
        if (zoom_delta - 1.0).abs() > f32::EPSILON {
            self.adjust_zoom_factor(ctx, zoom_delta);
        }
        if escape && self.search.visible {
            self.close_search();
        }
        if toggle_nav {
            self.nav.visible = !self.nav.visible;
            self.save_preferences();
        }
        if open_demo {
            self.request_action(PendingAction::OpenBundled(BundledDoc::Demo));
        }
        if open_verification {
            self.request_action(PendingAction::OpenBundled(BundledDoc::Verification));
        }
    }

    /// Render the toolbar panel with mode buttons, heading-colour toggle,
    /// format button, and navigation toggle.
    pub(crate) fn show_toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("toolbar").show(ctx, |ui| {
            let toolbar_size = ui.text_style_height(&egui::TextStyle::Body) * 0.85;
            let toolbar_font = egui::FontId::proportional(toolbar_size);
            let tb = |text: &str| egui::RichText::new(text).font(toolbar_font.clone());

            ui.horizontal(|ui| {
                for mode in [Mode::Edit, Mode::Preview, Mode::SideBySide] {
                    if ui
                        .selectable_label(self.mode == mode, tb(mode.icon()))
                        .on_hover_text(mode.tooltip())
                        .clicked()
                    {
                        self.set_mode(mode, ui.ctx());
                    }
                }

                if self.mode == Mode::SideBySide {
                    ui.separator();
                    if ui
                        .toggle_value(&mut self.side_by_side_scroll_sync, tb("🔒"))
                        .on_hover_text("Linked scrolling")
                        .changed()
                    {
                        self.clear_side_by_side_scroll_state();
                        self.save_preferences();
                    }
                }

                ui.separator();
                let color_rt = if self.heading_color_mode {
                    tb("Aa").color(egui::Color32::from_rgb(0xBD, 0x93, 0xF9))
                } else {
                    tb("Aa")
                };
                if ui
                    .toggle_value(&mut self.heading_color_mode, color_rt)
                    .on_hover_text("Heading colours")
                    .changed()
                {
                    self.doc.editor_galley_cache = None;
                    self.doc.preview_cache.clear();
                    self.save_preferences();
                }
                ui.separator();
                if ui
                    .button(tb("Fmt"))
                    .on_hover_text("Format document")
                    .clicked()
                {
                    self.format_document();
                }
                if ui
                    .toggle_value(&mut self.nav.visible, tb("Nav"))
                    .on_hover_text("Navigation")
                    .changed()
                {
                    self.save_preferences();
                }
            });
        });
    }

    /// Render the search/replace bar and execute a pending replace-all after
    /// the panel closure finishes (so the `&mut self` borrow is released).
    pub(crate) fn show_search_bar(&mut self, ctx: &egui::Context) {
        let mut run_replace_all = false;
        egui::TopBottomPanel::bottom("search").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Find:");
                let query_response = ui.add(
                    egui::TextEdit::singleline(&mut self.search.query)
                        .hint_text("Search")
                        .desired_width(180.0)
                        .id(egui::Id::new("search-query")),
                );
                if self.focus_search {
                    query_response.request_focus();
                    self.focus_search = false;
                }
                if query_response.changed() {
                    self.search.last_replace_count = None;
                }

                let matches = self
                    .search
                    .match_count(self.doc.text.as_str(), self.doc.edit_seq);
                let label = if matches == 1 { "match" } else { "matches" };
                ui.label(format!("{matches} {label}"));

                if self.search.replace_mode {
                    ui.separator();
                    ui.label("Replace:");
                    let replace_response = ui.add(
                        egui::TextEdit::singleline(&mut self.search.replacement)
                            .hint_text("Replace with")
                            .desired_width(180.0),
                    );
                    if replace_response.changed() {
                        self.search.last_replace_count = None;
                    }

                    let replace_button = ui.add_enabled(
                        !self.search.query.is_empty(),
                        egui::Button::new("Replace all"),
                    );
                    if replace_button.clicked() {
                        run_replace_all = true;
                    }

                    if let Some(count) = self.search.last_replace_count {
                        ui.label(format!("replaced {count}"));
                    }
                }

                if ui.button("Close").clicked() {
                    self.close_search();
                }
            });
        });
        if run_replace_all {
            let replaced = self.replace_all_matches();
            self.search.last_replace_count = Some(replaced);
        }
    }

    /// Render the status bar: file path, line count, dirty marker, error
    /// messages, and merge-sidecar controls.
    pub(crate) fn show_status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            let mut clear_error = false;

            let toolbar_size = ui.text_style_height(&egui::TextStyle::Body) * 0.85;
            let toolbar_font = egui::FontId::proportional(toolbar_size);
            let tb = |text: &str| egui::RichText::new(text).font(toolbar_font.clone());

            ui.horizontal(|ui| {
                ui.label(tb(&self.doc.path_label()));
                let stats = self.doc.stats();

                ui.separator();
                ui.label(
                    egui::RichText::new(format!("{} lines · {} words", stats.lines, stats.words))
                        .font(toolbar_font.clone()),
                );

                if self.doc.dirty {
                    ui.separator();
                    ui.colored_label(ui.visuals().warn_fg_color, tb("Modified"));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut clear_merge_sidecar = false;
                    let mut open_merge_path: Option<std::path::PathBuf> = None;
                    if let Some(path) = &self.disk.merge_sidecar_path {
                        if ui.button("x").clicked() {
                            clear_merge_sidecar = true;
                        }
                        if ui.button("Open merge file").clicked() {
                            open_merge_path = Some(path.clone());
                        }
                        ui.label(path.to_string_lossy());
                        ui.separator();
                    }
                    if let Some(path) = open_merge_path {
                        self.request_action(PendingAction::Open(path));
                    }

                    if let Some(error) = self.error.as_deref() {
                        if ui.button("x").clicked() {
                            clear_error = true;
                        }
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }

                    if clear_merge_sidecar {
                        self.disk.merge_sidecar_path = None;
                    }
                });
            });

            if clear_error {
                self.error = None;
            }
        });
    }

    /// Render the nav panel, side-by-side preview panel, central panel, and
    /// process any pending nav-scroll actions.  Extracted so the debug harness
    /// can reuse the same layout sequence.
    #[allow(clippy::cast_possible_truncation)] // PANEL_EDGE_PADDING=8.0, fits in i8
    pub(crate) fn show_content_panels(&mut self, ctx: &egui::Context) {
        let panel_frame = egui::Frame::new()
            .fill(ctx.style().visuals.panel_fill)
            .inner_margin(PANEL_EDGE_PADDING as i8);

        // Resolve any pending nav target to a y-pixel value *before* the
        // scroll areas render, so the smooth-scroll request is consumed on
        // this same frame (no 1-frame delay).
        self.resolve_nav_scroll_target(ctx);

        if self.nav.visible {
            self.nav.heading_color_mode = self.heading_color_mode;
            self.nav.refresh_outline(&self.doc.text, self.doc.edit_seq);
        }
        self.nav.show(ctx);

        if self.mode == Mode::SideBySide {
            egui::SidePanel::right("preview")
                .resizable(true)
                .min_width(240.0)
                .default_width(420.0)
                .frame(panel_frame)
                .show(ctx, |ui| self.show_preview(ui));
        }

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| match self.mode {
                Mode::Edit | Mode::SideBySide => self.show_editor(ui),
                Mode::Preview => self.show_preview(ui),
            });

        // Sync the active heading highlight from the current scroll position.
        if self.nav.visible {
            self.sync_nav_active_heading(ctx);
        }

        // In SideBySide, sync the preview scroll to match the editor position.
        if self.mode == Mode::SideBySide && self.side_by_side_scroll_sync {
            self.sync_side_by_side_scroll(ctx);
            self.animate_side_by_side_scroll(ctx);
        }
    }

    pub(crate) fn show_editor(&mut self, ui: &mut egui::Ui) {
        let heading_color_mode = self.heading_color_mode;
        let nav_visible = self.nav.visible;
        let (changed, next_seq) = {
            let seq = Cell::new(self.doc.edit_seq);
            let Document {
                text,
                editor_galley_cache,
                ..
            } = &mut self.doc;

            let mut buffer = TrackedTextBuffer { text, seq: &seq };

            let editor = egui::TextEdit::multiline(&mut buffer)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Body)
                .frame(false)
                .id(egui::Id::new("editor"));

            let mut layouter = |ui: &egui::Ui, text_buf: &dyn egui::TextBuffer, wrap_width: f32| {
                let string = text_buf.as_str();
                let seq = seq.get();
                let wrap_width_bits = wrap_width.to_bits();
                let zoom_factor_bits = ui.ctx().zoom_factor().to_bits();

                // Full cache hit: text, color, zoom, and wrap width all match.
                if let Some(cache) = editor_galley_cache.as_ref()
                    && cache.content_seq == seq
                    && cache.content_color_mode == heading_color_mode
                    && cache.wrap_width_bits == wrap_width_bits
                    && cache.zoom_factor_bits == zoom_factor_bits
                {
                    return cache.galley.clone();
                }

                // Partial cache hit: text unchanged but zoom/wrap changed.
                // Reuse cached sections (skip O(n) highlight scan) and rebuild
                // the LayoutJob from the document text — avoids cloning a full
                // LayoutJob which would duplicate the entire source text.
                let (job, sections) = if let Some(cache) = editor_galley_cache.as_ref()
                    && cache.content_seq == seq
                    && cache.content_color_mode == heading_color_mode
                {
                    let job = egui::text::LayoutJob {
                        text: string.to_owned(),
                        sections: cache.layout_sections.clone(),
                        wrap: egui::text::TextWrapping {
                            max_width: wrap_width,
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    // Reuse existing sections allocation by not cloning again
                    (job, None)
                } else {
                    let mut job = highlight::markdown_layout_job(
                        ui.style(),
                        ui.visuals(),
                        string,
                        heading_color_mode,
                    );
                    job.wrap.max_width = wrap_width;
                    let sections = job.sections.clone();
                    (job, Some(sections))
                };

                let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                let row_byte_offsets = if nav_visible {
                    editor::build_row_byte_offsets(&galley, string)
                } else {
                    Vec::new()
                };

                // On partial hit, only update metadata (keep existing sections).
                // On full rebuild, store the new sections.
                if let Some(sections) = sections {
                    *editor_galley_cache = Some(EditorGalleyCache {
                        content_seq: seq,
                        content_color_mode: heading_color_mode,
                        wrap_width_bits,
                        zoom_factor_bits,
                        layout_sections: sections,
                        galley: galley.clone(),
                        row_byte_offsets,
                    });
                } else if let Some(cache) = editor_galley_cache.as_mut() {
                    cache.wrap_width_bits = wrap_width_bits;
                    cache.zoom_factor_bits = zoom_factor_bits;
                    cache.galley = galley.clone();
                    cache.row_byte_offsets = row_byte_offsets;
                }
                galley
            };

            let editor_size = ui.available_size();
            let scroll_to = self.nav.pending_editor_scroll_y.take();
            let mut scroll_area = egui::ScrollArea::both()
                .id_salt("editor_scroll")
                .auto_shrink([false; 2])
                .wheel_scroll_multiplier(egui::vec2(1.0, SCROLL_WHEEL_MULTIPLIER));
            if let Some(y) = scroll_to {
                scroll_area = scroll_area.vertical_scroll_offset(y);
            }
            let response = scroll_area
                .show(ui, |ui| {
                    ui.add_sized(editor_size, editor.layouter(&mut layouter))
                })
                .inner;
            (response.changed(), seq.get())
        };

        self.doc.edit_seq = next_seq;
        if changed {
            self.note_text_changed(true);
        }
    }

    /// Rebuild the cached `MarkdownStyle` when the theme, colour mode, or
    /// image URI changes; otherwise reuse the previous value.
    pub(crate) fn ensure_preview_style(&mut self, visuals: &egui::Visuals) {
        let dark = visuals.dark_mode;
        let colored = self.heading_color_mode;
        let uri = &self.doc.image_uri_scheme;
        let c = &self.preview_style_cache;

        let needs_rebuild = match &c.style {
            Some(_) => c.dark_mode != dark || c.colored != colored || c.image_uri != *uri,
            None => true,
        };

        if needs_rebuild {
            let mut style = if colored {
                MarkdownStyle::colored(visuals)
            } else {
                MarkdownStyle::from_visuals(visuals)
            };
            style.image_base_uri.clone_from(uri);
            let c = &mut self.preview_style_cache;
            c.dark_mode = dark;
            c.colored = colored;
            c.image_uri.clone_from(uri);
            c.style = Some(style);
        }
    }

    pub(crate) fn show_preview(&mut self, ui: &mut egui::Ui) {
        if self.mode == Mode::SideBySide
            && let Some(remaining) = self.doc.debounce_remaining(DEBOUNCE)
        {
            ui.ctx().request_repaint_after(remaining);
            return;
        }

        self.doc.consume_preview_dirty();

        self.ensure_preview_style(ui.visuals());

        // Consume any pending nav-scroll target and pass it directly to the
        // ScrollArea, avoiding the ID-mismatch problem with external state lookup.
        let scroll_y = self.nav.pending_preview_scroll_y.take();

        if let Some(ref style) = self.preview_style_cache.style {
            MarkdownViewer::new("preview_markdown").show_scrollable(
                ui,
                &mut self.doc.preview_cache,
                style,
                self.doc.text.as_str(),
                scroll_y,
            );
        }
    }

    pub(crate) fn show_dialogs(&mut self, ctx: &egui::Context) {
        if self.pending_action.is_none() {
            return;
        }

        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if escape {
            self.pending_action = None;
            return;
        }

        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!("\"{}\" has unsaved changes.", self.doc.title()));
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() && self.save_doc(false) {
                        self.apply_pending_action_and_close_dialog();
                    }

                    if ui.button("Discard").clicked() {
                        self.apply_pending_action_and_close_dialog();
                    }

                    if ui.button("Cancel").clicked() {
                        self.pending_action = None;
                    }
                });
            });
    }

    pub(crate) fn show_disk_conflict_dialog(&mut self, ctx: &egui::Context) {
        if self.disk.conflict.is_none() {
            return;
        }

        egui::Window::new("File changed on disk")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!(
                    "\"{}\" changed on disk while you were editing.",
                    self.doc.title()
                ));
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    for (label, choice) in [
                        ("Open conflict merge", ConflictChoice::OpenConflictMerge),
                        ("Keep mine (+ merge file)", ConflictChoice::KeepMineWriteSidecar),
                    ] {
                        if ui.button(label).clicked() {
                            self.apply_conflict_choice(choice);
                        }
                    }
                });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    for (label, choice) in [
                        ("Save As…", ConflictChoice::SaveAs),
                        ("Reload disk", ConflictChoice::ReloadDisk),
                        ("Overwrite disk", ConflictChoice::OverwriteDisk),
                    ] {
                        if ui.button(label).clicked() {
                            self.apply_conflict_choice(choice);
                        }
                    }
                });

                ui.add_space(8.0);
                ui.small(
                    "Tip: “Keep mine” applies non-conflicting disk edits and writes a merge file so no changes are lost.",
                );
            });
    }
}
