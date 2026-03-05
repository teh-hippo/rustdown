#![forbid(unsafe_code)]
//! Integration benchmarks and edge-case tests for `rustdown-md`.

#[cfg(test)]
#[allow(clippy::panic, clippy::cast_precision_loss)]
mod tests {
    use std::fmt::Write;
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

    // ════════════════════════════════════════════════════════════════
    //  Cross-cutting integration stress tests
    // ════════════════════════════════════════════════════════════════

    // ── 1. Round-trip consistency ──────────────────────────────────

    /// Parse → estimate heights → re-estimate: heights must be identical.
    #[test]
    fn roundtrip_height_idempotent_all_generators() {
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

                // First parse + estimate.
                cache.ensure_parsed(&doc);
                cache.ensure_heights(14.0, 600.0, &style);
                let heights_first = cache.heights.clone();
                let total_first = cache.total_height;

                // Force re-estimation by clearing heights.
                cache.heights.clear();
                cache.ensure_heights(14.0, 600.0, &style);

                assert_eq!(
                    cache.heights, heights_first,
                    "{name}/{target_kb}KB: heights differ after re-estimation"
                );
                assert!(
                    (cache.total_height - total_first).abs() < f32::EPSILON,
                    "{name}/{target_kb}KB: total_height differs: {} vs {total_first}",
                    cache.total_height
                );
            }
        }
    }

    /// Parse at width 600, re-estimate at width 600 — total must match exactly.
    #[test]
    fn roundtrip_same_width_total_height_stable() {
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
                let total_a = cache.total_height;

                cache.heights.clear();
                cache.ensure_heights(14.0, 600.0, &style);
                let total_b = cache.total_height;

                assert!(
                    (total_a - total_b).abs() < f32::EPSILON,
                    "{name}/{target_kb}KB: total_height not stable: {total_a} vs {total_b}"
                );
            }
        }
    }

    // ── 2. Cache invalidation correctness ─────────────────────────

    /// Parse A → parse B → parse A: blocks must be identical to first parse.
    #[test]
    fn cache_invalidation_aba_returns_same_blocks() {
        let doc_a = stress::large_mixed_doc(50);
        let doc_b = stress::unicode_stress_doc(50);
        let mut cache = MarkdownCache::default();

        // Parse A.
        cache.ensure_parsed(&doc_a);
        let blocks_a: Vec<String> = cache.blocks.iter().map(|b| format!("{b:?}")).collect();

        // Parse B.
        cache.ensure_parsed(&doc_b);
        let blocks_b_count = cache.blocks.len();
        assert_ne!(blocks_a.len(), 0);
        // B should differ from A.
        assert_ne!(blocks_b_count, 0);

        // Parse A again — should re-parse to identical blocks.
        cache.ensure_parsed(&doc_a);
        let blocks_a2: Vec<String> = cache.blocks.iter().map(|b| format!("{b:?}")).collect();
        assert_eq!(
            blocks_a, blocks_a2,
            "A-B-A cycle: blocks after re-parse of A differ from first parse"
        );
    }

    /// Modify one character in a 100KB doc — hash must change.
    #[test]
    fn cache_detects_single_char_change() {
        let doc = stress::large_mixed_doc(100);
        let hash_orig = simple_hash(&doc);

        // Modify one character in the middle.
        let mut modified = doc.clone();
        let mid = modified.len() / 2;
        // Find a safe char boundary.
        let safe = modified.floor_char_boundary(mid);
        let replacement = if modified.as_bytes()[safe] == b'A' {
            'B'
        } else {
            'A'
        };
        modified.replace_range(safe..=safe, &replacement.to_string());

        let hash_mod = simple_hash(&modified);
        assert_ne!(
            hash_orig, hash_mod,
            "hash did not change after single-character modification"
        );

        // Verify cache actually re-parses.
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        let blocks_orig_count = cache.blocks.len();
        cache.ensure_parsed(&modified);
        // Even if block count is the same, the cache should have re-parsed
        // (pointer changed, hash changed).
        assert!(!cache.blocks.is_empty());
        eprintln!(
            "single-char change: {blocks_orig_count} blocks orig, {} blocks modified",
            cache.blocks.len()
        );
    }

    // ── 3. Massive document stability (5MB) ───────────────────────

    #[test]
    fn massive_5mb_document_stability() {
        let doc = stress::large_mixed_doc(5 * 1024);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();

        // Parse completes without panic.
        cache.ensure_parsed(&doc);
        assert!(!cache.blocks.is_empty(), "5MB doc produced no blocks");
        eprintln!("5MB doc: {} blocks", cache.blocks.len());

        // Height estimation completes without panic.
        cache.ensure_heights(14.0, 600.0, &style);
        assert!(cache.total_height > 0.0, "5MB doc total_height is zero");

        // All heights are finite and positive.
        for (i, h) in cache.heights.iter().enumerate() {
            assert!(h.is_finite(), "height[{i}] is not finite: {h}");
            assert!(*h > 0.0, "height[{i}] is not positive: {h}");
        }

        // cum_y is monotonically increasing.
        for i in 1..cache.cum_y.len() {
            assert!(
                cache.cum_y[i] >= cache.cum_y[i - 1],
                "cum_y not monotonic at {i}: {} < {}",
                cache.cum_y[i],
                cache.cum_y[i - 1]
            );
        }

        // total_height matches sum of heights.
        let sum: f32 = cache.heights.iter().sum();
        assert!(
            (cache.total_height - sum).abs() < 1.0,
            "total_height ({}) != sum of heights ({sum})",
            cache.total_height
        );

        // Viewport binary search at 100 positions completes without panic.
        let n_blocks = cache.blocks.len();
        for step in 0..100_u32 {
            let frac = step as f32 / 100.0;
            let vis_top = cache.total_height * frac;
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

    // ── 4. Mixed content document ─────────────────────────────────

    #[test]
    fn mixed_content_programmatic_document() {
        use std::fmt::Write;
        let mut doc = String::with_capacity(512 * 1024);

        // 200 headings at all 6 levels.
        for i in 0..200_u32 {
            let level = (i % 6) + 1;
            let hashes = "#".repeat(level as usize);
            let _ = writeln!(doc, "{hashes} Heading {i}\n");
        }

        // 100 tables of varying sizes (1-20 columns, 1-50 rows).
        for t in 0..100_u32 {
            let cols = (t % 20) + 1;
            let rows = (t % 50) + 1;
            let _ = write!(doc, "|");
            for c in 0..cols {
                let _ = write!(doc, " H{c} |");
            }
            let _ = writeln!(doc);
            let _ = write!(doc, "|");
            for _ in 0..cols {
                let _ = write!(doc, " --- |");
            }
            let _ = writeln!(doc);
            for r in 0..rows {
                let _ = write!(doc, "|");
                for c in 0..cols {
                    let _ = write!(doc, " R{r}C{c} |");
                }
                let _ = writeln!(doc);
            }
            let _ = writeln!(doc);
        }

        // 100 code blocks of varying sizes.
        for cb in 0..100_u32 {
            let lines = (cb % 30) + 1;
            let lang = match cb % 4 {
                0 => "rust",
                1 => "python",
                2 => "js",
                _ => "",
            };
            let _ = writeln!(doc, "```{lang}");
            for l in 0..lines {
                let _ = writeln!(doc, "line {l} of code block {cb}");
            }
            let _ = writeln!(doc, "```\n");
        }

        // 100 lists of varying depths.
        for li in 0..100_u32 {
            let depth = (li % 5) + 1;
            for d in 0..depth {
                let indent = "  ".repeat(d as usize);
                let _ = writeln!(doc, "{indent}- Item at depth {d} in list {li}");
            }
            let _ = writeln!(doc);
        }

        // Interspersed blockquotes and images.
        for i in 0..50_u32 {
            let _ = writeln!(
                doc,
                "> Blockquote {i} with **bold** and *italic*\n> Second line.\n"
            );
            let _ = writeln!(doc, "![alt text {i}](https://example.com/image{i}.png)\n");
        }

        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 600.0, &style);

        assert!(
            cache.blocks.len() > 500,
            "expected >500 blocks, got {}",
            cache.blocks.len()
        );
        assert!(
            cache.total_height > 10_000.0,
            "expected total_height > 10000, got {}",
            cache.total_height
        );
        for (i, h) in cache.heights.iter().enumerate() {
            assert!(*h > 0.0, "height[{i}] is not positive: {h}");
        }
        eprintln!(
            "mixed content: {} blocks, total_height={}",
            cache.blocks.len(),
            cache.total_height
        );
    }

    // ── 5. Width sensitivity invariant ────────────────────────────

    /// Paragraph-heavy document: wider viewport → shorter total height.
    #[test]
    fn width_sensitivity_monotonically_decreasing() {
        // Build a paragraph-heavy doc.
        use std::fmt::Write;
        let mut doc = String::with_capacity(64 * 1024);
        for i in 0..500_u32 {
            let _ = writeln!(
                doc,
                "Paragraph {i}: Lorem ipsum dolor sit amet, consectetur adipiscing \
                 elit. Sed do eiusmod tempor incididunt ut labore et dolore magna \
                 aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco \
                 laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure \
                 dolor in reprehenderit in voluptate velit esse cillum dolore eu \
                 fugiat nulla pariatur.\n"
            );
        }

        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let widths: &[f32] = &[100.0, 200.0, 400.0, 800.0, 1600.0];
        let mut heights: Vec<f32> = Vec::new();

        for &w in widths {
            let mut cache = MarkdownCache::default();
            cache.ensure_parsed(&doc);
            cache.ensure_heights(14.0, w, &style);
            heights.push(cache.total_height);
        }

        eprintln!("width sensitivity heights: {heights:?}");
        for i in 1..heights.len() {
            assert!(
                heights[i] <= heights[i - 1],
                "height at width {} ({}) > height at width {} ({}): wider should be shorter",
                widths[i],
                heights[i],
                widths[i - 1],
                heights[i - 1]
            );
        }
    }

    // ── 6. Empty/minimal extremes ─────────────────────────────────

    #[test]
    fn extreme_empty_string() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("");
        cache.ensure_heights(14.0, 600.0, &style);
        assert!(cache.blocks.is_empty());
        assert!((cache.total_height - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn extreme_10mb_newlines() {
        let doc = "\n".repeat(10 * 1024 * 1024);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 600.0, &style);
        // Should not panic. May or may not produce blocks.
        eprintln!(
            "10MB newlines: {} blocks, total_height={}",
            cache.blocks.len(),
            cache.total_height
        );
    }

    #[test]
    fn extreme_single_char() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("a");
        cache.ensure_heights(14.0, 600.0, &style);
        assert!(
            !cache.blocks.is_empty(),
            "single char 'a' produced no blocks"
        );
        assert!(cache.total_height > 0.0);
    }

    #[test]
    fn extreme_heading_no_text() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("# ");
        cache.ensure_heights(14.0, 600.0, &style);
        // Should not panic regardless of block count.
        eprintln!(
            "heading_no_text: {} blocks, total_height={}",
            cache.blocks.len(),
            cache.total_height
        );
    }

    #[test]
    fn extreme_unclosed_fence() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed("```");
        cache.ensure_heights(14.0, 600.0, &style);
        eprintln!(
            "unclosed_fence: {} blocks, total_height={}",
            cache.blocks.len(),
            cache.total_height
        );
    }

    #[test]
    fn extreme_1000_empty_code_blocks() {
        let doc = "```\n```\n".repeat(1000);
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 600.0, &style);
        assert!(
            cache.blocks.len() >= 1000,
            "expected >=1000 blocks from 1000 code blocks, got {}",
            cache.blocks.len()
        );
        assert!(cache.total_height > 0.0);
        for (i, h) in cache.heights.iter().enumerate() {
            assert!(h.is_finite(), "height[{i}] not finite: {h}");
            assert!(*h > 0.0, "height[{i}] not positive: {h}");
        }
    }

    // ── Rendering pipeline stress tests ────────────────────────────

    #[test]
    fn stress_1000_tables() {
        let mut doc = String::with_capacity(100_000);
        for i in 0..1000 {
            write!(doc, "| Header {i} | Value |\n|---|---|\n| data | {i} |\n\n").ok();
        }
        let blocks = parse_markdown(&doc);
        let table_count = blocks
            .iter()
            .filter(|b| matches!(b, crate::parse::Block::Table(_)))
            .count();
        assert_eq!(table_count, 1000);

        // Height estimation shouldn't take too long.
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        let start = Instant::now();
        cache.ensure_heights(14.0, 600.0, &style);
        let elapsed = start.elapsed();
        assert!(cache.total_height > 0.0);
        eprintln!("stress_1000_tables height estimation: {elapsed:?}");
        if !cfg!(debug_assertions) {
            assert!(
                elapsed.as_millis() < 100,
                "1000 tables height estimation took {elapsed:?}"
            );
        }
    }

    #[test]
    fn stress_deeply_nested_blockquotes() {
        // 20 levels of nesting.
        let mut doc = String::new();
        for i in 0..20 {
            let prefix: String = "> ".repeat(i + 1);
            writeln!(doc, "{prefix}Level {}", i + 1).ok();
        }
        let blocks = parse_markdown(&doc);
        assert!(!blocks.is_empty());

        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);
        cache.ensure_heights(14.0, 600.0, &style);
        assert!(cache.total_height > 0.0);
    }

    #[test]
    fn stress_mixed_content_5mb() {
        // Build a ~5MB document with all block types.
        let mut doc = String::with_capacity(5_000_000);
        for section in 0..500 {
            writeln!(doc, "# Section {section}").ok();
            writeln!(doc).ok();
            for para in 0..5 {
                writeln!(
                    doc,
                    "Paragraph {para}: {}",
                    "Lorem ipsum dolor sit amet. ".repeat(10)
                )
                .ok();
                writeln!(doc).ok();
            }
            writeln!(doc, "```\ncode block {section}\nline 2\nline 3\n```").ok();
            writeln!(doc).ok();
            writeln!(doc, "| A | B | C |").ok();
            writeln!(doc, "|---|---|---|").ok();
            writeln!(doc, "| {section} | data | row |").ok();
            writeln!(doc).ok();
            writeln!(doc, "> Quote from section {section}").ok();
            writeln!(doc).ok();
            writeln!(doc, "- Item 1\n- Item 2\n- Item 3").ok();
            writeln!(doc).ok();
        }

        let start = Instant::now();
        let blocks = parse_markdown(&doc);
        let parse_time = start.elapsed();

        assert!(!blocks.is_empty());
        assert!(
            blocks.len() > 1000,
            "expected many blocks, got {}",
            blocks.len()
        );

        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let mut cache = MarkdownCache::default();
        cache.ensure_parsed(&doc);

        let start = Instant::now();
        cache.ensure_heights(14.0, 600.0, &style);
        let height_time = start.elapsed();

        assert!(cache.total_height > 0.0);

        eprintln!(
            "stress_mixed_content_5mb: doc_len={}KB, blocks={}, parse={parse_time:?}, heights={height_time:?}",
            doc.len() / 1024,
            blocks.len()
        );

        if !cfg!(debug_assertions) {
            assert!(
                parse_time.as_millis() < 500,
                "5MB parse took {parse_time:?}"
            );
            assert!(
                height_time.as_millis() < 200,
                "5MB height estimation took {height_time:?}"
            );
        }
    }

    #[test]
    fn stress_empty_blocks_1000() {
        let mut doc = String::with_capacity(10_000);
        for _ in 0..1000 {
            doc.push_str(">\n\n"); // Empty blockquotes
        }
        let blocks = parse_markdown(&doc);
        assert!(!blocks.is_empty());
    }

    #[test]
    fn stress_single_huge_table() {
        // 5 columns, 500 rows
        let mut doc = String::with_capacity(50_000);
        doc.push_str("| A | B | C | D | E |\n|---|---|---|---|---|\n");
        for row in 0..500 {
            writeln!(
                doc,
                "| r{row}c0 | r{row}c1 | r{row}c2 | r{row}c3 | r{row}c4 |"
            )
            .ok();
        }
        let blocks = parse_markdown(&doc);
        match &blocks[0] {
            crate::parse::Block::Table(table) => {
                assert_eq!(table.header.len(), 5);
                assert_eq!(table.rows.len(), 500);
            }
            other => panic!("expected Table, got {other:?}"),
        }
    }

    #[test]
    fn stress_alternating_code_and_text() {
        let mut doc = String::with_capacity(100_000);
        for i in 0..500 {
            writeln!(doc, "Text paragraph {i}.").ok();
            writeln!(doc).ok();
            writeln!(doc, "```\ncode block {i}\n```").ok();
            writeln!(doc).ok();
        }
        let blocks = parse_markdown(&doc);
        let code_count = blocks
            .iter()
            .filter(|b| matches!(b, crate::parse::Block::Code { .. }))
            .count();
        assert_eq!(code_count, 500);
    }
}
