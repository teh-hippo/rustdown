#![forbid(unsafe_code)]
//! Debug-only agentic testing harness for the Navigation panel.
//!
//! Compiled only in debug builds (`#[cfg(debug_assertions)]`).
//! Provides a headless test pipeline invoked via `--diagnostics-nav <file.md>`.

use std::{io, path::Path, sync::Arc, time::Instant};

use eframe::egui;
use egui_commonmark::CommonMarkCache;

use crate::{
    Document, DocumentStats, Mode, RustdownApp, configure_single_font, configure_ui_style,
    default_image_uri_scheme, diagnostics_raw_input, disk_io::read_stable_utf8,
    nav_panel::NavScrollTarget,
};

/// Render one simulated frame using the same layout as the real app.
fn run_frame(ctx: &egui::Context, app: &mut RustdownApp) {
    let raw = diagnostics_raw_input();
    let _ = ctx.run(raw, |ctx| {
        app.show_content_panels(ctx);
    });
}

/// Run the navigation panel diagnostics pipeline.
///
/// Returns `Ok(())` if all assertions pass.  Prints timing and results to
/// stdout in key=value format for easy agentic parsing.
pub fn run_nav_diagnostics(path: Option<&Path>) -> io::Result<()> {
    let Some(path) = path else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing markdown path (usage: rustdown --diagnostics-nav <file.md>)",
        ));
    };

    let total_start = Instant::now();

    // --- Setup ---
    let (text, disk_rev) = read_stable_utf8(path)?;
    let text = Arc::new(text);
    let base_text = text.clone();
    let stats = DocumentStats::from_text(text.as_str());
    let md_cache = CommonMarkCache::default();

    let ctx = egui::Context::default();
    configure_single_font(&ctx).map_err(io::Error::other)?;
    configure_ui_style(&ctx);
    // Warm up: egui needs one frame for fonts.
    let _ = ctx.run(egui::RawInput::default(), |_ctx| {});

    let doc = Document {
        path: Some(path.to_path_buf()),
        image_uri_scheme: default_image_uri_scheme(Some(path)),
        text,
        base_text,
        disk_rev: Some(disk_rev),
        stats,
        stats_dirty: false,
        preview_dirty: false,
        dirty: false,
        md_cache,
        last_edit_at: None,
        edit_seq: 1,
        editor_galley_cache: None,
    };

    let mut app = RustdownApp {
        mode: Mode::Edit,
        doc,
        ..Default::default()
    };

    // Open the nav panel.
    app.nav.visible = true;

    println!("rustdown_diagnostics=nav_pipeline");
    println!("nav_diag_path={}", path.display());

    // --- Phase 1: Outline extraction ---
    let outline_start = Instant::now();
    app.nav
        .refresh_outline(app.doc.text.as_str(), app.doc.edit_seq);
    let outline_us = outline_start.elapsed().as_micros();
    let heading_count = app.nav.outline.len();

    println!("nav_heading_count={heading_count}");
    println!("nav_outline_us={outline_us}");

    assert_result(heading_count > 0, "at least one heading found")?;

    // --- Phase 2: Render a frame with the nav panel visible ---
    let frame_start = Instant::now();
    run_frame(&ctx, &mut app);
    let frame_us = frame_start.elapsed().as_micros();
    println!("nav_first_frame_us={frame_us}");

    // --- Phase 3: Programmatic heading navigation ---
    let last_heading_offset = app.nav.outline.last().map(|h| h.byte_offset).unwrap_or(0);

    app.nav.pending_scroll = Some(NavScrollTarget::ByteOffset(last_heading_offset));
    let nav_start = Instant::now();
    run_frame(&ctx, &mut app);
    let nav_us = nav_start.elapsed().as_micros();
    println!("nav_jump_last_heading_us={nav_us}");

    assert_result(
        app.nav.pending_scroll.is_none(),
        "pending_scroll consumed after frame",
    )?;

    // --- Phase 4: Return to top ---
    app.nav.pending_scroll = Some(NavScrollTarget::Top);
    let top_start = Instant::now();
    run_frame(&ctx, &mut app);
    let top_us = top_start.elapsed().as_micros();
    println!("nav_return_to_top_us={top_us}");

    assert_result(
        app.nav.pending_scroll.is_none(),
        "return-to-top scroll consumed",
    )?;

    // --- Phase 5: Navigate each heading in sequence ---
    let offsets: Vec<usize> = app.nav.outline.iter().map(|h| h.byte_offset).collect();
    let seq_start = Instant::now();
    for offset in &offsets {
        app.nav.pending_scroll = Some(NavScrollTarget::ByteOffset(*offset));
        run_frame(&ctx, &mut app);
    }
    let seq_us = seq_start.elapsed().as_micros();
    let avg_nav = if heading_count > 0 {
        seq_us / heading_count as u128
    } else {
        0
    };
    println!("nav_sequential_all_us={seq_us}");
    println!("nav_sequential_avg_us={avg_nav}");

    // --- Phase 6: Depth control cycling ---
    let orig_depth = app.nav.max_depth;
    app.nav.max_depth = 1;
    run_frame(&ctx, &mut app);
    app.nav.max_depth = orig_depth;

    // --- Phase 7: Preview mode navigation ---
    app.mode = Mode::Preview;
    if let Some(&first_offset) = offsets.first() {
        app.nav.pending_scroll = Some(NavScrollTarget::ByteOffset(first_offset));
        run_frame(&ctx, &mut app);
        assert_result(
            app.nav.pending_scroll.is_none(),
            "preview mode scroll consumed",
        )?;
    }

    let total_us = total_start.elapsed().as_micros();
    println!("nav_total_us={total_us}");
    println!("nav_diagnostics_result=ok");
    Ok(())
}

fn assert_result(condition: bool, label: &str) -> io::Result<()> {
    if condition {
        println!("nav_assert_pass={label}");
        Ok(())
    } else {
        let msg = format!("nav assertion failed: {label}");
        println!("nav_assert_FAIL={label}");
        Err(io::Error::other(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn run_nav_diagnostics_rejects_missing_path() {
        let result = run_nav_diagnostics(None);
        assert!(result.is_err());
    }

    #[test]
    fn run_nav_diagnostics_rejects_nonexistent_path() {
        let result = run_nav_diagnostics(Some(Path::new("/tmp/rustdown_does_not_exist_12345.md")));
        assert!(result.is_err());
    }

    #[test]
    fn run_nav_diagnostics_passes_on_temp_file() {
        let dir = std::env::temp_dir().join("rustdown_nav_diag_test");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("test.md");
        let Ok(mut f) = std::fs::File::create(&file) else {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        let write_ok = writeln!(
            f,
            "# Title\n\nSome text.\n\n## Section A\n\nMore.\n\n### Sub\n\nEnd."
        );
        assert!(write_ok.is_ok(), "failed to write temp file");
        drop(f);

        let result = run_nav_diagnostics(Some(&file));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.is_ok(), "diagnostics should pass: {result:?}");
    }
}
