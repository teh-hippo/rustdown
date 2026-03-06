#![forbid(unsafe_code)]
//! Integration benchmarks and edge-case tests for `rustdown-md`.

#[cfg(test)]
#[allow(clippy::panic, clippy::cast_precision_loss)]
mod tests {
    use std::fmt::Write;
    use std::time::{Duration, Instant};

    use crate::parse::parse_markdown;
    use crate::render::{MarkdownCache, MarkdownViewer, simple_hash};
    use crate::stress;
    use crate::style::MarkdownStyle;

    fn bench<F: FnMut()>(label: &str, iterations: u32, mut f: F) -> Duration {
        f();
        let start = Instant::now();
        for _ in 0..iterations {
            f();
        }
        let elapsed = start.elapsed();
        let per_iter = elapsed / iterations;
        eprintln!("{label}: {per_iter:?}/iter ({iterations} iters, total {elapsed:?})");
        per_iter
    }

    macro_rules! assert_perf {
        ($cond:expr, $($arg:tt)*) => {
            if cfg!(not(debug_assertions)) {
                assert!($cond, $($arg)*);
            }
        };
    }

    fn viewport_scan(cache: &MarkdownCache, steps: u32) {
        let total_h = cache.total_height;
        let n_blocks = cache.blocks.len();
        for step in 0..steps {
            let vis_top = total_h * (step as f32 / steps as f32);
            let vis_bottom = vis_top + 800.0;
            let first = match cache
                .cum_y
                .binary_search_by(|y| y.partial_cmp(&vis_top).unwrap_or(std::cmp::Ordering::Equal))
            {
                Ok(i) => i,
                Err(i) => i.saturating_sub(1),
            };
            let mut idx = first;
            while idx < n_blocks && cache.cum_y[idx] <= vis_bottom {
                idx += 1;
            }
            std::hint::black_box((first, idx));
        }
    }

    fn test_input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        }
    }

    // ── Parse benchmarks (parameterized by size) ─────────────────

    #[test]
    fn bench_parse_mixed_sizes() {
        for (target_kb, iters, max_ms) in
            [(100, 50, 10), (500, 10, 50), (1024, 5, 100), (2048, 3, 250)]
        {
            let doc = stress::large_mixed_doc(target_kb);
            let kb = doc.len() / 1024;
            let per_iter = bench(&format!("parse_{kb}kb_mixed"), iters, || {
                assert!(!parse_markdown(&doc).is_empty());
            });
            assert_perf!(
                per_iter < Duration::from_millis(max_ms),
                "parse {kb}KB too slow: {per_iter:?}"
            );
        }
    }

    #[test]
    fn bench_parse_specialized_content() {
        type GenFn = fn(usize) -> String;
        let cases: &[(&str, GenFn, u64)] = &[
            ("unicode", stress::unicode_stress_doc, 15),
            ("pathological", stress::pathological_doc, 15),
            ("tasklist", stress::task_list_doc, 15),
            ("emoji", stress::emoji_heavy_doc, 15),
            ("table", stress::table_heavy_doc, 15),
        ];
        for (name, gen_fn, max_ms) in cases {
            let doc = gen_fn(100);
            let kb = doc.len() / 1024;
            let per_iter = bench(&format!("parse_{kb}kb_{name}"), 50, || {
                assert!(!parse_markdown(&doc).is_empty());
            });
            assert_perf!(
                per_iter < Duration::from_millis(*max_ms),
                "parse {kb}KB {name} too slow: {per_iter:?}"
            );
        }
    }

    // ── Hash benchmarks ──────────────────────────────────────────

    #[test]
    fn bench_hash_large_inputs() {
        let doc_100 = stress::large_mixed_doc(100);
        let doc_500 = stress::large_mixed_doc(500);
        let per_100 = bench("hash_100kb", 1000, || {
            std::hint::black_box(simple_hash(&doc_100));
        });
        let per_500 = bench("hash_500kb", 200, || {
            std::hint::black_box(simple_hash(&doc_500));
        });
        assert_perf!(
            per_100 < Duration::from_millis(1),
            "hash 100KB too slow: {per_100:?}"
        );
        assert_perf!(
            per_500 < Duration::from_millis(5),
            "hash 500KB too slow: {per_500:?}"
        );
    }

    #[test]
    fn hash_correctness() {
        // No unicode collisions
        let strings = [
            "hello",
            "héllo",
            "hëllo",
            "hèllo",
            "hêllo",
            "日本語",
            "日本语",
            "日本吾",
            "🎉",
            "🎊",
            "🎈",
            "a\u{200D}b",
            "a\u{200C}b",
            "a\u{200B}b",
            "",
            " ",
            "\n",
            "\t",
            "# heading",
            "## heading",
            "### heading",
        ];
        let hashes: Vec<u64> = strings.iter().map(|s| simple_hash(s)).collect();
        for i in 0..hashes.len() {
            for j in (i + 1)..hashes.len() {
                assert_ne!(
                    hashes[i], hashes[j],
                    "collision: {:?} vs {:?}",
                    strings[i], strings[j]
                );
            }
        }
        // Empty and single byte
        let (he, ha, hb) = (simple_hash(""), simple_hash("a"), simple_hash("b"));
        assert_ne!(he, ha);
        assert_ne!(ha, hb);
        assert_ne!(he, 0);
    }

    // ── Height estimation benchmarks (parameterized) ─────────────

    #[test]
    fn bench_height_estimation_sizes() {
        for (target_kb, iters, max_us) in [(100, 500, 500), (500, 100, 3000), (1024, 50, 5000)] {
            let doc = stress::large_mixed_doc(target_kb);
            let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
            let mut cache = MarkdownCache::default();
            cache.ensure_parsed(&doc);
            let per_iter = bench(&format!("height_est_{target_kb}kb"), iters, || {
                cache.heights.clear();
                cache.ensure_heights(14.0, 600.0, &style);
            });
            assert_perf!(
                per_iter < Duration::from_micros(max_us),
                "height estimation {target_kb}KB too slow: {per_iter:?}"
            );
        }
    }

    // ── Cache benchmarks ─────────────────────────────────────────

    #[test]
    fn bench_cache_same_text_100kb() {
        let doc = stress::large_mixed_doc(100);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 600.0, &style);
        let per_iter = bench("cache_noop_100kb", 10_000, || {
            cache.ensure_parsed(&doc);
            cache.ensure_heights(14.0, 600.0, &style);
        });
        assert_perf!(
            per_iter < Duration::from_micros(50),
            "cache noop too slow: {per_iter:?}"
        );
    }

    #[test]
    fn bench_cache_invalidation_cycle() {
        let doc_a = stress::large_mixed_doc(50);
        let doc_b = stress::unicode_stress_doc(50);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        let per_iter = bench("cache_flip_50kb", 20, || {
            cache.ensure_parsed(&doc_a);
            cache.ensure_heights(14.0, 600.0, &style);
            cache.ensure_parsed(&doc_b);
            cache.ensure_heights(14.0, 600.0, &style);
        });
        assert_perf!(
            per_iter < Duration::from_millis(20),
            "cache flip too slow: {per_iter:?}"
        );
    }

    #[test]
    fn bench_viewport_binary_search() {
        let doc = stress::large_mixed_doc(200);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 600.0, &style);
        let per_iter = bench("viewport_scan_200kb", 1000, || {
            viewport_scan(&cache, 20);
        });
        assert_perf!(
            per_iter < Duration::from_micros(10),
            "viewport scan too slow: {per_iter:?}"
        );
    }

    #[test]
    fn bench_cache_rapid_scroll_simulation() {
        let doc = stress::large_mixed_doc(200);
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 600.0, &style);
        let per_iter = bench("rapid_scroll_200kb", 100, || {
            cache.ensure_parsed(&doc);
            cache.ensure_heights(14.0, 600.0, &style);
            viewport_scan(&cache, 5);
        });
        assert_perf!(
            per_iter < Duration::from_millis(1),
            "rapid scroll too slow: {per_iter:?}"
        );
    }

    #[test]
    fn bench_full_pipeline_100kb() {
        let doc = stress::large_mixed_doc(100);
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        let per_iter = bench("full_pipeline_100kb", 50, || {
            cache.clear();
            cache.ensure_parsed(&doc);
            cache.ensure_heights(14.0, 600.0, &style);
            viewport_scan(&cache, 10);
        });
        assert_perf!(
            per_iter < Duration::from_millis(10),
            "full pipeline too slow: {per_iter:?}"
        );
    }

    // ── Scaling and throughput ────────────────────────────────────

    #[test]
    fn parse_scales_linearly() {
        let doc_100 = stress::large_mixed_doc(100);
        let doc_500 = stress::large_mixed_doc(500);
        let t100 = bench("linear_100kb", 20, || {
            std::hint::black_box(parse_markdown(&doc_100));
        });
        let t500 = bench("linear_500kb", 5, || {
            std::hint::black_box(parse_markdown(&doc_500));
        });
        let ratio = t500.as_nanos() as f64 / t100.as_nanos() as f64;
        eprintln!("500KB/100KB ratio: {ratio:.1}x");
        assert_perf!(ratio < 8.0, "non-linear: {ratio:.1}x");
    }

    #[test]
    fn throughput_summary() {
        let doc_ascii = stress::large_mixed_doc(100);
        let doc_uni = stress::unicode_stress_doc(100);
        bench("throughput_ascii_100kb", 50, || {
            std::hint::black_box(parse_markdown(&doc_ascii));
        });
        bench("throughput_unicode_100kb", 50, || {
            std::hint::black_box(parse_markdown(&doc_uni));
        });
    }

    // ── Roundtrip / stability ────────────────────────────────────

    #[test]
    fn roundtrip_height_idempotent_and_stable() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        type GenFn = fn(usize) -> String;
        let generators: &[(&str, GenFn)] = &[
            ("large_mixed", stress::large_mixed_doc),
            ("unicode", stress::unicode_stress_doc),
            ("pathological", stress::pathological_doc),
            ("table_heavy", stress::table_heavy_doc),
            ("emoji_heavy", stress::emoji_heavy_doc),
            ("task_list", stress::task_list_doc),
        ];
        for (name, gen_fn) in generators {
            for &target_kb in &[10, 50, 100] {
                let doc = gen_fn(target_kb);
                let mut cache = MarkdownCache::default();
                cache.ensure_parsed(&doc);
                cache.ensure_heights(14.0, 600.0, &style);
                let heights_first = cache.heights.clone();
                let total_first = cache.total_height;
                // Re-estimate
                cache.heights.clear();
                cache.ensure_heights(14.0, 600.0, &style);
                assert_eq!(
                    cache.heights, heights_first,
                    "{name}/{target_kb}KB: heights differ"
                );
                assert!(
                    (cache.total_height - total_first).abs() < f32::EPSILON,
                    "{name}/{target_kb}KB: total differs: {} vs {total_first}",
                    cache.total_height
                );
            }
        }
    }

    // ── Cache invalidation correctness ───────────────────────────

    #[test]
    fn cache_invalidation_aba_returns_same_blocks() {
        let doc_a = stress::large_mixed_doc(50);
        let doc_b = stress::unicode_stress_doc(50);
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc_a);
        let blocks_a: Vec<String> = cache.blocks.iter().map(|b| format!("{b:?}")).collect();
        cache.ensure_parsed(&doc_b);
        cache.ensure_parsed(&doc_a);
        let blocks_a2: Vec<String> = cache.blocks.iter().map(|b| format!("{b:?}")).collect();
        assert_eq!(blocks_a, blocks_a2, "A-B-A: blocks differ");
    }

    #[test]
    fn cache_detects_single_char_change() {
        let doc = stress::large_mixed_doc(100);
        let hash_orig = simple_hash(&doc);
        let mut modified = doc.clone();
        let safe = modified.floor_char_boundary(modified.len() / 2);
        let replacement = if modified.as_bytes()[safe] == b'A' {
            'B'
        } else {
            'A'
        };
        modified.replace_range(safe..=safe, &replacement.to_string());
        assert_ne!(hash_orig, simple_hash(&modified));
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_parsed(&modified);
        assert!(!cache.blocks.is_empty());
    }

    #[test]
    fn cache_length_shortcut_works() {
        let mut cache = MarkdownCache::default();
        let doc1 = "# Hello\n\nWorld";
        cache.ensure_parsed(doc1);
        assert_eq!(cache.blocks.len(), 2);
        let ptr = cache.blocks.as_ptr();
        cache.ensure_parsed(doc1);
        assert_eq!(cache.blocks.as_ptr(), ptr, "should not re-allocate");
        cache.ensure_parsed("# Hello\n\nWorld!!!");
        assert_eq!(cache.blocks.len(), 2);
    }

    // ── Massive document stability ───────────────────────────────

    #[test]
    fn massive_document_stability() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        // 5MB document
        let doc = stress::large_mixed_doc(5 * 1024);
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        assert!(!cache.blocks.is_empty(), "5MB: no blocks");
        cache.ensure_heights(14.0, 600.0, &style);
        assert!(cache.total_height > 0.0);
        for (i, h) in cache.heights.iter().enumerate() {
            assert!(h.is_finite() && *h > 0.0, "height[{i}]: {h}");
        }
        for i in 1..cache.cum_y.len() {
            assert!(
                cache.cum_y[i] >= cache.cum_y[i - 1],
                "cum_y not monotonic at {i}"
            );
        }
        let sum: f32 = cache.heights.iter().sum();
        assert!((cache.total_height - sum).abs() < 1.0);
        viewport_scan(&cache, 100);

        // 10MB newlines
        let doc2 = "\n".repeat(10 * 1024 * 1024);
        let mut cache2 = MarkdownCache::default();
        cache2.ensure_parsed(&doc2);
        cache2.ensure_heights(14.0, 600.0, &style);
    }

    // ── Edge case correctness ────────────────────────────────────

    #[test]
    fn parse_all_minimal_docs_without_panic() {
        for (_name, doc) in stress::minimal_docs() {
            std::hint::black_box(parse_markdown(&doc));
        }
    }

    #[test]
    fn parse_unicode_stress_correctness() {
        let blocks = parse_markdown(&stress::unicode_stress_doc(10));
        let hc = blocks
            .iter()
            .filter(|b| matches!(b, crate::parse::Block::Heading { .. }))
            .count();
        assert!(hc >= 5, "headings: {hc}");
        for block in &blocks {
            if let crate::parse::Block::Paragraph(st) = block {
                assert!(!st.text.is_empty());
            }
        }
    }

    #[test]
    fn parse_pathological_deep_nesting() {
        let blocks = parse_markdown(&stress::pathological_doc(10));
        assert!(!blocks.is_empty());
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, crate::parse::Block::UnorderedList(_)))
        );
    }

    #[test]
    fn height_estimation_edge_cases() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        // Empty
        let mut c = MarkdownCache::default();
        c.ensure_parsed("");
        c.ensure_heights(14.0, 600.0, &style);
        assert!(c.blocks.is_empty() && c.total_height.abs() < f32::EPSILON);
        // Unicode
        let mut c = MarkdownCache::default();
        c.ensure_parsed(&stress::unicode_stress_doc(5));
        c.ensure_heights(14.0, 600.0, &style);
        assert!(c.total_height > 0.0);
        for h in &c.heights {
            assert!(*h > 0.0);
        }
    }

    #[test]
    fn extreme_edge_cases() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        for (_label, md) in [
            ("single_char", "a"),
            ("heading_no_text", "# "),
            ("unclosed_fence", "```"),
        ] {
            let mut c = MarkdownCache::default();
            c.ensure_parsed(md);
            c.ensure_heights(14.0, 600.0, &style);
            std::hint::black_box((c.blocks.len(), c.total_height));
        }
        // 1000 empty code blocks
        let doc = "```\n```\n".repeat(1000);
        let mut c = MarkdownCache::default();
        c.ensure_parsed(&doc);
        c.ensure_heights(14.0, 600.0, &style);
        assert!(c.blocks.len() >= 1000);
        for (i, h) in c.heights.iter().enumerate() {
            assert!(h.is_finite() && *h > 0.0, "height[{i}]: {h}");
        }
    }

    // ── Rendering stress tests ───────────────────────────────────

    #[test]
    fn stress_repetitive_content() {
        // 1000 tables
        let mut doc = String::with_capacity(100_000);
        for i in 0..1000 {
            write!(doc, "| Header {i} | Value |\n|---|---|\n| data | {i} |\n\n").ok();
        }
        let blocks = parse_markdown(&doc);
        assert_eq!(
            blocks
                .iter()
                .filter(|b| matches!(b, crate::parse::Block::Table(_)))
                .count(),
            1000
        );
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        let start = Instant::now();
        cache.ensure_heights(14.0, 600.0, &style);
        let elapsed = start.elapsed();
        assert!(cache.total_height > 0.0);
        if !cfg!(debug_assertions) {
            assert!(elapsed.as_millis() < 100);
        }

        // Deeply nested blockquotes (20 levels)
        doc.clear();
        for i in 0..20 {
            let prefix: String = "> ".repeat(i + 1);
            writeln!(doc, "{prefix}Level {}", i + 1).ok();
        }
        let blocks = parse_markdown(&doc);
        assert!(!blocks.is_empty());
        cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 600.0, &style);
        assert!(cache.total_height > 0.0);

        // 500 alternating code/text
        doc.clear();
        for i in 0..500 {
            writeln!(doc, "Text paragraph {i}.\n\n```\ncode block {i}\n```\n").ok();
        }
        assert_eq!(
            parse_markdown(&doc)
                .iter()
                .filter(|b| matches!(b, crate::parse::Block::Code { .. }))
                .count(),
            500
        );

        // Empty blockquotes
        doc.clear();
        for _ in 0..1000 {
            doc.push_str(">\n\n");
        }
        assert!(!parse_markdown(&doc).is_empty());

        // Single huge table
        doc.clear();
        doc.push_str("| A | B | C | D | E |\n|---|---|---|---|---|\n");
        for row in 0..500 {
            writeln!(
                doc,
                "| r{row}c0 | r{row}c1 | r{row}c2 | r{row}c3 | r{row}c4 |"
            )
            .ok();
        }
        match &parse_markdown(&doc)[0] {
            crate::parse::Block::Table(t) => {
                assert_eq!(t.header.len(), 5);
                assert_eq!(t.rows.len(), 500);
            }
            other => panic!("expected Table, got {other:?}"),
        }
    }

    #[test]
    fn stress_mixed_content_5mb() {
        let doc = stress::large_mixed_doc(5 * 1024);
        let start = Instant::now();
        let blocks = parse_markdown(&doc);
        let parse_time = start.elapsed();
        assert!(blocks.len() > 1000);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        let start = Instant::now();
        cache.ensure_heights(14.0, 600.0, &style);
        let height_time = start.elapsed();
        assert!(cache.total_height > 0.0);
        assert_perf!(parse_time.as_millis() < 500, "parse: {parse_time:?}");
        assert_perf!(height_time.as_millis() < 200, "heights: {height_time:?}");
    }

    // ── Mixed content and width sensitivity ──────────────────────

    #[test]
    fn mixed_content_programmatic_document() {
        let doc = stress::large_mixed_doc(500);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 600.0, &style);
        assert!(cache.blocks.len() > 500);
        assert!(cache.total_height > 10_000.0);
        for (i, h) in cache.heights.iter().enumerate() {
            assert!(*h > 0.0, "height[{i}]");
        }
    }

    #[test]
    fn width_sensitivity_monotonically_decreasing() {
        let doc = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                   Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. \
                   Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris \
                   nisi ut aliquip ex ea commodo consequat.\n\n"
            .repeat(500);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let widths: &[f32] = &[100.0, 200.0, 400.0, 800.0, 1600.0];
        let mut heights: Vec<f32> = Vec::new();
        for &w in widths {
            let mut cache = MarkdownCache::default();
            cache.ensure_parsed(&doc);
            cache.ensure_heights(14.0, w, &style);
            heights.push(cache.total_height);
        }
        for i in 1..heights.len() {
            assert!(
                heights[i] <= heights[i - 1],
                "wider should be shorter: w={} h={} vs w={} h={}",
                widths[i],
                heights[i],
                widths[i - 1],
                heights[i - 1]
            );
        }
    }

    // ── Integration tests ────────────────────────────────────────

    #[test]
    fn integration_parse_render_round_trip() {
        let docs = [
            "# Hello\n\nWorld\n",
            "| A | B |\n|---|---|\n| 1 | 2 |\n",
            "```rust\nfn main() {}\n```\n",
            "> Quote\n>\n> More\n",
            "- Item 1\n- Item 2\n  - Sub\n",
            "1. First\n2. Second\n",
            "![alt](url)\n",
            "---\n",
            "**bold** *italic* `code` ~~strike~~\n",
            "[link](url)\n",
        ];
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        for (i, md) in docs.iter().enumerate() {
            let mut cache = MarkdownCache::default();
            let style = MarkdownStyle::colored(&egui::Visuals::dark());
            let viewer = MarkdownViewer::new("integration");
            let _ = ctx.run(test_input(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    viewer.show_scrollable(ui, &mut cache, &style, md, None);
                });
            });
            assert!(!cache.blocks.is_empty() || md.trim().is_empty(), "doc {i}");
            assert!(cache.total_height >= 0.0, "doc {i}");
        }
    }

    #[test]
    fn integration_cache_stability() {
        let md = "# Title\n\nParagraph.\n\n```\ncode\n```\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n> Quote\n\n- List\n";
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let viewer = MarkdownViewer::new("stability");
        let mut heights = Vec::new();
        for _ in 0..3 {
            let _ = ctx.run(test_input(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    viewer.show_scrollable(ui, &mut cache, &style, md, None);
                });
            });
            heights.push(cache.total_height);
        }
        let d12 = (heights[0] - heights[1]).abs();
        let d23 = (heights[1] - heights[2]).abs();
        assert!(d23 <= d12 + 1.0, "should converge: {heights:?}");
    }

    #[test]
    fn integration_document_switch() {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        let mut cache = MarkdownCache::default();
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let viewer = MarkdownViewer::new("switch");
        let _ = ctx.run(test_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show_scrollable(ui, &mut cache, &style, "# Doc A\n\nText.\n", None);
            });
        });
        let a = cache.blocks.len();
        let _ = ctx.run(test_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show_scrollable(
                    ui,
                    &mut cache,
                    &style,
                    "# Doc B\n\nDifferent.\n\nMore.\n\n## S2\n",
                    None,
                );
            });
        });
        assert_ne!(a, cache.blocks.len());
    }
}
