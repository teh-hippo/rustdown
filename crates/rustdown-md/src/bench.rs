#![forbid(unsafe_code)]
//! Integration benchmarks and edge-case tests for `rustdown-md`.

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::parse::parse_markdown;
    use crate::render::{MarkdownCache, simple_hash};
    use crate::stress;
    use crate::style::MarkdownStyle;

    // ── Helpers ────────────────────────────────────────────────────

    fn bench<F: FnMut()>(label: &str, iterations: u32, mut f: F) -> Duration {
        // Warm up.
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

    /// Assert timing only in release mode (debug mode is 10-50x slower).
    macro_rules! assert_perf {
        ($cond:expr, $($arg:tt)*) => {
            if cfg!(not(debug_assertions)) {
                assert!($cond, $($arg)*);
            }
        };
    }

    // ── Parse benchmarks ───────────────────────────────────────────

    #[test]
    fn bench_parse_100kb_mixed() {
        let doc = stress::large_mixed_doc(100);
        let kb = doc.len() / 1024;
        let per_iter = bench(&format!("parse_{kb}kb_mixed"), 50, || {
            let blocks = parse_markdown(&doc);
            assert!(!blocks.is_empty());
        });
        assert_perf!(
            per_iter < Duration::from_millis(10),
            "parse {kb}KB too slow: {per_iter:?}"
        );
    }

    #[test]
    fn bench_parse_500kb_mixed() {
        let doc = stress::large_mixed_doc(500);
        let kb = doc.len() / 1024;
        let per_iter = bench(&format!("parse_{kb}kb_mixed"), 10, || {
            let blocks = parse_markdown(&doc);
            assert!(!blocks.is_empty());
        });
        assert_perf!(
            per_iter < Duration::from_millis(50),
            "parse {kb}KB too slow: {per_iter:?}"
        );
    }

    #[test]
    fn bench_parse_100kb_unicode() {
        let doc = stress::unicode_stress_doc(100);
        let kb = doc.len() / 1024;
        let per_iter = bench(&format!("parse_{kb}kb_unicode"), 50, || {
            let blocks = parse_markdown(&doc);
            assert!(!blocks.is_empty());
        });
        assert_perf!(
            per_iter < Duration::from_millis(15),
            "parse {kb}KB unicode too slow: {per_iter:?}"
        );
    }

    #[test]
    fn bench_parse_100kb_pathological() {
        let doc = stress::pathological_doc(100);
        let kb = doc.len() / 1024;
        let per_iter = bench(&format!("parse_{kb}kb_pathological"), 50, || {
            let blocks = parse_markdown(&doc);
            assert!(!blocks.is_empty());
        });
        assert_perf!(
            per_iter < Duration::from_millis(15),
            "parse {kb}KB pathological too slow: {per_iter:?}"
        );
    }

    // ── Hash benchmarks ────────────────────────────────────────────

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

        // Hash should be roughly linear in input size.
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
    fn hash_no_unicode_collisions() {
        // Ensure our hash function doesn't collide on similar unicode strings.
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
                    "hash collision between {:?} and {:?}",
                    strings[i], strings[j]
                );
            }
        }
    }

    // ── Height estimation benchmarks ───────────────────────────────

    #[test]
    fn bench_height_estimation_100kb() {
        let doc = stress::large_mixed_doc(100);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);

        let per_iter = bench("height_est_100kb", 500, || {
            cache.heights.clear();
            cache.ensure_heights(14.0, 600.0, &style);
        });
        assert_perf!(
            per_iter < Duration::from_micros(500),
            "height estimation too slow: {per_iter:?}"
        );
    }

    #[test]
    fn bench_height_estimation_500kb() {
        let doc = stress::large_mixed_doc(500);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);

        let per_iter = bench("height_est_500kb", 100, || {
            cache.heights.clear();
            cache.ensure_heights(14.0, 600.0, &style);
        });
        assert_perf!(
            per_iter < Duration::from_millis(3),
            "height estimation 500KB too slow: {per_iter:?}"
        );
    }

    // ── Cache invalidation benchmarks ──────────────────────────────

    #[test]
    fn bench_cache_same_text_100kb() {
        let doc = stress::large_mixed_doc(100);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();

        // First parse.
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 600.0, &style);

        // Repeated calls with same text should be near-free.
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

        // Alternate between two docs to force re-parse each time.
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

    // ── Rapid scroll simulation ────────────────────────────────────

    #[test]
    fn bench_viewport_binary_search() {
        let doc = stress::large_mixed_doc(200);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 600.0, &style);

        let total_h = cache.total_height;
        let viewport_h = 800.0_f32;
        let n_blocks = cache.blocks.len();

        // Simulate rapid scrolling: 1000 random viewport positions.
        let per_iter = bench("viewport_scan_200kb", 1000, || {
            // Scroll through 20 positions per iteration.
            for step in 0..20_u32 {
                let frac = step as f32 / 20.0;
                let vis_top = total_h * frac;
                let vis_bottom = vis_top + viewport_h;

                let first = match cache.cum_y.binary_search_by(|y| {
                    y.partial_cmp(&vis_top).unwrap_or(std::cmp::Ordering::Equal)
                }) {
                    Ok(i) => i,
                    Err(i) => i.saturating_sub(1),
                };

                let mut idx = first;
                while idx < n_blocks {
                    if cache.cum_y[idx] > vis_bottom {
                        break;
                    }
                    idx += 1;
                }
                std::hint::black_box((first, idx));
            }
        });
        assert_perf!(
            per_iter < Duration::from_micros(10),
            "viewport scan too slow: {per_iter:?}"
        );
    }

    // ── Edge case correctness tests ────────────────────────────────

    #[test]
    fn parse_all_minimal_docs_without_panic() {
        for (name, doc) in stress::minimal_docs() {
            let blocks = parse_markdown(&doc);
            // Just verify it doesn't panic; block count varies.
            std::hint::black_box(&blocks);
            eprintln!(
                "minimal/{name}: {} blocks from {} bytes",
                blocks.len(),
                doc.len()
            );
        }
    }

    #[test]
    fn parse_unicode_stress_correctness() {
        let doc = stress::unicode_stress_doc(10);
        let blocks = parse_markdown(&doc);
        // Should have headings for CJK, Arabic, Hebrew, Emoji sections.
        let heading_count = blocks
            .iter()
            .filter(|b| matches!(b, crate::parse::Block::Heading { .. }))
            .count();
        assert!(
            heading_count >= 5,
            "expected at least 5 headings, got {heading_count}"
        );

        // Verify no empty text in paragraphs.
        for block in &blocks {
            if let crate::parse::Block::Paragraph(st) = block {
                assert!(
                    !st.text.is_empty(),
                    "empty paragraph text found in unicode doc"
                );
            }
        }
    }

    #[test]
    fn parse_pathological_deep_nesting() {
        let doc = stress::pathological_doc(10);
        let blocks = parse_markdown(&doc);
        assert!(!blocks.is_empty(), "pathological doc produced no blocks");

        // Check we can handle the deeply nested list without stack overflow.
        let list_count = blocks
            .iter()
            .filter(|b| matches!(b, crate::parse::Block::UnorderedList(_)))
            .count();
        assert!(list_count > 0, "no lists found in pathological doc");
    }

    #[test]
    fn height_estimation_handles_empty() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("");
        cache.ensure_heights(14.0, 600.0, &style);
        assert!((cache.total_height - 0.0).abs() < f32::EPSILON);
        assert!(cache.blocks.is_empty());
    }

    #[test]
    fn height_estimation_handles_unicode() {
        let doc = stress::unicode_stress_doc(5);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 600.0, &style);
        assert!(cache.total_height > 0.0);
        // All heights should be positive.
        for h in &cache.heights {
            assert!(*h > 0.0, "non-positive height found: {h}");
        }
    }

    #[test]
    fn cache_length_shortcut_works() {
        let mut cache = MarkdownCache::default();

        // First parse.
        let doc1 = "# Hello\n\nWorld";
        cache.ensure_parsed(doc1);
        assert_eq!(cache.blocks.len(), 2);

        // Same text → no re-parse (length matches, hash matches).
        let block_ptr = cache.blocks.as_ptr();
        cache.ensure_parsed(doc1);
        assert_eq!(
            cache.blocks.as_ptr(),
            block_ptr,
            "should not have re-allocated"
        );

        // Different length → definitely re-parse.
        let doc2 = "# Hello\n\nWorld!!!";
        cache.ensure_parsed(doc2);
        assert_eq!(cache.blocks.len(), 2); // still 2 blocks but text differs
    }

    #[test]
    fn hash_empty_and_single_byte() {
        let h_empty = simple_hash("");
        let h_a = simple_hash("a");
        let h_b = simple_hash("b");
        assert_ne!(h_empty, h_a);
        assert_ne!(h_a, h_b);
        assert_ne!(h_empty, 0, "empty hash should not be zero");
    }

    // ── Throughput summary ─────────────────────────────────────────

    #[test]
    fn throughput_summary() {
        let doc_100 = stress::large_mixed_doc(100);
        let doc_100_unicode = stress::unicode_stress_doc(100);

        let start = Instant::now();
        let iters: u32 = 50;
        for _ in 0..iters {
            std::hint::black_box(parse_markdown(&doc_100));
        }
        let elapsed_ascii = start.elapsed();
        let mb_per_sec_ascii = (doc_100.len() as f64 * f64::from(iters))
            / (elapsed_ascii.as_secs_f64() * 1024.0 * 1024.0);

        let start = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(parse_markdown(&doc_100_unicode));
        }
        let elapsed_unicode = start.elapsed();
        let mb_per_sec_unicode = (doc_100_unicode.len() as f64 * f64::from(iters))
            / (elapsed_unicode.as_secs_f64() * 1024.0 * 1024.0);

        eprintln!("=== Throughput Summary ===");
        eprintln!(
            "Mixed ASCII: {:.1} MB/s ({} KB in {:?})",
            mb_per_sec_ascii,
            doc_100.len() / 1024,
            elapsed_ascii / iters
        );
        eprintln!(
            "Unicode-heavy: {:.1} MB/s ({} KB in {:?})",
            mb_per_sec_unicode,
            doc_100_unicode.len() / 1024,
            elapsed_unicode / iters
        );
    }

    // ── Scaling benchmarks (1MB+) ──────────────────────────────────

    #[test]
    fn bench_parse_1mb_mixed() {
        let doc = stress::large_mixed_doc(1024);
        let kb = doc.len() / 1024;
        let per_iter = bench(&format!("parse_{kb}kb_mixed"), 5, || {
            let blocks = parse_markdown(&doc);
            assert!(!blocks.is_empty());
        });
        assert_perf!(
            per_iter < Duration::from_millis(100),
            "parse {kb}KB too slow: {per_iter:?}"
        );
    }

    #[test]
    fn bench_parse_2mb_mixed() {
        let doc = stress::large_mixed_doc(2048);
        let kb = doc.len() / 1024;
        let per_iter = bench(&format!("parse_{kb}kb_mixed"), 3, || {
            let blocks = parse_markdown(&doc);
            assert!(!blocks.is_empty());
        });
        assert_perf!(
            per_iter < Duration::from_millis(250),
            "parse {kb}KB too slow: {per_iter:?}"
        );
    }

    #[test]
    fn bench_height_estimation_1mb() {
        let doc = stress::large_mixed_doc(1024);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);

        let per_iter = bench("height_est_1mb", 50, || {
            cache.heights.clear();
            cache.ensure_heights(14.0, 600.0, &style);
        });
        assert_perf!(
            per_iter < Duration::from_millis(5),
            "height estimation 1MB too slow: {per_iter:?}"
        );
    }

    #[test]
    fn bench_full_pipeline_100kb() {
        let doc = stress::large_mixed_doc(100);
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();

        // Simulate full pipeline: parse + heights + viewport lookup.
        let per_iter = bench("full_pipeline_100kb", 50, || {
            cache.clear();
            cache.ensure_parsed(&doc);
            cache.ensure_heights(14.0, 600.0, &style);
            // Simulate viewport lookup at 10 positions.
            let total_h = cache.total_height;
            for i in 0..10_u32 {
                let vis_top = total_h * (i as f32 / 10.0);
                let vis_bottom = vis_top + 800.0;
                let _first = match cache.cum_y.binary_search_by(|y| {
                    y.partial_cmp(&vis_top).unwrap_or(std::cmp::Ordering::Equal)
                }) {
                    Ok(idx) => idx,
                    Err(idx) => idx.saturating_sub(1),
                };
                std::hint::black_box(vis_bottom);
            }
        });
        assert_perf!(
            per_iter < Duration::from_millis(10),
            "full pipeline 100KB too slow: {per_iter:?}"
        );
    }

    // ── Specialized content benchmarks ─────────────────────────────

    #[test]
    fn bench_parse_task_list_100kb() {
        let doc = stress::task_list_doc(100);
        let kb = doc.len() / 1024;
        let per_iter = bench(&format!("parse_{kb}kb_tasklist"), 50, || {
            let blocks = parse_markdown(&doc);
            assert!(!blocks.is_empty());
        });
        assert_perf!(
            per_iter < Duration::from_millis(15),
            "parse {kb}KB task list too slow: {per_iter:?}"
        );
    }

    #[test]
    fn bench_parse_emoji_100kb() {
        let doc = stress::emoji_heavy_doc(100);
        let kb = doc.len() / 1024;
        let per_iter = bench(&format!("parse_{kb}kb_emoji"), 50, || {
            let blocks = parse_markdown(&doc);
            assert!(!blocks.is_empty());
        });
        assert_perf!(
            per_iter < Duration::from_millis(15),
            "parse {kb}KB emoji too slow: {per_iter:?}"
        );
    }

    #[test]
    fn bench_parse_table_100kb() {
        let doc = stress::table_heavy_doc(100);
        let kb = doc.len() / 1024;
        let per_iter = bench(&format!("parse_{kb}kb_table"), 50, || {
            let blocks = parse_markdown(&doc);
            assert!(!blocks.is_empty());
        });
        assert_perf!(
            per_iter < Duration::from_millis(15),
            "parse {kb}KB table too slow: {per_iter:?}"
        );
    }

    #[test]
    fn bench_cache_rapid_scroll_simulation() {
        // Simulate rapid scrolling: same text, many viewport queries.
        let doc = stress::large_mixed_doc(200);
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 600.0, &style);

        let total_h = cache.total_height;
        let n_blocks = cache.blocks.len();

        // 100 frames of rapid scrolling (ensure_parsed noop + 5 viewport lookups each).
        let per_iter = bench("rapid_scroll_200kb", 100, || {
            // Re-check cache (should be noop).
            cache.ensure_parsed(&doc);
            cache.ensure_heights(14.0, 600.0, &style);

            // 5 viewport positions per frame.
            for step in 0..5_u32 {
                let frac = step as f32 / 5.0;
                let vis_top = total_h * frac;
                let vis_bottom = vis_top + 800.0;
                let first = match cache.cum_y.binary_search_by(|y| {
                    y.partial_cmp(&vis_top).unwrap_or(std::cmp::Ordering::Equal)
                }) {
                    Ok(i) => i,
                    Err(i) => i.saturating_sub(1),
                };
                let mut idx = first;
                while idx < n_blocks && cache.cum_y[idx] <= vis_bottom {
                    idx += 1;
                }
                std::hint::black_box((first, idx));
            }
        });
        // At 60fps, we have 16.6ms per frame. This should be <1ms.
        assert_perf!(
            per_iter < Duration::from_millis(1),
            "rapid scroll too slow: {per_iter:?}"
        );
    }

    // ── Scaling linearity check ────────────────────────────────────

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

        // 500KB should take less than 8x the 100KB time (allowing for overhead).
        let ratio = t500.as_nanos() as f64 / t100.as_nanos() as f64;
        eprintln!("500KB/100KB ratio: {ratio:.1}x (ideal: 5.0x)");
        assert_perf!(
            ratio < 8.0,
            "non-linear scaling: {ratio:.1}x (500KB={t500:?}, 100KB={t100:?})"
        );
    }
}
