#![forbid(unsafe_code)]
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
    fs, io,
    path::{Component, Path, PathBuf},
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
mod markdown_fence;

use disk_io::{
    DiskRevision, atomic_write_utf8, disk_revision, next_merge_sidecar_path, read_stable_utf8,
};
use live_merge::{Merge3Outcome, merge_three_way};

const DEBOUNCE: Duration = Duration::from_millis(150);
const DISK_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DISK_RELOAD_DEBOUNCE: Duration = Duration::from_millis(75);
const STATS_RECALC_DEBOUNCE: Duration = Duration::from_millis(120);
const ZOOM_STEP: f32 = 0.1;
const MIN_ZOOM_FACTOR: f32 = 0.5;
const MAX_ZOOM_FACTOR: f32 = 3.0;
const READING_SPEED_WPM: usize = 200;
const FONT_SIZE_DELTA: f32 = 2.0;
const SMALL_FONT_SIZE_DELTA: f32 = 1.0;
const PANEL_EDGE_PADDING: f32 = 8.0;
const DIAGNOSTICS_DEFAULT_ITERATIONS: usize = 200;
const DIAGNOSTICS_DEFAULT_RUNS: usize = 1;
const UI_FONT_NAME: &str = "rustdown-ui-font";
#[cfg(target_os = "linux")]
const UI_FONT_CANDIDATE_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf",
];
#[cfg(target_os = "linux")]
const UI_FONT_FALLBACK_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/noto/NotoEmoji-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
];
#[cfg(target_os = "macos")]
const UI_FONT_CANDIDATE_PATHS: &[&str] = &[
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/Library/Fonts/Arial.ttf",
];
#[cfg(target_os = "macos")]
const UI_FONT_FALLBACK_PATHS: &[&str] = &[
    "/System/Library/Fonts/Apple Color Emoji.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/Supplemental/Symbol.ttf",
];
#[cfg(target_os = "windows")]
const UI_FONT_CANDIDATE_PATHS: &[&str] = &[
    r"C:\Windows\Fonts\segoeui.ttf",
    r"C:\Windows\Fonts\arial.ttf",
];
#[cfg(target_os = "windows")]
const UI_FONT_FALLBACK_PATHS: &[&str] = &[
    r"C:\Windows\Fonts\seguiemj.ttf",
    r"C:\Windows\Fonts\seguisym.ttf",
    r"C:\Windows\Fonts\arialuni.ttf",
];
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const UI_FONT_CANDIDATE_PATHS: &[&str] = &[];
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const UI_FONT_FALLBACK_PATHS: &[&str] = &[];

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
fn app_version() -> &'static str {
    option_env!("RUSTDOWN_BUILD_VERSION")
        .filter(|value| !value.is_empty())
        .unwrap_or(env!("CARGO_PKG_VERSION"))
}

fn main() -> eframe::Result {
    let launch_options = parse_launch_options(std::env::args_os().skip(1));
    if launch_options.print_version {
        println!("{}", app_version());
        return Ok(());
    }
    if launch_options.diagnostics == DiagnosticsMode::OpenPipeline {
        for run in 0..launch_options.diagnostics_runs {
            if launch_options.diagnostics_runs > 1 {
                println!(
                    "diagnostics_run={}/{}",
                    run + 1,
                    launch_options.diagnostics_runs
                );
            }
            if let Err(err) = run_open_pipeline_diagnostics(
                launch_options.path.as_deref(),
                launch_options.diagnostics_iterations,
            ) {
                eprintln!("Diagnostics failed: {err}");
                break;
            }
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
            configure_single_font(&cc.egui_ctx).map_err(std::io::Error::other)?;
            configure_ui_style(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}

fn avg_duration_us(total: Duration, iterations: usize) -> f64 {
    total.as_secs_f64() * 1_000_000.0 / iterations.max(1) as f64
}

fn measure_iterations(iterations: usize, mut f: impl FnMut()) -> Duration {
    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    start.elapsed()
}

#[must_use]
fn diagnostics_raw_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1024.0, 768.0),
        )),
        ..Default::default()
    }
}

fn estimate_text_heap_bytes(text: &Arc<String>, base_text: &Arc<String>) -> usize {
    text.capacity()
        + if Arc::ptr_eq(text, base_text) {
            0
        } else {
            base_text.capacity()
        }
}

fn run_open_pipeline_diagnostics(
    path: Option<&Path>,
    diagnostics_iterations: usize,
) -> io::Result<()> {
    let Some(path) = path else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing markdown path (usage: rustdown --diagnostics-open <file.md>)",
        ));
    };
    let diagnostics_iterations = diagnostics_iterations.max(1);

    let total_start = Instant::now();

    let read_start = Instant::now();
    let (text, disk_rev) = read_stable_utf8(path)?;
    let read_ms = read_start.elapsed();

    // Simulate the app's clean-document state (text + base_text) so we can measure
    // the real open cost and memory footprint.
    let text = Arc::new(text);
    let clone_start = Instant::now();
    let base_text = std::hint::black_box(text.clone());
    let clone_ms = clone_start.elapsed();

    let stats_start = Instant::now();
    let stats = DocumentStats::from_text(std::hint::black_box(text.as_str()));
    let stats_ms = stats_start.elapsed();

    let html_start = Instant::now();
    let html = std::hint::black_box(markdown_to_html_document(std::hint::black_box(
        text.as_str(),
    )));
    let html_ms = html_start.elapsed();

    let cache_start = Instant::now();
    let md_cache = std::hint::black_box(CommonMarkCache::default());
    let cache_ms = cache_start.elapsed();

    let egui_start = Instant::now();
    let ctx = egui::Context::default();
    configure_single_font(&ctx).map_err(io::Error::other)?;
    configure_ui_style(&ctx);
    // egui only guarantees fonts are available after the first frame has run.
    let _ = ctx.run(egui::RawInput::default(), |_ctx| {});
    let egui_ms = egui_start.elapsed();

    let style = ctx.style();
    let highlight_job_start = Instant::now();
    let job = std::hint::black_box(highlight::markdown_layout_job(
        style.as_ref(),
        &style.visuals,
        std::hint::black_box(text.as_str()),
        false,
    ));
    let highlight_job_ms = highlight_job_start.elapsed();

    let highlight_layout_start = Instant::now();
    let galley = std::hint::black_box(ctx.fonts(|fonts| fonts.layout_job(job)));
    let highlight_layout_ms = highlight_layout_start.elapsed();

    let make_document = |image_uri_scheme: String,
                         text: Arc<String>,
                         base_text: Arc<String>,
                         edit_seq: u64,
                         md_cache: CommonMarkCache|
     -> Document {
        Document {
            path: Some(path.to_path_buf()),
            image_uri_scheme,
            text,
            base_text,
            disk_rev: Some(disk_rev),
            stats,
            stats_dirty: false,
            preview_dirty: false,
            dirty: false,
            md_cache,
            last_edit_at: None,
            edit_seq,
            editor_galley_cache: None,
        }
    };
    let make_app = |mode: Mode,
                    image_uri_scheme: String,
                    text: Arc<String>,
                    base_text: Arc<String>,
                    edit_seq: u64,
                    md_cache: CommonMarkCache|
     -> RustdownApp {
        RustdownApp {
            mode,
            doc: make_document(image_uri_scheme, text, base_text, edit_seq, md_cache),
            ..Default::default()
        }
    };

    let mut app = make_app(
        Mode::Edit,
        default_image_uri_scheme(Some(path)),
        text,
        base_text,
        0,
        md_cache,
    );
    let raw = diagnostics_raw_input();

    let editor_frame1_start = Instant::now();
    let _ = ctx.run(raw.clone(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
    });
    let editor_frame1_ms = editor_frame1_start.elapsed();

    let editor_frame2_start = Instant::now();
    let _ = ctx.run(raw, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
    });
    let editor_frame2_ms = editor_frame2_start.elapsed();
    let core_total_ms = total_start.elapsed();

    let mut preview_app = make_app(
        Mode::Preview,
        app.doc.image_uri_scheme.clone(),
        app.doc.text.clone(),
        app.doc.base_text.clone(),
        app.doc.edit_seq,
        CommonMarkCache::default(),
    );
    let raw = diagnostics_raw_input();
    let preview_frame1_start = Instant::now();
    let _ = ctx.run(raw.clone(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| preview_app.show_preview(ui));
    });
    let preview_frame1_ms = preview_frame1_start.elapsed();
    let preview_frame2_start = Instant::now();
    let _ = ctx.run(raw, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| preview_app.show_preview(ui));
    });
    let preview_frame2_ms = preview_frame2_start.elapsed();
    let frame_iterations = diagnostics_iterations.min(120);
    let editor_cached_loop = measure_iterations(frame_iterations, || {
        let _ = ctx.run(diagnostics_raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
        });
    });
    let preview_cached_loop = measure_iterations(frame_iterations, || {
        let _ = ctx.run(diagnostics_raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| preview_app.show_preview(ui));
        });
    });

    let stats_loop = measure_iterations(diagnostics_iterations, || {
        std::hint::black_box(DocumentStats::from_text(std::hint::black_box(
            app.doc.text.as_str(),
        )));
    });
    let html_loop = measure_iterations(diagnostics_iterations, || {
        std::hint::black_box(markdown_to_html_document(std::hint::black_box(
            app.doc.text.as_str(),
        )));
    });
    let highlight_job_loop = measure_iterations(diagnostics_iterations, || {
        std::hint::black_box(highlight::markdown_layout_job(
            style.as_ref(),
            &style.visuals,
            std::hint::black_box(app.doc.text.as_str()),
            false,
        ));
    });
    let highlight_layout_loop = measure_iterations(diagnostics_iterations, || {
        let loop_job = highlight::markdown_layout_job(
            style.as_ref(),
            &style.visuals,
            std::hint::black_box(app.doc.text.as_str()),
            false,
        );
        let loop_galley = ctx.fonts(|fonts| fonts.layout_job(loop_job));
        std::hint::black_box(loop_galley.rows.len());
    });
    let search_query = "az";
    let search_count_loop = measure_iterations(diagnostics_iterations, || {
        std::hint::black_box(find_match_count(
            std::hint::black_box(app.doc.text.as_str()),
            std::hint::black_box(search_query),
        ));
    });
    let mut search_state = SearchState {
        query: search_query.to_owned(),
        ..Default::default()
    };
    let search_cached_loop = measure_iterations(diagnostics_iterations, || {
        std::hint::black_box(search_state.match_count(app.doc.text.as_str(), app.doc.edit_seq));
    });

    let image_uri_recompute = measure_iterations(diagnostics_iterations, || {
        std::hint::black_box(default_image_uri_scheme(Some(path)));
    });
    let cached_image_uri = app.doc.image_uri_scheme.as_str();
    let image_uri_cached = measure_iterations(diagnostics_iterations, || {
        std::hint::black_box(cached_image_uri);
    });

    let new_edit_bench_app = || {
        let text = Arc::new(app.doc.text.as_str().to_owned());
        make_app(
            Mode::Edit,
            default_image_uri_scheme(Some(path)),
            text.clone(),
            text,
            0,
            CommonMarkCache::default(),
        )
    };
    let edit_iterations = diagnostics_iterations.min(256);
    let mut edit_bench_app = new_edit_bench_app();
    let edit_deferred_loop = measure_iterations(edit_iterations, || {
        Arc::make_mut(&mut edit_bench_app.doc.text).push('x');
        edit_bench_app.bump_edit_seq();
        edit_bench_app.note_text_changed(true);
    });

    let mut edit_immediate_bench_app = new_edit_bench_app();
    let edit_immediate_loop = measure_iterations(edit_iterations, || {
        Arc::make_mut(&mut edit_immediate_bench_app.doc.text).push('x');
        edit_immediate_bench_app.bump_edit_seq();
        edit_immediate_bench_app.note_text_changed(false);
    });

    let clean_text_heap_bytes = estimate_text_heap_bytes(&app.doc.text, &app.doc.base_text);
    let mut dirty_text = app.doc.text.clone();
    Arc::make_mut(&mut dirty_text).push('x');
    let dirty_text_heap_bytes = estimate_text_heap_bytes(&dirty_text, &app.doc.base_text);

    let total_ms = total_start.elapsed();
    macro_rules! metric {
        ($name:literal, $value:expr) => {
            println!(concat!($name, "={}"), $value);
        };
    }
    macro_rules! avg_metric {
        ($name:literal, $duration:expr, $iterations:expr) => {
            println!(
                concat!($name, "={:.2}"),
                avg_duration_us($duration, $iterations)
            );
        };
    }

    println!("rustdown_diagnostics=open_pipeline");
    metric!("path", path.display());
    metric!("disk_len", disk_rev.len);
    metric!("text_bytes", app.doc.text.len());
    metric!("base_text_bytes", app.doc.base_text.len());
    println!(
        "stats_words={} stats_chars={} stats_lines={}",
        stats.words, stats.chars, stats.lines
    );
    metric!("t_read_ms", read_ms.as_millis());
    metric!("t_clone_base_ms", clone_ms.as_millis());
    metric!("t_stats_ms", stats_ms.as_millis());
    metric!("t_html_ms", html_ms.as_millis());
    metric!("html_bytes", html.len());
    metric!("t_md_cache_ms", cache_ms.as_millis());
    metric!("t_egui_setup_ms", egui_ms.as_millis());
    metric!("t_highlight_job_ms", highlight_job_ms.as_millis());
    metric!("t_highlight_layout_ms", highlight_layout_ms.as_millis());
    metric!("galley_rows", galley.rows.len());
    metric!("t_editor_frame1_ms", editor_frame1_ms.as_millis());
    metric!("t_editor_frame2_ms", editor_frame2_ms.as_millis());
    metric!("t_core_total_ms", core_total_ms.as_millis());
    metric!("t_preview_frame1_ms", preview_frame1_ms.as_millis());
    metric!("t_preview_frame2_ms", preview_frame2_ms.as_millis());
    metric!("diag_iterations", diagnostics_iterations);
    metric!("diag_edit_iterations", edit_iterations);
    metric!("diag_frame_iterations", frame_iterations);
    avg_metric!("t_stats_loop_avg_us", stats_loop, diagnostics_iterations);
    avg_metric!("t_html_loop_avg_us", html_loop, diagnostics_iterations);
    avg_metric!(
        "t_highlight_job_loop_avg_us",
        highlight_job_loop,
        diagnostics_iterations
    );
    avg_metric!(
        "t_highlight_layout_loop_avg_us",
        highlight_layout_loop,
        diagnostics_iterations
    );
    avg_metric!(
        "t_search_count_loop_avg_us",
        search_count_loop,
        diagnostics_iterations
    );
    avg_metric!(
        "t_search_cached_count_loop_avg_us",
        search_cached_loop,
        diagnostics_iterations
    );
    avg_metric!(
        "t_image_uri_recompute_avg_us",
        image_uri_recompute,
        diagnostics_iterations
    );
    avg_metric!(
        "t_image_uri_cached_lookup_avg_us",
        image_uri_cached,
        diagnostics_iterations
    );
    avg_metric!(
        "t_edit_note_change_deferred_avg_us",
        edit_deferred_loop,
        edit_iterations
    );
    avg_metric!(
        "t_edit_note_change_immediate_avg_us",
        edit_immediate_loop,
        edit_iterations
    );
    avg_metric!(
        "t_editor_cached_frame_avg_us",
        editor_cached_loop,
        frame_iterations
    );
    avg_metric!(
        "t_preview_cached_frame_avg_us",
        preview_cached_loop,
        frame_iterations
    );
    avg_metric!(
        "t_edit_note_change_avg_us",
        edit_deferred_loop,
        edit_iterations
    );
    metric!("text_heap_clean_bytes", clean_text_heap_bytes);
    metric!("text_heap_dirty_bytes", dirty_text_heap_bytes);
    metric!("t_total_ms", total_ms.as_millis());

    std::hint::black_box(app);
    Ok(())
}

fn configure_single_font(ctx: &egui::Context) -> Result<(), String> {
    let primary_font_data = load_single_font()?;
    let primary_font_name = UI_FONT_NAME.to_owned();
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.clear();
    fonts.families.clear();
    fonts.font_data.insert(
        primary_font_name.clone(),
        Arc::new(egui::FontData::from_owned(primary_font_data)),
    );
    let mut proportional = vec![primary_font_name.clone()];
    let mut monospace = vec![primary_font_name];
    append_font_fallbacks(
        &mut fonts,
        &mut proportional,
        &mut monospace,
        UI_FONT_FALLBACK_PATHS,
    );
    fonts
        .families
        .insert(egui::FontFamily::Proportional, proportional);
    fonts
        .families
        .insert(egui::FontFamily::Monospace, monospace);
    ctx.set_fonts(fonts);
    Ok(())
}

fn append_font_fallbacks(
    fonts: &mut egui::FontDefinitions,
    proportional: &mut Vec<String>,
    monospace: &mut Vec<String>,
    paths: &[&str],
) -> usize {
    let mut loaded = 0usize;
    for path in paths {
        let Ok(data) = fs::read(path) else {
            continue;
        };
        let name = format!("{UI_FONT_NAME}-fallback-{loaded}");
        fonts
            .font_data
            .insert(name.clone(), Arc::new(egui::FontData::from_owned(data)));
        proportional.push(name.clone());
        monospace.push(name);
        loaded += 1;
    }
    loaded
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
fn default_image_uri_scheme(path: Option<&Path>) -> String {
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
    heading_color_mode: bool,

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

struct Document {
    path: Option<PathBuf>,
    image_uri_scheme: String,
    text: Arc<String>,
    base_text: Arc<String>,
    disk_rev: Option<DiskRevision>,
    stats: DocumentStats,
    stats_dirty: bool,
    preview_dirty: bool,
    dirty: bool,
    md_cache: CommonMarkCache,
    last_edit_at: Option<Instant>,
    edit_seq: u64,
    editor_galley_cache: Option<EditorGalleyCache>,
}

impl Default for Document {
    fn default() -> Self {
        let text = Arc::new(String::new());
        Self {
            path: None,
            image_uri_scheme: default_image_uri_scheme(None),
            text: text.clone(),
            base_text: text,
            disk_rev: None,
            stats: DocumentStats::default(),
            stats_dirty: false,
            preview_dirty: false,
            dirty: false,
            md_cache: CommonMarkCache::default(),
            last_edit_at: None,
            edit_seq: 0,
            editor_galley_cache: None,
        }
    }
}

#[derive(Clone)]
struct EditorGalleyCache {
    seq: u64,
    wrap_width_bits: u32,
    zoom_factor_bits: u32,
    heading_color_mode: bool,
    galley: Arc<egui::Galley>,
}

struct TrackedTextBuffer<'a, 'b> {
    text: &'a mut Arc<String>,
    seq: &'b Cell<u64>,
}

impl<'a, 'b> egui::TextBuffer for TrackedTextBuffer<'a, 'b> {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        self.text.as_str()
    }

    fn insert_text(&mut self, text: &str, char_index: usize) -> usize {
        let inserted = egui::TextBuffer::insert_text(Arc::make_mut(self.text), text, char_index);
        if inserted != 0 {
            self.seq.set(self.seq.get().wrapping_add(1));
        }
        inserted
    }

    fn delete_char_range(&mut self, char_range: std::ops::Range<usize>) {
        if char_range.start < char_range.end {
            self.seq.set(self.seq.get().wrapping_add(1));
        }
        egui::TextBuffer::delete_char_range(Arc::make_mut(self.text), char_range);
    }
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
        if text.is_ascii() {
            let mut words = 0;
            let mut lines = 1;
            let mut in_word = false;
            for &byte in text.as_bytes() {
                if byte == b'\n' {
                    lines += 1;
                }
                if byte.is_ascii_whitespace() {
                    in_word = false;
                } else if !in_word {
                    words += 1;
                    in_word = true;
                }
            }
            return Self {
                words,
                chars: text.len(),
                lines,
            };
        }

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
    match_count_query: String,
    match_count_seq: u64,
    match_count: usize,
}

impl SearchState {
    fn match_count(&mut self, haystack: &str, haystack_seq: u64) -> usize {
        if self.match_count_seq == haystack_seq && self.match_count_query == self.query {
            return self.match_count;
        }

        let count = find_match_count(haystack, self.query.as_str());
        self.match_count_query.clear();
        self.match_count_query.push_str(self.query.as_str());
        self.match_count_seq = haystack_seq;
        self.match_count = count;
        count
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveTrigger {
    Save,
    SaveAs,
}

#[must_use]
fn save_trigger_from_shortcut(command: bool, shift: bool, key_s: bool) -> Option<SaveTrigger> {
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
fn clamped_zoom_factor(zoom_factor: f32) -> f32 {
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
        self.tick_disk_sync(ctx);
        self.refresh_stats_if_due(ctx);

        let dialog_open = self.pending_action.is_some() || self.disk_conflict.is_some();
        let (
            dropped_path,
            open,
            save_trigger,
            new_doc,
            cycle_mode,
            search,
            replace_all_mode,
            format_doc,
            export_html,
            zoom_in,
            zoom_out,
            zoom_delta,
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
                save_trigger_from_shortcut(cmd, i.modifiers.shift, i.key_pressed(egui::Key::S)),
                cmd && i.key_pressed(egui::Key::N),
                cmd && i.key_pressed(egui::Key::Enter),
                cmd && !i.modifiers.shift && !i.modifiers.alt && i.key_pressed(egui::Key::F),
                cmd && i.modifiers.shift && !i.modifiers.alt && i.key_pressed(egui::Key::F),
                cmd && i.modifiers.alt && !i.modifiers.shift && i.key_pressed(egui::Key::F),
                cmd && i.key_pressed(egui::Key::E),
                cmd && i.key_pressed(egui::Key::Equals),
                cmd && i.key_pressed(egui::Key::Minus),
                i.zoom_delta(),
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
            if let Some(save_trigger) = save_trigger {
                let _ = self.save_doc(matches!(save_trigger, SaveTrigger::SaveAs));
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
            if (zoom_delta - 1.0).abs() > f32::EPSILON {
                self.adjust_zoom_factor(ctx, zoom_delta);
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
                if ui
                    .toggle_value(&mut self.heading_color_mode, "Color headings (exp)")
                    .changed()
                {
                    self.doc.editor_galley_cache = None;
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
    fn clear_disk_watcher(&mut self) {
        self.disk_watcher = None;
        self.disk_watch_root = None;
        self.disk_watch_target_name = None;
        self.disk_watch_rx = None;
    }

    fn schedule_disk_reload(&mut self, now: Instant) {
        let due_at = now + DISK_RELOAD_DEBOUNCE;
        if self
            .pending_disk_reload_at
            .is_none_or(|existing| existing > due_at)
        {
            self.pending_disk_reload_at = Some(due_at);
        }
    }

    fn apply_disk_text_state(
        &mut self,
        text: Arc<String>,
        base_text: Arc<String>,
        disk_rev: DiskRevision,
        dirty: bool,
        clear_last_edit: bool,
    ) {
        self.doc.text = text;
        self.doc.base_text = base_text;
        self.doc.disk_rev = Some(disk_rev);
        self.bump_edit_seq();
        self.doc.stats = DocumentStats::from_text(self.doc.text.as_str());
        self.doc.stats_dirty = false;
        self.doc.md_cache.clear_scrollable();
        self.doc.preview_dirty = false;
        self.doc.dirty = dirty;
        if clear_last_edit {
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
        self.disk_conflict = Some(DiskConflict {
            disk_text,
            disk_rev,
            conflict_marked,
            ours_wins,
        });
    }

    fn reset_disk_sync_state(&mut self) {
        self.disk_reload_nonce = self.disk_reload_nonce.wrapping_add(1);
        self.disk_poll_at = None;
        self.pending_disk_reload_at = None;
        self.disk_reload_in_flight = false;
        self.disk_conflict = None;
        self.clear_disk_watcher();
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
        let Some(rx) = self.disk_watch_rx.as_ref() else {
            return false;
        };

        let target_name = self.disk_watch_target_name.as_deref();
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
            self.schedule_disk_reload(now);
        }

        if self.disk_watcher.is_none() && !self.disk_reload_in_flight {
            match self.disk_poll_at {
                Some(next) if now < next => {}
                _ => {
                    self.disk_poll_at = Some(now + DISK_POLL_INTERVAL);

                    match disk_revision(path.as_path()) {
                        Ok(rev) if Some(rev) != self.doc.disk_rev => self.schedule_disk_reload(now),
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
        loop {
            let recv = match self.disk_read_rx.as_ref() {
                Some(rx) => rx.try_recv(),
                None => return,
            };
            match recv {
                Ok(msg) => {
                    if msg.nonce != self.disk_reload_nonce {
                        continue;
                    }
                    if self.doc.path.as_deref() != Some(msg.path.as_path()) {
                        continue;
                    }
                    self.disk_reload_in_flight = false;

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
                                false,
                                true,
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
                                true,
                                false,
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
                    self.disk_reload_in_flight = false;
                    self.disk_read_rx = None;
                    self.disk_read_tx = None;
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
            self.apply_disk_text_state(disk_text.clone(), disk_text, disk_rev, false, true);
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
                    true,
                    false,
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

        if mode == Mode::Preview {
            // Preview doesn't render the editor; drop any cached galley to reduce memory.
            self.doc.editor_galley_cache = None;
        }

        if mode == Mode::Edit {
            self.doc.md_cache.clear_scrollable();
            self.doc.preview_dirty = false;
            self.doc.last_edit_at = None;
        }
    }

    fn adjust_zoom(&self, ctx: &egui::Context, delta: f32) {
        ctx.set_zoom_factor(zoom_with_step(ctx.zoom_factor(), delta));
    }

    fn adjust_zoom_factor(&self, ctx: &egui::Context, factor: f32) {
        ctx.set_zoom_factor(zoom_with_factor(ctx.zoom_factor(), factor));
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

    fn bump_edit_seq(&mut self) {
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

            let mut layouter = |ui: &egui::Ui, string: &str, wrap_width: f32| {
                let seq = seq.get();
                let wrap_width_bits = wrap_width.to_bits();
                let zoom_factor_bits = ui.ctx().zoom_factor().to_bits();

                if let Some(cache) = editor_galley_cache.as_ref()
                    && cache.seq == seq
                    && cache.wrap_width_bits == wrap_width_bits
                    && cache.zoom_factor_bits == zoom_factor_bits
                    && cache.heading_color_mode == heading_color_mode
                {
                    return cache.galley.clone();
                }

                let mut job = highlight::markdown_layout_job(
                    ui.style(),
                    ui.visuals(),
                    string,
                    heading_color_mode,
                );
                job.wrap.max_width = wrap_width;
                let galley = ui.fonts(|fonts| fonts.layout_job(job));
                *editor_galley_cache = Some(EditorGalleyCache {
                    seq,
                    wrap_width_bits,
                    zoom_factor_bits,
                    heading_color_mode,
                    galley: galley.clone(),
                });
                galley
            };

            let editor_size = ui.available_size();
            let response = egui::ScrollArea::both()
                .auto_shrink([false; 2])
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
            self.doc.md_cache.clear_scrollable();
            self.doc.preview_dirty = false;
        }
        CommonMarkViewer::new()
            .default_implicit_uri_scheme(self.doc.image_uri_scheme.as_str())
            .show_scrollable(
                "preview_markdown",
                ui,
                &mut self.doc.md_cache,
                self.doc.text.as_str(),
            );
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
        self.doc = Document {
            path: Some(path.clone()),
            image_uri_scheme: default_image_uri_scheme(Some(path.as_path())),
            stats: DocumentStats::from_text(text.as_str()),
            text,
            base_text,
            disk_rev,
            stats_dirty: false,
            preview_dirty: false,
            dirty: false,
            md_cache: CommonMarkCache::default(),
            last_edit_at: None,
            edit_seq: 0,
            editor_galley_cache: None,
        };
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
                self.error.get_or_insert(format!("Open failed: {err}"));
            }
        }
    }

    fn save_path_choice(&self, save_as: bool) -> Option<(PathBuf, bool)> {
        if !save_as && let Some(path) = self.doc.path.clone() {
            return Some((path, false));
        }
        markdown_file_dialog().save_file().map(|path| (path, true))
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
        let Some((path, update_doc_path)) = self.save_path_choice(save_as) else {
            return false;
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

    fn write_merge_sidecar(&mut self, doc_path: &Path, conflict_marked: &str) {
        let sidecar_path = match next_merge_sidecar_path(doc_path) {
            Ok(path) => path,
            Err(err) => {
                self.error
                    .get_or_insert(format!("Merge file path failed: {err}"));
                return;
            }
        };
        match atomic_write_utf8(&sidecar_path, conflict_marked) {
            Ok(()) => self.merge_sidecar_path = Some(sidecar_path),
            Err(err) => {
                self.error
                    .get_or_insert(format!("Merge file write failed: {err}"));
            }
        }
    }

    fn apply_conflict_choice(&mut self, choice: ConflictChoice) {
        let Some(conflict) = self.disk_conflict.take() else {
            return;
        };

        match choice {
            ConflictChoice::OpenConflictMerge => {
                self.apply_disk_text_state(
                    Arc::new(conflict.conflict_marked),
                    Arc::new(conflict.disk_text),
                    conflict.disk_rev,
                    true,
                    false,
                );
            }
            ConflictChoice::KeepMineWriteSidecar => {
                let conflict_marked = conflict.conflict_marked;
                self.apply_disk_text_state(
                    Arc::new(conflict.ours_wins),
                    Arc::new(conflict.disk_text),
                    conflict.disk_rev,
                    true,
                    false,
                );
                if let Some(doc_path) = self.doc.path.clone() {
                    self.write_merge_sidecar(doc_path.as_path(), conflict_marked.as_str());
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
                let disk_text = Arc::new(conflict.disk_text);
                self.apply_disk_text_state(
                    disk_text.clone(),
                    disk_text,
                    conflict.disk_rev,
                    false,
                    false,
                );
            }
            ConflictChoice::OverwriteDisk => {
                let Some(path) = self.doc.path.as_deref() else {
                    self.disk_conflict = Some(conflict);
                    return;
                };

                match atomic_write_utf8(path, self.doc.text.as_str()) {
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

    fn disk_conflict(app: &RustdownApp) -> &DiskConflict {
        assert!(
            app.disk_conflict.is_some(),
            "Expected conflict prompt to be set"
        );
        app.disk_conflict.as_ref().unwrap_or_else(|| unreachable!())
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
    }

    #[test]
    fn document_stats_cover_empty_populated_and_default_document() {
        let stats = DocumentStats::from_text("one two\nthree");
        assert_eq!(stats.words, 3);
        assert_eq!(stats.chars, 13);
        assert_eq!(stats.lines, 2);
        assert_eq!(stats.reading_minutes(), 1);

        let unicode_stats = DocumentStats::from_text("héllo 世界\n🙂");
        assert_eq!(unicode_stats.words, 3);
        assert_eq!(unicode_stats.chars, 10);
        assert_eq!(unicode_stats.lines, 2);

        let empty_stats = DocumentStats::from_text("");
        assert_eq!(empty_stats, DocumentStats::default());
        assert_eq!(empty_stats.reading_minutes(), 0);

        let doc = Document::default();
        assert_eq!(doc.stats(), DocumentStats::from_text(""));
    }

    #[test]
    fn markdown_path_helpers_cover_detection_selection_and_html_name() {
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
        assert_eq!(
            suggested_html_file_name(Some(Path::new("/tmp/readme.md"))),
            "readme.html"
        );
        assert_eq!(suggested_html_file_name(None), "document.html");
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
    fn append_font_fallbacks_loads_existing_files_only() {
        let dir = make_temp_dir("rustdown-font-fallback-test");
        let existing = dir.join("emoji.ttf");
        let missing = dir.join("missing.ttf");
        assert!(fs::write(&existing, b"not-a-real-font").is_ok());

        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.clear();
        fonts.families.clear();
        let mut proportional = vec!["primary".to_owned()];
        let mut monospace = vec!["primary".to_owned()];

        let existing_path = existing.to_string_lossy().to_string();
        let missing_path = missing.to_string_lossy().to_string();
        let loaded = append_font_fallbacks(
            &mut fonts,
            &mut proportional,
            &mut monospace,
            &[existing_path.as_str(), missing_path.as_str()],
        );

        assert_eq!(loaded, 1);
        assert_eq!(proportional.len(), 2);
        assert_eq!(monospace.len(), 2);
        assert!(
            fonts
                .font_data
                .contains_key(&format!("{UI_FONT_NAME}-fallback-0"))
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

        let mut search = SearchState {
            query: "alpha".to_owned(),
            ..Default::default()
        };
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

        app.doc.last_edit_at = Some(Instant::now() - STATS_RECALC_DEBOUNCE);
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
            assert_eq!(app.disk_conflict.is_some(), case.expect_conflict);
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
        assert!(app.disk_conflict.is_none());
    }

    #[test]
    fn conflict_choice_keep_mine_writes_sidecar_and_applies_safe_disk_edits() {
        let dir = make_temp_dir("rustdown-merge-test");
        let original = dir.join("note.md");

        let _ = atomic_write_utf8(&original, "line1\nline2\nline3\n");

        let mut app = merge_app("line1\nline2\nline3\n", "line1\nO2\nline3\n", 1, 18, true);
        app.doc.path = Some(original.clone());

        app.incorporate_disk_text("line1\nT2\nT3\n".to_owned(), test_rev(2, 15));
        let conflict = disk_conflict(&app);
        let expected_sidecar = conflict.conflict_marked.clone();
        let expected_ours_wins = conflict.ours_wins.clone();

        app.apply_conflict_choice(ConflictChoice::KeepMineWriteSidecar);

        assert_eq!(app.doc.text.as_str(), expected_ours_wins.as_str());
        assert_eq!(app.doc.base_text.as_str(), "line1\nT2\nT3\n");
        assert_eq!(app.doc.disk_rev, Some(test_rev(2, 15)));
        assert!(app.disk_conflict.is_none());

        assert!(
            app.merge_sidecar_path.is_some(),
            "Expected merge sidecar path to be set"
        );
        let sidecar_path = app
            .merge_sidecar_path
            .clone()
            .unwrap_or_else(|| unreachable!());
        let sidecar_text = read_file(&sidecar_path);
        assert_eq!(sidecar_text, expected_sidecar);

        let _ = fs::remove_dir_all(&dir);
    }
}
