# Visual Reference Files

Reference markdown files for the snapshot tests in
`crates/rustdown-md/tests/snapshot_tests.rs`.

## How it works

Each numbered `.md` file corresponds to a test section in `snapshot_tests.rs`.
The automated tests verify **structural** correctness (block types, span
coverage, nesting depth, heights, positions). These reference files contain the
**exact same markdown** so you can open them in Rustdown and visually confirm
that the rendering matches the structural assertions.

Together, structural tests + visual references give full coverage:
tests catch regressions in the parse/layout pipeline, while screenshots
catch rendering issues (colours, fonts, spacing) that structure alone cannot.

## Generating reference screenshots

1. `cargo run -p rustdown`
2. Open a `.md` file from this directory (Ctrl+O or drag-and-drop)
3. Switch to Preview mode (Ctrl+Enter)
4. Take a screenshot (OS shortcut or Rustdown export if available)
5. Save the screenshot alongside the `.md` file with the same base name
   (e.g. `01-headings.png` next to `01-headings.md`)

## Updating after rendering changes

When the renderer changes intentionally:

1. Re-run the structural tests: `cargo test -p rustdown-md --test snapshot_tests`
2. If tests pass, regenerate screenshots using the steps above
3. Commit both the updated snapshots and any test changes together

## File index

| File | Tests | What to check |
|------|-------|---------------|
| `01-headings.md` | `headings_all_levels` | 6 decreasing sizes, H1/H2 rules, palette colours |
| `02-inline-styles.md` | `inline_styles_spans_cover_text` | Bold, italic, strikethrough, code, combined |
| `03-links.md` | `links_parsed_with_urls` | Hyperlink colour, underline, clickable |
| `04-blockquotes.md` | `blockquote_structure`, `blockquote_nested_depth` | Left bar, indentation, 4-level nesting |
| `05-lists.md` | `unordered_list_items`, `ordered_list_numbering`, `task_list_checked_state`, `nested_list_depth`, `list_with_child_blocks` | Bullets, numbers, checkboxes, nesting, child blocks |
| `06-code-blocks.md` | `code_block_language_and_content`, `code_block_no_language` | Monospace, background, language label, scroll |
| `07-tables.md` | `table_structure`, `table_alignment_parsed` | Grid, striped rows, header styling, alignment |
| `08-horizontal-rules.md` | `thematic_breaks_identical` | Full-width lines, identical rendering |
| `09-images.md` | `image_block_structure` | Alt text, image placeholder |
| `10-mixed-content.md` | `mixed_content_block_sequence`, `mixed_heading_then_table` | Block transitions, consistent spacing |
| `11-smart-punctuation.md` | `smart_punctuation_converted` | Curly quotes, em-dash, en-dash, ellipsis |
