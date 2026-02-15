#![forbid(unsafe_code)]
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

#[cfg(target_arch = "wasm32")]
compile_error!("rustdown is a native desktop app; web/wasm builds are not supported.");

use std::{
    borrow::Cow,
    ffi::OsString,
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

mod format;
mod highlight;

const DEBOUNCE: Duration = Duration::from_millis(150);
const ZOOM_STEP: f32 = 0.1;
const MIN_ZOOM_FACTOR: f32 = 0.5;
const MAX_ZOOM_FACTOR: f32 = 3.0;

#[derive(Clone, Debug, PartialEq, Eq)]
struct LaunchOptions {
    mode: Mode,
    path: Option<PathBuf>,
}

fn parse_launch_options<I, S>(args: I) -> LaunchOptions
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut mode = Mode::Edit;
    let mut path = None;

    for arg in args {
        let arg = arg.into();
        if arg == "-p" {
            mode = Mode::Preview;
            continue;
        }
        if arg == "-s" {
            mode = Mode::SideBySide;
            continue;
        }

        if path.is_none() {
            path = Some(PathBuf::from(arg));
        }
    }

    LaunchOptions { mode, path }
}

fn main() -> eframe::Result {
    let launch_options = parse_launch_options(std::env::args_os().skip(1));
    let app = RustdownApp::from_launch_options(launch_options);

    // Viewport sizes are in points, so they scale with the OS DPI factor.
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1024.0, 768.0])
            .with_min_inner_size([480.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native("rustdown", options, Box::new(move |_cc| Ok(Box::new(app))))
}

struct RustdownApp {
    doc: Document,
    mode: Mode,
    error: Option<String>,
    pending_action: Option<PendingAction>,
}

impl Default for RustdownApp {
    fn default() -> Self {
        Self {
            doc: Document::default(),
            mode: Mode::Edit,
            error: None,
            pending_action: None,
        }
    }
}

#[derive(Default)]
struct Document {
    path: Option<PathBuf>,
    text: String,
    dirty: bool,
    md_cache: CommonMarkCache,
    last_edit_at: Option<Instant>,
}

impl Document {
    fn debounce_remaining(&self, debounce: Duration) -> Option<Duration> {
        let last = self.last_edit_at?;
        let since = last.elapsed();
        (since < debounce).then(|| debounce - since)
    }

    fn title(&self) -> Cow<'_, str> {
        self.path
            .as_ref()
            .and_then(|path| path.file_name())
            .map_or_else(|| Cow::Borrowed("Untitled"), |name| name.to_string_lossy())
    }

    fn path_label(&self) -> Cow<'_, str> {
        self.path
            .as_ref()
            .map_or_else(|| Cow::Borrowed("Unsaved"), |path| path.to_string_lossy())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Edit,
    Preview,
    SideBySide,
}

impl Mode {
    fn cycle(self) -> Self {
        [Mode::Preview, Mode::SideBySide, Mode::Edit][self as usize]
    }

    fn label(self) -> &'static str {
        ["Edit", "Preview", "Side-by-side"][self as usize]
    }
}

#[derive(Clone, Debug)]
enum PendingAction {
    NewBlank,
    Open(PathBuf),
}

impl eframe::App for RustdownApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let dialog_open = self.pending_action.is_some();
        let (open, save, save_as, new_doc, cycle_mode, format_doc, zoom_in, zoom_out) =
            ctx.input(|i| {
                let cmd = i.modifiers.command;
                (
                    cmd && i.key_pressed(egui::Key::O),
                    cmd && i.key_pressed(egui::Key::S) && !i.modifiers.shift,
                    cmd && i.key_pressed(egui::Key::S) && i.modifiers.shift,
                    cmd && i.key_pressed(egui::Key::N),
                    cmd && i.key_pressed(egui::Key::Enter),
                    cmd && i.modifiers.shift && i.key_pressed(egui::Key::F),
                    cmd && i.key_pressed(egui::Key::Equals),
                    cmd && i.key_pressed(egui::Key::Minus),
                )
            });

        if !dialog_open {
            if open {
                self.open_file();
            }
            if save_as || save {
                let _ = self.save_doc(save_as);
            }
            if new_doc {
                self.request_action(PendingAction::NewBlank);
            }
            if cycle_mode {
                self.set_mode(self.mode.cycle());
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
        }

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            let mut clear_error = false;

            ui.horizontal(|ui| {
                for mode in [Mode::Edit, Mode::Preview, Mode::SideBySide] {
                    if ui
                        .selectable_label(self.mode == mode, mode.label())
                        .clicked()
                    {
                        self.set_mode(mode);
                    }
                }

                ui.separator();

                ui.label(self.doc.path_label());

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

        let panel_frame = egui::Frame::none()
            .fill(ctx.style().visuals.panel_fill)
            .inner_margin(egui::Margin::same(0.0));
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

        self.show_dialogs(ctx);
        self.update_viewport_title(ctx);
    }
}

impl RustdownApp {
    fn from_launch_options(options: LaunchOptions) -> Self {
        let mut app = Self::default();
        app.set_mode(options.mode);
        if let Some(path) = options.path {
            app.open_path(path);
        }
        app
    }

    fn set_mode(&mut self, mode: Mode) {
        if self.mode == mode {
            return;
        }

        self.mode = mode;

        if mode == Mode::Edit {
            self.doc.md_cache.clear_scrollable();
            self.doc.last_edit_at = None;
        }
    }

    fn adjust_zoom(&self, ctx: &egui::Context, delta: f32) {
        let zoom = (ctx.zoom_factor() + delta).clamp(MIN_ZOOM_FACTOR, MAX_ZOOM_FACTOR);
        ctx.set_zoom_factor(zoom);
    }

    fn update_viewport_title(&self, ctx: &egui::Context) {
        let mode = match self.mode {
            Mode::Preview => " (Preview)",
            Mode::SideBySide => " (Side-by-side)",
            Mode::Edit => "",
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "rustdown — {}{}{}",
            self.doc.title(),
            if self.doc.dirty { "*" } else { "" },
            mode
        )));
    }

    fn note_text_changed(&mut self) {
        self.doc.dirty = true;
        self.doc.last_edit_at = Some(Instant::now());
        self.doc.md_cache.clear_scrollable();
    }

    fn format_document(&mut self) {
        let options = format::options_for_path(self.doc.path.as_deref());
        let formatted = format::format_markdown(self.doc.text.as_str(), options);
        if formatted == self.doc.text {
            return;
        }

        self.doc.text = formatted;
        self.note_text_changed();
    }

    fn show_editor(&mut self, ui: &mut egui::Ui) {
        let Document { text, .. } = &mut self.doc;

        let editor = egui::TextEdit::multiline(text)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Body)
            .frame(false)
            .id(egui::Id::new("editor"));

        let mut layouter = |ui: &egui::Ui, string: &str, wrap_width: f32| {
            let mut job = highlight::markdown_layout_job(ui, string);
            job.wrap.max_width = wrap_width;
            ui.fonts(|fonts| fonts.layout_job(job))
        };

        let response = ui.add_sized(ui.available_size(), editor.layouter(&mut layouter));

        if response.changed() {
            self.note_text_changed();
        }
    }

    fn show_preview(&mut self, ui: &mut egui::Ui) {
        if self.mode == Mode::SideBySide
            && let Some(remaining) = self.doc.debounce_remaining(DEBOUNCE)
        {
            ui.ctx().request_repaint_after(remaining);
            return;
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
        } else {
            self.apply_action(action);
        }
    }

    fn apply_action(&mut self, action: PendingAction) {
        match action {
            PendingAction::NewBlank => {
                self.doc = Document::default();
                self.error = None;
            }
            PendingAction::Open(path) => self.open_path(path),
        }
    }

    fn apply_pending_action_and_close_dialog(&mut self) {
        if let Some(action) = self.pending_action.take() {
            self.apply_action(action);
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
        match fs::read_to_string(&path) {
            Ok(text) => {
                self.doc = Document {
                    path: Some(path),
                    text,
                    dirty: false,
                    md_cache: CommonMarkCache::default(),
                    last_edit_at: None,
                };
                self.error = None;
            }
            Err(err) => {
                self.error.get_or_insert(format!("Open failed: {err}"));
            }
        }
    }

    fn save_doc(&mut self, save_as: bool) -> bool {
        let chosen = if save_as { None } else { self.doc.path.clone() };
        let path = match chosen {
            Some(path) => path,
            None => {
                let Some(path) = rfd::FileDialog::new()
                    .add_filter("Markdown", &["md", "markdown"])
                    .save_file()
                else {
                    return false;
                };
                path
            }
        };

        match fs::write(&path, &self.doc.text) {
            Ok(()) => {
                self.doc.path = Some(path);
                self.doc.dirty = false;

                self.error = None;
                true
            }
            Err(err) => {
                self.error = Some(format!("Save failed: {err}"));
                false
            }
        }
    }

    fn show_dialogs(&mut self, ctx: &egui::Context) {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> LaunchOptions {
        parse_launch_options(args.iter().copied().map(OsString::from))
    }

    #[test]
    fn parse_launch_options_parses_modes_and_paths() {
        let cases = [
            (&[][..], Mode::Edit, None),
            (&["-p"][..], Mode::Preview, None),
            (&["-s"][..], Mode::SideBySide, None),
            (
                &["README.md", "OTHER.md"][..],
                Mode::Edit,
                Some("README.md"),
            ),
            (&["-p", "README.md"][..], Mode::Preview, Some("README.md")),
        ];

        for (args, mode, path) in cases {
            let options = parse(args);
            assert_eq!(options.mode, mode);
            assert_eq!(options.path.as_deref(), path.map(PathBuf::from).as_deref());
        }
    }
}
