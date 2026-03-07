use super::helpers::*;

#[test]
fn integration_list_depth_in_blockquote() {
    // Standalone list vs list inside blockquote should have
    // similar rendered proportions (no double indent).
    let standalone = "- Alpha\n- Beta\n- Gamma\n";
    let in_quote = "> - Alpha\n> - Beta\n> - Gamma\n";

    let (_, _, h_standalone) = headless_render_at_width(standalone, 800.0);
    let (_, _, h_in_quote) = headless_render_at_width(in_quote, 800.0);

    // Blockquote adds bar + margin + bottom spacing, but the list
    // content should not be significantly taller from over-indent.
    let ratio = h_in_quote / h_standalone;
    assert!(
        ratio > 0.8 && ratio < 2.0,
        "list in blockquote: ratio={ratio:.2} (standalone={h_standalone:.1}, in_quote={h_in_quote:.1})"
    );
}

#[test]
fn integration_nested_list_in_blockquote_height() {
    let md = "> - Parent\n>   - Child A\n>   - Child B\n>     - Grandchild\n";
    let style = dark_style();

    let blocks = crate::parse::parse_markdown(md);
    let estimated = estimate_block_height(&blocks[0], 14.0, 600.0, &style);
    let (_, _, rendered) = headless_render_at_width(md, 616.0); // 600 + margin

    assert!(estimated > 0.0 && rendered > 0.0);
    // Estimate should be within 5× of rendered (generous for headless).
    let ratio = estimated / rendered;
    assert!(
        ratio > 0.2 && ratio < 5.0,
        "nested list in blockquote: est/render ratio={ratio:.2} (est={estimated:.1}, rendered={rendered:.1})"
    );
}

#[test]
fn integration_wide_table_scrollbar() {
    let wide_md = "| A | B | C | D | E | F | G | H | I | J |\n|---|---|---|---|---|---|---|---|---|---|\n| 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 |\n";
    let narrow_md = "| A | B |\n|---|---|\n| 1 | 2 |\n";

    let style = dark_style();
    let wide_blocks = crate::parse::parse_markdown(wide_md);
    let narrow_blocks = crate::parse::parse_markdown(narrow_md);

    let h_wide = estimate_block_height(&wide_blocks[0], 14.0, 400.0, &style);
    let h_narrow = estimate_block_height(&narrow_blocks[0], 14.0, 400.0, &style);

    // Wide table at 400px should need scrollbar (10 cols × ~36px > 400px).
    // So its estimate should be > narrow table's estimate.
    assert!(
        h_wide > h_narrow,
        "wide table ({h_wide:.1}) should be taller than narrow ({h_narrow:.1}) at 400px width"
    );
}

#[test]
fn integration_spacing_consistency() {
    let md = "\
Paragraph one.

Paragraph two.

```rust
fn code() {}
```

> Blockquote.

- List item.

| A | B |
|---|---|
| 1 | 2 |

---

Another paragraph.
";
    let (blocks, height) = headless_render(md);
    // Should have: 2 paragraphs, code, quote, list, table, HR, paragraph = 8+
    assert!(
        blocks.len() >= 7,
        "expected ≥7 blocks, got {}",
        blocks.len()
    );
    assert!(height > 100.0);

    // Height estimation should be reasonable.
    let style = dark_style();
    let mut cache = MarkdownCache::default();
    cache.ensure_parsed(md);
    cache.ensure_heights(14.0, 900.0, &style);

    // cum_y must be monotonically increasing.
    for i in 1..cache.cum_y.len() {
        assert!(
            cache.cum_y[i] >= cache.cum_y[i - 1],
            "cum_y must be monotonic at block {i}"
        );
    }
}

#[test]
fn integration_verification_doc_height_accuracy() {
    let md = include_str!("../../../../rustdown-gui/src/bundled/verification.md");

    let style = dark_colored_style();
    let mut cache = MarkdownCache::default();
    cache.ensure_parsed(md);
    cache.ensure_heights(14.0, 900.0, &style);

    // All heights should be positive.
    for (i, h) in cache.heights.iter().enumerate() {
        assert!(
            *h > 0.0 || matches!(cache.blocks[i], Block::Heading { .. }),
            "block {i}: height should be positive, got {h}"
        );
    }

    // Total height should be substantial (813 lines of markdown).
    assert!(
        cache.total_height > 5000.0,
        "verification doc total height: {:.0} (expected >5000)",
        cache.total_height
    );

    // Render scrollable at multiple scroll positions — no panic.
    let ctx = headless_ctx();
    let viewer = MarkdownViewer::new("verify_test");
    for &frac in &[0.0, 0.25, 0.5, 0.75, 1.0] {
        let scroll_to = Some(cache.total_height * frac);
        let _ = ctx.run(raw_input_1024x768(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.show_scrollable(ui, &mut cache, &style, md, scroll_to);
            });
        });
    }
}

#[test]
fn integration_verification_doc_block_types() {
    let md = include_str!("../../../../rustdown-gui/src/bundled/verification.md");
    let blocks = crate::parse::parse_markdown(md);

    let has = |pred: fn(&Block) -> bool| blocks.iter().any(pred);
    assert!(has(|b| matches!(b, Block::Heading { .. })), "headings");
    assert!(has(|b| matches!(b, Block::Paragraph(_))), "paragraphs");
    assert!(has(|b| matches!(b, Block::Code { .. })), "code blocks");
    assert!(has(|b| matches!(b, Block::Quote(_))), "blockquotes");
    assert!(
        has(|b| matches!(b, Block::UnorderedList(_))),
        "unordered lists"
    );
    assert!(
        has(|b| matches!(b, Block::OrderedList { .. })),
        "ordered lists"
    );
    assert!(has(|b| matches!(b, Block::Table(_))), "tables");
    assert!(
        has(|b| matches!(b, Block::ThematicBreak)),
        "thematic breaks"
    );
    assert!(has(|b| matches!(b, Block::Image { .. })), "images");
}

#[test]
fn integration_height_accuracy_multi_width() {
    let cases: &[(&str, &str)] = &[
        ("heading+para", "# Title\n\nSome paragraph text.\n"),
        ("nested_lists", "- A\n  - B\n    - C\n  - D\n- E\n"),
        (
            "blockquote_with_list",
            "> - Item 1\n> - Item 2\n>   - Nested\n",
        ),
        (
            "table_3col",
            "| A | B | C |\n|---|---|---|\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |\n",
        ),
        (
            "code_block",
            "```rust\nfn main() {\n    let x = 42;\n}\n```\n",
        ),
    ];

    for &(label, md) in cases {
        for &width in &[400.0_f32, 800.0, 1200.0] {
            let (_, est, rendered) = headless_render_at_width(md, width);
            assert!(
                est > 0.0 && rendered > 0.0,
                "{label}@{width}: est={est}, rendered={rendered}"
            );
            // Ratio should be between 0.1 and 10.0 (generous for headless).
            let ratio = est / rendered;
            assert!(
                ratio > 0.1 && ratio < 10.0,
                "{label}@{width}: ratio={ratio:.2} (est={est:.1}, rendered={rendered:.1})"
            );
        }
    }
}
