#![forbid(unsafe_code)]

use eframe::egui;

use crate::markdown_fence::{FenceState, consume_fence_delimiter};

/// Index into a small, pre-built array of `TextFormat` values so that
/// section construction only needs a cheap copy of the index, not a
/// full `TextFormat::clone()` for the common batched-run path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FmtIdx {
    Base,
    Weak,
    InlineCode,
    Heading(usize),
    Table,
}

pub fn heading_color(visuals: &egui::Visuals, level: usize, color_mode: bool) -> egui::Color32 {
    if !color_mode {
        return visuals.hyperlink_color;
    }

    let palette = if visuals.dark_mode {
        &rustdown_md::DARK_HEADING_COLORS
    } else {
        &rustdown_md::LIGHT_HEADING_COLORS
    };
    palette[level.saturating_sub(1).min(palette.len() - 1)]
}

/// Resolve a `FmtIdx` to a cloned `TextFormat`.
fn resolve_format(
    idx: FmtIdx,
    base: &egui::TextFormat,
    weak: &egui::TextFormat,
    inline_code: &egui::TextFormat,
    heading_formats: &[egui::TextFormat; 6],
    table_format: &egui::TextFormat,
) -> egui::TextFormat {
    match idx {
        FmtIdx::Base => base.clone(),
        FmtIdx::Weak => weak.clone(),
        FmtIdx::InlineCode => inline_code.clone(),
        FmtIdx::Heading(level) => heading_formats[level - 1].clone(),
        FmtIdx::Table => table_format.clone(),
    }
}

/// Push a section directly into the job using pre-set byte ranges.
/// Avoids the per-append string concatenation of `LayoutJob::append`.
fn push_section(
    job: &mut egui::text::LayoutJob,
    range: std::ops::Range<usize>,
    format: egui::TextFormat,
) {
    if range.start < range.end {
        job.sections.push(egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: range,
            format,
        });
    }
}

#[must_use]
pub fn markdown_layout_job(
    style: &egui::Style,
    visuals: &egui::Visuals,
    source: &str,
    heading_color_mode: bool,
) -> egui::text::LayoutJob {
    // Set the text once; all sections reference byte ranges into it.
    let mut job = egui::text::LayoutJob {
        text: source.to_owned(),
        sections: Vec::with_capacity(source.len() / 40 + 8),
        ..Default::default()
    };

    let base_font = egui::TextStyle::Body.resolve(style);
    let code_font = egui::TextStyle::Monospace.resolve(style);
    let base = egui::TextFormat::simple(base_font, visuals.text_color());
    let weak = {
        let mut w = base.clone();
        w.color = visuals.weak_text_color();
        w
    };
    let heading_scales = rustdown_md::HEADING_FONT_SCALES;
    let heading_formats = std::array::from_fn(|idx| {
        let mut format = base.clone();
        format.font_id.size *= heading_scales[idx];
        format.color = heading_color(visuals, idx + 1, heading_color_mode);
        format
    });

    let mut inline_code = base.clone();
    inline_code.font_id = code_font.clone();
    inline_code.background = visuals.faint_bg_color;

    let mut table_format = base.clone();
    table_format.font_id = code_font;
    table_format.color = visuals.weak_text_color();

    // Pending run: consecutive lines with the same format are batched into
    // a single section to minimize section count.
    let mut pending_fmt: Option<FmtIdx> = None;
    let mut pending_start: usize = 0;
    let mut pending_end: usize = 0;

    let flush =
        |job: &mut egui::text::LayoutJob, fmt: &Option<FmtIdx>, start: usize, end: usize| {
            if let Some(idx) = *fmt {
                let format = resolve_format(
                    idx,
                    &base,
                    &weak,
                    &inline_code,
                    &heading_formats,
                    &table_format,
                );
                push_section(job, start..end, format);
            }
        };

    let mut in_fence: Option<FenceState> = None;
    let bytes = source.as_bytes();
    let mut offset = 0usize;

    while offset < bytes.len() {
        let line_start = offset;
        let line_end =
            memchr::memchr(b'\n', &bytes[offset..]).map_or(bytes.len(), |pos| offset + pos + 1);
        let line = &source[line_start..line_end];
        offset = line_end;

        if consume_fence_delimiter(line, &mut in_fence) {
            flush(&mut job, &pending_fmt, pending_start, pending_end);
            pending_fmt = None;
            push_section(
                &mut job,
                line_start..line_end,
                resolve_format(
                    FmtIdx::Weak,
                    &base,
                    &weak,
                    &inline_code,
                    &heading_formats,
                    &table_format,
                ),
            );
            continue;
        }
        if in_fence.is_some() {
            let kind = FmtIdx::InlineCode;
            if pending_fmt != Some(kind) {
                flush(&mut job, &pending_fmt, pending_start, pending_end);
                pending_fmt = Some(kind);
                pending_start = line_start;
            }
            pending_end = line_end;
            continue;
        }

        let trimmed = line.trim_start();
        // CommonMark: ATX headings allow 0-3 spaces of indentation only.
        let indent = line.len() - trimmed.len();
        let indent_ok = indent <= 3 && line.as_bytes()[..indent].iter().all(|&b| b == b' ');
        if indent_ok && trimmed.as_bytes().first() == Some(&b'#') {
            let level = trimmed.bytes().take_while(|b| *b == b'#').count();
            if (1..=6).contains(&level) && trimmed.as_bytes().get(level) == Some(&b' ') {
                let kind = FmtIdx::Heading(level);
                if pending_fmt != Some(kind) {
                    flush(&mut job, &pending_fmt, pending_start, pending_end);
                    pending_fmt = Some(kind);
                    pending_start = line_start;
                }
                pending_end = line_end;
                continue;
            }
        }

        // Table rows: lines starting with `|` (pipe-delimited).
        if trimmed.as_bytes().first() == Some(&b'|') {
            let kind = FmtIdx::Table;
            if pending_fmt != Some(kind) {
                flush(&mut job, &pending_fmt, pending_start, pending_end);
                pending_fmt = Some(kind);
                pending_start = line_start;
            }
            pending_end = line_end;
            continue;
        }

        if memchr::memchr(b'`', line.as_bytes()).is_none() {
            let kind = FmtIdx::Base;
            if pending_fmt != Some(kind) {
                flush(&mut job, &pending_fmt, pending_start, pending_end);
                pending_fmt = Some(kind);
                pending_start = line_start;
            }
            pending_end = line_end;
            continue;
        }

        // Line contains inline code - emit individual sections for each fragment.
        flush(&mut job, &pending_fmt, pending_start, pending_end);
        pending_fmt = None;
        emit_inline_code_sections(
            &mut job,
            line_start,
            line_end,
            line,
            &base,
            &weak,
            &inline_code,
        );
    }

    flush(&mut job, &pending_fmt, pending_start, pending_end);
    job
}

/// Emit layout sections for a line that contains inline backtick code spans.
/// Uses `FmtIdx` to defer format resolution, matching the batched-run path.
fn emit_inline_code_sections(
    job: &mut egui::text::LayoutJob,
    line_start: usize,
    line_end: usize,
    line: &str,
    base: &egui::TextFormat,
    weak: &egui::TextFormat,
    inline_code: &egui::TextFormat,
) {
    let mut pos = line_start;
    let line_bytes = line.as_bytes();
    let mut i = 0;

    // Helper: push a section with a specific format reference (avoids per-section clone).
    let push =
        |job: &mut egui::text::LayoutJob, range: std::ops::Range<usize>, fmt: &egui::TextFormat| {
            if range.start < range.end {
                job.sections.push(egui::text::LayoutSection {
                    leading_space: 0.0,
                    byte_range: range,
                    format: fmt.clone(),
                });
            }
        };

    while let Some(tick_rel) = memchr::memchr(b'`', &line_bytes[i..]) {
        let tick_i = i + tick_rel;
        push(job, pos..line_start + tick_i, base);
        if let Some(close) = memchr::memchr(b'`', &line_bytes[tick_i + 1..]) {
            let tick_start = line_start + tick_i;
            let code_start = tick_start + 1;
            let code_end = code_start + close;
            let tick_end = code_end + 1;
            push(job, tick_start..code_start, weak);
            push(job, code_start..code_end, inline_code);
            push(job, code_end..tick_end, weak);
            pos = tick_end;
            i = tick_i + 1 + close + 1;
        } else {
            push(job, line_start + tick_i..line_start + tick_i + 1, weak);
            push(job, line_start + tick_i + 1..line_end, base);
            pos = line_end;
            i = line_bytes.len();
        }
    }
    push(job, pos..line_end, base);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section_for_snippet<'a>(
        job: &'a egui::text::LayoutJob,
        snippet: &str,
    ) -> &'a egui::text::LayoutSection {
        let start = job.text.find(snippet);
        assert!(
            start.is_some(),
            "Expected snippet '{snippet}' in rendered text"
        );
        let start = start.unwrap_or_else(|| unreachable!());
        let end = start + snippet.len();
        let section = job
            .sections
            .iter()
            .find(|section| section.byte_range.start <= start && section.byte_range.end >= end);
        assert!(
            section.is_some(),
            "Expected section for snippet '{snippet}'"
        );
        section.unwrap_or_else(|| unreachable!())
    }

    #[test]
    fn markdown_layout_job_marks_fence_content_and_delimiters() {
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "~~~azurecli\naz aks list\n~~~\n~~~bash\necho hi\n~~~\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
        let code_section = section_for_snippet(&job, "az aks list");
        assert_eq!(code_section.format.background, visuals.faint_bg_color);
        assert_eq!(
            code_section.format.font_id,
            egui::TextStyle::Monospace.resolve(&style)
        );
        let fence_section = section_for_snippet(&job, "~~~bash");
        assert_eq!(fence_section.format.color, visuals.weak_text_color());
    }

    #[test]
    fn markdown_layout_job_color_mode_applies_level_specific_heading_colors() {
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "# Top\n## Next\n";
        let default_job = markdown_layout_job(&style, &visuals, source, false);
        let color_job = markdown_layout_job(&style, &visuals, source, true);

        let default_h1 = section_for_snippet(&default_job, "Top");
        let default_h2 = section_for_snippet(&default_job, "Next");
        let color_h1 = section_for_snippet(&color_job, "Top");
        let color_h2 = section_for_snippet(&color_job, "Next");

        assert_eq!(default_h1.format.color, visuals.hyperlink_color);
        assert_eq!(default_h2.format.color, visuals.hyperlink_color);
        assert_ne!(color_h1.format.color, visuals.hyperlink_color);
        assert_ne!(color_h2.format.color, visuals.hyperlink_color);
        assert_ne!(color_h1.format.color, color_h2.format.color);
    }

    #[test]
    fn inline_code_renders_backtick_delimiters_and_content() {
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "Use `foo` here\n";
        let job = markdown_layout_job(&style, &visuals, source, false);

        let code = section_for_snippet(&job, "foo");
        assert_eq!(code.format.background, visuals.faint_bg_color);
        assert_eq!(
            code.format.font_id,
            egui::TextStyle::Monospace.resolve(&style)
        );
        // Backtick delimiters should be styled as weak text.
        let open_tick = job
            .sections
            .iter()
            .find(|s| s.byte_range.start == 4 && s.byte_range.end == 5);
        assert!(open_tick.is_some(), "Expected section for opening backtick");
        assert_eq!(
            open_tick.unwrap_or_else(|| unreachable!()).format.color,
            visuals.weak_text_color()
        );
    }

    #[test]
    fn unmatched_backtick_does_not_crash_and_emits_weak_section() {
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "text `orphan\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
        // The unmatched backtick should produce a weak-text section.
        let tick = job
            .sections
            .iter()
            .find(|s| s.byte_range.start == 5 && s.byte_range.end == 6);
        assert!(tick.is_some(), "Expected section for unmatched backtick");
        assert_eq!(
            tick.unwrap_or_else(|| unreachable!()).format.color,
            visuals.weak_text_color()
        );
    }

    #[test]
    fn light_mode_heading_colors_differ_from_dark() {
        let dark = egui::Visuals::dark();
        let light = egui::Visuals::light();
        let dark_h1 = heading_color(&dark, 1, true);
        let light_h1 = heading_color(&light, 1, true);
        assert_ne!(dark_h1, light_h1);
    }

    #[test]
    fn heading_color_clamps_at_palette_bounds() {
        let visuals = egui::Visuals::dark();
        let h0 = heading_color(&visuals, 0, true);
        let h1 = heading_color(&visuals, 1, true);
        let h7 = heading_color(&visuals, 7, true);
        let h6 = heading_color(&visuals, 6, true);
        // Level 0 (out of range) saturates to first palette entry.
        assert_eq!(h0, h1);
        // Level 7 (out of range) saturates to last palette entry.
        assert_eq!(h7, h6);
    }

    #[test]
    fn all_six_heading_levels_get_unique_colors() {
        let visuals = egui::Visuals::dark();
        let colors: Vec<_> = (1..=6)
            .map(|level| heading_color(&visuals, level, true))
            .collect();
        for i in 0..colors.len() {
            for j in (i + 1)..colors.len() {
                assert_ne!(
                    colors[i],
                    colors[j],
                    "Heading levels {} and {} share a color",
                    i + 1,
                    j + 1
                );
            }
        }
    }

    #[test]
    fn plain_text_produces_single_base_section() {
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "just plain text\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
        assert_eq!(job.sections.len(), 1);
        assert_eq!(job.sections[0].byte_range, 0..source.len());
        assert_eq!(job.sections[0].format.color, visuals.text_color());
    }

    #[test]
    fn empty_source_produces_no_sections() {
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let job = markdown_layout_job(&style, &visuals, "", false);
        assert!(job.sections.is_empty());
    }

    #[test]
    fn multiple_inline_code_spans_in_one_line() {
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "`a` and `b`\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
        let a_sec = section_for_snippet(&job, "a");
        let b_sec = section_for_snippet(&job, "b");
        assert_eq!(a_sec.format.background, visuals.faint_bg_color);
        assert_eq!(b_sec.format.background, visuals.faint_bg_color);
    }

    #[test]
    fn table_rows_get_monospace_weak_format() {
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "| A | B |\n|---|---|\n| 1 | 2 |\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
        let header_sec = section_for_snippet(&job, "| A | B |");
        assert_eq!(header_sec.format.color, visuals.weak_text_color());
        assert_eq!(
            header_sec.format.font_id,
            egui::TextStyle::Monospace.resolve(&style)
        );
    }

    #[test]
    fn table_rows_are_batched_into_single_section() {
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "| A |\n| B |\n| C |\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
        // All three pipe-lines should be batched into one Table section.
        assert_eq!(job.sections.len(), 1);
        assert_eq!(job.sections[0].byte_range, 0..source.len());
    }

    #[test]
    fn backtick_fence_with_backtick_in_info_not_highlighted_as_fence() {
        // CommonMark: backtick fence info strings must not contain backticks.
        // Such lines should be treated as normal text, not fence delimiters.
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "```foo`bar\nsome text\n```\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
        // The second line "some text" should be base-styled (not monospace
        // code), because the first line is not a valid fence opener.
        let text_sec = section_for_snippet(&job, "some text");
        assert_eq!(
            text_sec.format.color,
            visuals.text_color(),
            "content after invalid backtick fence should be base-styled, not code",
        );
    }

    // ── Indentation-limit tests ─────────────────────────────────────

    #[test]
    fn four_space_indented_heading_not_styled_as_heading() {
        // CommonMark: 4+ spaces makes it an indented code block, not a heading.
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "    # Not a heading\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
        assert_eq!(job.sections.len(), 1);
        assert_eq!(
            job.sections[0].format.color,
            visuals.text_color(),
            "4-space indented heading should be base text, not heading"
        );
    }

    #[test]
    fn three_space_indented_heading_styled_as_heading() {
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "   # Heading\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
        let heading_sec = section_for_snippet(&job, "Heading");
        // Heading format uses a different color than body text.
        assert_ne!(
            heading_sec.format.color,
            visuals.text_color(),
            "3-space indented heading should be styled as heading"
        );
    }

    #[test]
    fn four_space_indented_fence_not_highlighted_as_fence() {
        // 4-space indented fences are indented code blocks per CommonMark.
        // The content line "    code" (no backticks) must NOT get the fenced
        // code block styling (monospace + faint background).
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "    ```rust\n    code\n    ```\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
        let code_sec = section_for_snippet(&job, "code");
        assert_ne!(
            code_sec.format.background, visuals.faint_bg_color,
            "4-space indented content should not be fenced-code-styled"
        );
    }

    // ── Edge-case tests ─────────────────────────────────────────────

    #[test]
    fn heading_with_inline_code_styled_entirely_as_heading() {
        // Heading detection takes priority: the whole line (including
        // backticks) is styled as heading, not split into inline code.
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "# Title with `code`\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
        let sec = section_for_snippet(&job, "Title with `code`");
        assert_ne!(
            sec.format.color,
            visuals.text_color(),
            "heading line with backticks should be heading-styled, not base"
        );
        // Font size should be scaled up for H1.
        assert!(
            sec.format.font_id.size > egui::TextStyle::Body.resolve(&style).size,
            "H1 font should be larger than body"
        );
    }

    #[test]
    fn fence_with_long_language_name_highlighted_as_fence() {
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "```really-long-language-name\ncontent\n```\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
        let fence_sec = section_for_snippet(&job, "```really-long-language-name");
        assert_eq!(
            fence_sec.format.color,
            visuals.weak_text_color(),
            "fence with unusual language should be weak-text styled"
        );
        let content_sec = section_for_snippet(&job, "content");
        assert_eq!(
            content_sec.format.background, visuals.faint_bg_color,
            "content inside fenced block should have code background"
        );
    }

    #[test]
    fn double_backtick_inline_code_does_not_panic() {
        // The highlighter matches single backticks greedily, so ``double``
        // is parsed as two empty code spans around "double" (not a
        // CommonMark double-backtick span). Verify no panic and full
        // byte coverage.
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "Use ``double`` backticks\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
        let covered: usize = job
            .sections
            .iter()
            .map(|s| s.byte_range.end - s.byte_range.start)
            .sum();
        assert_eq!(
            covered,
            source.len(),
            "all bytes should be covered by sections"
        );
    }
}
