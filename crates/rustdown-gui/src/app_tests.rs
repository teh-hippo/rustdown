use super::*;
use crate::cli::{LaunchOptions, parse_launch_options};
use crate::disk::io::{DiskRevision, atomic_write_utf8};
use crate::disk::sync::{DiskConflict, ReloadKind};
use crate::document::{EditorGalleyCache, TrackedTextBuffer, bytecount_newlines};
use crate::search::replace_all_occurrences;
use std::{
    borrow::Cow,
    cell::Cell,
    ffi::OsString,
    fs,
    sync::Arc,
    time::{Instant, SystemTime},
};

fn parse(args: &[&str]) -> LaunchOptions {
    parse_launch_options(args.iter().copied().map(OsString::from))
}

fn warm_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |_| {});
    ctx
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

fn dummy_editor_cache(ctx: &egui::Context, row_byte_offsets: Vec<(f32, u32)>) -> EditorGalleyCache {
    let galley = ctx.fonts_mut(|fonts| {
        fonts.layout_no_wrap(
            "dummy".to_owned(),
            egui::FontId::default(),
            egui::Color32::WHITE,
        )
    });
    EditorGalleyCache {
        content_seq: 0,
        content_color_mode: true,
        wrap_width_bits: 0,
        zoom_factor_bits: 0,
        layout_sections: Vec::new(),
        galley,
        row_byte_offsets,
    }
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
fn document_stats_and_path_helpers() {
    for (label, text, expected_lines) in [
        ("two lines", "one two\nthree", 2),
        ("unicode", "héllo 世界\n🙂", 2),
        ("empty", "", 1),
        ("single newline", "\n", 2),
    ] {
        assert_eq!(
            DocumentStats::from_text(text).lines,
            expected_lines,
            "{label}"
        );
    }
    assert_eq!(DocumentStats::from_text(""), DocumentStats::default());
    assert_eq!(Document::default().stats(), DocumentStats::from_text(""));

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

    // Edge cases: invalid factors and clamping.
    for (label, input, expected) in [
        (
            "negative",
            zoom_with_factor(1.0, -1.0),
            clamped_zoom_factor(1.0),
        ),
        (
            "infinity",
            zoom_with_factor(1.0, f32::INFINITY),
            clamped_zoom_factor(1.0),
        ),
        ("clamp low", clamped_zoom_factor(0.1), MIN_ZOOM_FACTOR),
        ("clamp high", clamped_zoom_factor(10.0), MAX_ZOOM_FACTOR),
        ("clamp mid", clamped_zoom_factor(1.5), 1.5),
    ] {
        assert_eq!(input, expected, "{label}");
    }
}

#[test]
fn edit_seq_dirty_flags_stats_and_replace_all() {
    // bump_edit_seq and note_text_changed.
    let mut app = RustdownApp::default();
    let seq = app.doc.edit_seq;
    app.bump_edit_seq();
    assert_eq!(app.doc.edit_seq, seq + 1);
    app.note_text_changed(true);
    assert!(app.doc.dirty && app.doc.stats_dirty && app.doc.preview_dirty);
    assert!(app.doc.last_edit_at.is_some());

    // Deferred stats refresh.
    let mut app = RustdownApp::default();
    app.doc.text = Arc::new("alpha beta".to_owned());
    app.doc.stats = DocumentStats::from_text(app.doc.text.as_str());
    app.doc.base_text = app.doc.text.clone();
    app.doc.text = Arc::new("alpha beta gamma".to_owned());
    app.bump_edit_seq();
    app.note_text_changed(true);
    assert!(app.doc.stats_dirty);
    app.doc.last_edit_at = Instant::now().checked_sub(STATS_RECALC_DEBOUNCE);
    let ctx = egui::Context::default();
    app.refresh_stats_if_due(&ctx);
    assert!(!app.doc.stats_dirty);
    assert_eq!(app.doc.stats, DocumentStats::from_text("alpha beta gamma"));

    // Replace all matches.
    let mut app = RustdownApp::default();
    app.doc.text = Arc::new("alpha beta alpha".to_owned());
    app.doc.stats = DocumentStats::from_text(app.doc.text.as_str());
    app.search.query = "alpha".to_owned();
    app.search.replacement = "zeta".to_owned();
    assert_eq!(app.replace_all_matches(), 2);
    assert_eq!(app.doc.text.as_str(), "zeta beta zeta");
    assert!(app.doc.dirty);
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
#[allow(clippy::type_complexity)]
fn incorporate_disk_text_handles_clean_merge_and_conflict_outcomes() {
    // (base, ours, dirty, disk_text, init_rev, incoming_rev, exp_text, exp_base, exp_rev, exp_dirty, conflict)
    let cases: &[(
        &str,
        &str,
        bool,
        &str,
        (u64, u64),
        (u64, u64),
        &str,
        &str,
        (u64, u64),
        bool,
        bool,
    )] = &[
        (
            "old",
            "old",
            false,
            "new",
            (1, 3),
            (2, 3),
            "new",
            "new",
            (2, 3),
            false,
            false,
        ),
        (
            "a\nb\n",
            "a\nB\n",
            true,
            "A\nb\n",
            (1, 4),
            (2, 4),
            "A\nB\n",
            "A\nb\n",
            (2, 4),
            true,
            false,
        ),
        (
            "a\nb\n",
            "a\nO\n",
            true,
            "a\nT\n",
            (1, 4),
            (2, 4),
            "a\nO\n",
            "a\nb\n",
            (1, 4),
            true,
            true,
        ),
    ];
    for &(
        base,
        ours,
        dirty,
        disk_text,
        init,
        inc,
        exp_text,
        exp_base,
        exp_rev,
        exp_dirty,
        expect_conflict,
    ) in cases
    {
        let mut app = merge_app(base, ours, init.0, init.1, dirty);
        app.incorporate_disk_text(disk_text.to_owned(), test_rev(inc.0, inc.1));
        assert_eq!(app.doc.text.as_str(), exp_text);
        assert_eq!(app.doc.base_text.as_str(), exp_base);
        assert_eq!(app.doc.disk_rev, Some(test_rev(exp_rev.0, exp_rev.1)));
        assert_eq!(app.doc.dirty, exp_dirty);
        assert_eq!(app.disk.conflict.is_some(), expect_conflict);
    }
}

#[test]
fn conflict_resolution_open_merge_and_keep_mine() {
    // OpenConflictMerge: replaces buffer with conflict markers.
    let mut app = merge_app("a\nb\n", "a\nO\n", 1, 4, true);
    app.incorporate_disk_text("a\nT\n".to_owned(), test_rev(2, 4));
    let expected_merge = disk_conflict(&app).conflict_marked.clone();
    app.apply_conflict_choice(ConflictChoice::OpenConflictMerge);
    assert_eq!(app.doc.text.as_str(), expected_merge.as_str());
    assert_eq!(app.doc.base_text.as_str(), "a\nT\n");
    assert_eq!(app.doc.disk_rev, Some(test_rev(2, 4)));
    assert!(app.doc.dirty);
    assert!(app.disk.conflict.is_none());

    // KeepMineWriteSidecar: writes sidecar and applies safe disk edits.
    let dir = make_temp_dir("rustdown-merge-test");
    let original = dir.join("note.md");
    let _ = atomic_write_utf8(&original, "line1\nline2\nline3\n");
    let mut app = merge_app("line1\nline2\nline3\n", "line1\nO2\nline3\n", 1, 18, true);
    app.doc.path = Some(original);
    app.incorporate_disk_text("line1\nT2\nT3\n".to_owned(), test_rev(2, 15));
    let expected_sidecar = disk_conflict(&app).conflict_marked.clone();
    let expected_ours_wins = disk_conflict(&app).ours_wins.clone();
    app.apply_conflict_choice(ConflictChoice::KeepMineWriteSidecar);
    assert_eq!(app.doc.text.as_str(), expected_ours_wins.as_str());
    assert_eq!(app.doc.base_text.as_str(), "line1\nT2\nT3\n");
    assert_eq!(app.doc.disk_rev, Some(test_rev(2, 15)));
    assert!(app.disk.conflict.is_none());
    assert!(app.disk.merge_sidecar_path.is_some());
    let sidecar_path = app
        .disk
        .merge_sidecar_path
        .clone()
        .unwrap_or_else(|| unreachable!());
    assert_eq!(read_file(&sidecar_path), expected_sidecar);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn document_metadata_title_path_debounce_and_bytecount() {
    let default_doc = Document::default();
    assert_eq!(default_doc.title().as_ref(), "Untitled");
    assert_eq!(default_doc.path_label().as_ref(), "Unsaved");
    assert!(
        default_doc
            .debounce_remaining(Duration::from_millis(500))
            .is_none()
    );

    let doc = Document {
        path: Some(PathBuf::from("/home/user/notes.md")),
        ..Document::default()
    };
    assert_eq!(doc.title().as_ref(), "notes.md");
    assert_eq!(doc.path_label().as_ref(), "/home/user/notes.md");

    for (label, text, expected) in [
        ("empty", "", 0),
        ("no nl", "no newline", 0),
        ("3 nl", "a\nb\nc\n", 3),
        ("all nl", "\n\n\n", 3),
    ] {
        assert_eq!(bytecount_newlines(text), expected, "{label}");
    }
}

#[test]
fn mode_transitions_icons_and_uses_editor() {
    // Mode cycling
    assert_eq!(Mode::Edit.cycle(), Mode::Preview);
    assert_eq!(Mode::Preview.cycle(), Mode::SideBySide);
    assert_eq!(Mode::SideBySide.cycle(), Mode::Edit);
    for (mode, icon, tooltip) in [
        (Mode::Edit, "Ed", "Edit"),
        (Mode::Preview, "Pr", "Preview"),
        (Mode::SideBySide, "S|S", "Side-by-Side"),
    ] {
        assert_eq!(mode.icon(), icon, "{tooltip} icon");
        assert_eq!(mode.tooltip(), tooltip);
    }

    let ctx = egui::Context::default();
    let mut app = RustdownApp::default();
    assert_eq!(app.mode, Mode::Edit);
    assert!(app.uses_editor());

    app.set_mode(Mode::Preview, &ctx);
    assert_eq!(app.mode, Mode::Preview);
    assert!(app.doc.editor_galley_cache.is_none());
    assert!(!app.uses_editor());

    app.set_mode(Mode::Edit, &ctx);
    assert_eq!(app.mode, Mode::Edit);

    // Same-mode is noop
    let app2 = RustdownApp::default();
    assert!(app2.nav.pending_scroll.is_none());

    // SideBySide uses editor
    app.set_mode(Mode::SideBySide, &ctx);
    assert!(app.uses_editor());
}

#[test]
fn animate_side_by_side_scroll_advances_preview_and_snaps() {
    let ctx = warm_ctx();
    let md = "# A\n\n## B\n";
    let outline = nav::outline::extract_headings(md);
    let mut app = RustdownApp {
        mode: Mode::SideBySide,
        side_by_side_scroll_sync: true,
        ..RustdownApp::default()
    };
    app.nav.outline = outline.clone();
    app.side_by_side_scroll_source = Some(SideBySideScrollSource::Editor);
    app.side_by_side_scroll_target = Some(120.0);
    app.doc.preview_cache.total_height = 240.0;
    app.doc.preview_cache.last_scroll_y = 20.0;

    app.animate_side_by_side_scroll(&ctx);
    assert_eq!(
        app.nav.pending_preview_scroll_y,
        Some((120.0_f32 - 20.0).mul_add(SIDE_BY_SIDE_SCROLL_LERP, 20.0))
    );
    assert_eq!(
        app.side_by_side_scroll_source,
        Some(SideBySideScrollSource::Editor)
    );

    app.doc.preview_cache.last_scroll_y = 119.5;
    app.side_by_side_scroll_target = Some(120.0);
    app.animate_side_by_side_scroll(&ctx);
    assert_eq!(app.nav.pending_preview_scroll_y, Some(120.0));
    assert!(app.side_by_side_scroll_target.is_none());
    assert!(app.side_by_side_scroll_source.is_none());
    assert_eq!(
        app.last_sync_preview_byte,
        Some(nav::panel::preview_scroll_y_to_byte(
            &outline,
            120.0,
            app.doc.preview_cache.total_height
        ))
    );
}

#[test]
fn side_by_side_sync_respects_toggle_and_clear_helper() {
    let ctx = warm_ctx();
    let md = "# A\n\n## B\n";
    let outline = nav::outline::extract_headings(md);
    let mut app = RustdownApp {
        mode: Mode::SideBySide,
        side_by_side_scroll_sync: false,
        ..RustdownApp::default()
    };
    app.nav.outline = outline.clone();
    app.doc.text = Arc::new(md.to_owned());
    app.doc.editor_galley_cache = Some(dummy_editor_cache(
        &ctx,
        vec![(0.0, 0), (40.0, outline[1].byte_offset as u32)],
    ));
    app.doc.preview_cache.total_height = 200.0;
    app.nav.pending_editor_scroll_y = Some(40.0);
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
    });
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
    });

    app.sync_side_by_side_scroll(&ctx);
    assert!(app.side_by_side_scroll_target.is_none());

    app.side_by_side_scroll_sync = true;
    app.side_by_side_scroll_source = Some(SideBySideScrollSource::Editor);
    app.side_by_side_scroll_target = Some(120.0);
    app.last_sync_editor_byte = Some(outline[1].byte_offset);
    app.last_sync_preview_byte = Some(outline[0].byte_offset);
    app.clear_side_by_side_scroll_state();
    assert!(app.side_by_side_scroll_source.is_none());
    assert!(app.side_by_side_scroll_target.is_none());
    assert!(app.last_sync_editor_byte.is_none());
    assert!(app.last_sync_preview_byte.is_none());
}

#[test]
fn tracked_text_buffer_seq_bumps_on_real_edits_only() {
    for (label, init, op, expected_seq, expected_text) in [
        ("insert", "hello", Op::Insert(" world", 5), 1, "hello world"),
        ("empty insert", "hello", Op::Insert("", 5), 0, "hello"),
        ("delete", "hello", Op::Delete(2..4), 1, "heo"),
        ("empty delete", "hello", Op::Delete(3..3), 0, "hello"),
    ] {
        let seq = Cell::new(0_u64);
        let mut text = Arc::new(init.to_owned());
        {
            let mut buf = TrackedTextBuffer {
                text: &mut text,
                seq: &seq,
            };
            match op {
                Op::Insert(s, pos) => {
                    egui::TextBuffer::insert_text(&mut buf, s, pos);
                }
                Op::Delete(r) => egui::TextBuffer::delete_char_range(&mut buf, r),
            }
        }
        assert_eq!(seq.get(), expected_seq, "{label}: seq");
        assert_eq!(text.as_str(), expected_text, "{label}: text");
    }
}

enum Op {
    Insert(&'static str, usize),
    Delete(std::ops::Range<usize>),
}

#[test]
fn search_state_caches_and_invalidates_across_loads() {
    // Caching by query and seq.
    let mut search = SearchState::with_query("a");
    assert_eq!(search.match_count("aaa", 1), 3);
    assert_eq!(search.match_count("aaa", 1), 3); // cached
    assert_eq!(search.match_count("aa", 2), 2); // seq change
    search.query = "b".to_owned();
    assert_eq!(search.match_count("bb", 2), 2); // query change

    // find_match_count edge cases.
    for (label, text, needle, expected) in [
        ("repeat", "aaa", "a", 3),
        ("pattern", "abcabc", "abc", 2),
        ("miss", "hello", "xyz", 0),
        ("empty text", "", "a", 0),
    ] {
        assert_eq!(find_match_count(text, needle), expected, "{label}");
    }

    // Cache invalidation across document loads.
    let mut app = RustdownApp::default();
    app.load_document(PathBuf::from("a.md"), "hello hello".to_owned(), None);
    app.search.query = "hello".to_owned();
    assert_eq!(
        app.search
            .match_count(app.doc.text.as_str(), app.doc.edit_seq),
        2
    );
    app.load_document(PathBuf::from("b.md"), "hello world".to_owned(), None);
    assert_eq!(
        app.search
            .match_count(app.doc.text.as_str(), app.doc.edit_seq),
        1,
        "stale cache"
    );

    // replace_all_occurrences returns borrowed on noop
    for (label, haystack, needle) in [
        ("no match", "hello world", "xyz"),
        ("empty needle", "hello", ""),
    ] {
        let (result, count) = replace_all_occurrences(haystack, needle, "abc");
        assert_eq!(count, 0, "{label}");
        assert!(matches!(result, Cow::Borrowed(_)), "{label}");
    }
}

#[test]
fn default_image_uri_scheme_covers_all_paths() {
    assert_eq!(default_image_uri_scheme(None), "file://");
    assert_eq!(
        default_image_uri_scheme(Some(Path::new("/a/b/c/file.md"))),
        "file:///a/b/c/"
    );
    assert_eq!(
        default_image_uri_scheme(Some(Path::new("relative/file.md"))),
        "file:///relative/"
    );
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
fn resolve_nav_scroll_target_covers_modes_and_anchor() {
    let ctx = egui::Context::default();

    // Preview sets preview only.
    let mut app = RustdownApp {
        mode: Mode::Preview,
        ..RustdownApp::default()
    };
    app.nav.pending_scroll = Some(nav::panel::NavScrollTarget::Top);
    app.resolve_nav_scroll_target(&ctx);
    assert_eq!(app.nav.pending_preview_scroll_y, Some(0.0));
    assert!(app.nav.pending_editor_scroll_y.is_none());

    // SideBySide sets both panes and clears stale animation target.
    let mut app = RustdownApp {
        mode: Mode::SideBySide,
        ..RustdownApp::default()
    };
    app.side_by_side_scroll_target = Some(999.0);
    app.nav.pending_scroll = Some(nav::panel::NavScrollTarget::Top);
    app.resolve_nav_scroll_target(&ctx);
    assert_eq!(app.nav.pending_editor_scroll_y, Some(0.0));
    assert_eq!(app.nav.pending_preview_scroll_y, Some(0.0));
    assert!(
        app.side_by_side_scroll_target.is_none(),
        "nav scroll should cancel animation"
    );

    // Preview with heading anchor uses heading_y.
    let mut app = RustdownApp {
        mode: Mode::Preview,
        ..RustdownApp::default()
    };
    let md = "# A\n\ntext\n\n## B\n";
    app.nav.outline = nav::outline::extract_headings(md);
    let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
    app.doc.preview_cache.ensure_parsed(md);
    app.doc.preview_cache.ensure_heights(14.0, 400.0, &style);
    let b_offset = app.nav.outline[1].byte_offset;
    let expected_y = app.doc.preview_cache.heading_y(1).unwrap_or(0.0);
    app.nav.pending_scroll = Some(nav::panel::NavScrollTarget::ByteOffset(b_offset));
    app.resolve_nav_scroll_target(&ctx);
    let actual_y = app.nav.pending_preview_scroll_y.unwrap_or(0.0);
    assert!((actual_y - expected_y).abs() < 0.01);
    assert!(app.nav.pending_editor_scroll_y.is_none());
}

#[test]
fn reload_kind_flags() {
    let mut app = RustdownApp::default();
    let text = Arc::new("test".to_owned());
    let rev = test_rev(0, 4);

    app.apply_disk_text_state(text.clone(), text.clone(), rev, ReloadKind::Clean);
    assert!(!app.doc.dirty);
    assert!(app.doc.last_edit_at.is_none());

    app.doc.last_edit_at = Some(Instant::now());
    app.apply_disk_text_state(text.clone(), text.clone(), rev, ReloadKind::Merged);
    assert!(app.doc.dirty && app.doc.last_edit_at.is_some());

    app.apply_disk_text_state(text.clone(), text, rev, ReloadKind::ConflictResolved);
    assert!(app.doc.dirty);
    assert!(app.doc.last_edit_at.is_none());
}

#[test]
fn document_lifecycle_load_new_blank_and_sidecar_clearing() {
    let dir = make_temp_dir("rustdown-doc-lifecycle-test");
    let path = dir.join("test.md");
    fs::write(&path, "test content").ok();
    let rev = disk::io::disk_revision(&path).ok();

    let mut app = RustdownApp::default();
    let seq0 = app.doc.edit_seq;

    // Load resets state.
    app.doc.dirty = true;
    app.load_document(path.clone(), "test content".to_owned(), rev);
    assert_eq!(app.doc.path.as_deref(), Some(path.as_path()));
    assert_eq!(app.doc.text.as_str(), "test content");
    assert_eq!(app.doc.base_text.as_str(), "test content");
    assert!(!app.doc.dirty);
    assert!(!app.doc.stats_dirty);
    assert_eq!(app.doc.stats, DocumentStats::from_text("test content"));
    let seq1 = app.doc.edit_seq;
    assert!(seq1 > seq0, "edit_seq should advance on first load");

    // Second load also advances edit_seq.
    app.load_document(PathBuf::from("b.md"), "bbb".to_owned(), None);
    let seq2 = app.doc.edit_seq;
    assert!(seq2 > seq1, "edit_seq should advance on second load");

    // NewBlank advances edit_seq and invalidates nav outline.
    app.load_document(PathBuf::from("a.md"), "# Heading\n".to_owned(), None);
    app.nav.refresh_outline(&app.doc.text, app.doc.edit_seq);
    assert_eq!(app.nav.outline.len(), 1);
    let seq_before = app.doc.edit_seq;
    app.apply_action(PendingAction::NewBlank);
    assert!(
        app.doc.edit_seq > seq_before,
        "NewBlank must advance edit_seq"
    );
    let text = app.doc.text.clone();
    app.nav.refresh_outline(&text, app.doc.edit_seq);
    assert!(
        app.nav.outline.is_empty(),
        "nav outline should be empty after NewBlank"
    );

    // Merge sidecar: write, verify file exists with content, then test clearing.
    let doc_path = dir.join("sc.md");
    fs::write(&doc_path, "# doc").ok();
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
    app.open_path(dir.join("other.md"));
    assert!(app.disk.merge_sidecar_path.is_none(), "cleared on open");

    app.write_merge_sidecar(&doc_path, "more conflict");
    app.apply_action(PendingAction::NewBlank);
    assert!(app.disk.merge_sidecar_path.is_none(), "cleared on NewBlank");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn app_operations_search_format_stats_action_reload() {
    // Launch options.
    let opts = LaunchOptions {
        mode: Mode::Preview,
        mode_explicit: true,
        path: None,
        print_version: false,
        diagnostics: DiagnosticsMode::Off,
        diagnostics_iterations: 200,
        diagnostics_runs: 1,
    };
    assert_eq!(RustdownApp::from_launch_options(opts).mode, Mode::Preview);

    // Search open/close.
    let mut app = RustdownApp::default();
    assert!(!app.search.visible);
    app.open_search(false);
    assert!(app.search.visible && !app.search.replace_mode && app.focus_search);
    app.open_search(true);
    assert!(app.search.replace_mode);
    app.close_search();
    assert!(!app.search.visible && !app.search.replace_mode && !app.focus_search);

    // Format document.
    let mut app = RustdownApp::default();
    app.doc.text = Arc::new("# Hello\n\nworld".to_owned());
    let seq_before = app.doc.edit_seq;
    app.format_document();
    assert!(app.doc.text.ends_with('\n'));
    assert!(app.doc.edit_seq > seq_before && app.doc.dirty);

    // Refresh stats.
    let mut app = RustdownApp::default();
    app.doc.text = Arc::new("a\nb\nc\n".to_owned());
    app.doc.stats_dirty = true;
    app.refresh_stats_now();
    assert_eq!(app.doc.stats.lines, 4);
    assert!(!app.doc.stats_dirty);

    // Request action defers when dirty.
    let mut app = RustdownApp::default();
    app.doc.dirty = true;
    app.request_action(PendingAction::NewBlank);
    assert!(app.pending_action.is_some());

    // Schedule disk reload keeps earliest time.
    let mut app = RustdownApp::default();
    let now = Instant::now();
    app.schedule_disk_reload(now);
    assert!(app.disk.pending_reload_at.is_some());
    let first = app.disk.pending_reload_at;
    app.schedule_disk_reload(now);
    assert_eq!(app.disk.pending_reload_at, first);
}

#[test]
fn reload_clean_dirty_conflict_and_large_file() {
    // Clean buffer reload
    let mut app = RustdownApp::default();
    app.doc.text = Arc::new("original".into());
    app.doc.base_text = Arc::new("original".into());
    let old_seq = app.doc.edit_seq;
    let new_text = Arc::new("new content".to_owned());
    app.apply_disk_text_state(
        new_text.clone(),
        new_text,
        test_rev(11, 11),
        ReloadKind::Clean,
    );
    assert_eq!(app.doc.text.as_str(), "new content");
    assert!(!app.doc.dirty && !app.doc.stats_dirty && !app.doc.preview_dirty);
    assert!(app.doc.edit_seq > old_seq);
    assert!(app.doc.editor_galley_cache.is_none());

    // Successive reloads each advance edit_seq.
    let mut prev_seq = app.doc.edit_seq;
    for i in 0u64..5 {
        let content = Arc::new(format!("version {i}\n"));
        app.apply_disk_text_state(
            content.clone(),
            content,
            test_rev(i + 20, 12),
            ReloadKind::Clean,
        );
        assert!(app.doc.edit_seq > prev_seq, "reload {i}");
        prev_seq = app.doc.edit_seq;
    }

    // Dirty buffer merge
    let original = "line one\nline two\n";
    let ours = format!("{original}our addition\n");
    let mut app = merge_app(original, &ours, 1, original.len() as u64, true);
    app.incorporate_disk_text("CHANGED\nline two\n".to_string(), test_rev(2, 18));
    assert!(app.doc.text.contains("CHANGED") && app.doc.text.contains("our addition"));
    assert!(app.disk.conflict.is_none());

    // Overlapping edits → conflict.
    let mut app = merge_app("shared line\n", "our version\n", 1, 12, true);
    app.incorporate_disk_text("their version\n".to_owned(), test_rev(2, 14));
    assert!(app.disk.conflict.is_some());
    let conflict = disk_conflict(&app);
    assert!(conflict.conflict_marked.contains("<<<<<<< ours"));
    assert!(conflict.ours_wins.contains("our version"));

    // Disk truncated to empty while dirty → preserves edits.
    let mut app2 = merge_app("original\n", "user edits here\n", 1, 9, true);
    app2.incorporate_disk_text(String::new(), test_rev(2, 0));
    if app2.disk.conflict.is_some() {
        assert!(disk_conflict(&app2).ours_wins.contains("user edits"));
    } else {
        assert!(app2.doc.text.contains("user edits"));
    }

    // Large file reload
    use std::fmt::Write;
    let mut app = RustdownApp::default();
    let mut large_content = String::new();
    for i in 0..50_000 {
        writeln!(large_content, "line {i}").unwrap_or_default();
    }
    assert!(large_content.len() > 400_000);
    let text = Arc::new(large_content.clone());
    app.apply_disk_text_state(
        text.clone(),
        text,
        test_rev(1, large_content.len() as u64),
        ReloadKind::Clean,
    );
    assert_eq!(app.doc.text.as_str(), large_content.as_str());
    assert_eq!(app.doc.base_text.as_str(), large_content.as_str());
    assert!(!app.doc.dirty);
    assert!(
        app.doc.stats.lines > 49_000,
        "stats should reflect large file"
    );
}

// ---------------------------------------------------------------
// Bundled document tests
// ---------------------------------------------------------------

#[test]
fn bundled_docs_are_parseable_and_loadable() {
    // Both bundled documents must be parseable.
    for (doc, min_len, min_events, heading_pat) in [
        (BundledDoc::Demo, 500, 50, "# "),
        (BundledDoc::Verification, 10_000, 500, "# 1 "),
    ] {
        let content = doc.content();
        assert!(content.len() > min_len);
        assert!(content.contains(heading_pat));
        assert!(pulldown_cmark::Parser::new(content).count() > min_events);
        if matches!(doc, BundledDoc::Verification) {
            let headings = nav::outline::extract_headings(content);
            assert_eq!(
                headings.first().map(|heading| heading.label(content)),
                Some("🔬 Rustdown Verification Document"),
                "verification doc should preserve its emoji-led title"
            );
        }
    }

    // Loading creates pathless clean document and advances edit_seq.
    let mut app = RustdownApp::default();
    let seq_before = app.doc.edit_seq;
    app.load_bundled(BundledDoc::Demo);
    assert!(app.doc.path.is_none() && !app.doc.dirty);
    assert!(app.doc.text.len() > 500);
    assert_eq!(app.doc.text.as_str(), app.doc.base_text.as_str());
    assert!(app.doc.edit_seq > seq_before);
}
