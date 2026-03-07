use std::{
    borrow::Cow,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use eframe::egui;

use super::{
    BundledDoc, ConflictChoice, Mode, PendingAction, RustdownApp, STATS_RECALC_DEBOUNCE,
    default_image_uri_scheme, markdown_file_dialog, zoom_with_factor, zoom_with_step,
};
use crate::{
    cli::{LaunchOptions, app_version},
    disk::io::{
        DiskRevision, atomic_write_utf8, disk_revision, next_merge_sidecar_path, read_stable_utf8,
    },
    disk::sync::ReloadKind,
    document::{Document, DocumentStats},
    format, nav, preferences,
    search::replace_all_occurrences,
};

impl RustdownApp {
    pub(crate) fn from_launch_options(options: LaunchOptions) -> Self {
        let prefs = preferences::UserPreferences::load();

        // Apply persisted mode only when no file is opened and no explicit
        // CLI flag was given (e.g. `rustdown` with no args).
        let mode = if !options.mode_explicit && options.path.is_none() {
            Mode::from_str_lossy(&prefs.mode).unwrap_or(options.mode)
        } else {
            options.mode
        };

        let mut app = Self {
            mode,
            heading_color_mode: prefs.heading_color_mode,
            side_by_side_scroll_sync: prefs.side_by_side_scroll_sync,
            persisted_zoom: prefs.zoom_factor,
            ..Self::default()
        };
        app.nav.visible = prefs.nav_visible;
        app.nav.heading_color_mode = prefs.heading_color_mode;
        if let Some(path) = options.path {
            app.open_path(path);
        }
        // Auto-show nav in preview modes if heading count exceeds threshold.
        app.maybe_auto_show_nav();
        app
    }

    pub(crate) fn set_mode(&mut self, mode: Mode, ctx: &egui::Context) {
        if self.mode == mode {
            return;
        }

        // Capture the current scroll position as a byte offset before
        // switching modes, so the new mode can jump to the same location.
        let scroll_byte = self.current_scroll_byte_offset(ctx);

        self.mode = mode;
        self.clear_side_by_side_scroll_state();

        if mode == Mode::Preview {
            // Preview doesn't render the editor; drop any cached galley to reduce memory.
            self.doc.editor_galley_cache = None;
        }

        if mode == Mode::Edit {
            self.doc.preview_cache.clear();
            self.doc.preview_dirty = false;
            self.doc.last_edit_at = None;
        }

        // Auto-show nav in preview modes if heading count exceeds threshold.
        self.maybe_auto_show_nav();

        // Request a scroll to the same position in the new mode.
        if let Some(byte_offset) = scroll_byte {
            self.nav.pending_scroll = Some(nav::panel::NavScrollTarget::ByteOffset(byte_offset));
        }

        self.save_preferences_with_zoom(ctx.zoom_factor());
    }

    pub(crate) const fn clear_side_by_side_scroll_state(&mut self) {
        self.last_sync_editor_byte = None;
        self.last_sync_preview_byte = None;
        self.side_by_side_scroll_source = None;
        self.side_by_side_scroll_target = None;
    }

    /// Returns `true` when the current mode renders the code editor.
    pub(crate) const fn uses_editor(&self) -> bool {
        matches!(self.mode, Mode::Edit | Mode::SideBySide)
    }

    /// Auto-show the nav panel in Preview/SideBySide when the document has
    /// enough headings to benefit from a table of contents.
    #[allow(clippy::missing_const_for_fn)] // Vec::len() in const context is unstable
    pub(crate) fn maybe_auto_show_nav(&mut self) {
        if matches!(self.mode, Mode::Preview | Mode::SideBySide)
            && self.nav.outline.len() >= preferences::AUTO_NAV_MIN_HEADINGS
        {
            self.nav.visible = true;
        }
    }

    /// Persist the current nav/colour/zoom/mode preferences to disk.
    pub(crate) fn save_preferences(&self) {
        self.save_preferences_with_zoom(1.0);
    }

    /// Persist preferences including a specific zoom factor.
    pub(crate) fn save_preferences_with_zoom(&self, zoom: f32) {
        let prefs = preferences::UserPreferences {
            nav_visible: self.nav.visible,
            heading_color_mode: self.heading_color_mode,
            side_by_side_scroll_sync: self.side_by_side_scroll_sync,
            zoom_factor: zoom,
            mode: self.mode.as_str().to_owned(),
        };
        prefs.save();
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn adjust_zoom(&self, ctx: &egui::Context, delta: f32) {
        let z = zoom_with_step(ctx.zoom_factor(), delta);
        ctx.set_zoom_factor(z);
        self.save_preferences_with_zoom(z);
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn adjust_zoom_factor(&self, ctx: &egui::Context, factor: f32) {
        let z = zoom_with_factor(ctx.zoom_factor(), factor);
        ctx.set_zoom_factor(z);
        self.save_preferences_with_zoom(z);
    }

    pub(crate) fn update_viewport_title(&mut self, ctx: &egui::Context) {
        // Avoid format! allocation when nothing changed.
        use std::fmt::Write;
        let ver = app_version();
        let file_title = self.doc.title();
        let dirty_mark = if self.doc.dirty { "*" } else { "" };

        // Quick length pre-check: "rustdown v" + ver + " - " + title + dirty
        let expected_len = 13 + ver.len() + file_title.len() + dirty_mark.len();
        if self.last_viewport_title.len() == expected_len
            && self.last_viewport_title.ends_with(dirty_mark)
            && self.last_viewport_title.contains(file_title.as_ref())
        {
            return;
        }

        self.last_viewport_title.clear();
        let _ = write!(
            self.last_viewport_title,
            "rustdown v{ver} - {file_title}{dirty_mark}"
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
            self.last_viewport_title.clone(),
        ));
    }

    pub(crate) const fn bump_edit_seq(&mut self) {
        self.doc.bump_edit_seq();
    }

    pub(crate) fn refresh_stats_now(&mut self) {
        // Force dirty so the Document method will recompute.
        self.doc.stats_dirty = true;
        self.doc.refresh_stats_if_dirty();
    }

    pub(crate) fn refresh_stats_if_due(&mut self, ctx: &egui::Context) {
        if !self.doc.stats_dirty {
            return;
        }
        if let Some(remaining) = self.doc.debounce_remaining(STATS_RECALC_DEBOUNCE) {
            ctx.request_repaint_after(remaining);
            return;
        }
        self.refresh_stats_now();
    }

    pub(crate) fn note_text_changed(&mut self, defer_stats_recalc: bool) {
        self.doc.mark_text_changed();
        if !defer_stats_recalc {
            self.refresh_stats_now();
        }
    }

    pub(crate) const fn open_search(&mut self, replace_mode: bool) {
        self.search.visible = true;
        self.search.replace_mode = replace_mode;
        self.search.last_replace_count = None;
        self.focus_search = true;
    }

    pub(crate) const fn close_search(&mut self) {
        self.search.visible = false;
        self.search.replace_mode = false;
        self.search.last_replace_count = None;
        self.focus_search = false;
    }

    pub(crate) fn replace_all_matches(&mut self) -> usize {
        let (text, replaced) = replace_all_occurrences(
            self.doc.text.as_str(),
            self.search.query.as_str(),
            self.search.replacement.as_str(),
        );
        if let Cow::Owned(text) = text {
            self.doc.text = Arc::new(text);
            self.bump_edit_seq();
            self.note_text_changed(false);
        }
        replaced
    }

    pub(crate) fn format_document(&mut self) {
        let options = format::options_for_path(self.doc.path.as_deref());
        let formatted = format::format_markdown(self.doc.text.as_str(), options);
        if formatted == self.doc.text.as_str() {
            return;
        }

        self.doc.text = Arc::new(formatted);
        self.bump_edit_seq();
        self.note_text_changed(false);
    }

    pub(crate) fn request_action(&mut self, action: PendingAction) {
        if self.doc.dirty {
            self.pending_action = Some(action);
        } else {
            self.apply_action(action);
        }
    }

    /// Common initialisation for loading a new document (file-based or bundled).
    pub(crate) fn init_document(
        &mut self,
        path: Option<PathBuf>,
        text: String,
        disk_rev: Option<DiskRevision>,
    ) {
        let text = Arc::new(text);
        let base_text = text.clone();
        let image_uri_scheme = path
            .as_deref()
            .map_or_else(String::new, |p| default_image_uri_scheme(Some(p)));
        let next_seq = self.doc.edit_seq.wrapping_add(1);
        self.doc = Document {
            path,
            image_uri_scheme,
            stats: DocumentStats::from_text(text.as_str()),
            text,
            base_text,
            disk_rev,
            stats_dirty: false,
            preview_dirty: false,
            dirty: false,
            preview_cache: rustdown_md::MarkdownCache::default(),
            last_edit_at: None,
            edit_seq: next_seq,
            editor_galley_cache: None,
        };
        self.disk.merge_sidecar_path = None;
        self.nav.invalidate_outline();
        self.clear_side_by_side_scroll_state();
    }

    pub(crate) fn load_document(
        &mut self,
        path: PathBuf,
        text: String,
        disk_rev: Option<DiskRevision>,
    ) {
        self.init_document(Some(path), text, disk_rev);
    }

    /// Load a bundled (compile-time embedded) markdown document.
    /// The document has no file path, so Save will trigger Save As.
    pub(crate) fn load_bundled(&mut self, bundled: BundledDoc) {
        self.init_document(None, bundled.content().to_owned(), None);
        self.error = None;
        self.reset_disk_sync_state();
    }

    pub(crate) fn apply_action(&mut self, action: PendingAction) {
        match action {
            PendingAction::NewBlank => {
                let next_seq = self.doc.edit_seq.wrapping_add(1);
                self.doc = Document::default();
                self.doc.edit_seq = next_seq;
                self.error = None;
                self.disk.merge_sidecar_path = None;
                self.reset_disk_sync_state();
                self.nav.invalidate_outline();
                self.clear_side_by_side_scroll_state();
            }
            PendingAction::Open(path) => self.open_path(path),
            PendingAction::OpenBundled(bundled) => self.load_bundled(bundled),
        }
    }

    pub(crate) fn apply_pending_action_and_close_dialog(&mut self) {
        if let Some(action) = self.pending_action.take() {
            self.apply_action(action);
        }
    }

    pub(crate) fn open_file(&mut self) {
        let Some(path) = markdown_file_dialog().pick_file() else {
            return;
        };
        self.request_action(PendingAction::Open(path));
    }

    pub(crate) fn open_path(&mut self, path: PathBuf) {
        match read_stable_utf8(&path) {
            Ok((text, disk_rev)) => {
                self.load_document(path, text, Some(disk_rev));
                self.error = None;
                self.reset_disk_sync_state();
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                self.load_document(path, String::new(), None);
                self.error = None;
                self.reset_disk_sync_state();
            }
            Err(err) => {
                self.error
                    .get_or_insert_with(|| format!("Open failed: {err}"));
            }
        }
    }

    pub(crate) fn save_path_choice(&self, save_as: bool) -> Option<(PathBuf, bool)> {
        if !save_as && let Some(path) = self.doc.path.clone() {
            return Some((path, false));
        }
        markdown_file_dialog().save_file().map(|path| (path, true))
    }

    pub(crate) fn save_doc(&mut self, save_as: bool) -> bool {
        let Some((path, update_doc_path)) = self.save_path_choice(save_as) else {
            return false;
        };

        let saving_to_current_path = self.doc.path.as_deref() == Some(path.as_path());

        if self.disk.conflict.is_none() && saving_to_current_path {
            match read_stable_utf8(&path) {
                Ok((disk_text, disk_rev)) => self.incorporate_disk_text(disk_text, disk_rev),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => {
                    self.error
                        .get_or_insert_with(|| format!("Pre-save reload failed: {err}"));
                }
            }
        }

        if self.disk.conflict.is_some() {
            return false;
        }

        match atomic_write_utf8(&path, self.doc.text.as_str()) {
            Ok(()) => {
                if update_doc_path {
                    self.doc.path = Some(path.clone());
                    self.doc.image_uri_scheme = default_image_uri_scheme(Some(path.as_path()));
                }
                self.doc.dirty = false;
                self.doc.base_text = self.doc.text.clone();
                self.doc.disk_rev = disk_revision(&path).ok();

                self.error = None;
                self.reset_disk_sync_state();
                true
            }
            Err(err) => {
                self.error = Some(format!("Save failed: {err}"));
                false
            }
        }
    }

    pub(crate) fn write_merge_sidecar(&mut self, doc_path: &Path, conflict_marked: &str) {
        let sidecar_path = match next_merge_sidecar_path(doc_path) {
            Ok(path) => path,
            Err(err) => {
                self.error
                    .get_or_insert_with(|| format!("Merge file path failed: {err}"));
                return;
            }
        };
        match atomic_write_utf8(&sidecar_path, conflict_marked) {
            Ok(()) => self.disk.merge_sidecar_path = Some(sidecar_path),
            Err(err) => {
                self.error
                    .get_or_insert_with(|| format!("Merge file write failed: {err}"));
            }
        }
    }

    pub(crate) fn apply_conflict_choice(&mut self, choice: ConflictChoice) {
        let Some(conflict) = self.disk.conflict.take() else {
            return;
        };

        match choice {
            ConflictChoice::OpenConflictMerge => {
                self.apply_disk_text_state(
                    Arc::new(conflict.conflict_marked),
                    Arc::new(conflict.disk_text),
                    conflict.disk_rev,
                    ReloadKind::ConflictResolved,
                );
            }
            ConflictChoice::KeepMineWriteSidecar => {
                let conflict_marked = conflict.conflict_marked;
                self.apply_disk_text_state(
                    Arc::new(conflict.ours_wins),
                    Arc::new(conflict.disk_text),
                    conflict.disk_rev,
                    ReloadKind::ConflictResolved,
                );
                if let Some(doc_path) = self.doc.path.clone() {
                    self.write_merge_sidecar(doc_path.as_path(), conflict_marked.as_str());
                }
            }
            ConflictChoice::SaveAs => {
                // Save-as switches the active path, so the conflict prompt is no longer relevant.
                if !self.save_doc(true) {
                    self.disk.conflict = Some(conflict);
                    return;
                }
            }
            ConflictChoice::ReloadDisk => {
                let disk_text = Arc::new(conflict.disk_text);
                self.apply_disk_text_state(
                    disk_text.clone(),
                    disk_text,
                    conflict.disk_rev,
                    ReloadKind::Clean,
                );
            }
            ConflictChoice::OverwriteDisk => {
                let Some(path) = self.doc.path.as_deref() else {
                    self.disk.conflict = Some(conflict);
                    return;
                };

                match atomic_write_utf8(path, self.doc.text.as_str()) {
                    Ok(()) => {}
                    Err(err) => {
                        self.disk.conflict = Some(conflict);
                        self.error
                            .get_or_insert_with(|| format!("Overwrite failed: {err}"));
                        return;
                    }
                }

                self.doc.base_text = self.doc.text.clone();
                self.doc.disk_rev = disk_revision(path).ok();
                self.doc.dirty = false;
                self.error = None;
            }
        }

        self.reset_disk_sync_state();
    }
}
