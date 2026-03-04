use std::{
    io,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui;
use egui_commonmark::CommonMarkCache;

use crate::disk_io::read_stable_utf8;
use crate::highlight;
use crate::ui_style;
use crate::{
    Document, DocumentStats, Mode, RustdownApp, SearchState, default_image_uri_scheme,
    find_match_count,
};

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
pub(crate) fn diagnostics_raw_input() -> egui::RawInput {
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

pub(crate) fn run_open_pipeline_diagnostics(
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

    let cache_start = Instant::now();
    let md_cache = std::hint::black_box(CommonMarkCache::default());
    let cache_ms = cache_start.elapsed();

    let egui_start = Instant::now();
    let ctx = egui::Context::default();
    ui_style::configure_fonts(&ctx).map_err(io::Error::other)?;
    ui_style::configure_style(&ctx);
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
    let galley = std::hint::black_box(ctx.fonts_mut(|fonts| fonts.layout_job(job)));
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
        let loop_galley = ctx.fonts_mut(|fonts| fonts.layout_job(loop_job));
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
    metric!("stats_lines", stats.lines);
    metric!("t_read_ms", read_ms.as_millis());
    metric!("t_clone_base_ms", clone_ms.as_millis());
    metric!("t_stats_ms", stats_ms.as_millis());
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
