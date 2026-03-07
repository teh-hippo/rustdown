use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use rustdown_md::heading_level_to_u8;

/// A single heading extracted from a Markdown document.
///
/// For simple headings (text and inline code only), the label is stored
/// as a byte range into the source text, avoiding per-heading heap
/// allocations.  For headings that contain links or other inline markup
/// whose raw syntax would leak into the byte range, a separate
/// plain-text label is built from the event content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadingEntry {
    /// Heading depth (1 = H1, 2 = H2, … 6 = H6).
    pub level: u8,
    /// Byte offset of the heading start in the source text.
    pub byte_offset: usize,
    /// Start of the plain-text label within the source.
    label_start: u32,
    /// Length of the plain-text label in bytes.
    label_len: u16,
    /// If the heading contains links, the label is built from event
    /// content and stored here (source byte ranges would include URLs).
    label_owned: Option<Box<str>>,
}

impl HeadingEntry {
    /// Resolve the heading label, preferring the owned plain-text label
    /// (for headings with links) and falling back to a source byte range.
    #[inline]
    pub fn label<'a>(&'a self, source: &'a str) -> &'a str {
        if let Some(ref owned) = self.label_owned {
            return owned;
        }
        let start = self.label_start as usize;
        let end = start + self.label_len as usize;
        source.get(start..end).unwrap_or("")
    }
}

/// Extract all headings from `source` markdown text.
///
/// Produces a flat list ordered by document position.
/// Each entry records the heading level, its plain-text label, and the byte
/// offset where the heading markup begins.
#[allow(clippy::cast_possible_truncation)] // heading offsets < 4GB, label len < 65K
pub fn extract_headings(source: &str) -> Vec<HeadingEntry> {
    // Only enable the options needed for heading detection — avoids
    // expensive table/footnote parsing that extract_headings never uses.
    let parser = Parser::new_ext(source, Options::ENABLE_HEADING_ATTRIBUTES);

    let mut entries = Vec::new();
    let mut in_heading: Option<(u8, usize)> = None; // (level, byte_offset)
    let mut label_start: usize = 0;
    let mut label_end: usize = 0;
    let mut label_has_content = false;
    // Track whether the heading contains links (whose raw markdown
    // syntax would leak into a source byte-range label).
    let mut has_link = false;
    // Collected plain text fragments for headings with links.
    let mut label_buf = String::new();

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let lvl = heading_level_to_u8(level);
                in_heading = Some((lvl, range.start));
                label_start = 0;
                label_end = 0;
                label_has_content = false;
                has_link = false;
                label_buf.clear();
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, byte_offset)) = in_heading.take()
                    && label_has_content
                {
                    if has_link {
                        // Build label from collected text fragments.
                        let trimmed = label_buf.trim();
                        if !trimmed.is_empty() {
                            entries.push(HeadingEntry {
                                level,
                                byte_offset,
                                label_start: 0,
                                label_len: 0,
                                label_owned: Some(trimmed.into()),
                            });
                        }
                    } else {
                        // No links — use the efficient source byte range.
                        let slice = source.get(label_start..label_end).unwrap_or("");
                        let trimmed = slice.trim();
                        if !trimmed.is_empty() {
                            let trim_off = label_start + slice.find(trimmed).unwrap_or_default();
                            let trim_len = trimmed.len();
                            entries.push(HeadingEntry {
                                level,
                                byte_offset,
                                label_start: trim_off as u32,
                                label_len: trim_len.min(u16::MAX as usize) as u16,
                                label_owned: None,
                            });
                        }
                    }
                }
            }
            Event::Text(t) if in_heading.is_some() => {
                if !label_has_content {
                    label_start = range.start;
                    label_has_content = true;
                }
                label_end = range.end;
                if has_link {
                    label_buf.push_str(&t);
                }
            }
            Event::Code(c) if in_heading.is_some() => {
                if !label_has_content {
                    label_start = range.start;
                    label_has_content = true;
                }
                label_end = range.end;
                if has_link {
                    label_buf.push('`');
                    label_buf.push_str(&c);
                    label_buf.push('`');
                }
            }
            Event::Start(Tag::Link { .. }) if in_heading.is_some() => {
                if !has_link {
                    // Retroactively fill label_buf with text collected so far.
                    has_link = true;
                    if label_has_content {
                        let prior = source.get(label_start..label_end).unwrap_or("");
                        label_buf.push_str(prior);
                    }
                }
            }
            _ => {}
        }
    }

    entries
}

/// Find the index of the heading that is "active" given a byte position.
///
/// Returns the index of the last heading whose `byte_offset` ≤ `position`,
/// considering only entries with `level ≤ max_depth`.  Returns `None` when
/// no heading precedes `position`.
///
/// Uses binary search on sorted `byte_offset` values (O(log n) to find the
/// neighbourhood, then a short backward scan for the depth filter).
pub fn active_heading_index(
    entries: &[HeadingEntry],
    max_depth: u8,
    position: usize,
) -> Option<usize> {
    if entries.is_empty() {
        return None;
    }
    // Binary search: find the rightmost entry with byte_offset ≤ position.
    let upper = match entries.binary_search_by_key(&position, |e| e.byte_offset) {
        Ok(i) => i,
        Err(0) => return None, // all headings are after position
        Err(i) => i - 1,
    };
    // Walk backwards from `upper` to find the first entry matching depth.
    (0..=upper).rev().find(|&i| entries[i].level <= max_depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_headings_covers_basic_edge_and_unicode_cases() {
        // Basic headings.
        let md = "# Title\n\nSome text.\n\n## Section A\n\nMore text.\n\n### Sub-section\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 3);
        assert_eq!((headings[0].level, headings[0].label(md)), (1, "Title"));
        assert_eq!((headings[1].level, headings[1].label(md)), (2, "Section A"));
        assert_eq!(
            (headings[2].level, headings[2].label(md)),
            (3, "Sub-section")
        );

        // Inline code in heading.
        let md = "# Hello `world`\n\n## Plain\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 2);
        assert!(headings[0].label(md).contains("Hello"));

        // Byte offsets strictly increase.
        let md = "# A\n\n## B\n\n### C\n\n#### D\n";
        let headings = extract_headings(md);
        for w in headings.windows(2) {
            assert!(w[0].byte_offset < w[1].byte_offset);
        }

        // Empty heading is skipped; whitespace-only heading too.
        for md in ["# \n\n## Real\n", "#    \n## Real\n"] {
            let headings = extract_headings(md);
            assert_eq!(headings.len(), 1, "md={md:?}");
            assert_eq!(headings[0].label(md), "Real");
        }

        // No headings.
        assert!(extract_headings("Just a paragraph.\n").is_empty());
        assert!(extract_headings("").is_empty());

        // All six levels.
        let md = "# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 6);
        for (i, h) in headings.iter().enumerate() {
            assert_eq!(usize::from(h.level), i + 1);
        }

        // Setext headings.
        let md = "Title\n=====\n\nSection\n-------\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 2);
        assert_eq!((headings[0].level, headings[0].label(md)), (1, "Title"));
        assert_eq!((headings[1].level, headings[1].label(md)), (2, "Section"));

        // Unicode headings.
        let md = "# café\n## über\n### 日本語テスト\n#### 🦀 Rust\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 4);
        assert_eq!(headings[0].label(md), "café");
        assert_eq!(headings[3].label(md), "🦀 Rust");

        // Leading emoji survives trimming when it is the first visible glyph.
        let md = "#   🔬 Rustdown Verification Document\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].label(md), "🔬 Rustdown Verification Document");

        // HeadingEntry is compact.
        assert!(std::mem::size_of::<HeadingEntry>() <= 40);
    }

    #[test]
    fn active_heading_index_covers_all_boundary_cases() {
        // At start.
        let md = "# A\n\n## B\n\n### C\n";
        let headings = extract_headings(md);
        assert_eq!(active_heading_index(&headings, 4, 0), Some(0));

        // Between headings.
        let md = "# A\n\nParagraph.\n\n## B\n\nMore text.\n\n### C\n";
        let headings = extract_headings(md);
        let mid = headings[1].byte_offset.midpoint(headings[2].byte_offset);
        assert_eq!(active_heading_index(&headings, 4, mid), Some(1));

        // Respects max_depth.
        let md = "# A\n\n#### D\n\n## B\n";
        let headings = extract_headings(md);
        assert_eq!(
            active_heading_index(&headings, 2, headings[1].byte_offset),
            Some(0)
        );

        // Before any heading.
        let md = "Some text.\n\n# A\n";
        let headings = extract_headings(md);
        assert_eq!(active_heading_index(&headings, 4, 0), None);

        // Empty entries, single entry, max_depth=0.
        assert_eq!(active_heading_index(&[], 4, 0), None);
        assert_eq!(active_heading_index(&[], 4, 1000), None);
        let md = "# Only\n";
        let headings = extract_headings(md);
        assert_eq!(active_heading_index(&headings, 4, 0), Some(0));
        assert_eq!(active_heading_index(&headings, 4, 100), Some(0));
        let md = "# A\n## B\n";
        let headings = extract_headings(md);
        assert_eq!(active_heading_index(&headings, 0, 0), None);

        // Exact match on last heading.
        let md = "# A\n\n## B\n";
        let headings = extract_headings(md);
        let last = headings.last().map_or(0, |h| h.byte_offset);
        assert_eq!(
            active_heading_index(&headings, 4, last),
            Some(headings.len() - 1)
        );

        // Far beyond document.
        let md = "# A\n## B\n";
        let headings = extract_headings(md);
        assert_eq!(
            active_heading_index(&headings, 6, usize::MAX),
            Some(headings.len() - 1)
        );

        // Alternating H1/H6.
        let md = "# A\n###### deep\n# B\n###### deeper\n";
        let headings = extract_headings(md);
        assert_eq!(
            active_heading_index(&headings, 1, headings[1].byte_offset),
            Some(0)
        );
        assert_eq!(
            active_heading_index(&headings, 1, headings[3].byte_offset),
            Some(2)
        );
        assert_eq!(
            active_heading_index(&headings, 6, headings[1].byte_offset),
            Some(1)
        );
        assert_eq!(
            active_heading_index(&headings, 6, headings[3].byte_offset),
            Some(3)
        );

        // All same level.
        let md = "## A\n## B\n## C\n";
        let headings = extract_headings(md);
        for (i, h) in headings.iter().enumerate() {
            assert_eq!(active_heading_index(&headings, 6, h.byte_offset), Some(i));
        }

        // Exact offset and one-byte-before for each heading.
        let md = "# A\n\ntext\n\n## B\n\nmore\n\n### C\n";
        let headings = extract_headings(md);
        for (i, h) in headings.iter().enumerate() {
            assert_eq!(
                active_heading_index(&headings, 6, h.byte_offset),
                Some(i),
                "exact {i}"
            );
            if h.byte_offset > 0 {
                assert!(
                    active_heading_index(&headings, 6, h.byte_offset - 1).is_some_and(|v| v < i),
                    "before {i}"
                );
            }
        }
    }

    #[test]
    fn stress_extract_and_active_heading_sweep() {
        // 200 headings: offsets strictly increase.
        use std::fmt::Write;
        let mut md = String::new();
        for i in 0..200 {
            let _ = write!(md, "## Heading {i}\n\nBody.\n\n");
        }
        let headings = extract_headings(&md);
        assert_eq!(headings.len(), 200);
        for w in headings.windows(2) {
            assert!(w[0].byte_offset < w[1].byte_offset);
        }

        // Walk all byte positions: index only advances forward.
        let md = "# A\n\ntext\n\n## B\n\ntext\n\n### C\n\ntext\n";
        let headings = extract_headings(md);
        let mut last_idx: Option<usize> = None;
        for pos in 0..md.len() {
            let idx = active_heading_index(&headings, 6, pos);
            if let Some(i) = idx {
                assert!(i < headings.len());
                if let Some(prev) = last_idx {
                    assert!(i >= prev);
                }
            }
            last_idx = idx;
        }
    }

    /// Heading labels with links should show only the visible text, not
    /// the raw markdown URL syntax.  The label should be something like
    /// "Visit Rustdown on GitHub" — never including `[...](...url...)`.
    #[test]
    fn heading_label_with_link_excludes_url() {
        let md = "### Visit [Rustdown](https://github.com/teh-hippo/rustdown) on GitHub\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 1);
        let label = headings[0].label(md);
        assert!(
            !label.contains("https://"),
            "nav label should not contain URL, got: {label:?}"
        );
        assert!(
            !label.contains("]("),
            "nav label should not contain markdown link syntax, got: {label:?}"
        );
    }

    /// Heading labels with adjacent links should capture all visible text
    /// fragments without including URLs.
    #[test]
    fn heading_label_with_multiple_links_excludes_urls() {
        let md = "## [Alpha](https://a.com) and [Beta](https://b.com)\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 1);
        let label = headings[0].label(md);
        assert!(
            !label.contains("https://"),
            "multi-link heading label should not contain URLs, got: {label:?}"
        );
    }

    // ── Diagnostic: emphasis / bold markers leak into heading labels ──
    //
    // Bug: When a heading contains bold or italic text (but no links), the
    // source byte-range label includes the raw emphasis delimiters.
    // e.g. `## A **bold** heading` → label "A **bold** heading"
    //        instead of "A bold heading".
    //
    // Root cause: `extract_headings` uses `source[label_start..label_end]`
    // where label_start/label_end come from the first/last `Text` or `Code`
    // events. The inter-event bytes (emphasis markers) are included.
    //
    // Severity: Low visual. The nav panel shows raw `**`/`*`/`~~` markers.
    // File: crates/rustdown-gui/src/nav/outline.rs:89-103

    #[test]
    fn diag_heading_label_with_emphasis_leaks_markers() {
        // Bold in heading — source range includes **
        let md = "## A **bold** heading\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 1);
        let label = headings[0].label(md);
        // CURRENT BEHAVIOR: label contains raw markers
        // This test documents the bug — when fixed, flip the assertion.
        assert!(
            label.contains("**"),
            "BUG CONFIRMED: emphasis markers leak into label: {label:?}"
        );
        // Ideal: label should be "A bold heading"
    }

    #[test]
    fn diag_heading_label_with_italic_leaks_markers() {
        let md = "### An *italic* word\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 1);
        let label = headings[0].label(md);
        // CURRENT BEHAVIOR: label contains raw * markers
        assert!(
            label.contains('*'),
            "BUG CONFIRMED: italic markers leak into label: {label:?}"
        );
    }

    #[test]
    fn diag_heading_label_with_strikethrough_leaks_markers() {
        // pulldown_cmark needs ENABLE_STRIKETHROUGH for ~~
        // extract_headings uses ENABLE_HEADING_ATTRIBUTES only,
        // so ~~ is treated as literal text and doesn't leak.
        let md = "## A ~~struck~~ heading\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 1);
        let label = headings[0].label(md);
        // With only ENABLE_HEADING_ATTRIBUTES, ~~ is literal text
        // so the label should contain ~~ as literal text (not a bug).
        assert!(
            label.contains("~~"),
            "~~ is literal text with current parser options: {label:?}"
        );
    }

    #[test]
    fn diag_heading_label_bold_plus_code_leaks_markers() {
        // Heading with both bold and inline code (no links).
        let md = "## **Bold** and `code`\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 1);
        let label = headings[0].label(md);
        // Source range spans from first Text to last Code event,
        // includes ** markers between runs.
        assert!(
            label.contains("**"),
            "BUG CONFIRMED: bold markers leak when combined with code: {label:?}"
        );
    }

    // ── Diagnostic: bundled document heading coverage ──

    #[test]
    fn diag_bundled_demo_heading_count() {
        let demo = include_str!("../bundled/demo.md");
        let headings = extract_headings(demo);
        // demo.md has: ✨ Rustdown Feature Demo, Headings, Heading 1-6,
        // Inline Styles, Smart Punctuation, Block Quotes, Lists,
        // Unordered, Ordered, Task Lists, Code Blocks, Rust, Python,
        // JSON, Plain (No Language), Tables, Simple, Column Alignment,
        // Wide Table, Images, Horizontal Rules, Mixed Content, End
        assert!(
            headings.len() >= 20,
            "demo.md should have ≥20 headings, got {}",
            headings.len()
        );
        // All labels non-empty
        for (i, h) in headings.iter().enumerate() {
            let label = h.label(demo);
            assert!(
                !label.is_empty(),
                "demo.md heading {i} has empty label at offset {}",
                h.byte_offset
            );
        }
    }

    #[test]
    fn diag_bundled_verification_heading_count() {
        let verif = include_str!("../bundled/verification.md");
        let headings = extract_headings(verif);
        // verification.md is large with 16 top-level sections + many subsections
        assert!(
            headings.len() >= 60,
            "verification.md should have ≥60 headings, got {}",
            headings.len()
        );
        // All labels non-empty
        for (i, h) in headings.iter().enumerate() {
            let label = h.label(verif);
            assert!(
                !label.is_empty(),
                "verification.md heading {i} has empty label at offset {}",
                h.byte_offset
            );
        }
        // Levels 1-6 all present
        for level in 1..=6u8 {
            assert!(
                headings.iter().any(|h| h.level == level),
                "verification.md missing heading level {level}"
            );
        }
        assert_eq!(
            headings.first().map(|heading| heading.label(verif)),
            Some("🔬 Rustdown Verification Document"),
            "verification.md should keep the leading emoji in its first heading"
        );
    }

    #[test]
    fn diag_bundled_verification_no_setext_headings() {
        // verification.md exercises ATX headings only; setext headings
        // are tested in extract_headings_covers_basic_edge_and_unicode_cases
        // but NOT in the bundled verification document. This is a
        // coverage gap — setext headings should be added to verification.md.
        let verif = include_str!("../bundled/verification.md");
        let has_setext = verif.lines().any(|l: &str| {
            let trimmed = l.trim();
            (trimmed.chars().all(|c| c == '=') && trimmed.len() >= 3)
                || (trimmed.chars().all(|c| c == '-')
                    && trimmed.len() >= 3
                    && !trimmed.starts_with("---"))
        });
        // Documents the gap: verification.md has no setext headings
        assert!(
            !has_setext,
            "COVERAGE GAP: verification.md should exercise setext headings"
        );
    }

    // ── Cross-module: nav_outline ↔ render heading_y ordinal alignment ──

    #[test]
    fn heading_count_matches_render_heading_y_ordinals() {
        use rustdown_md::{MarkdownCache, MarkdownStyle};
        let style = MarkdownStyle::from_visuals(&eframe::egui::Visuals::dark());
        for md in [
            "# \n\n## Real\n",
            "# First\n\n## \n\n### Third\n",
            "# \n## \n### \n",
            "# A\n## B\n### C\n",
            "# \n\n## Real\n\n# \n\n## Also Real\n",
        ] {
            let outline = extract_headings(md);
            let mut cache = MarkdownCache::default();
            cache.ensure_parsed(md);
            cache.ensure_heights(14.0, 400.0, &style);
            let render_count = (0..100)
                .take_while(|&ord| cache.heading_y(ord).is_some())
                .count();
            assert_eq!(outline.len(), render_count, "mismatch for: {md:?}");
        }
    }
}
