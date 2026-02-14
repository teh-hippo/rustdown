#![forbid(unsafe_code)]

use std::{fs, path::PathBuf};

use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

mod highlight;

fn main() -> eframe::Result {
    let paths = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let app = RustdownApp::from_paths(paths);

    let options = eframe::NativeOptions::default();
    eframe::run_native("rustdown", options, Box::new(move |_cc| Ok(Box::new(app))))
}

#[derive(Default)]
struct RustdownApp {
    docs: Vec<Document>,
    active: usize,
    mode: Mode,
    error: Option<String>,
    dialog: Option<Dialog>,
}

#[derive(Default)]
struct Document {
    title: String,
    path: Option<PathBuf>,
    text: String,
    dirty: bool,
    md_cache: CommonMarkCache,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Edit,
    Preview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialog {
    ConfirmClose { idx: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveResult {
    Saved,
    Cancelled,
    Failed,
}

impl eframe::App for RustdownApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.docs.is_empty() {
            self.new_blank_doc("Untitled".to_owned());
        } else {
            self.active = self.active.min(self.docs.len().saturating_sub(1));
        }

        let window_title = {
            let doc = &self.docs[self.active];
            let mut title = format!("rustdown — {}", doc.title);
            if doc.dirty {
                title.push('*');
            }
            if self.mode == Mode::Preview {
                title.push_str(" (Preview)");
            }
            title
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(window_title));

        let dialog_open = self.dialog.is_some();
        let (open, save, save_as, new_tab, close_tab, toggle_mode, next_tab, prev_tab) =
            ctx.input(|i| {
                let cmd = i.modifiers.command;
                (
                    cmd && i.key_pressed(egui::Key::O),
                    cmd && i.key_pressed(egui::Key::S) && !i.modifiers.shift,
                    cmd && i.key_pressed(egui::Key::S) && i.modifiers.shift,
                    cmd && i.key_pressed(egui::Key::N),
                    cmd && i.key_pressed(egui::Key::W),
                    cmd && i.key_pressed(egui::Key::Enter),
                    cmd && i.key_pressed(egui::Key::Tab) && !i.modifiers.shift,
                    cmd && i.key_pressed(egui::Key::Tab) && i.modifiers.shift,
                )
            });

        if !dialog_open {
            if open {
                self.open_file();
            }
            if save_as {
                self.save_as_active();
            } else if save {
                self.save_active();
            }
            if new_tab {
                let next = self.docs.len() + 1;
                self.new_blank_doc(format!("Untitled {next}"));
            }
            if close_tab {
                self.request_close_doc(self.active);
            }
            if toggle_mode {
                self.toggle_mode();
            }
            if next_tab && !self.docs.is_empty() {
                self.active = (self.active + 1) % self.docs.len();
            }
            if prev_tab && !self.docs.is_empty() {
                self.active = (self.active + self.docs.len() - 1) % self.docs.len();
            }
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Open…").clicked() {
                    self.open_file();
                }

                if ui.button("Save").clicked() {
                    self.save_active();
                }

                if ui.button("Save As…").clicked() {
                    self.save_as_active();
                }

                if ui.button("Save All").clicked() {
                    self.save_all();
                }

                ui.separator();

                let mut tab_action = None;
                for idx in 0..self.docs.len() {
                    let selected = idx == self.active;
                    let doc_title = self.docs[idx].title.clone();
                    let doc_path = self.docs[idx].path.clone();
                    let doc_dirty = self.docs[idx].dirty;

                    let mut label = doc_title;
                    if doc_dirty {
                        label.push('*');
                    }

                    let mut response = ui.selectable_label(selected, label);
                    if let Some(path) = doc_path.as_deref() {
                        response = response.on_hover_text(path.display().to_string());
                    }
                    if response.clicked() {
                        self.active = idx;
                    }

                    response.context_menu(|ui| {
                        if ui.button("Save").clicked() {
                            tab_action = Some(TabAction::Save(idx));
                            ui.close_menu();
                        }
                        if ui.button("Save As…").clicked() {
                            tab_action = Some(TabAction::SaveAs(idx));
                            ui.close_menu();
                        }
                        if ui.button("Save all").clicked() {
                            tab_action = Some(TabAction::SaveAll);
                            ui.close_menu();
                        }

                        ui.separator();

                        if ui.button("Close").clicked() {
                            tab_action = Some(TabAction::Close(idx));
                            ui.close_menu();
                        }
                        if ui.button("Close others").clicked() {
                            tab_action = Some(TabAction::CloseOthers(idx));
                            ui.close_menu();
                        }
                        if ui.button("Close all").clicked() {
                            tab_action = Some(TabAction::CloseAll);
                            ui.close_menu();
                        }
                    });
                }

                if let Some(action) = tab_action {
                    self.apply_tab_action(action);
                }

                if ui.button("+").clicked() {
                    let next = self.docs.len() + 1;
                    self.new_blank_doc(format!("Untitled {next}"));
                }

                if ui.button("Close").clicked() {
                    self.request_close_doc(self.active);
                }

                ui.separator();

                let label = match self.mode {
                    Mode::Edit => "Preview",
                    Mode::Preview => "Edit",
                };
                if ui.button(label).clicked() {
                    self.toggle_mode();
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let doc = &mut self.docs[self.active];
            match self.mode {
                Mode::Edit => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let mut layouter = |ui: &egui::Ui, string: &str, wrap_width: f32| {
                            let mut job = highlight::markdown_layout_job(ui, string);
                            job.wrap.max_width = wrap_width;
                            ui.fonts(|fonts| fonts.layout_job(job))
                        };

                        let response = ui.add(
                            egui::TextEdit::multiline(&mut doc.text)
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace)
                                .layouter(&mut layouter),
                        );
                        if response.changed() {
                            doc.md_cache = CommonMarkCache::default();
                            doc.dirty = true;
                        }
                    });
                }
                Mode::Preview => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        CommonMarkViewer::new().show(ui, &mut doc.md_cache, &doc.text);
                    });
                }
            }
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            let doc = &self.docs[self.active];
            let mode = match self.mode {
                Mode::Edit => "Edit",
                Mode::Preview => "Preview",
            };

            let mut clear_error = false;

            ui.horizontal(|ui| {
                ui.label(mode);
                ui.separator();

                if let Some(path) = doc.path.as_deref() {
                    ui.label(path.display().to_string());
                } else {
                    ui.label("Unsaved");
                }

                if doc.dirty {
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

        self.show_dialogs(ctx);
    }
}

impl RustdownApp {
    fn from_paths(paths: Vec<PathBuf>) -> Self {
        let mut app = Self::default();

        for path in paths {
            app.open_path(path);
        }

        if app.docs.is_empty() {
            app.new_blank_doc("Untitled".to_owned());
        } else {
            app.active = 0;
        }

        app
    }

    fn new_blank_doc(&mut self, title: String) {
        self.docs.push(Document {
            title,
            path: None,
            text: String::new(),
            dirty: false,
            md_cache: CommonMarkCache::default(),
        });
        self.active = self.docs.len() - 1;
    }

    fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            Mode::Edit => Mode::Preview,
            Mode::Preview => Mode::Edit,
        };
    }

    fn open_file(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Markdown", &["md", "markdown"])
            .pick_file()
        else {
            return;
        };

        self.open_path(path);
    }

    fn save_active(&mut self) {
        let _ = self.save_doc(self.active, false);
    }

    fn save_as_active(&mut self) {
        let _ = self.save_doc(self.active, true);
    }

    fn save_all(&mut self) {
        for idx in 0..self.docs.len() {
            if !self.docs[idx].dirty {
                continue;
            }

            let save_as = self.docs[idx].path.is_none();
            match self.save_doc(idx, save_as) {
                SaveResult::Saved => {}
                SaveResult::Cancelled => {
                    self.error = Some("Save all cancelled".to_owned());
                    break;
                }
                SaveResult::Failed => break,
            }
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        let path = fs::canonicalize(&path).unwrap_or(path);
        if let Some(idx) = self
            .docs
            .iter()
            .position(|doc| doc.path.as_deref() == Some(&path))
        {
            self.active = idx;
            self.error = None;
            return;
        }

        if let Ok(meta) = fs::metadata(&path)
            && meta.len() > rustdown_core::MAX_FILE_BYTES
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
                let title = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Untitled")
                    .to_owned();

                self.docs.push(Document {
                    title,
                    path: Some(path),
                    text,
                    dirty: false,
                    md_cache: CommonMarkCache::default(),
                });
                self.active = self.docs.len() - 1;
                self.error = None;
            }
            Err(err) => {
                self.error.get_or_insert(format!("Open failed: {err}"));
            }
        }
    }

    fn save_doc(&mut self, idx: usize, save_as: bool) -> SaveResult {
        let doc = &mut self.docs[idx];

        let chosen = if save_as { None } else { doc.path.clone() };
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

        match fs::write(&path, &doc.text) {
            Ok(()) => {
                let path = fs::canonicalize(&path).unwrap_or(path);
                doc.title = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Untitled")
                    .to_owned();
                doc.path = Some(path);
                doc.dirty = false;
                self.error = None;
                SaveResult::Saved
            }
            Err(err) => {
                self.error = Some(format!("Save failed: {err}"));
                SaveResult::Failed
            }
        }
    }

    fn request_close_doc(&mut self, idx: usize) {
        if idx >= self.docs.len() {
            return;
        }

        if self.docs[idx].dirty {
            self.dialog = Some(Dialog::ConfirmClose { idx });
        } else {
            self.force_close_doc(idx);
        }
    }

    fn force_close_doc(&mut self, idx: usize) {
        self.docs.remove(idx);
        if self.docs.is_empty() {
            self.new_blank_doc("Untitled".to_owned());
            return;
        }

        if self.active > idx {
            self.active -= 1;
        }
        self.active = self.active.min(self.docs.len() - 1);
    }

    fn close_others(&mut self, keep: usize) {
        if self
            .docs
            .iter()
            .enumerate()
            .any(|(idx, doc)| idx != keep && doc.dirty)
        {
            self.error = Some("Unsaved changes — close tabs individually".to_owned());
            return;
        }

        let keep_doc = self.docs.remove(keep);
        self.docs.clear();
        self.docs.push(keep_doc);
        self.active = 0;
    }

    fn close_all(&mut self) {
        if self.docs.iter().any(|doc| doc.dirty) {
            self.error = Some("Unsaved changes — close tabs individually".to_owned());
            return;
        }

        self.docs.clear();
        self.new_blank_doc("Untitled".to_owned());
    }

    fn apply_tab_action(&mut self, action: TabAction) {
        match action {
            TabAction::Save(idx) => {
                let _ = self.save_doc(idx, false);
            }
            TabAction::SaveAs(idx) => {
                let _ = self.save_doc(idx, true);
            }
            TabAction::SaveAll => self.save_all(),
            TabAction::Close(idx) => self.request_close_doc(idx),
            TabAction::CloseOthers(idx) => self.close_others(idx),
            TabAction::CloseAll => self.close_all(),
        }
    }

    fn show_dialogs(&mut self, ctx: &egui::Context) {
        let Some(Dialog::ConfirmClose { idx }) = self.dialog else {
            return;
        };

        if idx >= self.docs.len() {
            self.dialog = None;
            return;
        }

        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if escape {
            self.dialog = None;
            return;
        }

        let title = self.docs[idx].title.clone();

        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!("\"{title}\" has unsaved changes."));
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() && self.save_doc(idx, false) == SaveResult::Saved
                    {
                        self.force_close_doc(idx);
                        self.dialog = None;
                    }

                    if ui.button("Discard").clicked() {
                        self.force_close_doc(idx);
                        self.dialog = None;
                    }

                    if ui.button("Cancel").clicked() {
                        self.dialog = None;
                    }
                });
            });
    }
}

#[derive(Clone, Copy, Debug)]
enum TabAction {
    Save(usize),
    SaveAs(usize),
    SaveAll,
    Close(usize),
    CloseOthers(usize),
    CloseAll,
}
