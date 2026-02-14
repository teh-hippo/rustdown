#![forbid(unsafe_code)]

use eframe::egui;

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
}

#[derive(Default)]
struct Document {
    title: String,
    text: String,
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
            self.docs.push(Document {
                title: "Untitled".to_owned(),
                text: String::new(),
            });
            self.active = 0;
        } else {
            self.active = self.active.min(self.docs.len().saturating_sub(1));
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (idx, doc) in self.docs.iter().enumerate() {
                    let selected = idx == self.active;
                    if ui.selectable_label(selected, &doc.title).clicked() {
                        self.active = idx;
                    }
                }

                if ui.button("+").clicked() {
                    self.docs.push(Document {
                        title: format!("Untitled {}", self.docs.len() + 1),
                        text: String::new(),
                    });
                    self.active = self.docs.len() - 1;
                }

                ui.separator();

                let label = match self.mode {
                    Mode::Edit => "Preview",
                    Mode::Preview => "Edit",
                };
                if ui.button(label).clicked() {
                    self.mode = match self.mode {
                        Mode::Edit => Mode::Preview,
                        Mode::Preview => Mode::Edit,
                    };
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let doc = &mut self.docs[self.active];
            match self.mode {
                Mode::Edit => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut doc.text)
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace),
                        );
                    });
                }
                Mode::Preview => {
                    let rendered = rustdown_core::markdown::plain_text(&doc.text);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.add(egui::Label::new(rendered).selectable(true));
                    });
                }
            }
        });
    }
}
