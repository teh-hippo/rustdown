use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

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

const fn heading_level_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_basic_headings() {
        let md = "# Title\n\nSome text.\n\n## Section A\n\nMore text.\n\n### Sub-section\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 3);
        assert_eq!(headings[0].level, 1);
        assert_eq!(headings[0].label(md), "Title");
        assert_eq!(headings[1].level, 2);
        assert_eq!(headings[1].label(md), "Section A");
        assert_eq!(headings[2].level, 3);
        assert_eq!(headings[2].label(md), "Sub-section");
    }

    #[test]
    fn extract_headings_with_inline_code() {
        let md = "# Hello `world`\n\n## Plain\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 2);
        // With byte-range approach, label spans from "Hello" to end of "world"
        // including the backtick-wrapped region in the source
        let label = headings[0].label(md);
        assert!(label.contains("Hello"), "got: {label}");
    }

    #[test]
    fn extract_headings_byte_offsets_increase() {
        let md = "# A\n\n## B\n\n### C\n\n#### D\n";
        let headings = extract_headings(md);
        for w in headings.windows(2) {
            assert!(w[0].byte_offset < w[1].byte_offset);
        }
    }

    #[test]
    fn extract_empty_heading_is_skipped() {
        let md = "# \n\n## Real\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].label(md), "Real");
    }

    #[test]
    fn extract_no_headings() {
        let md = "Just a paragraph.\n\nAnother one.\n";
        let headings = extract_headings(md);
        assert!(headings.is_empty());
    }

    #[test]
    fn active_heading_at_start() {
        let md = "# A\n\n## B\n\n### C\n";
        let headings = extract_headings(md);
        // Before any heading
        assert_eq!(active_heading_index(&headings, 4, 0), Some(0));
    }

    #[test]
    fn active_heading_between() {
        let md = "# A\n\nParagraph.\n\n## B\n\nMore text.\n\n### C\n";
        let headings = extract_headings(md);
        // Position after "## B" but before "### C"
        let b_offset = headings[1].byte_offset;
        let c_offset = headings[2].byte_offset;
        let mid = b_offset.midpoint(c_offset);
        assert_eq!(active_heading_index(&headings, 4, mid), Some(1));
    }

    #[test]
    fn active_heading_respects_max_depth() {
        let md = "# A\n\n#### D\n\n## B\n";
        let headings = extract_headings(md);
        // With max_depth=2, the #### D is ignored
        let d_offset = headings[1].byte_offset;
        assert_eq!(active_heading_index(&headings, 2, d_offset), Some(0));
    }

    #[test]
    fn active_heading_before_any() {
        let md = "Some text.\n\n# A\n";
        let headings = extract_headings(md);
        assert_eq!(active_heading_index(&headings, 4, 0), None);
    }

    #[test]
    fn extract_all_six_levels() {
        let md = "# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 6);
        for (i, h) in headings.iter().enumerate() {
            assert_eq!(usize::from(h.level), i + 1);
        }
    }

    #[test]
    fn setext_headings_extracted() {
        let md = "Title\n=====\n\nSection\n-------\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].level, 1);
        assert_eq!(headings[0].label(md), "Title");
        assert_eq!(headings[1].level, 2);
        assert_eq!(headings[1].label(md), "Section");
    }

    #[test]
    fn heading_entry_is_compact() {
        assert!(
            std::mem::size_of::<HeadingEntry>() <= 24,
            "HeadingEntry should be compact, got {} bytes",
            std::mem::size_of::<HeadingEntry>()
        );
    }

    // ── Edge-case stress tests ──────────────────────────────────────

    #[test]
    fn extract_empty_document() {
        let headings = extract_headings("");
        assert!(headings.is_empty());
    }

    #[test]
    fn extract_only_headings_no_body() {
        let md = "# A\n## B\n### C\n#### D\n##### E\n###### F\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 6);
        for (i, h) in headings.iter().enumerate() {
            assert_eq!(usize::from(h.level), i + 1);
            assert!(!h.label(md).is_empty());
        }
    }

    #[test]
    fn extract_unicode_headings() {
        let md = "# café\n## über\n### 日本語テスト\n#### 🦀 Rust\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 4);
        assert_eq!(headings[0].label(md), "café");
        assert_eq!(headings[1].label(md), "über");
        assert_eq!(headings[2].label(md), "日本語テスト");
        assert_eq!(headings[3].label(md), "🦀 Rust");
    }

    #[test]
    fn extract_heading_with_only_whitespace() {
        // Heading with only spaces should be skipped (trimmed to empty).
        let md = "#    \n## Real\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].label(md), "Real");
    }

    #[test]
    fn active_heading_empty_entries() {
        assert_eq!(active_heading_index(&[], 4, 0), None);
        assert_eq!(active_heading_index(&[], 4, 1000), None);
    }

    #[test]
    fn active_heading_single_entry() {
        let md = "# Only\n";
        let headings = extract_headings(md);
        assert_eq!(active_heading_index(&headings, 4, 0), Some(0));
        assert_eq!(active_heading_index(&headings, 4, 100), Some(0));
    }

    #[test]
    fn active_heading_exact_match_on_last() {
        let md = "# A\n\n## B\n";
        let headings = extract_headings(md);
        let last_offset = headings.last().map_or(0, |h| h.byte_offset);
        assert_eq!(
            active_heading_index(&headings, 4, last_offset),
            Some(headings.len() - 1)
        );
    }

    #[test]
    fn active_heading_max_depth_zero_returns_none() {
        let md = "# A\n## B\n";
        let headings = extract_headings(md);
        // max_depth=0 should match nothing since all levels are >= 1.
        assert_eq!(active_heading_index(&headings, 0, 0), None);
    }

    #[test]
    fn extract_many_headings_offsets_strictly_increase() {
        let mut md = String::new();
        for i in 0..200 {
            md.push_str("## Heading ");
            md.push_str(&i.to_string());
            md.push_str("\n\nSome body text.\n\n");
        }
        let headings = extract_headings(&md);
        assert_eq!(headings.len(), 200);
        for w in headings.windows(2) {
            assert!(w[0].byte_offset < w[1].byte_offset);
        }
    }

    #[test]
    fn active_heading_binary_search_all_positions() {
        let md = "# A\n\ntext\n\n## B\n\ntext\n\n### C\n\ntext\n";
        let headings = extract_headings(md);
        // Walk through every byte position and verify active_heading_index
        // returns a valid index or None consistently.
        let mut last_idx: Option<usize> = None;
        for pos in 0..md.len() {
            let idx = active_heading_index(&headings, 6, pos);
            if let Some(i) = idx {
                assert!(i < headings.len());
                // Active index should only advance forward.
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
        // nav_outline::extract_headings skips empty headings.
        // MarkdownCache::heading_y also skips empty headings (by design).
        // Verify the two agree on heading count so ordinal-based nav-scroll
        // lookups stay aligned.
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

            // Count how many ordinals heading_y accepts (bounded to
            // a safe maximum since we're testing small inputs).
            let render_count = (0..100)
                .take_while(|&ord| cache.heading_y(ord).is_some())
                .count();
            assert_eq!(
                outline.len(),
                render_count,
                "heading ordinal count mismatch for: {md:?}\n  nav_outline={}, heading_y accepts={}",
                outline.len(),
                render_count
            );
        }
    }
}
