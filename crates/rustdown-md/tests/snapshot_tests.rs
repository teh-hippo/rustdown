// Visual rendering integration tests for the Markdown preview renderer.
//
// Each test renders markdown headlessly through egui, then verifies
// structural properties (block types, heights, positions, spans) that
// correspond to correct visual output.
//
// ## Reference images
//
// Each test section is paired with a `.md` file in `test-assets/visual-refs/`.
// To generate reference screenshots:
//
//   1. `cargo run -p rustdown`
//   2. Open the `.md` file (Ctrl+O or drag-and-drop)
//   3. Switch to Preview mode (Ctrl+Enter)
//   4. Take a screenshot and save alongside the `.md` file
//
// Reference images serve as visual documentation for humans and agents.
// The automated tests verify the underlying structural correctness.
//
// ## Running
//
//   cargo test -p rustdown-md --test snapshot_tests

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::float_cmp
)]

use rustdown_md::{MarkdownCache, MarkdownStyle, MarkdownViewer};

// ── Test infrastructure ────────────────────────────────────────────

fn headless_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |_| {});
    ctx
}

fn raw_input(width: f32, height: f32) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(width, height),
        )),
        ..Default::default()
    }
}

struct RenderResult {
    blocks: Vec<rustdown_md::Block>,
    total_height: f32,
    heights: Vec<f32>,
    cum_y: Vec<f32>,
}

/// Parse and render markdown, returning full layout data.
fn render(source: &str) -> RenderResult {
    render_at(source, 800.0, 600.0)
}

fn render_at(source: &str, width: f32, height: f32) -> RenderResult {
    let ctx = headless_ctx();
    let mut cache = MarkdownCache::default();
    let style = MarkdownStyle::colored(&egui::Visuals::dark());
    let viewer = MarkdownViewer::new("snap");

    let _ = ctx.run(raw_input(width, height), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            viewer.show(ui, &mut cache, &style, source);
        });
    });

    let body_size = 14.0;
    let wrap_width = width - 32.0; // approximate panel margins
    cache.ensure_heights(body_size, wrap_width, &style);

    RenderResult {
        blocks: cache.blocks.clone(),
        total_height: cache.total_height,
        heights: cache.heights.clone(),
        cum_y: cache.cum_y.clone(),
    }
}

const fn block_name(block: &rustdown_md::Block) -> &'static str {
    match block {
        rustdown_md::Block::Heading { .. } => "Heading",
        rustdown_md::Block::Paragraph(_) => "Paragraph",
        rustdown_md::Block::Code { .. } => "Code",
        rustdown_md::Block::Quote(_) => "Quote",
        rustdown_md::Block::UnorderedList(_) => "UnorderedList",
        rustdown_md::Block::OrderedList { .. } => "OrderedList",
        rustdown_md::Block::ThematicBreak => "ThematicBreak",
        rustdown_md::Block::Table(_) => "Table",
        rustdown_md::Block::Image { .. } => "Image",
    }
}

// ── 1. Headings ────────────────────────────────────────────────────
// Reference: test-assets/visual-refs/01-headings.md
//
// Expected rendering:
//   - 6 headings in decreasing font size (H1 largest, H6 smallest)
//   - Each heading uses a distinct colour from the Dracula palette
//   - Headings are vertically separated with consistent spacing

#[test]
fn headings_all_levels() {
    let r = render(
        "# Heading 1\n## Heading 2\n### Heading 3\n\
         #### Heading 4\n##### Heading 5\n###### Heading 6\n",
    );

    assert_eq!(r.blocks.len(), 6, "should produce 6 heading blocks");

    let mut prev_height = f32::MAX;
    for (i, block) in r.blocks.iter().enumerate() {
        match block {
            rustdown_md::Block::Heading { level, text } => {
                assert_eq!(*level as usize, i + 1, "heading {i} level mismatch");
                assert!(
                    !text.text.is_empty(),
                    "heading {i} text should not be empty"
                );
            }
            _ => panic!("block {i} should be Heading, got {}", block_name(block)),
        }
        assert!(
            r.heights[i] <= prev_height + 2.0,
            "heading {i} height {} should not exceed previous {}",
            r.heights[i],
            prev_height
        );
        prev_height = r.heights[i];
    }

    for i in 1..r.cum_y.len() {
        assert!(
            r.cum_y[i] > r.cum_y[i - 1],
            "cum_y[{i}] should exceed cum_y[{}]",
            i - 1
        );
    }
}

#[test]
fn heading_empty_skipped() {
    let r = render("## \n\nSome text\n");
    assert!(r.heights[0] < 1.0, "empty heading should have ~0 height");
}

// ── 2. Inline Styles ──────────────────────────────────────────────
// Reference: test-assets/visual-refs/02-inline-styles.md
//
// Expected rendering:
//   - Bold text appears brighter/darker than body text
//   - Italic text is slanted
//   - Strikethrough has a horizontal line through the text
//   - Inline code uses monospace font with a background tint
//   - Combined styles (bold+italic) work correctly

#[test]
fn inline_styles_spans_cover_text() {
    let r = render("**Bold**, *italic*, ~~strike~~, `code`, and ***bold italic***.\n");
    assert_eq!(r.blocks.len(), 1, "should be one paragraph");
    if let rustdown_md::Block::Paragraph(st) = &r.blocks[0] {
        assert!(!st.spans.is_empty(), "should have styled spans");
        assert_eq!(st.spans[0].start, 0, "first span starts at 0");
        let last = st.spans.last().unwrap();
        assert_eq!(
            last.end as usize,
            st.text.len(),
            "last span should end at text length"
        );
        for w in st.spans.windows(2) {
            assert_eq!(
                w[0].end, w[1].start,
                "gap between spans at byte {}",
                w[0].end
            );
        }
        let has_bold = st.spans.iter().any(|s| s.style.strong());
        let has_italic = st.spans.iter().any(|s| s.style.emphasis());
        let has_strike = st.spans.iter().any(|s| s.style.strikethrough());
        let has_code = st.spans.iter().any(|s| s.style.code());
        assert!(has_bold, "should have bold span");
        assert!(has_italic, "should have italic span");
        assert!(has_strike, "should have strikethrough span");
        assert!(has_code, "should have code span");
    } else {
        panic!("expected Paragraph");
    }
}

// ── 3. Links ──────────────────────────────────────────────────────
// Reference: test-assets/visual-refs/03-links.md
//
// Expected rendering:
//   - Links appear in hyperlink colour (blue-ish) with underline
//   - Multiple links in one line are each independently clickable
//   - Links with styled content (bold link) render correctly

#[test]
fn links_parsed_with_urls() {
    let r = render("Visit [Rust](https://rust-lang.org/) and [GitHub](https://github.com).\n");
    if let rustdown_md::Block::Paragraph(st) = &r.blocks[0] {
        assert!(st.has_links, "paragraph should have links");
        assert_eq!(st.links.len(), 2, "should have 2 link URLs");
        assert!(
            st.links[0].contains("rust-lang"),
            "first link should be Rust"
        );
        assert!(
            st.links[1].contains("github"),
            "second link should be GitHub"
        );
    } else {
        panic!("expected Paragraph");
    }
}

// ── 4. Block Quotes ───────────────────────────────────────────────
// Reference: test-assets/visual-refs/04-blockquotes.md
//
// Expected rendering:
//   - Left vertical bar indicator in weak colour
//   - Content indented past the bar (no text-bar overlap)
//   - Nested quotes show stacking bars at increasing indent
//   - Styled content within quotes renders correctly

#[test]
fn blockquote_structure() {
    let r = render("> Simple quote text.\n");
    assert_eq!(r.blocks.len(), 1);
    match &r.blocks[0] {
        rustdown_md::Block::Quote(inner) => {
            assert_eq!(inner.len(), 1, "one inner paragraph");
        }
        _ => panic!("expected Quote"),
    }
}

#[test]
fn blockquote_nested_depth() {
    let r = render("> L1\n>\n> > L2\n> >\n> > > L3\n> > >\n> > > > L4\n");
    assert_eq!(r.blocks.len(), 1, "one top-level quote");
    fn max_depth(blocks: &[rustdown_md::Block]) -> usize {
        blocks
            .iter()
            .map(|b| match b {
                rustdown_md::Block::Quote(inner) => 1 + max_depth(inner),
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }
    let depth = max_depth(&r.blocks);
    assert!(depth >= 4, "should nest at least 4 levels, got {depth}");
}

#[test]
fn blockquote_has_positive_height() {
    let r = render("> Quoted text here.\n");
    assert!(
        r.heights[0] > 10.0,
        "blockquote should have meaningful height, got {}",
        r.heights[0]
    );
}

// ── 5. Lists ──────────────────────────────────────────────────────
// Reference: test-assets/visual-refs/05-lists.md
//
// Expected rendering:
//   - Unordered: bullet markers at increasing indent
//   - Ordered: sequential numbers right-aligned in a column
//   - Task lists: checkbox markers replacing bullets
//   - Child blocks (paragraphs, code) indented with item text

#[test]
fn unordered_list_items() {
    let r = render("- Alpha\n- Beta\n- Gamma\n");
    assert_eq!(r.blocks.len(), 1);
    match &r.blocks[0] {
        rustdown_md::Block::UnorderedList(items) => {
            assert_eq!(items.len(), 3);
            assert_eq!(items[0].content.text, "Alpha");
            assert_eq!(items[1].content.text, "Beta");
            assert_eq!(items[2].content.text, "Gamma");
        }
        _ => panic!("expected UnorderedList"),
    }
}

#[test]
fn ordered_list_numbering() {
    let r = render("1. First\n2. Second\n3. Third\n");
    match &r.blocks[0] {
        rustdown_md::Block::OrderedList { start, items } => {
            assert_eq!(*start, 1);
            assert_eq!(items.len(), 3);
        }
        _ => panic!("expected OrderedList"),
    }
}

#[test]
fn task_list_checked_state() {
    let r = render("- [x] Done\n- [ ] Not done\n- [x] Also done\n");
    match &r.blocks[0] {
        rustdown_md::Block::UnorderedList(items) => {
            assert_eq!(items[0].checked, Some(true));
            assert_eq!(items[1].checked, Some(false));
            assert_eq!(items[2].checked, Some(true));
        }
        _ => panic!("expected UnorderedList"),
    }
}

#[test]
fn nested_list_depth() {
    let r = render("- L0\n  - L1\n    - L2\n      - L3\n");
    fn list_depth(blocks: &[rustdown_md::Block]) -> usize {
        blocks
            .iter()
            .map(|b| match b {
                rustdown_md::Block::UnorderedList(items) => {
                    1 + items
                        .iter()
                        .map(|i| list_depth(&i.children))
                        .max()
                        .unwrap_or(0)
                }
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }
    assert!(list_depth(&r.blocks) >= 4, "should nest 4 levels");
}

#[test]
fn list_with_child_blocks() {
    let r = render(
        "1. Item text\n\n   Child paragraph.\n\n   ```rust\n   fn code() {}\n   ```\n\n2. Next\n",
    );
    match &r.blocks[0] {
        rustdown_md::Block::OrderedList { items, .. } => {
            assert!(
                !items[0].children.is_empty(),
                "first item should have child blocks"
            );
            let has_para = items[0]
                .children
                .iter()
                .any(|b| matches!(b, rustdown_md::Block::Paragraph(_)));
            let has_code = items[0]
                .children
                .iter()
                .any(|b| matches!(b, rustdown_md::Block::Code { .. }));
            assert!(has_para, "should have child paragraph");
            assert!(has_code, "should have child code block");
        }
        _ => panic!("expected OrderedList"),
    }
}

// ── 6. Code Blocks ────────────────────────────────────────────────
// Reference: test-assets/visual-refs/06-code-blocks.md
//
// Expected rendering:
//   - Monospace font at 0.9x body size
//   - Dark background with rounded corners
//   - Language label above the code content
//   - Horizontal scroll for long lines

#[test]
fn code_block_language_and_content() {
    let r = render("```rust\nfn main() {\n    println!(\"Hello\");\n}\n```\n");
    match &r.blocks[0] {
        rustdown_md::Block::Code { language, code } => {
            assert_eq!(&**language, "rust");
            assert!(code.contains("fn main()"));
            assert!(code.contains("println!"));
        }
        _ => panic!("expected Code"),
    }
}

#[test]
fn code_block_no_language() {
    let r = render("```\nplain text\n```\n");
    match &r.blocks[0] {
        rustdown_md::Block::Code { language, code } => {
            assert!(language.is_empty(), "no language tag");
            assert_eq!(code.trim(), "plain text");
        }
        _ => panic!("expected Code"),
    }
}

#[test]
fn code_block_height_scales_with_lines() {
    let short = render("```\nline1\n```\n");
    let long = render("```\nline1\nline2\nline3\nline4\nline5\n```\n");
    assert!(
        long.heights[0] > short.heights[0],
        "5-line block ({}) should be taller than 1-line ({})",
        long.heights[0],
        short.heights[0]
    );
}

// ── 7. Tables ─────────────────────────────────────────────────────
// Reference: test-assets/visual-refs/07-tables.md
//
// Expected rendering:
//   - Grid with striped rows
//   - Header row in strengthened (bolder) colour
//   - Column alignment: left/center/right per alignment markers
//   - Horizontal scroll for wide tables

#[test]
fn table_structure() {
    let r = render(
        "| Name  | Role      |\n\
         |-------|-----------|\n\
         | Alice | Developer |\n\
         | Bob   | Designer  |\n",
    );
    match &r.blocks[0] {
        rustdown_md::Block::Table(table) => {
            assert_eq!(table.header.len(), 2, "2 header columns");
            assert_eq!(table.rows.len(), 2, "2 data rows");
            assert_eq!(table.header[0].text, "Name");
            assert_eq!(table.rows[0][1].text, "Developer");
        }
        _ => panic!("expected Table"),
    }
}

#[test]
fn table_alignment_parsed() {
    let r = render("| L | C | R |\n|:--|:-:|--:|\n| a | b | c |\n");
    match &r.blocks[0] {
        rustdown_md::Block::Table(table) => {
            use rustdown_md::Alignment;
            assert_eq!(table.alignments[0], Alignment::Left);
            assert_eq!(table.alignments[1], Alignment::Center);
            assert_eq!(table.alignments[2], Alignment::Right);
        }
        _ => panic!("expected Table"),
    }
}

#[test]
fn table_height_scales_with_rows() {
    let small = render("| A |\n|---|\n| 1 |\n");
    let large = render("| A |\n|---|\n| 1 |\n| 2 |\n| 3 |\n| 4 |\n| 5 |\n| 6 |\n| 7 |\n| 8 |\n");
    assert!(
        large.heights[0] > small.heights[0],
        "8-row table should be taller than 1-row"
    );
}

// ── 8. Horizontal Rules ───────────────────────────────────────────
// Reference: test-assets/visual-refs/08-horizontal-rules.md
//
// Expected rendering:
//   - Full-width horizontal line
//   - All three syntaxes (---, ***, ___) render identically
//   - Vertical spacing above and below the rule

#[test]
fn thematic_breaks_identical() {
    let r = render("---\n\n***\n\n___\n");
    let breaks: Vec<_> = r
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b, rustdown_md::Block::ThematicBreak))
        .collect();
    assert_eq!(breaks.len(), 3, "3 thematic breaks");
    let h0 = r.heights[breaks[0].0];
    for &(idx, _) in &breaks[1..] {
        assert!(
            (r.heights[idx] - h0).abs() < 0.1,
            "all HRs should have same height"
        );
    }
}

// ── 9. Images ─────────────────────────────────────────────────────
// Reference: test-assets/visual-refs/09-images.md

#[test]
fn image_block_structure() {
    let r = render("![Alt text](https://example.com/img.png)\n");
    match &r.blocks[0] {
        rustdown_md::Block::Image { url, alt } => {
            assert_eq!(&**alt, "Alt text");
            assert!(url.contains("example.com"));
        }
        _ => panic!("expected Image"),
    }
}

// ── 10. Mixed Content ─────────────────────────────────────────────
// Reference: test-assets/visual-refs/10-mixed-content.md
//
// Expected rendering:
//   - Heading -> paragraph -> code -> blockquote -> list sequence
//   - Consistent spacing between block types
//   - No visual glitches at block transitions

#[test]
fn mixed_content_block_sequence() {
    let r =
        render("## Title\n\nA paragraph.\n\n```rust\ncode()\n```\n\n> A quote.\n\n- List item\n");
    let types: Vec<_> = r.blocks.iter().map(block_name).collect();
    assert_eq!(
        types,
        vec!["Heading", "Paragraph", "Code", "Quote", "UnorderedList"],
        "block sequence mismatch"
    );
    assert!(
        r.total_height > 50.0,
        "mixed content should have meaningful height"
    );
    for i in 1..r.cum_y.len() {
        assert!(r.cum_y[i] >= r.cum_y[i - 1], "blocks should not overlap");
    }
}

#[test]
fn mixed_heading_then_table() {
    let r =
        render("### Data Summary\n| Metric | Value |\n|--------|-------|\n| Users  | 1,234 |\n");
    assert_eq!(block_name(&r.blocks[0]), "Heading");
    assert_eq!(block_name(&r.blocks[1]), "Table");
    assert!(r.heights[0] > 10.0, "heading should include bottom spacing");
}

// ── 11. Smart Punctuation ─────────────────────────────────────────
// Reference: test-assets/visual-refs/11-smart-punctuation.md

#[test]
fn smart_punctuation_converted() {
    let r = render("\"quotes\" and 'single' and em---dash and en--dash and dots...\n");
    if let rustdown_md::Block::Paragraph(st) = &r.blocks[0] {
        assert!(
            st.text.contains('\u{201c}') || st.text.contains('\u{201d}'),
            "should have curly double quotes, got: {}",
            st.text
        );
        assert!(
            st.text.contains('\u{2014}'),
            "should have em-dash, got: {}",
            st.text
        );
    }
}

// ── 12. Viewport Culling ──────────────────────────────────────────

#[test]
fn large_document_height_positive() {
    let mut doc = String::with_capacity(10_000);
    for i in 0..100 {
        use std::fmt::Write;
        let _ = write!(doc, "## Section {i}\n\nParagraph {i} content.\n\n");
    }
    let r = render(&doc);
    assert!(r.blocks.len() >= 200, "should have many blocks");
    assert!(r.total_height > 1000.0, "should have large total height");
    for i in 1..r.cum_y.len() {
        assert!(r.cum_y[i] >= r.cum_y[i - 1]);
    }
}

// ── 13. Bundled Documents ─────────────────────────────────────────

#[test]
fn bundled_demo_renders() {
    let demo = include_str!("../../rustdown-gui/src/bundled/demo.md");
    let r = render(demo);
    assert!(
        r.blocks.len() > 20,
        "demo should produce many blocks, got {}",
        r.blocks.len()
    );
    assert!(
        r.total_height > 500.0,
        "demo should have substantial height"
    );
    let types: std::collections::HashSet<_> = r.blocks.iter().map(block_name).collect();
    assert!(types.contains("Heading"), "demo should have headings");
    assert!(types.contains("Paragraph"), "demo should have paragraphs");
    assert!(types.contains("Code"), "demo should have code blocks");
    assert!(types.contains("Table"), "demo should have tables");
    assert!(types.contains("Quote"), "demo should have blockquotes");
    assert!(types.contains("ThematicBreak"), "demo should have HRs");
    assert!(types.contains("Image"), "demo should have images");
}

#[test]
fn bundled_verification_renders() {
    let verification = include_str!("../../rustdown-gui/src/bundled/verification.md");
    let r = render(verification);
    assert!(
        r.blocks.len() > 50,
        "verification should produce many blocks, got {}",
        r.blocks.len()
    );
    assert!(
        r.total_height > 2000.0,
        "verification should have large total height"
    );
}

// ── 14. Edge Cases ────────────────────────────────────────────────

#[test]
fn empty_document() {
    let r = render("");
    assert!(r.blocks.is_empty());
    assert!(r.total_height < 1.0);
}

#[test]
fn whitespace_only_document() {
    let r = render("   \n\n   \n");
    assert!(r.total_height < 50.0);
}

#[test]
fn dense_inline_styles() {
    let r = render("**B** *I* ~~S~~ `C` **B** *I* ~~S~~ `C` **B** *I* ~~S~~ `C`\n");
    if let rustdown_md::Block::Paragraph(st) = &r.blocks[0] {
        assert!(
            st.spans.len() >= 12,
            "should have many spans for dense styling"
        );
    }
}
