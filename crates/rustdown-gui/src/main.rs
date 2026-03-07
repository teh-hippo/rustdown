#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

#[cfg(target_arch = "wasm32")]
compile_error!("rustdown is a native desktop app; web/wasm builds are not supported.");

use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};

use eframe::egui;
use rustdown_md::MarkdownStyle;

mod app_actions;
mod app_panels;
mod app_scroll;
#[cfg(test)]
#[allow(clippy::float_cmp)]
#[allow(clippy::wildcard_imports)]
mod app_tests;
mod cli;
mod diagnostics;
mod disk;
mod document;
mod editor;
mod format;
mod highlight;
mod live_merge;
mod markdown_fence;
mod nav;
mod preferences;
mod scroll_math;
mod search;
mod ui_style;

use disk::sync::DiskSyncState;
pub(crate) use document::{Document, DocumentStats};
pub(crate) use search::{SearchState, find_match_count};

const DEBOUNCE: Duration = Duration::from_millis(150);
const DISK_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DISK_RELOAD_DEBOUNCE: Duration = Duration::from_millis(75);
const STATS_RECALC_DEBOUNCE: Duration = Duration::from_millis(120);
const ZOOM_STEP: f32 = 0.1;
const MIN_ZOOM_FACTOR: f32 = 0.5;
const MAX_ZOOM_FACTOR: f32 = 3.0;
const PANEL_EDGE_PADDING: f32 = 8.0;
const SCROLL_WHEEL_MULTIPLIER: f32 = 1.15;
const SIDE_BY_SIDE_SCROLL_LERP: f32 = 0.35;
const DIAGNOSTICS_DEFAULT_ITERATIONS: usize = 200;
const DIAGNOSTICS_DEFAULT_RUNS: usize = 1;

use cli::{DiagnosticsMode, app_version, parse_launch_options};

fn main() -> eframe::Result {
    let launch_options = parse_launch_options(std::env::args_os().skip(1));
    if launch_options.print_version {
        // On Windows, GUI-subsystem binaries have no console by default.
        // Attach to the parent console so the output is visible in
        // PowerShell / cmd.
        #[cfg(windows)]
        cli::attach_parent_console();

        println!("{}", app_version());
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    cli::apply_wsl_workarounds();

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
        if let Err(err) = nav::debug::run_nav_diagnostics(launch_options.path.as_deref()) {
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
struct PreviewStyleCache {
    style: Option<MarkdownStyle>,
    dark_mode: bool,
    colored: bool,
    image_uri: String,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Default)]
struct RustdownApp {
    doc: Document,
    mode: Mode,
    search: SearchState,
    nav: nav::panel::NavState,
    error: Option<String>,
    pending_action: Option<PendingAction>,
    last_viewport_title: String,
    focus_search: bool,
    heading_color_mode: bool,
    side_by_side_scroll_sync: bool,

    /// Zoom factor loaded from preferences, applied on first frame.
    persisted_zoom: f32,

    /// Last editor scroll byte offset observed by the side-by-side sync loop.
    last_sync_editor_byte: Option<usize>,
    /// Last preview scroll byte offset observed by the side-by-side sync loop.
    last_sync_preview_byte: Option<usize>,
    /// Set to `true` when `resolve_nav_scroll_target` applies a target this
    /// frame; prevents `sync_side_by_side_scroll` from overriding it.
    nav_scroll_applied_this_frame: bool,
    /// Current side-by-side sync source while a follower pane is animating.
    side_by_side_scroll_source: Option<SideBySideScrollSource>,
    /// Target scroll Y for the follower pane in `SideBySide` mode.
    side_by_side_scroll_target: Option<f32>,

    /// Cached preview style; rebuilt only when theme/colour-mode/URI changes.
    preview_style_cache: PreviewStyleCache,

    disk: DiskSyncState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Edit,
    Preview,
    SideBySide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SideBySideScrollSource {
    Editor,
    Preview,
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

    #[must_use]
    const fn as_str(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Preview => "preview",
            Self::SideBySide => "sidebyside",
        }
    }

    #[must_use]
    fn from_str_lossy(s: &str) -> Option<Self> {
        match s {
            "edit" => Some(Self::Edit),
            "preview" => Some(Self::Preview),
            "sidebyside" => Some(Self::SideBySide),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
enum PendingAction {
    NewBlank,
    Open(PathBuf),
    OpenBundled(BundledDoc),
}

#[derive(Clone, Copy, Debug)]
enum BundledDoc {
    Demo,
    Verification,
}

impl BundledDoc {
    const fn content(self) -> &'static str {
        match self {
            Self::Demo => include_str!("bundled/demo.md"),
            Self::Verification => include_str!("bundled/verification.md"),
        }
    }
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Apply persisted zoom on the first frame (needs ctx to be available).
        if self.persisted_zoom != 0.0 {
            ctx.set_zoom_factor(clamped_zoom_factor(self.persisted_zoom));
            self.persisted_zoom = 0.0;
        }
        self.tick_disk_sync(ctx);
        self.refresh_stats_if_due(ctx);
        self.handle_keyboard_shortcuts(ctx);
        self.show_status_bar(ctx);
        if self.search.visible {
            self.show_search_bar(ctx);
        }
        self.show_toolbar(ctx);
        self.show_content_panels(ctx);
        self.show_dialogs(ctx);
        self.show_disk_conflict_dialog(ctx);
        self.update_viewport_title(ctx);
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
