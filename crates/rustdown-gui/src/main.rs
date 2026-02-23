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
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

mod disk_io;
mod format;
mod highlight;
mod live_merge;

use disk_io::{
    DiskRevision, atomic_write_utf8, disk_revision, next_merge_sidecar_path, read_stable_utf8,
};
use live_merge::{Merge3Outcome, merge_three_way};

const DEBOUNCE: Duration = Duration::from_millis(150);
const DISK_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DISK_RELOAD_DEBOUNCE: Duration = Duration::from_millis(75);
const ZOOM_STEP: f32 = 0.1;
const MIN_ZOOM_FACTOR: f32 = 0.5;
const MAX_ZOOM_FACTOR: f32 = 3.0;
const READING_SPEED_WPM: usize = 200;
const FONT_SIZE_DELTA: f32 = 2.0;
const SMALL_FONT_SIZE_DELTA: f32 = 1.0;
const PANEL_EDGE_PADDING: f32 = 8.0;
const UI_FONT_NAME: &str = "rustdown-ui-font";
#[cfg(target_os = "linux")]
const UI_FONT_CANDIDATE_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf",
];
#[cfg(target_os = "macos")]
const UI_FONT_CANDIDATE_PATHS: &[&str] = &[
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/Library/Fonts/Arial.ttf",
];
#[cfg(target_os = "windows")]
const UI_FONT_CANDIDATE_PATHS: &[&str] = &[
    r"C:\Windows\Fonts\segoeui.ttf",
    r"C:\Windows\Fonts\arial.ttf",
];
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const UI_FONT_CANDIDATE_PATHS: &[&str] = &[];

#[derive(Clone, Debug, PartialEq, Eq)]
struct LaunchOptions {
    mode: Mode,
    path: Option<PathBuf>,
}

#[must_use]
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
    eframe::run_native(
        "rustdown",
        options,
        Box::new(move |cc| {
            configure_single_font(&cc.egui_ctx).map_err(std::io::Error::other)?;
            configure_ui_style(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}

fn configure_single_font(ctx: &egui::Context) -> Result<(), String> {
    let font_data = load_single_font()?;
    let font_name = UI_FONT_NAME.to_owned();
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.clear();
    fonts.families.clear();
    fonts.font_data.insert(
        font_name.clone(),
        Arc::new(egui::FontData::from_owned(font_data)),
    );
    fonts
        .families
        .insert(egui::FontFamily::Proportional, vec![font_name.clone()]);
    fonts
        .families
        .insert(egui::FontFamily::Monospace, vec![font_name]);
    ctx.set_fonts(fonts);
    Ok(())
}

fn configure_ui_style(ctx: &egui::Context) {
    ctx.style_mut(|style| {
        for text_style in [
            egui::TextStyle::Body,
            egui::TextStyle::Button,
            egui::TextStyle::Monospace,
        ] {
            if let Some(font_id) = style.text_styles.get_mut(&text_style) {
                font_id.size += FONT_SIZE_DELTA;
            }
        }
        if let Some(font_id) = style.text_styles.get_mut(&egui::TextStyle::Small) {
            font_id.size += SMALL_FONT_SIZE_DELTA;
        }
    });
}

fn load_single_font() -> Result<Vec<u8>, String> {
    if let Ok(path) = std::env::var("RUSTDOWN_FONT_PATH") {
        if path.trim().is_empty() {
            return Err("RUSTDOWN_FONT_PATH is set but empty".to_owned());
        }
        return fs::read(&path).map_err(|err| {
            format!("Failed to read UI font from RUSTDOWN_FONT_PATH '{path}': {err}")
        });
    }

    for path in UI_FONT_CANDIDATE_PATHS {
        match fs::read(path) {
            Ok(data) => return Ok(data),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(format!("Failed to read UI font at '{path}': {err}")),
        }
    }

    if UI_FONT_CANDIDATE_PATHS.is_empty() {
        return Err("No UI font candidates are configured for this platform".to_owned());
    }

    Err(format!(
        "No UI font files found. Tried: {}",
        UI_FONT_CANDIDATE_PATHS.join(", ")
    ))
}

#[must_use]
fn markdown_file_dialog() -> rfd::FileDialog {
    rfd::FileDialog::new().add_filter("Markdown", &["md", "markdown"])
}

#[must_use]
fn html_file_dialog(suggested_name: &str) -> rfd::FileDialog {
    rfd::FileDialog::new()
        .add_filter("HTML", &["html"])
        .set_file_name(suggested_name)
}

#[must_use]
fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

#[must_use]
fn first_markdown_path<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Option<PathBuf> {
    paths
        .into_iter()
        .find(|path| is_markdown_path(path))
        .map(Path::to_path_buf)
}

#[must_use]
fn suggested_html_file_name(path: Option<&Path>) -> String {
    path.and_then(|path| path.file_stem())
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .map_or_else(|| "document.html".to_owned(), |stem| format!("{stem}.html"))
}

#[must_use]
fn markdown_to_html_document(source: &str) -> String {
    let mut options = pulldown_cmark::Options::empty();
    options.insert(pulldown_cmark::Options::ENABLE_TABLES);
    options.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    options.insert(pulldown_cmark::Options::ENABLE_TASKLISTS);
    let parser = pulldown_cmark::Parser::new_ext(source, options);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    html
}

#[derive(Default)]
struct RustdownApp {
    doc: Document,
    mode: Mode,
    search: SearchState,
    error: Option<String>,
    pending_action: Option<PendingAction>,
    last_viewport_title: String,
    focus_search: bool,

    disk_reload_nonce: u64,

    disk_watcher: Option<RecommendedWatcher>,
    disk_watch_root: Option<PathBuf>,
    disk_watch_target_name: Option<OsString>,
    disk_watch_rx: Option<mpsc::Receiver<notify::Result<Event>>>,

    disk_poll_at: Option<Instant>,
    pending_disk_reload_at: Option<Instant>,
    disk_reload_in_flight: bool,
    disk_read_tx: Option<mpsc::Sender<DiskReadMessage>>,
    disk_read_rx: Option<mpsc::Receiver<DiskReadMessage>>,
    disk_conflict: Option<DiskConflict>,
    merge_sidecar_path: Option<PathBuf>,
}

#[derive(Default)]
struct Document {
    path: Option<PathBuf>,
    text: String,
    base_text: String,
    disk_rev: Option<DiskRevision>,
    stats: DocumentStats,
    dirty: bool,
    md_cache: CommonMarkCache,
    last_edit_at: Option<Instant>,
    edit_seq: u64,
}

#[derive(Debug)]
enum DiskReloadOutcome {
    Replace {
        disk_text: String,
        disk_rev: DiskRevision,
    },
    MergeClean {
        merged_text: String,
        disk_text: String,
        disk_rev: DiskRevision,
    },
    MergeConflict {
        disk_text: String,
        disk_rev: DiskRevision,
        conflict_marked: String,
        ours_wins: String,
    },
}

#[derive(Debug)]
struct DiskReadMessage {
    path: PathBuf,
    nonce: u64,
    edit_seq: u64,
    outcome: io::Result<DiskReloadOutcome>,
}

#[derive(Clone, Debug)]
struct DiskConflict {
    disk_text: String,
    disk_rev: DiskRevision,
    conflict_marked: String,
    ours_wins: String,
}

impl Document {
    #[must_use]
    fn debounce_remaining(&self, debounce: Duration) -> Option<Duration> {
        let last = self.last_edit_at?;
        let since = last.elapsed();
        (since < debounce).then(|| debounce - since)
    }

    #[must_use]
    fn title(&self) -> Cow<'_, str> {
        self.path
            .as_ref()
            .and_then(|path| path.file_name())
            .map_or_else(|| Cow::Borrowed("Untitled"), |name| name.to_string_lossy())
    }

    #[must_use]
    fn path_label(&self) -> Cow<'_, str> {
        self.path
            .as_ref()
            .map_or_else(|| Cow::Borrowed("Unsaved"), |path| path.to_string_lossy())
    }

    #[must_use]
    fn stats(&self) -> DocumentStats {
        self.stats
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DocumentStats {
    words: usize,
    chars: usize,
    lines: usize,
}

impl DocumentStats {
    #[must_use]
    fn from_text(text: &str) -> Self {
        let mut words = 0;
        let mut chars = 0;
        let mut lines = 1;
        let mut in_word = false;
        for ch in text.chars() {
            chars += 1;
            if ch == '\n' {
                lines += 1;
            }
            if ch.is_whitespace() {
                in_word = false;
            } else if !in_word {
                words += 1;
                in_word = true;
            }
        }
        Self {
            words,
            chars,
            lines,
        }
    }

    #[must_use]
    fn reading_minutes(self) -> usize {
        if self.words == 0 {
            return 0;
        }
        self.words.div_ceil(READING_SPEED_WPM)
    }
}

impl Default for DocumentStats {
    fn default() -> Self {
        Self {
            words: 0,
            chars: 0,
            lines: 1,
        }
    }
}

#[derive(Default)]
struct SearchState {
    visible: bool,
    replace_mode: bool,
    query: String,
    replacement: String,
    last_replace_count: Option<usize>,
}

#[must_use]
fn find_match_count(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.match_indices(needle).count()
}

#[must_use]
fn replace_all_occurrences<'a>(
    haystack: &'a str,
    needle: &str,
    replacement: &str,
) -> (Cow<'a, str>, usize) {
    if needle.is_empty() || needle == replacement {
        return (Cow::Borrowed(haystack), 0);
    }

    let matches = find_match_count(haystack, needle);
    if matches == 0 {
        return (Cow::Borrowed(haystack), 0);
    }

    (Cow::Owned(haystack.replace(needle, replacement)), matches)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Edit,
    Preview,
    SideBySide,
}

impl Mode {
    #[must_use]
    fn cycle(self) -> Self {
        match self {
            Mode::Edit => Mode::Preview,
            Mode::Preview => Mode::SideBySide,
            Mode::SideBySide => Mode::Edit,
        }
    }

    #[must_use]
    fn label(self) -> &'static str {
        match self {
            Mode::Edit => "Edit",
            Mode::Preview => "Preview",
            Mode::SideBySide => "Side-by-side",
        }
    }
}

#[derive(Clone, Debug)]
enum PendingAction {
    NewBlank,
    Open(PathBuf),
}

impl eframe::App for RustdownApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick_disk_sync(ctx);

        let dialog_open = self.pending_action.is_some() || self.disk_conflict.is_some();
        let (
            dropped_path,
            open,
            save,
            save_as,
            new_doc,
            cycle_mode,
            search,
            replace_all_mode,
            format_doc,
            export_html,
            zoom_in,
            zoom_out,
            escape,
        ) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            (
                first_markdown_path(
                    i.raw
                        .dropped_files
                        .iter()
                        .filter_map(|file| file.path.as_deref()),
                ),
                cmd && i.key_pressed(egui::Key::O),
                cmd && i.key_pressed(egui::Key::S) && !i.modifiers.shift,
                cmd && i.key_pressed(egui::Key::S) && i.modifiers.shift,
                cmd && i.key_pressed(egui::Key::N),
                cmd && i.key_pressed(egui::Key::Enter),
                cmd && !i.modifiers.shift && !i.modifiers.alt && i.key_pressed(egui::Key::F),
                cmd && i.modifiers.shift && !i.modifiers.alt && i.key_pressed(egui::Key::F),
                cmd && i.modifiers.alt && !i.modifiers.shift && i.key_pressed(egui::Key::F),
                cmd && i.key_pressed(egui::Key::E),
                cmd && i.key_pressed(egui::Key::Equals),
                cmd && i.key_pressed(egui::Key::Minus),
                i.key_pressed(egui::Key::Escape),
            )
        });

        if !dialog_open {
            if let Some(path) = dropped_path {
                self.request_action(PendingAction::Open(path));
            }
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
            if search {
                self.open_search(false);
            }
            if replace_all_mode {
                self.open_search(true);
            }
            if format_doc {
                self.format_document();
            }
            if export_html {
                let _ = self.export_html();
            }
            if zoom_in {
                self.adjust_zoom(ctx, ZOOM_STEP);
            }
            if zoom_out {
                self.adjust_zoom(ctx, -ZOOM_STEP);
            }
            if escape && self.search.visible {
                self.close_search();
            }
        }

        let mut run_replace_all = false;
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
                if ui.button("Export HTML").clicked() {
                    let _ = self.export_html();
                }
                if ui.button("Format").clicked() {
                    self.format_document();
                }

                ui.separator();

                ui.label(self.doc.path_label());
                let stats = self.doc.stats();

                ui.separator();
                ui.label(format!(
                    "{} words · {} chars · {} lines · {} min read",
                    stats.words,
                    stats.chars,
                    stats.lines,
                    stats.reading_minutes()
                ));

                if self.doc.dirty {
                    ui.separator();
                    ui.colored_label(ui.visuals().warn_fg_color, "Modified");
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut clear_merge_sidecar = false;
                    if let Some(path) = self.merge_sidecar_path.clone() {
                        if ui.button("x").clicked() {
                            clear_merge_sidecar = true;
                        }
                        if ui.button("Open merge file").clicked() {
                            self.request_action(PendingAction::Open(path.clone()));
                        }
                        ui.label(path.to_string_lossy());
                        ui.separator();
                    }

                    if let Some(error) = self.error.as_deref() {
                        if ui.button("x").clicked() {
                            clear_error = true;
                        }
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }

                    if clear_merge_sidecar {
                        self.merge_sidecar_path = None;
                    }
                });
            });

            if clear_error {
                self.error = None;
            }

            if self.search.visible {
                ui.separator();
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

                    let matches =
                        find_match_count(self.doc.text.as_str(), self.search.query.as_str());
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
            }
        });
        if run_replace_all {
            let replaced = self.replace_all_matches();
            self.search.last_replace_count = Some(replaced);
        }

        let panel_frame = egui::Frame::none()
            .fill(ctx.style().visuals.panel_fill)
            .inner_margin(egui::Margin::same(PANEL_EDGE_PADDING));
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
        self.show_disk_conflict_dialog(ctx);
        self.update_viewport_title(ctx);
    }
}

impl RustdownApp {
    fn reset_disk_sync_state(&mut self) {
        self.disk_reload_nonce = self.disk_reload_nonce.wrapping_add(1);
        self.disk_poll_at = None;
        self.pending_disk_reload_at = None;
        self.disk_reload_in_flight = false;
        self.disk_conflict = None;

        self.disk_watcher = None;
        self.disk_watch_root = None;
        self.disk_watch_target_name = None;
        self.disk_watch_rx = None;
    }

    fn ensure_disk_read_channel(&mut self) {
        if self.disk_read_tx.is_some() {
            return;
        }

        let (tx, rx) = mpsc::channel();
        self.disk_read_tx = Some(tx);
        self.disk_read_rx = Some(rx);
    }

    fn ensure_disk_watcher(&mut self, ctx: &egui::Context, path: &Path) {
        let watch_root = path.parent().unwrap_or_else(|| Path::new("."));
        let target_name = path.file_name().map(ToOwned::to_owned);

        if self.disk_watcher.is_some() && self.disk_watch_root.as_deref() == Some(watch_root) {
            self.disk_watch_target_name = target_name;
            return;
        }

        self.disk_watcher = None;
        self.disk_watch_rx = None;
        self.disk_watch_root = None;
        self.disk_watch_target_name = None;

        let Some(target_name) = target_name else {
            return;
        };

        let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
        let ctx = ctx.clone();
        let handler = move |res| {
            let _ = tx.send(res);
            ctx.request_repaint();
        };

        let mut watcher = match notify::recommended_watcher(handler) {
            Ok(watcher) => watcher,
            Err(err) => {
                self.error
                    .get_or_insert(format!("Watch setup failed: {err}"));
                return;
            }
        };

        if let Err(err) = watcher.watch(watch_root, RecursiveMode::NonRecursive) {
            self.error
                .get_or_insert(format!("Watch start failed: {err}"));
            return;
        }

        self.disk_watcher = Some(watcher);
        self.disk_watch_root = Some(watch_root.to_path_buf());
        self.disk_watch_target_name = Some(target_name);
        self.disk_watch_rx = Some(rx);
        self.disk_poll_at = None;
    }

    fn drain_disk_watch_events(&mut self) -> bool {
        let Some(rx) = self.disk_watch_rx.take() else {
            return false;
        };

        let target_name = self.disk_watch_target_name.clone();
        let mut saw_change = false;

        loop {
            match rx.try_recv() {
                Ok(Ok(event)) => {
                    if let Some(target) = target_name.as_deref()
                        && event
                            .paths
                            .iter()
                            .any(|path| path.file_name().is_some_and(|name| name == target))
                    {
                        saw_change = true;
                    }
                }
                Ok(Err(err)) => {
                    self.error.get_or_insert(format!("Watch error: {err}"));
                    self.disk_watcher = None;
                    self.disk_watch_rx = None;
                    self.disk_watch_root = None;
                    self.disk_watch_target_name = None;
                    return false;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.disk_watcher = None;
                    self.disk_watch_rx = None;
                    self.disk_watch_root = None;
                    self.disk_watch_target_name = None;
                    return false;
                }
            }
        }

        self.disk_watch_rx = Some(rx);
        saw_change
    }

    fn tick_disk_sync(&mut self, ctx: &egui::Context) {
        self.drain_disk_read_results();

        if self.disk_conflict.is_some() {
            return;
        }

        let Some(path) = self.doc.path.clone() else {
            self.reset_disk_sync_state();
            return;
        };

        self.ensure_disk_watcher(ctx, path.as_path());

        let now = Instant::now();
        if self.drain_disk_watch_events() {
            let due_at = now + DISK_RELOAD_DEBOUNCE;
            match self.pending_disk_reload_at {
                Some(existing) if existing <= due_at => {}
                _ => self.pending_disk_reload_at = Some(due_at),
            }
        }

        if self.disk_watcher.is_none() && !self.disk_reload_in_flight {
            match self.disk_poll_at {
                Some(next) if now < next => {}
                _ => {
                    self.disk_poll_at = Some(now + DISK_POLL_INTERVAL);

                    match disk_revision(path.as_path()) {
                        Ok(rev) if Some(rev) != self.doc.disk_rev => {
                            let due_at = now + DISK_RELOAD_DEBOUNCE;
                            match self.pending_disk_reload_at {
                                Some(existing) if existing <= due_at => {}
                                _ => self.pending_disk_reload_at = Some(due_at),
                            }
                        }
                        Ok(_) => {}
                        Err(err) => {
                            self.error
                                .get_or_insert(format!("Disk check failed: {err}"));
                        }
                    }
                }
            }
        } else {
            self.disk_poll_at = None;
        }

        if self.disk_reload_in_flight {
            return;
        }

        if let Some(due_at) = self.pending_disk_reload_at
            && now >= due_at
        {
            self.pending_disk_reload_at = None;
            self.start_disk_reload(ctx, path.clone());
        }

        let mut next_wake = self.pending_disk_reload_at;
        if self.disk_watcher.is_none() {
            next_wake = match (next_wake, self.disk_poll_at) {
                (Some(existing), Some(poll)) => Some(existing.min(poll)),
                (Some(existing), None) => Some(existing),
                (None, Some(poll)) => Some(poll),
                (None, None) => None,
            };
        }

        if let Some(next) = next_wake
            && now < next
        {
            ctx.request_repaint_after(next - now);
        }
    }

    fn start_disk_reload(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.ensure_disk_read_channel();
        let Some(tx) = self.disk_read_tx.clone() else {
            return;
        };

        let edit_seq = self.doc.edit_seq;
        let dirty = self.doc.dirty;
        let base_text = dirty.then(|| self.doc.base_text.clone());
        let ours_text = dirty.then(|| self.doc.text.clone());

        self.disk_reload_nonce = self.disk_reload_nonce.wrapping_add(1);
        let nonce = self.disk_reload_nonce;

        self.disk_reload_in_flight = true;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let outcome = match read_stable_utf8(&path) {
                Ok((disk_text, disk_rev)) => {
                    if dirty {
                        match (base_text, ours_text) {
                            (Some(base_text), Some(ours_text)) => match merge_three_way(
                                base_text.as_str(),
                                ours_text.as_str(),
                                disk_text.as_str(),
                            ) {
                                Merge3Outcome::Clean(merged_text) => {
                                    Ok(DiskReloadOutcome::MergeClean {
                                        merged_text,
                                        disk_text,
                                        disk_rev,
                                    })
                                }
                                Merge3Outcome::Conflicted {
                                    conflict_marked,
                                    ours_wins,
                                } => Ok(DiskReloadOutcome::MergeConflict {
                                    disk_text,
                                    disk_rev,
                                    conflict_marked,
                                    ours_wins,
                                }),
                            },
                            _ => Err(io::Error::other("missing merge inputs")),
                        }
                    } else {
                        Ok(DiskReloadOutcome::Replace {
                            disk_text,
                            disk_rev,
                        })
                    }
                }
                Err(err) => Err(err),
            };

            let _ = tx.send(DiskReadMessage {
                path,
                nonce,
                edit_seq,
                outcome,
            });
            ctx.request_repaint();
        });
    }

    fn drain_disk_read_results(&mut self) {
        let Some(rx) = self.disk_read_rx.take() else {
            return;
        };

        loop {
            match rx.try_recv() {
                Ok(msg) => {
                    if msg.nonce != self.disk_reload_nonce {
                        continue;
                    }
                    if self.doc.path.as_deref() != Some(msg.path.as_path()) {
                        continue;
                    }
                    self.disk_reload_in_flight = false;

                    if self.doc.edit_seq != msg.edit_seq {
                        let now = Instant::now();
                        let due_at = now + DISK_RELOAD_DEBOUNCE;
                        match self.pending_disk_reload_at {
                            Some(existing) if existing <= due_at => {}
                            _ => self.pending_disk_reload_at = Some(due_at),
                        }
                        continue;
                    }

                    match msg.outcome {
                        Ok(DiskReloadOutcome::Replace {
                            disk_text,
                            disk_rev,
                        }) => {
                            self.doc.base_text = disk_text.clone();
                            self.doc.text = disk_text;
                            self.doc.disk_rev = Some(disk_rev);
                            self.doc.stats = DocumentStats::from_text(self.doc.text.as_str());
                            self.doc.md_cache.clear_scrollable();
                            self.doc.last_edit_at = None;
                            self.doc.dirty = false;
                            self.error = None;
                        }
                        Ok(DiskReloadOutcome::MergeClean {
                            merged_text,
                            disk_text,
                            disk_rev,
                        }) => {
                            self.doc.text = merged_text;
                            self.doc.base_text = disk_text;
                            self.doc.disk_rev = Some(disk_rev);
                            self.doc.stats = DocumentStats::from_text(self.doc.text.as_str());
                            self.doc.md_cache.clear_scrollable();
                            self.doc.dirty = true;
                            self.error = None;
                        }
                        Ok(DiskReloadOutcome::MergeConflict {
                            disk_text,
                            disk_rev,
                            conflict_marked,
                            ours_wins,
                        }) => {
                            self.disk_conflict = Some(DiskConflict {
                                disk_text,
                                disk_rev,
                                conflict_marked,
                                ours_wins,
                            });
                        }
                        Err(err) => {
                            self.error = Some(format!("Reload failed: {err}"));
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.disk_reload_in_flight = false;
                    self.disk_read_rx = None;
                    self.disk_read_tx = None;
                    return;
                }
            }
        }

        self.disk_read_rx = Some(rx);
    }

    fn incorporate_disk_text(&mut self, disk_text: String, disk_rev: DiskRevision) {
        if self.doc.disk_rev == Some(disk_rev) && disk_text == self.doc.base_text {
            return;
        }

        if !self.doc.dirty {
            self.doc.base_text = disk_text.clone();
            self.doc.text = disk_text;
            self.doc.disk_rev = Some(disk_rev);
            self.doc.stats = DocumentStats::from_text(self.doc.text.as_str());
            self.doc.md_cache.clear_scrollable();
            self.doc.last_edit_at = None;
            self.error = None;
            return;
        }

        match merge_three_way(
            self.doc.base_text.as_str(),
            self.doc.text.as_str(),
            disk_text.as_str(),
        ) {
            Merge3Outcome::Clean(merged) => {
                self.doc.text = merged;
                self.doc.base_text = disk_text;
                self.doc.disk_rev = Some(disk_rev);
                self.doc.stats = DocumentStats::from_text(self.doc.text.as_str());
                self.doc.md_cache.clear_scrollable();
                self.error = None;
            }
            Merge3Outcome::Conflicted {
                conflict_marked,
                ours_wins,
            } => {
                self.disk_conflict = Some(DiskConflict {
                    disk_text,
                    disk_rev,
                    conflict_marked,
                    ours_wins,
                });
            }
        }
    }

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

    fn update_viewport_title(&mut self, ctx: &egui::Context) {
        let mode = match self.mode {
            Mode::Preview => " (Preview)",
            Mode::SideBySide => " (Side-by-side)",
            Mode::Edit => "",
        };
        let title = format!(
            "rustdown — {}{}{}",
            self.doc.title(),
            if self.doc.dirty { "*" } else { "" },
            mode
        );
        if self.last_viewport_title == title {
            return;
        }
        self.last_viewport_title = title.clone();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    fn note_text_changed(&mut self) {
        self.doc.dirty = true;
        self.doc.edit_seq = self.doc.edit_seq.wrapping_add(1);
        self.doc.last_edit_at = Some(Instant::now());
        self.doc.stats = DocumentStats::from_text(self.doc.text.as_str());
        self.doc.md_cache.clear_scrollable();
    }

    fn open_search(&mut self, replace_mode: bool) {
        self.search.visible = true;
        self.search.replace_mode = replace_mode;
        self.search.last_replace_count = None;
        self.focus_search = true;
    }

    fn close_search(&mut self) {
        self.search.visible = false;
        self.search.replace_mode = false;
        self.search.last_replace_count = None;
        self.focus_search = false;
    }

    fn replace_all_matches(&mut self) -> usize {
        let (text, replaced) = replace_all_occurrences(
            self.doc.text.as_str(),
            self.search.query.as_str(),
            self.search.replacement.as_str(),
        );
        if let Cow::Owned(text) = text {
            self.doc.text = text;
            self.note_text_changed();
        }
        replaced
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
                self.reset_disk_sync_state();
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
        let Some(path) = markdown_file_dialog().pick_file() else {
            return;
        };
        self.request_action(PendingAction::Open(path));
    }

    fn open_path(&mut self, path: PathBuf) {
        match read_stable_utf8(&path) {
            Ok((text, disk_rev)) => {
                let base_text = text.clone();
                let stats = DocumentStats::from_text(text.as_str());
                self.doc = Document {
                    path: Some(path),
                    text,
                    base_text,
                    disk_rev: Some(disk_rev),
                    stats,
                    dirty: false,
                    md_cache: CommonMarkCache::default(),
                    last_edit_at: None,
                    edit_seq: 0,
                };
                self.error = None;
                self.reset_disk_sync_state();
            }
            Err(err) => {
                self.error.get_or_insert(format!("Open failed: {err}"));
            }
        }
    }

    fn export_html(&mut self) -> bool {
        let suggested_name = suggested_html_file_name(self.doc.path.as_deref());
        let Some(path) = html_file_dialog(suggested_name.as_str()).save_file() else {
            return false;
        };
        let html = markdown_to_html_document(self.doc.text.as_str());
        match fs::write(path, html) {
            Ok(()) => {
                self.error = None;
                true
            }
            Err(err) => {
                self.error = Some(format!("Export failed: {err}"));
                false
            }
        }
    }

    fn save_doc(&mut self, save_as: bool) -> bool {
        let (path, update_doc_path) = if save_as {
            let Some(path) = markdown_file_dialog().save_file() else {
                return false;
            };
            (path, true)
        } else if let Some(path) = self.doc.path.clone() {
            (path, false)
        } else {
            let Some(path) = markdown_file_dialog().save_file() else {
                return false;
            };
            (path, true)
        };

        let saving_to_current_path = self.doc.path.as_deref() == Some(path.as_path());

        if self.disk_conflict.is_none() && saving_to_current_path {
            match read_stable_utf8(&path) {
                Ok((disk_text, disk_rev)) => self.incorporate_disk_text(disk_text, disk_rev),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => {
                    self.error
                        .get_or_insert(format!("Pre-save reload failed: {err}"));
                }
            }
        }

        if self.disk_conflict.is_some() {
            return false;
        }

        match atomic_write_utf8(&path, &self.doc.text) {
            Ok(()) => {
                if update_doc_path {
                    self.doc.path = Some(path.clone());
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

    fn show_disk_conflict_dialog(&mut self, ctx: &egui::Context) {
        if self.disk_conflict.is_none() {
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
                    if ui.button("Open conflict merge").clicked() {
                        self.apply_conflict_choice(ConflictChoice::OpenConflictMerge);
                    }
                    if ui.button("Keep mine (+ merge file)").clicked() {
                        self.apply_conflict_choice(ConflictChoice::KeepMineWriteSidecar);
                    }
                });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save As…").clicked() {
                        self.apply_conflict_choice(ConflictChoice::SaveAs);
                    }
                    if ui.button("Reload disk").clicked() {
                        self.apply_conflict_choice(ConflictChoice::ReloadDisk);
                    }
                    if ui.button("Overwrite disk").clicked() {
                        self.apply_conflict_choice(ConflictChoice::OverwriteDisk);
                    }
                });

                ui.add_space(8.0);
                ui.small(
                    "Tip: “Keep mine” applies non-conflicting disk edits and writes a merge file so no changes are lost.",
                );
            });
    }

    fn apply_conflict_choice(&mut self, choice: ConflictChoice) {
        let Some(conflict) = self.disk_conflict.take() else {
            return;
        };

        match choice {
            ConflictChoice::OpenConflictMerge => {
                self.doc.text = conflict.conflict_marked;
                self.doc.base_text = conflict.disk_text;
                self.doc.disk_rev = Some(conflict.disk_rev);
                self.doc.stats = DocumentStats::from_text(self.doc.text.as_str());
                self.doc.md_cache.clear_scrollable();
                self.doc.dirty = true;
                self.error = None;
            }
            ConflictChoice::KeepMineWriteSidecar => {
                self.doc.text = conflict.ours_wins;
                self.doc.base_text = conflict.disk_text;
                self.doc.disk_rev = Some(conflict.disk_rev);
                self.doc.stats = DocumentStats::from_text(self.doc.text.as_str());
                self.doc.md_cache.clear_scrollable();
                self.doc.dirty = true;
                self.error = None;

                if let Some(doc_path) = self.doc.path.as_deref() {
                    match next_merge_sidecar_path(doc_path) {
                        Ok(sidecar_path) => {
                            match atomic_write_utf8(&sidecar_path, &conflict.conflict_marked) {
                                Ok(()) => self.merge_sidecar_path = Some(sidecar_path),
                                Err(err) => {
                                    self.error
                                        .get_or_insert(format!("Merge file write failed: {err}"));
                                }
                            }
                        }
                        Err(err) => {
                            self.error
                                .get_or_insert(format!("Merge file path failed: {err}"));
                        }
                    }
                }
            }
            ConflictChoice::SaveAs => {
                // Save-as switches the active path, so the conflict prompt is no longer relevant.
                if !self.save_doc(true) {
                    self.disk_conflict = Some(conflict);
                    return;
                }
            }
            ConflictChoice::ReloadDisk => {
                self.doc.text = conflict.disk_text;
                self.doc.base_text = self.doc.text.clone();
                self.doc.disk_rev = Some(conflict.disk_rev);
                self.doc.stats = DocumentStats::from_text(self.doc.text.as_str());
                self.doc.md_cache.clear_scrollable();
                self.doc.dirty = false;
                self.error = None;
            }
            ConflictChoice::OverwriteDisk => {
                let Some(path) = self.doc.path.as_deref() else {
                    self.disk_conflict = Some(conflict);
                    return;
                };

                match atomic_write_utf8(path, &self.doc.text) {
                    Ok(()) => {}
                    Err(err) => {
                        self.disk_conflict = Some(conflict);
                        self.error.get_or_insert(format!("Overwrite failed: {err}"));
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

#[derive(Clone, Copy, Debug)]
enum ConflictChoice {
    OpenConflictMerge,
    KeepMineWriteSidecar,
    SaveAs,
    ReloadDisk,
    OverwriteDisk,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn parse(args: &[&str]) -> LaunchOptions {
        parse_launch_options(args.iter().copied().map(OsString::from))
    }

    fn test_rev(seconds: u64, len: u64) -> DiskRevision {
        DiskRevision {
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
            len,
            #[cfg(unix)]
            dev: 0,
            #[cfg(unix)]
            inode: 0,
        }
    }

    fn make_temp_dir(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        dir.push(format!("{name}-{nanos}-{}", std::process::id()));
        let created = fs::create_dir_all(&dir);
        assert!(created.is_ok(), "Failed to create temp dir: {created:?}");
        dir
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

    #[test]
    fn document_stats_counts_words_chars_lines_and_read_time() {
        let stats = DocumentStats::from_text("one two\nthree");
        assert_eq!(
            stats,
            DocumentStats {
                words: 3,
                chars: 13,
                lines: 2
            }
        );
        assert_eq!(stats.reading_minutes(), 1);
    }

    #[test]
    fn document_stats_empty_document_is_zero_words_and_one_line() {
        let stats = DocumentStats::from_text("");
        assert_eq!(stats.words, 0);
        assert_eq!(stats.chars, 0);
        assert_eq!(stats.lines, 1);
        assert_eq!(stats.reading_minutes(), 0);
    }

    #[test]
    fn document_default_stats_match_empty_text() {
        let doc = Document::default();
        assert_eq!(doc.stats(), DocumentStats::from_text(""));
    }

    #[test]
    fn is_markdown_path_matches_supported_extensions_case_insensitive() {
        assert!(is_markdown_path(Path::new("note.md")));
        assert!(is_markdown_path(Path::new("README.Markdown")));
        assert!(!is_markdown_path(Path::new("notes.txt")));
        assert!(!is_markdown_path(Path::new("README")));
    }

    #[test]
    fn first_markdown_path_returns_first_supported_file() {
        let files = [
            Path::new("notes.txt"),
            Path::new("chapter.markdown"),
            Path::new("later.md"),
        ];
        assert_eq!(
            first_markdown_path(files),
            Some(PathBuf::from("chapter.markdown"))
        );
    }

    #[test]
    fn suggested_html_file_name_uses_document_stem_or_default() {
        assert_eq!(
            suggested_html_file_name(Some(Path::new("/tmp/readme.md"))),
            "readme.html"
        );
        assert_eq!(suggested_html_file_name(None), "document.html");
    }

    #[test]
    fn markdown_to_html_document_renders_common_markdown() {
        let html = markdown_to_html_document("# Heading\n\n- item");
        assert!(html.contains("<h1>Heading</h1>"));
        assert!(html.contains("<li>item</li>"));
    }

    #[test]
    fn find_match_count_returns_zero_for_empty_query() {
        assert_eq!(find_match_count("abc abc", ""), 0);
    }

    #[test]
    fn replace_all_occurrences_replaces_and_counts_matches() {
        let (text, replaced) = replace_all_occurrences("alpha beta alpha", "alpha", "zeta");
        assert_eq!(text.as_ref(), "zeta beta zeta");
        assert_eq!(replaced, 2);
    }

    #[test]
    fn replace_all_occurrences_ignores_identical_replacement() {
        let (text, replaced) = replace_all_occurrences("alpha beta", "alpha", "alpha");
        assert_eq!(text.as_ref(), "alpha beta");
        assert_eq!(replaced, 0);
    }

    #[test]
    fn replace_all_matches_updates_document_and_stats() {
        let mut app = RustdownApp::default();
        app.doc.text = "alpha beta alpha".to_owned();
        app.doc.stats = DocumentStats::from_text(app.doc.text.as_str());
        app.search.query = "alpha".to_owned();
        app.search.replacement = "zeta".to_owned();

        let replaced = app.replace_all_matches();
        assert_eq!(replaced, 2);
        assert_eq!(app.doc.text, "zeta beta zeta");
        assert!(app.doc.dirty);
        assert_eq!(app.doc.stats, DocumentStats::from_text("zeta beta zeta"));
    }

    #[test]
    fn incorporate_disk_text_when_clean_replaces_buffer_and_advances_base() {
        let mut app = RustdownApp::default();
        app.doc.path = Some(PathBuf::from("note.md"));
        app.doc.text = "old".to_owned();
        app.doc.base_text = "old".to_owned();
        app.doc.disk_rev = Some(test_rev(1, 3));
        app.doc.dirty = false;

        app.incorporate_disk_text("new".to_owned(), test_rev(2, 3));

        assert_eq!(app.doc.text, "new");
        assert_eq!(app.doc.base_text, "new");
        assert_eq!(app.doc.disk_rev, Some(test_rev(2, 3)));
        assert!(!app.doc.dirty);
        assert!(app.disk_conflict.is_none());
    }

    #[test]
    fn incorporate_disk_text_when_dirty_and_merge_is_clean_applies_both_changes() {
        let mut app = RustdownApp::default();
        app.doc.path = Some(PathBuf::from("note.md"));
        app.doc.base_text = "a\nb\n".to_owned();
        app.doc.text = "a\nB\n".to_owned();
        app.doc.disk_rev = Some(test_rev(1, 4));
        app.doc.dirty = true;

        app.incorporate_disk_text("A\nb\n".to_owned(), test_rev(2, 4));

        assert_eq!(app.doc.text, "A\nB\n");
        assert_eq!(app.doc.base_text, "A\nb\n");
        assert_eq!(app.doc.disk_rev, Some(test_rev(2, 4)));
        assert!(app.doc.dirty);
        assert!(app.disk_conflict.is_none());
    }

    #[test]
    fn incorporate_disk_text_when_dirty_and_conflicts_sets_prompt_without_mutating_buffer() {
        let mut app = RustdownApp::default();
        app.doc.path = Some(PathBuf::from("note.md"));
        app.doc.base_text = "a\nb\n".to_owned();
        app.doc.text = "a\nO\n".to_owned();
        app.doc.disk_rev = Some(test_rev(1, 4));
        app.doc.dirty = true;

        app.incorporate_disk_text("a\nT\n".to_owned(), test_rev(2, 4));

        assert_eq!(app.doc.text, "a\nO\n");
        assert_eq!(app.doc.base_text, "a\nb\n");
        assert_eq!(app.doc.disk_rev, Some(test_rev(1, 4)));
        assert!(app.disk_conflict.is_some());
    }

    #[test]
    fn conflict_choice_open_merge_replaces_buffer_with_conflict_markers() {
        let mut app = RustdownApp::default();
        app.doc.path = Some(PathBuf::from("note.md"));
        app.doc.base_text = "a\nb\n".to_owned();
        app.doc.text = "a\nO\n".to_owned();
        app.doc.disk_rev = Some(test_rev(1, 4));
        app.doc.dirty = true;

        app.incorporate_disk_text("a\nT\n".to_owned(), test_rev(2, 4));
        assert!(
            app.disk_conflict.is_some(),
            "Expected conflict prompt to be set"
        );
        let expected_merge = match app.disk_conflict.as_ref() {
            Some(conflict) => conflict.conflict_marked.clone(),
            None => return,
        };

        app.apply_conflict_choice(ConflictChoice::OpenConflictMerge);

        assert_eq!(app.doc.text, expected_merge);
        assert_eq!(app.doc.base_text, "a\nT\n");
        assert_eq!(app.doc.disk_rev, Some(test_rev(2, 4)));
        assert!(app.doc.dirty);
        assert!(app.disk_conflict.is_none());
    }

    #[test]
    fn conflict_choice_keep_mine_writes_sidecar_and_applies_safe_disk_edits() {
        let dir = make_temp_dir("rustdown-merge-test");
        let original = dir.join("note.md");

        let _ = atomic_write_utf8(&original, "line1\nline2\nline3\n");

        let mut app = RustdownApp::default();
        app.doc.path = Some(original.clone());
        app.doc.base_text = "line1\nline2\nline3\n".to_owned();
        app.doc.text = "line1\nO2\nline3\n".to_owned();
        app.doc.disk_rev = Some(test_rev(1, 18));
        app.doc.dirty = true;

        app.incorporate_disk_text("line1\nT2\nT3\n".to_owned(), test_rev(2, 15));
        assert!(
            app.disk_conflict.is_some(),
            "Expected conflict prompt to be set"
        );
        let (expected_sidecar, expected_ours_wins) = match app.disk_conflict.as_ref() {
            Some(conflict) => (conflict.conflict_marked.clone(), conflict.ours_wins.clone()),
            None => {
                let _ = fs::remove_dir_all(&dir);
                return;
            }
        };

        app.apply_conflict_choice(ConflictChoice::KeepMineWriteSidecar);

        assert_eq!(app.doc.text, expected_ours_wins);
        assert_eq!(app.doc.base_text, "line1\nT2\nT3\n");
        assert_eq!(app.doc.disk_rev, Some(test_rev(2, 15)));
        assert!(app.disk_conflict.is_none());

        assert!(
            app.merge_sidecar_path.is_some(),
            "Expected merge sidecar path to be set"
        );
        let sidecar_path = match app.merge_sidecar_path.clone() {
            Some(path) => path,
            None => {
                let _ = fs::remove_dir_all(&dir);
                return;
            }
        };

        let sidecar_text_res = fs::read_to_string(&sidecar_path);
        assert!(
            sidecar_text_res.is_ok(),
            "Failed to read sidecar file: {sidecar_text_res:?}"
        );
        let sidecar_text = match sidecar_text_res {
            Ok(text) => text,
            Err(_) => {
                let _ = fs::remove_dir_all(&dir);
                return;
            }
        };
        assert_eq!(sidecar_text, expected_sidecar);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_utf8_replaces_existing_file_contents() {
        let dir = make_temp_dir("rustdown-atomic-save-test");
        let path = dir.join("save.md");

        assert!(atomic_write_utf8(&path, "first").is_ok());
        assert!(atomic_write_utf8(&path, "second").is_ok());

        let text_res = fs::read_to_string(&path);
        assert!(text_res.is_ok(), "Failed to read saved file: {text_res:?}");
        let text = match text_res {
            Ok(text) => text,
            Err(_) => {
                let _ = fs::remove_dir_all(&dir);
                return;
            }
        };
        assert_eq!(text, "second");

        let _ = fs::remove_dir_all(&dir);
    }
}
