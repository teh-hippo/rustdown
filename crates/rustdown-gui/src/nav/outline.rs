use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use rustdown_md::heading_level_to_u8;

/// A single heading extracted from a Markdown document.
///
/// Labels are stored as byte ranges into the source text to avoid per-heading
/// heap allocations.  Use [`HeadingEntry::label`] to resolve to `&str`.
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
}

impl HeadingEntry {
    /// Resolve the heading label from the source text.
    #[inline]
    pub fn label<'a>(&self, source: &'a str) -> &'a str {
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

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let lvl = heading_level_to_u8(level);
                in_heading = Some((lvl, range.start));
                label_start = 0;
                label_end = 0;
                label_has_content = false;
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, byte_offset)) = in_heading.take()
                    && label_has_content
                {
                    let slice = source.get(label_start..label_end).unwrap_or("");
                    let trimmed = slice.trim();
                    if !trimmed.is_empty() {
                        let trim_off = label_start + (slice.len() - slice.trim_start().len());
                        let trim_len = trimmed.len();
                        entries.push(HeadingEntry {
                            level,
                            byte_offset,
                            label_start: trim_off as u32,
                            label_len: trim_len.min(u16::MAX as usize) as u16,
                        });
                    }
                }
            }
            Event::Text(_) | Event::Code(_) if in_heading.is_some() => {
                if !label_has_content {
                    label_start = range.start;
                    label_has_content = true;
                }
                label_end = range.end;
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

        // HeadingEntry is compact.
        assert!(std::mem::size_of::<HeadingEntry>() <= 24);
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
