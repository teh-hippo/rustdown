#![forbid(unsafe_code)]
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

#[cfg(target_arch = "wasm32")]
compile_error!("rustdown is a native desktop app; web/wasm builds are not supported.");

use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

mod format;
mod highlight;

/// Hard cap on file sizes we will load into memory.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Debounce expensive syntax highlighting while typing.
const EDITOR_HIGHLIGHT_DEBOUNCE: Duration = Duration::from_millis(150);

/// Side-by-side preview is throttled for responsiveness while typing.
const SIDEBAR_LIVE_PREVIEW_DEBOUNCE: Duration = Duration::from_millis(150);

fn main() -> eframe::Result {
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let app = RustdownApp::from_paths(paths);

    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "rustdown",
        options,
        Box::new(move |cc| {
            configure_ui(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}

fn configure_ui(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(16.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::new(16.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(20.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        egui::FontId::new(15.0, egui::FontFamily::Monospace),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::new(13.0, egui::FontFamily::Proportional),
    );
    ctx.set_style(style);
}

struct RustdownApp {
    doc: Document,
    mode: Mode,
    error: Option<String>,
    dialog: Option<Dialog>,
    pending_action: Option<PendingAction>,
    needs_title_update: bool,
}

impl Default for RustdownApp {
    fn default() -> Self {
        Self {
            doc: Document::blank(),
            mode: Mode::Edit,
            error: None,
            dialog: None,
            pending_action: None,
            needs_title_update: true,
        }
    }
}

struct Document {
    title: String,
    path: Option<PathBuf>,
    path_label: String,
    text: String,
    dirty: bool,
    md_cache: CommonMarkCache,
    edit_revision: u64,
    md_cache_revision: u64,
    last_edit_at: Option<Instant>,
    highlight_cache: Option<HighlightCache>,
}

impl Document {
    fn blank() -> Self {
        Self {
            title: "Untitled".to_owned(),
            path: None,
            path_label: "Unsaved".to_owned(),
            text: String::new(),
            dirty: false,
            md_cache: CommonMarkCache::default(),
            edit_revision: 0,
            md_cache_revision: 0,
            last_edit_at: None,
            highlight_cache: None,
        }
    }

    fn from_loaded_file(path: PathBuf, text: String) -> Self {
        let title = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled")
            .to_owned();
        let path_label = path.display().to_string();

        Self {
            title,
            path: Some(path),
            path_label,
            text,
            dirty: false,
            md_cache: CommonMarkCache::default(),
            edit_revision: 0,
            md_cache_revision: 0,
            last_edit_at: None,
            highlight_cache: None,
        }
    }

    fn set_saved_path(&mut self, path: PathBuf) {
        self.title = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled")
            .to_owned();
        self.path_label = path.display().to_string();
        self.path = Some(path);
    }

    fn debounce_remaining(&self, debounce: Duration) -> Option<Duration> {
        let last = self.last_edit_at?;
        let since = last.elapsed();
        if since < debounce {
            Some(debounce - since)
        } else {
            None
        }
    }
}

struct HighlightCache {
    revision: u64,
    wrap_width_bits: u32,
    text_len: usize,
    galley: std::sync::Arc<egui::Galley>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Edit,
    Preview,
    SideBySide,
}

impl Mode {
    fn cycle(self) -> Self {
        match self {
            Mode::Edit => Mode::Preview,
            Mode::Preview => Mode::SideBySide,
            Mode::SideBySide => Mode::Edit,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Mode::Edit => "Edit",
            Mode::Preview => "Preview",
            Mode::SideBySide => "Side-by-side",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialog {
    UnsavedChanges,
}

#[derive(Clone, Debug)]
enum PendingAction {
    NewBlank,
    Open(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveResult {
    Saved,
    Cancelled,
    Failed,
}

impl eframe::App for RustdownApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let dialog_open = self.dialog.is_some();
        let (open, save, save_as, new_doc, cycle_mode, format_doc) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            (
                cmd && i.key_pressed(egui::Key::O),
                cmd && i.key_pressed(egui::Key::S) && !i.modifiers.shift,
                cmd && i.key_pressed(egui::Key::S) && i.modifiers.shift,
                cmd && i.key_pressed(egui::Key::N),
                cmd && i.key_pressed(egui::Key::Enter),
                cmd && i.modifiers.shift && i.key_pressed(egui::Key::F),
            )
        });

        if !dialog_open {
            if open {
                self.open_file();
            }
            if save_as {
                let _ = self.save_doc(true);
            } else if save {
                let _ = self.save_doc(false);
            }
            if new_doc {
                self.request_action(PendingAction::NewBlank);
            }
            if cycle_mode {
                let next = self.mode.cycle();
                self.set_mode(next);
            }
            if format_doc {
                self.format_document();
            }
        }

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            let mut clear_error = false;

            ui.horizontal(|ui| {
                let mut mode_selected = None;
                for mode in [Mode::Edit, Mode::Preview, Mode::SideBySide] {
                    if ui
                        .selectable_label(self.mode == mode, mode.label())
                        .clicked()
                    {
                        mode_selected = Some(mode);
                    }
                }
                if let Some(mode) = mode_selected {
                    self.set_mode(mode);
                }

                ui.separator();

                ui.label(self.doc.path_label.as_str());

                if self.doc.dirty {
                    ui.separator();
                    ui.colored_label(ui.visuals().warn_fg_color, "Modified");
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(error) = self.error.as_deref() {
                        if ui.button("x").clicked() {
                            clear_error = true;
                        }
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                });
            });

            if clear_error {
                self.error = None;
            }
        });

        // Side-by-side preview goes on the right.
        if self.mode == Mode::SideBySide {
            egui::SidePanel::right("preview")
                .resizable(true)
                .min_width(240.0)
                .default_width(420.0)
                .frame(
                    egui::Frame::none()
                        .fill(ctx.style().visuals.panel_fill)
                        .inner_margin(egui::Margin::same(0.0)),
                )
                .show(ctx, |ui| self.show_preview(ui));
        }

        // CentralPanel should always be last (egui panel layout depends on the order they are added).
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(ctx.style().visuals.panel_fill)
                    .inner_margin(egui::Margin::same(0.0)),
            )
            .show(ctx, |ui| match self.mode {
                Mode::Edit | Mode::SideBySide => self.show_editor(ui),
                Mode::Preview => self.show_preview(ui),
            });

        self.show_dialogs(ctx);
        self.update_viewport_title(ctx);
    }
}

impl RustdownApp {
    fn from_paths(paths: Vec<PathBuf>) -> Self {
        let mut app = Self::default();
        if let Some(path) = paths.into_iter().next() {
            app.open_path(path);
        }
        app
    }

    fn set_mode(&mut self, mode: Mode) {
        if self.mode == mode {
            return;
        }

        self.mode = mode;
        self.needs_title_update = true;

        // Free preview cache when preview isn't visible.
        if mode == Mode::Edit {
            self.doc.md_cache = CommonMarkCache::default();
            self.doc.md_cache_revision = self.doc.edit_revision;
            self.doc.last_edit_at = None;
        }
    }

    fn update_viewport_title(&mut self, ctx: &egui::Context) {
        if !self.needs_title_update {
            return;
        }

        let mut title = String::with_capacity("rustdown — ".len() + self.doc.title.len() + 32);
        title.push_str("rustdown — ");
        title.push_str(&self.doc.title);
        if self.doc.dirty {
            title.push('*');
        }
        match self.mode {
            Mode::Edit => {}
            Mode::Preview => title.push_str(" (Preview)"),
            Mode::SideBySide => title.push_str(" (Side-by-side)"),
        }

        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        self.needs_title_update = false;
    }

    fn note_text_changed(&mut self) {
        if !self.doc.dirty {
            self.needs_title_update = true;
        }
        self.doc.dirty = true;
        self.doc.edit_revision = self.doc.edit_revision.wrapping_add(1);
        self.doc.last_edit_at = Some(Instant::now());
        self.doc.highlight_cache = None;
    }

    fn format_document(&mut self) {
        let options = format::options_for_path(self.doc.path.as_deref());
        let formatted = format::format_markdown(self.doc.text.as_str(), options);
        if formatted == self.doc.text {
            return;
        }

        self.doc.text = formatted;
        self.note_text_changed();
        self.doc.md_cache = CommonMarkCache::default();
        self.doc.md_cache_revision = self.doc.edit_revision;
    }

    fn show_editor(&mut self, ui: &mut egui::Ui) {
        let mut highlight = true;
        if let Some(remaining) = self.doc.debounce_remaining(EDITOR_HIGHLIGHT_DEBOUNCE) {
            highlight = false;
            ui.ctx().request_repaint_after(remaining);
        }

        let revision = self.doc.edit_revision;
        let Document {
            text,
            highlight_cache,
            ..
        } = &mut self.doc;

        // Allocate the full panel to the editor so it visually blends with the window.
        let editor = egui::TextEdit::multiline(text)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Body)
            .frame(false)
            .id(egui::Id::new("editor"));

        let response = if highlight {
            let mut layouter = |ui: &egui::Ui, string: &str, wrap_width: f32| {
                let wrap_width_bits = wrap_width.to_bits();
                let text_len = string.len();
                if let Some(cache) = highlight_cache.as_ref()
                    && cache.revision == revision
                    && cache.wrap_width_bits == wrap_width_bits
                    && cache.text_len == text_len
                {
                    return cache.galley.clone();
                }

                let mut job = highlight::markdown_layout_job(ui, string);
                job.wrap.max_width = wrap_width;
                let galley = ui.fonts(|fonts| fonts.layout_job(job));
                *highlight_cache = Some(HighlightCache {
                    revision,
                    wrap_width_bits,
                    text_len,
                    galley: galley.clone(),
                });
                galley
            };

            ui.add_sized(ui.available_size(), editor.layouter(&mut layouter))
        } else {
            ui.add_sized(ui.available_size(), editor)
        };

        if response.changed() {
            self.note_text_changed();
        }
    }

    fn show_preview(&mut self, ui: &mut egui::Ui) {
        let side_by_side = self.mode == Mode::SideBySide;

        if side_by_side
            && let Some(remaining) = self.doc.debounce_remaining(SIDEBAR_LIVE_PREVIEW_DEBOUNCE)
        {
            ui.ctx().request_repaint_after(remaining);
            ui.centered_and_justified(|ui| {
                ui.label("Preview paused while typing…");
            });
            return;
        }

        if self.doc.md_cache_revision != self.doc.edit_revision {
            self.doc.md_cache = CommonMarkCache::default();
            self.doc.md_cache_revision = self.doc.edit_revision;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                CommonMarkViewer::new().show(ui, &mut self.doc.md_cache, &self.doc.text);
            });
    }

    fn request_action(&mut self, action: PendingAction) {
        if self.doc.dirty {
            self.pending_action = Some(action);
            self.dialog = Some(Dialog::UnsavedChanges);
        } else {
            self.apply_action(action);
        }
    }

    fn apply_action(&mut self, action: PendingAction) {
        match action {
            PendingAction::NewBlank => {
                self.doc = Document::blank();
                self.error = None;
                self.needs_title_update = true;
            }
            PendingAction::Open(path) => self.open_path(path),
        }
    }

    fn open_file(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Markdown", &["md", "markdown"])
            .pick_file()
        else {
            return;
        };

        self.request_action(PendingAction::Open(path));
    }

    fn open_path(&mut self, path: PathBuf) {
        let path = fs::canonicalize(&path).unwrap_or(path);

        if let Ok(meta) = fs::metadata(&path)
            && meta.len() > MAX_FILE_BYTES
        {
            self.error = Some(format!(
                "Refusing to open {} ({} MiB) — too large",
                path.display(),
                meta.len() / (1024 * 1024)
            ));
            return;
        }

        match fs::read_to_string(&path) {
            Ok(text) => {
                self.doc = Document::from_loaded_file(path, text);
                self.error = None;
                self.needs_title_update = true;
            }
            Err(err) => {
                self.error.get_or_insert(format!("Open failed: {err}"));
            }
        }
    }

    fn save_doc(&mut self, save_as: bool) -> SaveResult {
        let chosen = if save_as { None } else { self.doc.path.clone() };
        let path = match chosen {
            Some(path) => path,
            None => {
                let Some(path) = rfd::FileDialog::new()
                    .add_filter("Markdown", &["md", "markdown"])
                    .save_file()
                else {
                    return SaveResult::Cancelled;
                };
                path
            }
        };

        match fs::write(&path, &self.doc.text) {
            Ok(()) => {
                let path = fs::canonicalize(&path).unwrap_or(path);
                self.doc.set_saved_path(path);
                self.doc.dirty = false;
                self.needs_title_update = true;

                let len = self.doc.text.len();
                if self.doc.text.capacity() > len.saturating_mul(2) {
                    self.doc.text.shrink_to_fit();
                }

                self.error = None;
                SaveResult::Saved
            }
            Err(err) => {
                self.error = Some(format!("Save failed: {err}"));
                SaveResult::Failed
            }
        }
    }

    fn show_dialogs(&mut self, ctx: &egui::Context) {
        let Some(Dialog::UnsavedChanges) = self.dialog else {
            return;
        };

        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if escape {
            self.dialog = None;
            self.pending_action = None;
            return;
        }

        let title = self.doc.title.clone();

        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!("\"{title}\" has unsaved changes."));
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() && self.save_doc(false) == SaveResult::Saved {
                        if let Some(action) = self.pending_action.take() {
                            self.apply_action(action);
                        }
                        self.dialog = None;
                    }

                    if ui.button("Discard").clicked() {
                        if let Some(action) = self.pending_action.take() {
                            self.apply_action(action);
                        }
                        self.dialog = None;
                    }

                    if ui.button("Cancel").clicked() {
                        self.dialog = None;
                        self.pending_action = None;
                    }
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounce_remaining_is_some_when_recent() {
        let mut doc = Document::blank();
        doc.last_edit_at = Some(Instant::now());
        assert!(doc.debounce_remaining(Duration::from_millis(10)).is_some());
    }

    #[test]
    fn debounce_remaining_is_none_when_old() {
        let mut doc = Document::blank();
        let now = Instant::now();
        doc.last_edit_at = match now.checked_sub(Duration::from_millis(20)) {
            Some(t) => Some(t),
            None => Some(now),
        };
        assert!(doc.debounce_remaining(Duration::from_millis(5)).is_none());
    }

    #[test]
    fn document_from_loaded_file_sets_labels() {
        let path = PathBuf::from("note.md");
        let doc = Document::from_loaded_file(path.clone(), "hello\n".to_owned());

        assert_eq!(doc.title, "note.md");
        assert_eq!(doc.path, Some(path));
        assert_eq!(doc.path_label, "note.md");
        assert_eq!(doc.text, "hello\n");
        assert!(!doc.dirty);
    }

    #[test]
    fn document_set_saved_path_updates_title_and_label() {
        let mut doc = Document::blank();
        doc.set_saved_path(PathBuf::from("saved.md"));
        assert_eq!(doc.title, "saved.md");
        assert_eq!(doc.path, Some(PathBuf::from("saved.md")));
        assert_eq!(doc.path_label, "saved.md");
    }

    #[test]
    fn typing_first_character_keeps_cursor_position() {
        let ctx = egui::Context::default();
        let mut app = RustdownApp::default();

        let screen_rect =
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(800.0, 600.0));

        ctx.memory_mut(|mem| mem.request_focus(egui::Id::new("editor")));

        // Type the first character.
        let input = egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::Text("H".to_owned())],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
        });
        assert_eq!(app.doc.text, "H");

        // The second character should be inserted after the first one.
        let input = egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::Text("e".to_owned())],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
        });
        assert_eq!(app.doc.text, "He");
    }

    #[test]
    #[ignore]
    fn perf_commonmark_render() {
        let ctx = egui::Context::default();
        let iters = 10u32;
        let mut cache = CommonMarkCache::default();
        let chunk = "# Heading\n\nSome text with `code` and **bold**.\n\n- item a\n- item b\n\n```rs\nlet x = 1;\n```\n\n";

        for target_bytes in [32 * 1024usize, 96 * 1024, 160 * 1024] {
            let mut markdown = String::with_capacity(target_bytes + chunk.len());
            while markdown.len() < target_bytes {
                markdown.push_str(chunk);
            }

            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let _ = ctx.run(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let t0 = Instant::now();
                        CommonMarkViewer::new().show(ui, &mut cache, &markdown);
                        total += t0.elapsed();
                    });
                });
            }

            eprintln!(
                "commonmark: bytes={} avg={:?}",
                markdown.len(),
                total / iters
            );
        }
    }
}
