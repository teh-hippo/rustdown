#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

#[cfg(target_arch = "wasm32")]
compile_error!("rustdown is a native desktop app; web/wasm builds are not supported.");

use std::{
    borrow::Cow,
    cell::Cell,
    ffi::OsString,
    io,
    path::{Component, Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use eframe::egui;
use notify::{Event, RecursiveMode, Watcher};
use rustdown_md::{MarkdownStyle, MarkdownViewer};

mod diagnostics;
mod disk_io;
mod disk_sync;
mod document;
mod editor;
mod format;
mod highlight;
mod live_merge;
mod markdown_fence;
mod nav_outline;
mod nav_panel;
mod search;
mod ui_style;

#[cfg(debug_assertions)]
mod nav_debug;

use disk_io::{
    DiskRevision, atomic_write_utf8, disk_revision, next_merge_sidecar_path, read_stable_utf8,
};
use disk_sync::{DiskConflict, DiskReadMessage, DiskReloadOutcome, DiskSyncState, ReloadKind};
use document::{Document, DocumentStats, EditorGalleyCache, TrackedTextBuffer};
use live_merge::{Merge3Outcome, merge_three_way};
use search::{SearchState, find_match_count, replace_all_occurrences};

const DEBOUNCE: Duration = Duration::from_millis(150);
const DISK_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DISK_RELOAD_DEBOUNCE: Duration = Duration::from_millis(75);
const STATS_RECALC_DEBOUNCE: Duration = Duration::from_millis(120);
const ZOOM_STEP: f32 = 0.1;
const MIN_ZOOM_FACTOR: f32 = 0.5;
const MAX_ZOOM_FACTOR: f32 = 3.0;
const PANEL_EDGE_PADDING: f32 = 8.0;
const DIAGNOSTICS_DEFAULT_ITERATIONS: usize = 200;
const DIAGNOSTICS_DEFAULT_RUNS: usize = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
struct LaunchOptions {
    mode: Mode,
    path: Option<PathBuf>,
    print_version: bool,
    diagnostics: DiagnosticsMode,
    diagnostics_iterations: usize,
    diagnostics_runs: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DiagnosticsMode {
    #[default]
    Off,
    OpenPipeline,
    #[cfg(debug_assertions)]
    NavPipeline,
}

#[must_use]
fn parse_launch_options<I, S>(args: I) -> LaunchOptions
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut mode = None;
    let mut path = None;
    let mut print_version = false;
    let mut diagnostics = DiagnosticsMode::Off;
    let mut diagnostics_iterations = DIAGNOSTICS_DEFAULT_ITERATIONS;
    let mut diagnostics_runs = DIAGNOSTICS_DEFAULT_RUNS;
    let mut parse_flags = true;

    for arg in args {
        let arg = arg.into();
        if arg == "-v" || arg == "--version" {
            print_version = true;
            continue;
        }
        if parse_flags {
            if arg == "--" {
                parse_flags = false;
                continue;
            }
            if arg == "-p" {
                mode = Some(Mode::Preview);
                continue;
            }
            if arg == "-s" {
                mode = Some(Mode::SideBySide);
                continue;
            }
            if arg == "--diagnostics-open" || arg == "--diag-open" {
                diagnostics = DiagnosticsMode::OpenPipeline;
                continue;
            }
            #[cfg(debug_assertions)]
            if arg == "--diagnostics-nav" || arg == "--diag-nav" {
                diagnostics = DiagnosticsMode::NavPipeline;
                continue;
            }
            if let Some(value) = arg
                .to_str()
                .and_then(|value| {
                    value
                        .strip_prefix("--diag-iterations=")
                        .or_else(|| value.strip_prefix("--diagnostics-iterations="))
                })
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|value| *value > 0)
            {
                diagnostics_iterations = value;
                continue;
            }
            if let Some(value) = arg
                .to_str()
                .and_then(|value| {
                    value
                        .strip_prefix("--diag-runs=")
                        .or_else(|| value.strip_prefix("--diagnostics-runs="))
                })
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|value| *value > 0)
            {
                diagnostics_runs = value;
                continue;
            }
            if arg.to_str().is_some_and(|value| value.starts_with('-')) {
                continue;
            }
        }

        if path.is_none() {
            path = Some(PathBuf::from(arg));
        }
    }

    let mode = mode.unwrap_or_else(|| {
        if path.is_some() {
            Mode::Preview
        } else {
            Mode::Edit
        }
    });

    LaunchOptions {
        mode,
        path,
        print_version,
        diagnostics,
        diagnostics_iterations,
        diagnostics_runs,
    }
}

#[must_use]
const fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// On WSL, smithay-clipboard connects via Wayland and panics with
/// "Broken pipe (os error 32)" during window resize.  Clearing
/// `WAYLAND_DISPLAY` forces the clipboard backend to X11 (arboard),
/// which avoids the crash while keeping clipboard fully functional.
/// See <https://github.com/emilk/egui/issues/3805>.
///
/// The workaround is only applied when `libxkbcommon-x11.so` is present;
/// without it the X11 backend cannot initialise and the app would panic
/// on startup.  When the library is absent, Wayland remains active and
/// the user sees a diagnostic hint to install it.
#[cfg(target_os = "linux")]
fn apply_wsl_workarounds() {
    if let Ok(ver) = std::fs::read_to_string("/proc/version")
        && ver.to_ascii_lowercase().contains("microsoft")
    {
        if x11_keyboard_lib_available() {
            // SAFETY: called at the top of main() before any threads are
            // spawned, so there is no concurrent access to the environment.
            #[allow(unsafe_code)]
            unsafe {
                std::env::remove_var("WAYLAND_DISPLAY");
            }
        } else {
            eprintln!(
                "rustdown: WSL detected but libxkbcommon-x11.so not found; \
                 X11 clipboard workaround disabled. Install libxkbcommon-x11-dev \
                 to avoid resize crashes."
            );
        }
    }
}

/// Returns `true` when `libxkbcommon-x11.so` can be loaded by the dynamic
/// linker, meaning the X11 keyboard backend will work at runtime.
#[cfg(target_os = "linux")]
fn x11_keyboard_lib_available() -> bool {
    // SAFETY: libxkbcommon-x11 is a well-known system library with no
    // harmful init-time side effects.  We load only to probe availability
    // and the library is dropped immediately.
    #[allow(unsafe_code)]
    let result = unsafe { libloading::Library::new("libxkbcommon-x11.so") };
    result.is_ok()
}

fn main() -> eframe::Result {
    let launch_options = parse_launch_options(std::env::args_os().skip(1));
    if launch_options.print_version {
        println!("{}", app_version());
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    apply_wsl_workarounds();

    if launch_options.diagnostics == DiagnosticsMode::OpenPipeline {
        for run in 0..launch_options.diagnostics_runs {
            if launch_options.diagnostics_runs > 1 {
                println!(
                    "diagnostics_run={}/{}",
                    run + 1,
                    launch_options.diagnostics_runs
                );
            }
            if let Err(err) = diagnostics::run_open_pipeline_diagnostics(
                launch_options.path.as_deref(),
                launch_options.diagnostics_iterations,
            ) {
                eprintln!("Diagnostics failed: {err}");
                break;
            }
        }
        return Ok(());
    }
    #[cfg(debug_assertions)]
    if launch_options.diagnostics == DiagnosticsMode::NavPipeline {
        if let Err(err) = nav_debug::run_nav_diagnostics(launch_options.path.as_deref()) {
            eprintln!("Nav diagnostics failed: {err}");
        }
        return Ok(());
    }
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
            egui_extras::install_image_loaders(&cc.egui_ctx);
            ui_style::configure_fonts(&cc.egui_ctx).map_err(std::io::Error::other)?;
            ui_style::configure_style(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}

#[must_use]
fn markdown_file_dialog() -> rfd::FileDialog {
    rfd::FileDialog::new().add_filter("Markdown", &["md", "markdown"])
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
pub fn default_image_uri_scheme(path: Option<&Path>) -> String {
    let Some(parent) = path.and_then(Path::parent) else {
        return "file://".to_owned();
    };

    let needs_canonicalize = !parent.is_absolute()
        || parent
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir));
    let base = if needs_canonicalize {
        parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf())
    } else {
        parent.to_path_buf()
    };
    let mut normalized = base.to_string_lossy().replace('\\', "/");
    if !normalized.starts_with('/') {
        normalized.insert(0, '/');
    }
    if !normalized.ends_with('/') {
        normalized.push('/');
    }
    format!("file://{normalized}")
}

#[derive(Default)]
struct RustdownApp {
    doc: Document,
    mode: Mode,
    search: SearchState,
    nav: nav_panel::NavState,
    error: Option<String>,
    pending_action: Option<PendingAction>,
    last_viewport_title: String,
    focus_search: bool,
    heading_color_mode: bool,

    /// Last editor scroll byte offset used for `SideBySide` sync.
    /// Prevents feedback loops by only syncing when the source changes.
    last_sync_editor_byte: Option<usize>,
    /// Set to `true` when `resolve_nav_scroll_target` applies a target this
    /// frame; prevents `sync_side_by_side_scroll` from overriding it.
    nav_scroll_applied_this_frame: bool,

    disk: DiskSyncState,
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
    const fn cycle(self) -> Self {
        match self {
            Self::Edit => Self::Preview,
            Self::Preview => Self::SideBySide,
            Self::SideBySide => Self::Edit,
        }
    }

    #[must_use]
    const fn icon(self) -> &'static str {
        match self {
            Self::Edit => "Ed",
            Self::Preview => "Pr",
            Self::SideBySide => "S|S",
        }
    }

    #[must_use]
    const fn tooltip(self) -> &'static str {
        match self {
            Self::Edit => "Edit",
            Self::Preview => "Preview",
            Self::SideBySide => "Side-by-Side",
        }
    }
}

#[derive(Clone, Debug)]
enum PendingAction {
    NewBlank,
    Open(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveTrigger {
    Save,
    SaveAs,
}

#[must_use]
const fn save_trigger_from_shortcut(
    command: bool,
    shift: bool,
    key_s: bool,
) -> Option<SaveTrigger> {
    if !(command && key_s) {
        return None;
    }
    if shift {
        Some(SaveTrigger::SaveAs)
    } else {
        Some(SaveTrigger::Save)
    }
}

#[must_use]
const fn clamped_zoom_factor(zoom_factor: f32) -> f32 {
    zoom_factor.clamp(MIN_ZOOM_FACTOR, MAX_ZOOM_FACTOR)
}

#[must_use]
fn zoom_with_step(current_zoom: f32, delta: f32) -> f32 {
    clamped_zoom_factor(current_zoom + delta)
}

#[must_use]
fn zoom_with_factor(current_zoom: f32, factor: f32) -> f32 {
    if !factor.is_finite() || factor <= 0.0 {
        return clamped_zoom_factor(current_zoom);
    }
    clamped_zoom_factor(current_zoom * factor)
}

impl eframe::App for RustdownApp {
    #[allow(clippy::too_many_lines)] // main update loop — inherently long
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick_disk_sync(ctx);
        self.refresh_stats_if_due(ctx);

        let dialog_open = self.pending_action.is_some() || self.disk.conflict.is_some();
        let (
            dropped_path,
            open,
            save_trigger,
            new_doc,
            cycle_mode,
            search,
            replace_all_mode,
            format_doc,
            zoom_in,
            zoom_out,
            zoom_delta,
            escape,
            toggle_nav,
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
            )
        });

        if !dialog_open {
            if let Some(path) = dropped_path {
                self.request_action(PendingAction::Open(path));
            }
            if open {
                self.open_file();
            }
            if let Some(save_trigger) = save_trigger {
                let _ = self.save_doc(matches!(save_trigger, SaveTrigger::SaveAs));
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
            if replace_all_mode {
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
            }
        }

        let mut run_replace_all = false;
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            let mut clear_error = false;

            // Use a smaller font for the toolbar.
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

                ui.separator();
                let color_rt = if self.heading_color_mode {
                    tb("Aa").color(egui::Color32::from_rgb(0xFF, 0xB8, 0x6C))
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
                }
                ui.separator();
                if ui
                    .button(tb("Fmt"))
                    .on_hover_text("Format document")
                    .clicked()
                {
                    self.format_document();
                }
                ui.toggle_value(&mut self.nav.visible, tb("Nav"))
                    .on_hover_text("Navigation");

                ui.separator();

                ui.label(tb(&self.doc.path_label()));
                let stats = self.doc.stats();

                ui.separator();
                ui.label(
                    egui::RichText::new(format!("{} lines", stats.lines))
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
            }
        });
        if run_replace_all {
            let replaced = self.replace_all_matches();
            self.search.last_replace_count = Some(replaced);
        }

        self.show_content_panels(ctx);

        self.show_dialogs(ctx);
        self.show_disk_conflict_dialog(ctx);
        self.update_viewport_title(ctx);
    }
}

impl RustdownApp {
    /// Render the nav panel, side-by-side preview panel, central panel, and
    /// process any pending nav-scroll actions.  Extracted so the debug harness
    /// can reuse the same layout sequence.
    #[allow(clippy::cast_possible_truncation)] // PANEL_EDGE_PADDING=8.0, fits in i8
    fn show_content_panels(&mut self, ctx: &egui::Context) {
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
        if self.mode == Mode::SideBySide {
            self.sync_side_by_side_scroll(ctx);
        }
    }

    fn clear_disk_watcher(&mut self) {
        self.disk.watcher = None;
        self.disk.watch_root = None;
        self.disk.watch_target_name = None;
        self.disk.watch_rx = None;
    }

    fn schedule_disk_reload(&mut self, now: Instant) {
        let due_at = now + DISK_RELOAD_DEBOUNCE;
        if self
            .disk
            .pending_reload_at
            .is_none_or(|existing| existing > due_at)
        {
            self.disk.pending_reload_at = Some(due_at);
        }
    }

    fn apply_disk_text_state(
        &mut self,
        text: Arc<String>,
        base_text: Arc<String>,
        disk_rev: DiskRevision,
        kind: ReloadKind,
    ) {
        self.doc.text = text;
        self.doc.base_text = base_text;
        self.doc.disk_rev = Some(disk_rev);
        self.bump_edit_seq();
        self.doc.stats = DocumentStats::from_text(self.doc.text.as_str());
        self.doc.stats_dirty = false;
        self.doc.preview_cache.clear();
        self.doc.preview_dirty = false;
        self.doc.dirty = !matches!(kind, ReloadKind::Clean);
        if matches!(kind, ReloadKind::Clean | ReloadKind::ConflictResolved) {
            self.doc.last_edit_at = None;
        }
        self.doc.editor_galley_cache = None;
        self.error = None;
    }

    fn set_disk_conflict(
        &mut self,
        disk_text: String,
        disk_rev: DiskRevision,
        conflict_marked: String,
        ours_wins: String,
    ) {
        self.disk.conflict = Some(DiskConflict {
            disk_text,
            disk_rev,
            conflict_marked,
            ours_wins,
        });
    }

    fn reset_disk_sync_state(&mut self) {
        self.disk.reload_nonce = self.disk.reload_nonce.wrapping_add(1);
        self.disk.poll_at = None;
        self.disk.pending_reload_at = None;
        self.disk.reload_in_flight = false;
        self.disk.conflict = None;
        self.clear_disk_watcher();
    }

    fn ensure_disk_read_channel(&mut self) {
        if self.disk.read_tx.is_some() {
            return;
        }

        let (tx, rx) = mpsc::channel();
        self.disk.read_tx = Some(tx);
        self.disk.read_rx = Some(rx);
    }

    fn ensure_disk_watcher(&mut self, ctx: &egui::Context, path: &Path) {
        let watch_root = path.parent().unwrap_or_else(|| Path::new("."));
        let target_name = path.file_name().map(ToOwned::to_owned);

        if self.disk.watcher.is_some() && self.disk.watch_root.as_deref() == Some(watch_root) {
            self.disk.watch_target_name = target_name;
            return;
        }

        self.clear_disk_watcher();

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
                    .get_or_insert_with(|| format!("Watch setup failed: {err}"));
                return;
            }
        };

        if let Err(err) = watcher.watch(watch_root, RecursiveMode::NonRecursive) {
            self.error
                .get_or_insert_with(|| format!("Watch start failed: {err}"));
            return;
        }

        self.disk.watcher = Some(watcher);
        self.disk.watch_root = Some(watch_root.to_path_buf());
        self.disk.watch_target_name = Some(target_name);
        self.disk.watch_rx = Some(rx);
        self.disk.poll_at = None;
    }

    fn drain_disk_watch_events(&mut self) -> bool {
        let Some(rx) = self.disk.watch_rx.as_ref() else {
            return false;
        };

        let target_name = self.disk.watch_target_name.as_deref();
        let mut saw_change = false;
        let mut watch_error = None;
        let mut disconnected = false;

        loop {
            match rx.try_recv() {
                Ok(Ok(event)) => {
                    if let Some(target) = target_name
                        && event
                            .paths
                            .iter()
                            .any(|path| path.file_name().is_some_and(|name| name == target))
                    {
                        saw_change = true;
                    }
                }
                Ok(Err(err)) => {
                    watch_error = Some(format!("Watch error: {err}"));
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if let Some(err) = watch_error {
            self.error.get_or_insert(err);
            self.clear_disk_watcher();
            return false;
        }
        if disconnected {
            self.clear_disk_watcher();
            return false;
        }
        saw_change
    }

    fn tick_disk_sync(&mut self, ctx: &egui::Context) {
        self.drain_disk_read_results();

        if self.disk.conflict.is_some() {
            return;
        }

        let Some(path) = self.doc.path.clone() else {
            self.reset_disk_sync_state();
            return;
        };

        self.ensure_disk_watcher(ctx, path.as_path());

        let now = Instant::now();
        if self.drain_disk_watch_events() {
            self.schedule_disk_reload(now);
        }

        if self.disk.watcher.is_none() && !self.disk.reload_in_flight {
            match self.disk.poll_at {
                Some(next) if now < next => {}
                _ => {
                    self.disk.poll_at = Some(now + DISK_POLL_INTERVAL);

                    match disk_revision(path.as_path()) {
                        Ok(rev) if Some(rev) != self.doc.disk_rev => self.schedule_disk_reload(now),
                        Ok(_) => {}
                        Err(err) => {
                            self.error
                                .get_or_insert_with(|| format!("Disk check failed: {err}"));
                        }
                    }
                }
            }
        } else {
            self.disk.poll_at = None;
        }

        if self.disk.reload_in_flight {
            return;
        }

        if let Some(due_at) = self.disk.pending_reload_at
            && now >= due_at
        {
            self.disk.pending_reload_at = None;
            self.start_disk_reload(ctx, path.clone());
        }

        let mut next_wake = self.disk.pending_reload_at;
        if self.disk.watcher.is_none() {
            next_wake = match (next_wake, self.disk.poll_at) {
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
        let Some(tx) = self.disk.read_tx.clone() else {
            return;
        };

        let edit_seq = self.doc.edit_seq;
        let dirty = self.doc.dirty;
        let base_text = dirty.then(|| self.doc.base_text.clone());
        let ours_text = dirty.then(|| self.doc.text.clone());

        self.disk.reload_nonce = self.disk.reload_nonce.wrapping_add(1);
        let nonce = self.disk.reload_nonce;

        self.disk.reload_in_flight = true;
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
        loop {
            let recv = match self.disk.read_rx.as_ref() {
                Some(rx) => rx.try_recv(),
                None => return,
            };
            match recv {
                Ok(msg) => {
                    if msg.nonce != self.disk.reload_nonce {
                        continue;
                    }
                    if self.doc.path.as_deref() != Some(msg.path.as_path()) {
                        continue;
                    }
                    self.disk.reload_in_flight = false;

                    if self.doc.edit_seq != msg.edit_seq {
                        self.schedule_disk_reload(Instant::now());
                        continue;
                    }

                    match msg.outcome {
                        Ok(DiskReloadOutcome::Replace {
                            disk_text,
                            disk_rev,
                        }) => {
                            let disk_text = Arc::new(disk_text);
                            self.apply_disk_text_state(
                                disk_text.clone(),
                                disk_text,
                                disk_rev,
                                ReloadKind::Clean,
                            );
                        }
                        Ok(DiskReloadOutcome::MergeClean {
                            merged_text,
                            disk_text,
                            disk_rev,
                        }) => {
                            self.apply_disk_text_state(
                                Arc::new(merged_text),
                                Arc::new(disk_text),
                                disk_rev,
                                ReloadKind::Merged,
                            );
                        }
                        Ok(DiskReloadOutcome::MergeConflict {
                            disk_text,
                            disk_rev,
                            conflict_marked,
                            ours_wins,
                        }) => {
                            self.set_disk_conflict(disk_text, disk_rev, conflict_marked, ours_wins);
                        }
                        Err(err) => {
                            self.error = Some(format!("Reload failed: {err}"));
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.disk.reload_in_flight = false;
                    self.disk.read_rx = None;
                    self.disk.read_tx = None;
                    return;
                }
            }
        }
    }

    fn incorporate_disk_text(&mut self, disk_text: String, disk_rev: DiskRevision) {
        if self.doc.disk_rev == Some(disk_rev) && disk_text == self.doc.base_text.as_str() {
            return;
        }

        if !self.doc.dirty {
            let disk_text = Arc::new(disk_text);
            self.apply_disk_text_state(disk_text.clone(), disk_text, disk_rev, ReloadKind::Clean);
            return;
        }

        match merge_three_way(
            self.doc.base_text.as_str(),
            self.doc.text.as_str(),
            disk_text.as_str(),
        ) {
            Merge3Outcome::Clean(merged) => {
                self.apply_disk_text_state(
                    Arc::new(merged),
                    Arc::new(disk_text),
                    disk_rev,
                    ReloadKind::Merged,
                );
            }
            Merge3Outcome::Conflicted {
                conflict_marked,
                ours_wins,
            } => {
                self.set_disk_conflict(disk_text, disk_rev, conflict_marked, ours_wins);
            }
        }
    }

    fn from_launch_options(options: LaunchOptions) -> Self {
        let mut app = Self {
            mode: options.mode,
            ..Self::default()
        };
        if let Some(path) = options.path {
            app.open_path(path);
        }
        app
    }

    fn set_mode(&mut self, mode: Mode, ctx: &egui::Context) {
        if self.mode == mode {
            return;
        }

        // Capture the current scroll position as a byte offset before
        // switching modes, so the new mode can jump to the same location.
        let scroll_byte = self.current_scroll_byte_offset(ctx);

        self.mode = mode;
        // Clear stale sync state so the first SideBySide frame does not
        // override a nav-driven scroll target with a stale byte comparison.
        self.last_sync_editor_byte = None;

        if mode == Mode::Preview {
            // Preview doesn't render the editor; drop any cached galley to reduce memory.
            self.doc.editor_galley_cache = None;
        }

        if mode == Mode::Edit {
            self.doc.preview_cache.clear();
            self.doc.preview_dirty = false;
            self.doc.last_edit_at = None;
        }

        // Request a scroll to the same position in the new mode.
        if let Some(byte_offset) = scroll_byte {
            self.nav.pending_scroll = Some(nav_panel::NavScrollTarget::ByteOffset(byte_offset));
        }
    }

    /// Returns `true` when the current mode renders the code editor.
    const fn uses_editor(&self) -> bool {
        matches!(self.mode, Mode::Edit | Mode::SideBySide)
    }

    /// Convert a heading byte offset to a preview scroll-y value.
    ///
    /// Uses exact heading Y positions from the parsed preview cache when
    /// available, and falls back to byte-proportional mapping otherwise.
    fn preview_nav_scroll_y(&self, byte_offset: usize) -> f32 {
        if let Some(ordinal) = self
            .nav
            .outline
            .iter()
            .position(|h| h.byte_offset == byte_offset)
            && let Some(y) = self.doc.preview_cache.heading_y(ordinal)
        {
            return y;
        }
        nav_panel::preview_byte_to_scroll_y(
            &self.nav.outline,
            byte_offset,
            self.doc.preview_cache.total_height,
        )
    }

    /// Determine the current scroll position as a byte offset in the source
    /// text.  Works in both editor and preview modes.
    fn current_scroll_byte_offset(&mut self, ctx: &egui::Context) -> Option<usize> {
        if self.uses_editor() {
            self.ensure_row_byte_offsets();
            let state = egui::scroll_area::State::load(ctx, nav_panel::editor_scroll_id())?;
            self.editor_y_to_byte(state.offset.y)
        } else {
            Some(nav_panel::preview_scroll_y_to_byte(
                &self.nav.outline,
                self.doc.preview_cache.last_scroll_y,
                self.doc.preview_cache.total_height,
            ))
        }
    }

    #[allow(clippy::unused_self)]
    fn adjust_zoom(&self, ctx: &egui::Context, delta: f32) {
        ctx.set_zoom_factor(zoom_with_step(ctx.zoom_factor(), delta));
    }

    #[allow(clippy::unused_self)]
    fn adjust_zoom_factor(&self, ctx: &egui::Context, factor: f32) {
        ctx.set_zoom_factor(zoom_with_factor(ctx.zoom_factor(), factor));
    }

    fn update_viewport_title(&mut self, ctx: &egui::Context) {
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

    const fn bump_edit_seq(&mut self) {
        self.doc.edit_seq = self.doc.edit_seq.wrapping_add(1);
    }

    fn refresh_stats_now(&mut self) {
        self.doc.stats = DocumentStats::from_text(self.doc.text.as_str());
        self.doc.stats_dirty = false;
    }

    fn refresh_stats_if_due(&mut self, ctx: &egui::Context) {
        if !self.doc.stats_dirty {
            return;
        }
        if let Some(remaining) = self.doc.debounce_remaining(STATS_RECALC_DEBOUNCE) {
            ctx.request_repaint_after(remaining);
            return;
        }
        self.refresh_stats_now();
    }

    fn note_text_changed(&mut self, defer_stats_recalc: bool) {
        self.doc.dirty = true;
        self.doc.last_edit_at = Some(Instant::now());
        self.doc.stats_dirty = true;
        self.doc.preview_dirty = true;
        if !defer_stats_recalc {
            self.refresh_stats_now();
        }
    }

    const fn open_search(&mut self, replace_mode: bool) {
        self.search.visible = true;
        self.search.replace_mode = replace_mode;
        self.search.last_replace_count = None;
        self.focus_search = true;
    }

    const fn close_search(&mut self) {
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
            self.doc.text = Arc::new(text);
            self.bump_edit_seq();
            self.note_text_changed(false);
        }
        replaced
    }

    fn format_document(&mut self) {
        let options = format::options_for_path(self.doc.path.as_deref());
        let formatted = format::format_markdown(self.doc.text.as_str(), options);
        if formatted == self.doc.text.as_str() {
            return;
        }

        self.doc.text = Arc::new(formatted);
        self.bump_edit_seq();
        self.note_text_changed(false);
    }

    fn show_editor(&mut self, ui: &mut egui::Ui) {
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
                // Reuse the cached LayoutJob (skip O(n) highlight scan).
                let job = if let Some(cache) = editor_galley_cache.as_ref()
                    && cache.content_seq == seq
                    && cache.content_color_mode == heading_color_mode
                {
                    let mut job = cache.layout_job.clone();
                    job.wrap.max_width = wrap_width;
                    job
                } else {
                    let mut job = highlight::markdown_layout_job(
                        ui.style(),
                        ui.visuals(),
                        string,
                        heading_color_mode,
                    );
                    job.wrap.max_width = wrap_width;
                    job
                };

                let layout_job_copy = job.clone();
                let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                let row_byte_offsets = if nav_visible {
                    editor::build_row_byte_offsets(&galley, string)
                } else {
                    Vec::new()
                };
                *editor_galley_cache = Some(EditorGalleyCache {
                    content_seq: seq,
                    content_color_mode: heading_color_mode,
                    wrap_width_bits,
                    zoom_factor_bits,
                    layout_job: layout_job_copy,
                    galley: galley.clone(),
                    row_byte_offsets,
                });
                galley
            };

            let editor_size = ui.available_size();
            let scroll_to = self.nav.pending_editor_scroll_y.take();
            let mut scroll_area = egui::ScrollArea::both()
                .id_salt("editor_scroll")
                .auto_shrink([false; 2]);
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

    fn show_preview(&mut self, ui: &mut egui::Ui) {
        if self.mode == Mode::SideBySide
            && let Some(remaining) = self.doc.debounce_remaining(DEBOUNCE)
        {
            ui.ctx().request_repaint_after(remaining);
            return;
        }

        if self.doc.preview_dirty {
            self.doc.preview_cache.clear();
            self.doc.preview_dirty = false;
        }

        let mut style = if self.heading_color_mode {
            MarkdownStyle::colored(ui.visuals())
        } else {
            MarkdownStyle::from_visuals(ui.visuals())
        };
        style.image_base_uri = self.doc.image_uri_scheme.clone();

        // Consume any pending nav-scroll target and pass it directly to the
        // ScrollArea, avoiding the ID-mismatch problem with external state lookup.
        let scroll_y = self.nav.pending_preview_scroll_y.take();

        MarkdownViewer::new("preview_markdown").show_scrollable(
            ui,
            &mut self.doc.preview_cache,
            &style,
            self.doc.text.as_str(),
            scroll_y,
        );
    }

    /// Resolve a pending [`NavScrollTarget`] into per-pane y-pixel targets.
    /// Must run *before* the scroll areas render so targets are consumed
    /// on the same frame.
    fn resolve_nav_scroll_target(&mut self, ctx: &egui::Context) {
        use nav_panel::NavScrollTarget;
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
                // Pre-seed the sync byte so sync_side_by_side_scroll does
                // not override the precise nav-driven preview position with
                // a linearly-interpolated value on the next frame.
                self.last_sync_editor_byte = editor_target_y.and_then(|y| self.editor_y_to_byte(y));
            }
        }
        self.nav_scroll_applied_this_frame = true;
        ctx.request_repaint();
    }

    /// Read the current scroll offset and update the active heading in the
    /// nav panel.  Must run *after* the scroll areas render.
    fn sync_nav_active_heading(&mut self, ctx: &egui::Context) {
        if self.uses_editor() {
            self.ensure_row_byte_offsets();
            if let Some(state) = egui::scroll_area::State::load(ctx, nav_panel::editor_scroll_id())
                && let Some(byte_pos) = self.editor_y_to_byte(state.offset.y)
            {
                self.nav.update_active_from_position(byte_pos);
            }
        } else {
            // Preview mode: convert cached scroll-y to byte offset via outline.
            let byte_pos = nav_panel::preview_scroll_y_to_byte(
                &self.nav.outline,
                self.doc.preview_cache.last_scroll_y,
                self.doc.preview_cache.total_height,
            );
            self.nav.update_active_from_position(byte_pos);
        }
    }

    /// In Side-by-Side mode, sync the preview scroll position to track the
    /// editor scroll position.  Uses byte offsets as an intermediate
    /// representation so both panes show the same content region.
    fn sync_side_by_side_scroll(&mut self, ctx: &egui::Context) {
        // Skip sync if a nav-panel scroll target was already applied this frame;
        // re-syncing would override it and cause a visible snap.
        if self.nav_scroll_applied_this_frame {
            return;
        }

        self.ensure_row_byte_offsets();
        self.nav.refresh_outline(&self.doc.text, self.doc.edit_seq);

        // Read editor scroll position.
        let editor_state = egui::scroll_area::State::load(ctx, nav_panel::editor_scroll_id());
        let Some(editor_state) = editor_state else {
            return;
        };
        let Some(editor_byte) = self.editor_y_to_byte(editor_state.offset.y) else {
            return;
        };

        // Only sync when the editor position has actually changed.
        if self.last_sync_editor_byte == Some(editor_byte) {
            return;
        }
        self.last_sync_editor_byte = Some(editor_byte);

        // Map editor byte offset to preview scroll-y.
        let preview_y = nav_panel::preview_byte_to_scroll_y(
            &self.nav.outline,
            editor_byte,
            self.doc.preview_cache.total_height,
        );

        // Store as pending — show_preview will consume it via vertical_scroll_offset.
        self.nav.pending_preview_scroll_y = Some(preview_y);
        ctx.request_repaint();
    }

    /// Map a byte offset to a Y position using the cached row byte offsets.
    /// O(log n) binary search instead of O(n) char scan.
    fn editor_byte_to_y(&self, byte_offset: usize) -> Option<f32> {
        let cache = self.doc.editor_galley_cache.as_ref()?;
        Some(editor::row_byte_offset_to_y(
            &cache.row_byte_offsets,
            byte_offset,
        ))
    }

    /// Map a Y scroll position to a byte offset using the cached row byte
    /// offsets.  O(log n) binary search.
    fn editor_y_to_byte(&self, y: f32) -> Option<usize> {
        let cache = self.doc.editor_galley_cache.as_ref()?;
        Some(editor::row_y_to_byte_offset(&cache.row_byte_offsets, y))
    }

    fn ensure_row_byte_offsets(&mut self) {
        if let Some(cache) = self.doc.editor_galley_cache.as_mut()
            && cache.row_byte_offsets.is_empty()
        {
            cache.row_byte_offsets =
                editor::build_row_byte_offsets(&cache.galley, self.doc.text.as_str());
        }
    }

    fn request_action(&mut self, action: PendingAction) {
        if self.doc.dirty {
            self.pending_action = Some(action);
        } else {
            self.apply_action(action);
        }
    }

    fn load_document(&mut self, path: PathBuf, text: String, disk_rev: Option<DiskRevision>) {
        let text = Arc::new(text);
        let base_text = text.clone();
        let image_uri_scheme = default_image_uri_scheme(Some(path.as_path()));
        self.doc = Document {
            path: Some(path),
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
            edit_seq: 1, // Start at 1 so nav refresh triggers (default outline_seq may be 0)
            editor_galley_cache: None,
        };
        // Force nav outline refresh on next frame.
        self.nav.invalidate_outline();
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

    fn save_path_choice(&self, save_as: bool) -> Option<(PathBuf, bool)> {
        if !save_as && let Some(path) = self.doc.path.clone() {
            return Some((path, false));
        }
        markdown_file_dialog().save_file().map(|path| (path, true))
    }

    fn save_doc(&mut self, save_as: bool) -> bool {
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

    fn write_merge_sidecar(&mut self, doc_path: &Path, conflict_marked: &str) {
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

    fn apply_conflict_choice(&mut self, choice: ConflictChoice) {
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

#[derive(Clone, Copy, Debug)]
enum ConflictChoice {
    OpenConflictMerge,
    KeepMineWriteSidecar,
    SaveAs,
    ReloadDisk,
    OverwriteDisk,
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::document::bytecount_newlines;
    use std::{fs, time::SystemTime};

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

    fn disk_conflict(app: &RustdownApp) -> &DiskConflict {
        assert!(
            app.disk.conflict.is_some(),
            "Expected conflict prompt to be set"
        );
        app.disk.conflict.as_ref().unwrap_or_else(|| unreachable!())
    }

    fn read_file(path: &Path) -> String {
        let text_res = fs::read_to_string(path);
        assert!(text_res.is_ok(), "Failed to read file: {text_res:?}");
        text_res.unwrap_or_else(|_| unreachable!())
    }

    fn merge_app(
        base_text: &str,
        text: &str,
        rev_seconds: u64,
        rev_len: u64,
        dirty: bool,
    ) -> RustdownApp {
        let mut app = RustdownApp::default();
        app.doc.path = Some(PathBuf::from("note.md"));
        app.doc.base_text = Arc::new(base_text.to_owned());
        app.doc.text = Arc::new(text.to_owned());
        app.doc.disk_rev = Some(test_rev(rev_seconds, rev_len));
        app.doc.dirty = dirty;
        app
    }

    #[test]
    fn parse_launch_options_covers_modes_paths_and_diagnostics() {
        let mode_cases = [
            (&[][..], Mode::Edit, None),
            (&["-p"][..], Mode::Preview, None),
            (&["-s"][..], Mode::SideBySide, None),
            (
                &["README.md", "OTHER.md"][..],
                Mode::Preview,
                Some("README.md"),
            ),
            (&["-p", "README.md"][..], Mode::Preview, Some("README.md")),
            (
                &["--gapplication-service", "README.md"][..],
                Mode::Preview,
                Some("README.md"),
            ),
            (
                &["--", "--scratch.md"][..],
                Mode::Preview,
                Some("--scratch.md"),
            ),
        ];

        for (args, mode, path) in mode_cases {
            let options = parse(args);
            assert_eq!(options.mode, mode);
            assert_eq!(options.path.as_deref(), path.map(PathBuf::from).as_deref());
            assert!(!options.print_version);
            assert_eq!(options.diagnostics, DiagnosticsMode::Off);
            assert_eq!(
                options.diagnostics_iterations,
                DIAGNOSTICS_DEFAULT_ITERATIONS
            );
            assert_eq!(options.diagnostics_runs, DIAGNOSTICS_DEFAULT_RUNS);
        }

        let options = parse(&["--diagnostics-open", "README.md"]);
        assert_eq!(options.diagnostics, DiagnosticsMode::OpenPipeline);
        assert_eq!(
            options.path.as_deref(),
            Some(PathBuf::from("README.md")).as_deref()
        );
        assert!(!options.print_version);
        assert_eq!(
            options.diagnostics_iterations,
            DIAGNOSTICS_DEFAULT_ITERATIONS
        );
        assert_eq!(options.diagnostics_runs, DIAGNOSTICS_DEFAULT_RUNS);

        let options = parse(&["-v"]);
        assert!(options.print_version);
        assert_eq!(options.mode, Mode::Edit);
        assert!(options.path.is_none());

        let options = parse(&["--version", "README.md"]);
        assert!(options.print_version);
        assert_eq!(
            options.path.as_deref(),
            Some(PathBuf::from("README.md")).as_deref()
        );

        let options = parse(&["--", "-v"]);
        assert!(options.print_version);
        assert_eq!(options.mode, Mode::Edit);
        assert!(options.path.is_none());

        let options = parse(&["--", "--version"]);
        assert!(options.print_version);
        assert_eq!(options.mode, Mode::Edit);
        assert!(options.path.is_none());

        let cases = [
            ("--diag-iterations=25", 25),
            ("--diagnostics-iterations=10", 10),
            ("--diag-iterations=0", DIAGNOSTICS_DEFAULT_ITERATIONS),
        ];
        for (flag, expected) in cases {
            let options = parse(&[flag, "README.md"]);
            assert_eq!(options.diagnostics_iterations, expected);
        }

        let run_cases = [
            ("--diag-runs=3", 3),
            ("--diagnostics-runs=7", 7),
            ("--diag-runs=0", DIAGNOSTICS_DEFAULT_RUNS),
        ];
        for (flag, expected) in run_cases {
            let options = parse(&[flag, "README.md"]);
            assert_eq!(options.diagnostics_runs, expected);
        }

        #[cfg(debug_assertions)]
        {
            let options = parse(&["--diagnostics-nav", "README.md"]);
            assert_eq!(options.diagnostics, DiagnosticsMode::NavPipeline);
            assert_eq!(
                options.path.as_deref(),
                Some(PathBuf::from("README.md")).as_deref()
            );

            let options = parse(&["--diag-nav", "README.md"]);
            assert_eq!(options.diagnostics, DiagnosticsMode::NavPipeline);
        }
    }

    #[test]
    fn document_stats_cover_empty_populated_and_default_document() {
        let stats = DocumentStats::from_text("one two\nthree");
        assert_eq!(stats.lines, 2);

        let unicode_stats = DocumentStats::from_text("héllo 世界\n🙂");
        assert_eq!(unicode_stats.lines, 2);

        let empty_stats = DocumentStats::from_text("");
        assert_eq!(empty_stats, DocumentStats::default());

        let doc = Document::default();
        assert_eq!(doc.stats(), DocumentStats::from_text(""));
    }

    #[test]
    fn markdown_path_helpers_cover_detection_and_selection() {
        assert!(is_markdown_path(Path::new("note.md")));
        assert!(is_markdown_path(Path::new("README.Markdown")));
        assert!(!is_markdown_path(Path::new("notes.txt")));
        assert!(!is_markdown_path(Path::new("README")));
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
    fn default_image_uri_scheme_uses_document_directory_when_available() {
        assert_eq!(default_image_uri_scheme(None), "file://");
        let dir = make_temp_dir("rustdown-image-uri-scheme-test");
        let path = dir.join("report.md");
        let scheme = default_image_uri_scheme(Some(path.as_path()));

        assert!(scheme.starts_with("file://"));
        assert!(scheme.ends_with('/'));
        let dir_name = dir.file_name().and_then(|name| name.to_str()).unwrap_or("");
        assert!(
            scheme.contains(dir_name),
            "Expected '{scheme}' to contain '{dir_name}'"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_and_replace_helpers_handle_empty_and_replacement_cases() {
        assert_eq!(find_match_count("abc abc", ""), 0);
        let (text, replaced) = replace_all_occurrences("alpha beta alpha", "alpha", "zeta");
        assert_eq!(text.as_ref(), "zeta beta zeta");
        assert_eq!(replaced, 2);
        let (text, replaced) = replace_all_occurrences("alpha beta", "alpha", "alpha");
        assert_eq!(text.as_ref(), "alpha beta");
        assert_eq!(replaced, 0);

        let mut search = SearchState::with_query("alpha");
        assert_eq!(search.match_count("alpha beta alpha", 1), 2);
        assert_eq!(search.match_count("alpha beta alpha", 1), 2);
        search.query = "beta".to_owned();
        assert_eq!(search.match_count("alpha beta alpha", 1), 1);
        assert_eq!(search.match_count("alpha beta alpha", 2), 1);
    }

    #[test]
    fn save_trigger_and_zoom_helpers_cover_keyboard_and_scroll_paths() {
        assert_eq!(
            save_trigger_from_shortcut(true, false, true),
            Some(SaveTrigger::Save)
        );
        assert_eq!(
            save_trigger_from_shortcut(true, true, true),
            Some(SaveTrigger::SaveAs)
        );
        assert_eq!(save_trigger_from_shortcut(false, false, true), None);
        assert_eq!(save_trigger_from_shortcut(true, false, false), None);
        assert!((zoom_with_step(1.0, ZOOM_STEP) - 1.1).abs() < f32::EPSILON);
        assert_eq!(zoom_with_step(MAX_ZOOM_FACTOR, ZOOM_STEP), MAX_ZOOM_FACTOR);
        assert_eq!(zoom_with_step(MIN_ZOOM_FACTOR, -ZOOM_STEP), MIN_ZOOM_FACTOR);

        assert!((zoom_with_factor(1.0, 1.2) - 1.2).abs() < f32::EPSILON);
        assert_eq!(zoom_with_factor(MAX_ZOOM_FACTOR, 2.0), MAX_ZOOM_FACTOR);
        assert!((zoom_with_factor(1.0, 0.0) - 1.0).abs() < f32::EPSILON);
        assert!((zoom_with_factor(1.0, f32::NAN) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn deferred_note_text_changed_marks_stats_dirty_until_due_refresh() {
        let mut app = RustdownApp::default();
        app.doc.text = Arc::new("alpha beta".to_owned());
        app.doc.stats = DocumentStats::from_text(app.doc.text.as_str());
        app.doc.base_text = app.doc.text.clone();

        app.doc.text = Arc::new("alpha beta gamma".to_owned());
        app.bump_edit_seq();
        app.note_text_changed(true);

        assert!(app.doc.stats_dirty);
        assert_eq!(app.doc.stats, DocumentStats::from_text("alpha beta"));

        app.doc.last_edit_at = Instant::now().checked_sub(STATS_RECALC_DEBOUNCE);
        let ctx = egui::Context::default();
        app.refresh_stats_if_due(&ctx);

        assert!(!app.doc.stats_dirty);
        assert_eq!(app.doc.stats, DocumentStats::from_text("alpha beta gamma"));
    }

    #[test]
    fn replace_all_matches_updates_document_and_stats() {
        let mut app = RustdownApp::default();
        app.doc.text = Arc::new("alpha beta alpha".to_owned());
        app.doc.stats = DocumentStats::from_text(app.doc.text.as_str());
        app.search.query = "alpha".to_owned();
        app.search.replacement = "zeta".to_owned();

        let replaced = app.replace_all_matches();
        assert_eq!(replaced, 2);
        assert_eq!(app.doc.text.as_str(), "zeta beta zeta");
        assert!(app.doc.dirty);
        assert_eq!(app.doc.stats, DocumentStats::from_text("zeta beta zeta"));
    }

    #[test]
    fn open_path_missing_file_treats_path_as_new_document() {
        let dir = make_temp_dir("rustdown-open-new-file-test");
        let path = dir.join("new.md");

        let mut app = RustdownApp::default();
        app.doc.path = Some(PathBuf::from("old.md"));
        app.doc.text = Arc::new("existing text".to_owned());
        app.doc.base_text = Arc::new("existing text".to_owned());
        app.doc.stats = DocumentStats::from_text(app.doc.text.as_str());
        app.doc.dirty = true;
        app.error = Some("old error".to_owned());

        app.open_path(path.clone());

        assert_eq!(app.doc.path.as_deref(), Some(path.as_path()));
        assert_eq!(app.doc.text.as_str(), "");
        assert_eq!(app.doc.base_text.as_str(), "");
        assert_eq!(app.doc.disk_rev, None);
        assert_eq!(app.doc.stats, DocumentStats::default());
        assert!(!app.doc.dirty);
        assert!(app.error.is_none());
        assert!(!path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn incorporate_disk_text_handles_clean_merge_and_conflict_outcomes() {
        struct Case<'a> {
            base: &'a str,
            ours: &'a str,
            dirty: bool,
            disk_text: &'a str,
            initial_disk_rev: (u64, u64),
            incoming_disk_rev: (u64, u64),
            expected_text: &'a str,
            expected_base: &'a str,
            expected_disk_rev: (u64, u64),
            expected_dirty: bool,
            expect_conflict: bool,
        }
        for case in [
            Case {
                base: "old",
                ours: "old",
                dirty: false,
                disk_text: "new",
                initial_disk_rev: (1, 3),
                incoming_disk_rev: (2, 3),
                expected_text: "new",
                expected_base: "new",
                expected_disk_rev: (2, 3),
                expected_dirty: false,
                expect_conflict: false,
            },
            Case {
                base: "a\nb\n",
                ours: "a\nB\n",
                dirty: true,
                disk_text: "A\nb\n",
                initial_disk_rev: (1, 4),
                incoming_disk_rev: (2, 4),
                expected_text: "A\nB\n",
                expected_base: "A\nb\n",
                expected_disk_rev: (2, 4),
                expected_dirty: true,
                expect_conflict: false,
            },
            Case {
                base: "a\nb\n",
                ours: "a\nO\n",
                dirty: true,
                disk_text: "a\nT\n",
                initial_disk_rev: (1, 4),
                incoming_disk_rev: (2, 4),
                expected_text: "a\nO\n",
                expected_base: "a\nb\n",
                expected_disk_rev: (1, 4),
                expected_dirty: true,
                expect_conflict: true,
            },
        ] {
            let mut app = merge_app(
                case.base,
                case.ours,
                case.initial_disk_rev.0,
                case.initial_disk_rev.1,
                case.dirty,
            );
            app.incorporate_disk_text(
                case.disk_text.to_owned(),
                test_rev(case.incoming_disk_rev.0, case.incoming_disk_rev.1),
            );
            assert_eq!(app.doc.text.as_str(), case.expected_text);
            assert_eq!(app.doc.base_text.as_str(), case.expected_base);
            assert_eq!(
                app.doc.disk_rev,
                Some(test_rev(case.expected_disk_rev.0, case.expected_disk_rev.1))
            );
            assert_eq!(app.doc.dirty, case.expected_dirty);
            assert_eq!(app.disk.conflict.is_some(), case.expect_conflict);
        }
    }

    #[test]
    fn conflict_choice_open_merge_replaces_buffer_with_conflict_markers() {
        let mut app = merge_app("a\nb\n", "a\nO\n", 1, 4, true);

        app.incorporate_disk_text("a\nT\n".to_owned(), test_rev(2, 4));
        let expected_merge = disk_conflict(&app).conflict_marked.clone();

        app.apply_conflict_choice(ConflictChoice::OpenConflictMerge);

        assert_eq!(app.doc.text.as_str(), expected_merge.as_str());
        assert_eq!(app.doc.base_text.as_str(), "a\nT\n");
        assert_eq!(app.doc.disk_rev, Some(test_rev(2, 4)));
        assert!(app.doc.dirty);
        assert!(app.disk.conflict.is_none());
    }

    #[test]
    fn conflict_choice_keep_mine_writes_sidecar_and_applies_safe_disk_edits() {
        let dir = make_temp_dir("rustdown-merge-test");
        let original = dir.join("note.md");

        let _ = atomic_write_utf8(&original, "line1\nline2\nline3\n");

        let mut app = merge_app("line1\nline2\nline3\n", "line1\nO2\nline3\n", 1, 18, true);
        app.doc.path = Some(original);

        app.incorporate_disk_text("line1\nT2\nT3\n".to_owned(), test_rev(2, 15));
        let conflict = disk_conflict(&app);
        let expected_sidecar = conflict.conflict_marked.clone();
        let expected_ours_wins = conflict.ours_wins.clone();

        app.apply_conflict_choice(ConflictChoice::KeepMineWriteSidecar);

        assert_eq!(app.doc.text.as_str(), expected_ours_wins.as_str());
        assert_eq!(app.doc.base_text.as_str(), "line1\nT2\nT3\n");
        assert_eq!(app.doc.disk_rev, Some(test_rev(2, 15)));
        assert!(app.disk.conflict.is_none());

        assert!(
            app.disk.merge_sidecar_path.is_some(),
            "Expected merge sidecar path to be set"
        );
        let sidecar_path = app
            .disk
            .merge_sidecar_path
            .clone()
            .unwrap_or_else(|| unreachable!());
        let sidecar_text = read_file(&sidecar_path);
        assert_eq!(sidecar_text, expected_sidecar);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bytecount_newlines_counts_correctly() {
        assert_eq!(bytecount_newlines(""), 0);
        assert_eq!(bytecount_newlines("no newline"), 0);
        assert_eq!(bytecount_newlines("a\nb\nc\n"), 3);
        assert_eq!(bytecount_newlines("\n\n\n"), 3);
    }

    #[test]
    fn document_title_and_path_label() {
        let default_doc = Document::default();
        assert_eq!(default_doc.title().as_ref(), "Untitled");
        assert_eq!(default_doc.path_label().as_ref(), "Unsaved");

        let doc = Document {
            path: Some(PathBuf::from("/home/user/notes.md")),
            ..Document::default()
        };
        assert_eq!(doc.title().as_ref(), "notes.md");
        assert_eq!(doc.path_label().as_ref(), "/home/user/notes.md");
    }

    #[test]
    fn document_debounce_remaining_returns_none_when_no_edit() {
        let doc = Document::default();
        assert!(doc.debounce_remaining(Duration::from_millis(500)).is_none());
    }

    #[test]
    fn mode_cycle_covers_all_modes() {
        assert_eq!(Mode::Edit.cycle(), Mode::Preview);
        assert_eq!(Mode::Preview.cycle(), Mode::SideBySide);
        assert_eq!(Mode::SideBySide.cycle(), Mode::Edit);
    }

    #[test]
    fn mode_icons_and_tooltips() {
        assert_eq!(Mode::Edit.icon(), "Ed");
        assert_eq!(Mode::Preview.icon(), "Pr");
        assert_eq!(Mode::SideBySide.icon(), "S|S");
        assert_eq!(Mode::Edit.tooltip(), "Edit");
        assert_eq!(Mode::Preview.tooltip(), "Preview");
        assert_eq!(Mode::SideBySide.tooltip(), "Side-by-Side");
    }

    #[test]
    fn find_match_count_single_byte_and_multi_byte() {
        assert_eq!(find_match_count("aaa", "a"), 3);
        assert_eq!(find_match_count("abcabc", "abc"), 2);
        assert_eq!(find_match_count("hello", "xyz"), 0);
        assert_eq!(find_match_count("", "a"), 0);
    }

    #[test]
    fn tracked_text_buffer_increments_seq_on_edit() {
        let seq = Cell::new(0_u64);
        let mut text = Arc::new("hello".to_owned());
        {
            let mut buf = TrackedTextBuffer {
                text: &mut text,
                seq: &seq,
            };
            let inserted = egui::TextBuffer::insert_text(&mut buf, " world", 5);
            assert_eq!(inserted, 6);
        }
        assert_eq!(seq.get(), 1);
        assert_eq!(text.as_str(), "hello world");
    }

    #[test]
    fn tracked_text_buffer_no_op_insert_does_not_bump_seq() {
        let seq = Cell::new(0_u64);
        let mut text = Arc::new("hello".to_owned());
        {
            let mut buf = TrackedTextBuffer {
                text: &mut text,
                seq: &seq,
            };
            let inserted = egui::TextBuffer::insert_text(&mut buf, "", 5);
            assert_eq!(inserted, 0);
        }
        assert_eq!(seq.get(), 0);
    }

    #[test]
    fn tracked_text_buffer_delete_bumps_seq() {
        let seq = Cell::new(0_u64);
        let mut text = Arc::new("hello".to_owned());
        {
            let mut buf = TrackedTextBuffer {
                text: &mut text,
                seq: &seq,
            };
            egui::TextBuffer::delete_char_range(&mut buf, 2..4);
        }
        assert_eq!(seq.get(), 1);
        assert_eq!(text.as_str(), "heo");
    }

    #[test]
    fn tracked_text_buffer_empty_range_delete_no_bump() {
        let seq = Cell::new(0_u64);
        let mut text = Arc::new("hello".to_owned());
        {
            let mut buf = TrackedTextBuffer {
                text: &mut text,
                seq: &seq,
            };
            egui::TextBuffer::delete_char_range(&mut buf, 3..3);
        }
        assert_eq!(seq.get(), 0);
    }

    #[test]
    fn search_state_caches_by_query_and_seq() {
        let mut search = SearchState::with_query("a");
        assert_eq!(search.match_count("aaa", 1), 3);
        // Same query and seq should use cache.
        assert_eq!(search.match_count("aaa", 1), 3);
        // Changing seq forces recount.
        assert_eq!(search.match_count("aa", 2), 2);
        // Changing query forces recount.
        search.query = "b".to_owned();
        assert_eq!(search.match_count("bb", 2), 2);
    }

    #[test]
    fn zoom_with_factor_edge_cases() {
        assert_eq!(zoom_with_factor(1.0, -1.0), clamped_zoom_factor(1.0));
        assert_eq!(
            zoom_with_factor(1.0, f32::INFINITY),
            clamped_zoom_factor(1.0)
        );
        assert_eq!(zoom_with_factor(1.0, f32::NAN), clamped_zoom_factor(1.0));
        assert_eq!(zoom_with_factor(1.0, 0.0), clamped_zoom_factor(1.0));
    }

    #[test]
    fn clamped_zoom_factor_clamps_bounds() {
        assert_eq!(clamped_zoom_factor(0.1), MIN_ZOOM_FACTOR);
        assert_eq!(clamped_zoom_factor(10.0), MAX_ZOOM_FACTOR);
        assert_eq!(clamped_zoom_factor(1.5), 1.5);
    }

    #[test]
    fn document_stats_single_newline() {
        let stats = DocumentStats::from_text("\n");
        assert_eq!(stats.lines, 2);
    }

    #[test]
    fn replace_all_occurrences_returns_borrowed_on_no_match() {
        let (result, count) = replace_all_occurrences("hello world", "xyz", "abc");
        assert_eq!(count, 0);
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn replace_all_occurrences_empty_needle_returns_borrowed() {
        let (result, count) = replace_all_occurrences("hello", "", "abc");
        assert_eq!(count, 0);
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn default_image_uri_scheme_covers_paths_with_special_components() {
        assert_eq!(
            default_image_uri_scheme(Some(Path::new("/a/b/c/file.md"))),
            "file:///a/b/c/"
        );
        assert_eq!(
            default_image_uri_scheme(Some(Path::new("relative/file.md"))),
            "file:///relative/"
        );
    }

    #[test]
    fn set_mode_changes_mode() {
        let ctx = egui::Context::default();
        let mut app = RustdownApp::default();
        assert_eq!(app.mode, Mode::Edit);
        app.set_mode(Mode::Preview, &ctx);
        assert_eq!(app.mode, Mode::Preview);
        // Going to Preview drops editor galley cache.
        assert!(app.doc.editor_galley_cache.is_none());
        // Going back to Edit clears preview state.
        app.set_mode(Mode::Edit, &ctx);
        assert_eq!(app.mode, Mode::Edit);
    }

    #[test]
    fn set_mode_same_mode_is_noop() {
        let ctx = egui::Context::default();
        let mut app = RustdownApp::default();
        app.set_mode(Mode::Edit, &ctx);
        assert_eq!(app.mode, Mode::Edit);
        // No pending scroll should be set for same-mode transition.
        assert!(app.nav.pending_scroll.is_none());
    }

    #[test]
    fn uses_editor_returns_correct_value() {
        let app = RustdownApp::default();
        assert!(app.uses_editor()); // default mode is Edit
        let app = RustdownApp {
            mode: Mode::SideBySide,
            ..RustdownApp::default()
        };
        assert!(app.uses_editor());
        let app = RustdownApp {
            mode: Mode::Preview,
            ..RustdownApp::default()
        };
        assert!(!app.uses_editor());
    }

    #[test]
    fn resolve_nav_scroll_target_preview_sets_preview_only() {
        let ctx = egui::Context::default();
        let mut app = RustdownApp {
            mode: Mode::Preview,
            ..RustdownApp::default()
        };
        app.nav.pending_scroll = Some(nav_panel::NavScrollTarget::Top);
        app.resolve_nav_scroll_target(&ctx);
        assert_eq!(app.nav.pending_preview_scroll_y, Some(0.0));
        assert!(app.nav.pending_editor_scroll_y.is_none());
    }

    #[test]
    fn resolve_nav_scroll_target_side_by_side_sets_both_panes() {
        let ctx = egui::Context::default();
        let mut app = RustdownApp {
            mode: Mode::SideBySide,
            ..RustdownApp::default()
        };
        app.nav.pending_scroll = Some(nav_panel::NavScrollTarget::Top);
        app.resolve_nav_scroll_target(&ctx);
        assert_eq!(app.nav.pending_editor_scroll_y, Some(0.0));
        assert_eq!(app.nav.pending_preview_scroll_y, Some(0.0));
    }

    #[test]
    fn resolve_nav_scroll_target_preview_uses_heading_anchor_when_available() {
        let ctx = egui::Context::default();
        let mut app = RustdownApp {
            mode: Mode::Preview,
            ..RustdownApp::default()
        };
        let md = "# A\n\ntext\n\n## B\n";
        app.nav.outline = nav_outline::extract_headings(md);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        app.doc.preview_cache.ensure_parsed(md);
        app.doc.preview_cache.ensure_heights(14.0, 400.0, &style);
        let b_offset = app.nav.outline[1].byte_offset;
        let expected_y_opt = app.doc.preview_cache.heading_y(1);
        assert!(expected_y_opt.is_some());
        let expected_y = expected_y_opt.unwrap_or(0.0);

        app.nav.pending_scroll = Some(nav_panel::NavScrollTarget::ByteOffset(b_offset));
        app.resolve_nav_scroll_target(&ctx);

        let actual_y_opt = app.nav.pending_preview_scroll_y;
        assert!(actual_y_opt.is_some());
        let actual_y = actual_y_opt.unwrap_or(0.0);
        assert!((actual_y - expected_y).abs() < 0.01);
        assert!(app.nav.pending_editor_scroll_y.is_none());
    }

    #[test]
    fn reload_kind_flags() {
        // Verify ReloadKind semantics via apply_disk_text_state.
        let mut app = RustdownApp::default();
        let text = Arc::new("test".to_owned());
        let rev = DiskRevision {
            modified: std::time::SystemTime::UNIX_EPOCH,
            len: 4,
            #[cfg(unix)]
            dev: 0,
            #[cfg(unix)]
            inode: 0,
        };

        app.apply_disk_text_state(text.clone(), text.clone(), rev, ReloadKind::Clean);
        assert!(!app.doc.dirty);
        assert!(app.doc.last_edit_at.is_none());

        app.doc.last_edit_at = Some(Instant::now());
        app.apply_disk_text_state(text.clone(), text.clone(), rev, ReloadKind::Merged);
        assert!(app.doc.dirty);
        assert!(app.doc.last_edit_at.is_some()); // Not cleared for Merged.

        app.apply_disk_text_state(text.clone(), text, rev, ReloadKind::ConflictResolved);
        assert!(app.doc.dirty);
        assert!(app.doc.last_edit_at.is_none()); // Cleared for ConflictResolved.
    }

    #[test]
    fn load_document_resets_state() {
        let dir = make_temp_dir("rustdown-load-doc-test");
        let path = dir.join("test.md");
        fs::write(&path, "test content").ok();
        let rev = disk_io::disk_revision(&path).ok();

        let mut app = RustdownApp::default();
        app.doc.dirty = true;
        app.load_document(path.clone(), "test content".to_owned(), rev);

        assert_eq!(app.doc.path.as_deref(), Some(path.as_path()));
        assert_eq!(app.doc.text.as_str(), "test content");
        assert_eq!(app.doc.base_text.as_str(), "test content");
        assert!(!app.doc.dirty);
        assert!(!app.doc.stats_dirty);
        assert_eq!(app.doc.stats, DocumentStats::from_text("test content"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn from_launch_options_sets_mode() {
        let opts = LaunchOptions {
            mode: Mode::Preview,
            path: None,
            print_version: false,
            diagnostics: DiagnosticsMode::Off,
            diagnostics_iterations: 200,
            diagnostics_runs: 1,
        };
        let app = RustdownApp::from_launch_options(opts);
        assert_eq!(app.mode, Mode::Preview);
    }

    #[test]
    fn open_and_close_search() {
        let mut app = RustdownApp::default();
        assert!(!app.search.visible);
        assert!(!app.focus_search);

        app.open_search(false);
        assert!(app.search.visible);
        assert!(!app.search.replace_mode);
        assert!(app.focus_search);

        app.open_search(true);
        assert!(app.search.replace_mode);

        app.close_search();
        assert!(!app.search.visible);
        assert!(!app.search.replace_mode);
        assert!(!app.focus_search);
    }

    #[test]
    fn format_document_applies_formatting() {
        let mut app = RustdownApp::default();
        // Default format options insert a final newline.
        app.doc.text = Arc::new("# Hello\n\nworld".to_owned());
        let seq_before = app.doc.edit_seq;
        app.format_document();
        // Should have appended a final newline.
        assert!(app.doc.text.ends_with('\n'));
        assert!(app.doc.edit_seq > seq_before);
        assert!(app.doc.dirty);
    }

    #[test]
    fn refresh_stats_now_updates_line_count() {
        let mut app = RustdownApp::default();
        app.doc.text = Arc::new("a\nb\nc\n".to_owned());
        app.doc.stats_dirty = true;
        app.refresh_stats_now();
        assert_eq!(app.doc.stats.lines, 4);
        assert!(!app.doc.stats_dirty);
    }

    #[test]
    fn request_action_defers_when_dirty() {
        let mut app = RustdownApp::default();
        app.doc.dirty = true;
        app.request_action(PendingAction::NewBlank);
        assert!(app.pending_action.is_some());
    }

    #[test]
    fn schedule_disk_reload_sets_earliest_time() {
        let mut app = RustdownApp::default();
        let now = Instant::now();
        app.schedule_disk_reload(now);
        assert!(app.disk.pending_reload_at.is_some());
        // A second schedule at the same time should not push the reload later.
        let first = app.disk.pending_reload_at;
        app.schedule_disk_reload(now);
        assert_eq!(app.disk.pending_reload_at, first);
    }

    #[test]
    fn write_merge_sidecar_creates_file() {
        let dir = make_temp_dir("rustdown-sidecar-write-test");
        let doc_path = dir.join("test.md");
        fs::write(&doc_path, "# doc").ok();

        let mut app = RustdownApp::default();
        app.write_merge_sidecar(&doc_path, "conflict content");
        assert!(app.disk.merge_sidecar_path.is_some());
        let sidecar = app
            .disk
            .merge_sidecar_path
            .as_ref()
            .unwrap_or_else(|| unreachable!());
        assert!(sidecar.exists());
        assert_eq!(
            fs::read_to_string(sidecar).unwrap_or_default(),
            "conflict content"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bump_edit_seq_wraps() {
        let mut app = RustdownApp::default();
        let seq = app.doc.edit_seq;
        app.bump_edit_seq();
        assert_eq!(app.doc.edit_seq, seq + 1);
    }

    #[test]
    fn note_text_changed_marks_all_dirty() {
        let mut app = RustdownApp::default();
        app.note_text_changed(true);
        assert!(app.doc.dirty);
        assert!(app.doc.stats_dirty);
        assert!(app.doc.preview_dirty);
        assert!(app.doc.last_edit_at.is_some());
    }
}
