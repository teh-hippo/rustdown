#![forbid(unsafe_code)]

use std::{fs, path::PathBuf};

use eframe::egui;

mod highlight;
mod preview;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "rustdown",
        options,
        Box::new(|_cc| Ok(Box::new(RustdownApp::default()))),
    )
}

#[derive(Default)]
struct RustdownApp {
    docs: Vec<Document>,
    active: usize,
    mode: Mode,
    error: Option<String>,
}

#[derive(Default)]
struct Document {
    title: String,
    path: Option<PathBuf>,
    text: String,
    dirty: bool,
    preview: Option<preview::PreviewDoc>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Edit,
    Preview,
}

impl eframe::App for RustdownApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.docs.is_empty() {
            self.new_blank_doc("Untitled".to_owned());
        } else {
            self.active = self.active.min(self.docs.len().saturating_sub(1));
        }

        let (open, save, new_tab, close_tab, toggle_mode) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            (
                cmd && i.key_pressed(egui::Key::O),
                cmd && i.key_pressed(egui::Key::S),
                cmd && i.key_pressed(egui::Key::N),
                cmd && i.key_pressed(egui::Key::W),
                cmd && i.key_pressed(egui::Key::Enter),
            )
        });

        if open {
            self.open_file();
        }
        if save {
            self.save_active();
        }
        if new_tab {
            let next = self.docs.len() + 1;
            self.new_blank_doc(format!("Untitled {next}"));
        }
        if close_tab {
            self.close_active();
        }
        if toggle_mode {
            self.toggle_mode();
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Open…").clicked() {
                    self.open_file();
                }

                if ui.button("Save").clicked() {
                    self.save_active();
                }

                ui.separator();

                for (idx, doc) in self.docs.iter().enumerate() {
                    let selected = idx == self.active;
                    let mut label = doc.title.clone();
                    if doc.dirty {
                        label.push('*');
                    }
                    if ui.selectable_label(selected, label).clicked() {
                        self.active = idx;
                    }
                }

                if ui.button("+").clicked() {
                    let next = self.docs.len() + 1;
                    self.new_blank_doc(format!("Untitled {next}"));
                }

                if ui.button("Close").clicked() {
                    self.close_active();
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
                            doc.preview = None;
                            doc.dirty = true;
                        }
                    });
                }
                Mode::Preview => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let preview = doc.preview.get_or_insert_with(|| preview::parse(&doc.text));
                        preview::show(ui, preview);
                    });
                }
            }
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            let mut clear_error = false;
            if let Some(error) = self.error.as_deref() {
                ui.horizontal(|ui| {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    if ui.button("x").clicked() {
                        clear_error = true;
                    }
                });
            }
            if clear_error {
                self.error = None;
            }
        });
    }
}

impl RustdownApp {
    fn new_blank_doc(&mut self, title: String) {
        self.docs.push(Document {
            title,
            path: None,
            text: String::new(),
            dirty: false,
            preview: None,
        });
        self.active = self.docs.len() - 1;
    }

    fn close_active(&mut self) {
        if self.docs.is_empty() {
            return;
        }

        if self.docs[self.active].dirty {
            self.error = Some("Unsaved changes — save first".to_owned());
            return;
        }

        self.docs.remove(self.active);
        if self.docs.is_empty() {
            self.new_blank_doc("Untitled".to_owned());
        } else {
            self.active = self.active.min(self.docs.len().saturating_sub(1));
        }
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
                    preview: None,
                });
                self.active = self.docs.len() - 1;
                self.error = None;
            }
            Err(err) => self.error = Some(format!("Open failed: {err}")),
        }
    }

    fn save_active(&mut self) {
        let doc = &mut self.docs[self.active];

        let path = match &doc.path {
            Some(path) => path.clone(),
            None => {
                let Some(path) = rfd::FileDialog::new()
                    .add_filter("Markdown", &["md", "markdown"])
                    .save_file()
                else {
                    return;
                };

                doc.title = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Untitled")
                    .to_owned();
                doc.path = Some(path.clone());
                path
            }
        };

        match fs::write(&path, &doc.text) {
            Ok(()) => {
                doc.dirty = false;
                self.error = None;
            }
            Err(err) => self.error = Some(format!("Save failed: {err}")),
        }
    }
}
