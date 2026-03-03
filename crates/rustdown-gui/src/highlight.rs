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
}

fn heading_color(visuals: &egui::Visuals, level: usize, color_mode: bool) -> egui::Color32 {
    if !color_mode {
        return visuals.hyperlink_color;
    }

    let dark_palette = [
        egui::Color32::from_rgb(0xFF, 0xB8, 0x6C),
        egui::Color32::from_rgb(0x8B, 0xE9, 0xFD),
        egui::Color32::from_rgb(0x50, 0xFA, 0x7B),
        egui::Color32::from_rgb(0xBD, 0x93, 0xF9),
        egui::Color32::from_rgb(0xFF, 0x79, 0xC6),
        egui::Color32::from_rgb(0xF1, 0xFA, 0x8C),
    ];
    let light_palette = [
        egui::Color32::from_rgb(0x9C, 0x3D, 0x00),
        egui::Color32::from_rgb(0x00, 0x5F, 0x9A),
        egui::Color32::from_rgb(0x2E, 0x7D, 0x32),
        egui::Color32::from_rgb(0x6A, 0x1B, 0x9A),
        egui::Color32::from_rgb(0xAD, 0x14, 0x57),
        egui::Color32::from_rgb(0x5D, 0x40, 0x37),
    ];
    let palette = if visuals.dark_mode {
        &dark_palette
    } else {
        &light_palette
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
) -> egui::TextFormat {
    match idx {
        FmtIdx::Base => base.clone(),
        FmtIdx::Weak => weak.clone(),
        FmtIdx::InlineCode => inline_code.clone(),
        FmtIdx::Heading(level) => heading_formats[level - 1].clone(),
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
pub(crate) fn markdown_layout_job(
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
    let base = egui::TextFormat::simple(base_font.clone(), visuals.text_color());
    let weak = egui::TextFormat::simple(base_font, visuals.weak_text_color());
    let heading_scales = [2.0, 1.5, 1.25, 1.1, 1.0, 0.95];
    let heading_formats = std::array::from_fn(|idx| {
        let mut format = base.clone();
        format.font_id.size *= heading_scales[idx];
        format.color = heading_color(visuals, idx + 1, heading_color_mode);
        format
    });

    let mut inline_code = base.clone();
    inline_code.font_id = code_font;
    inline_code.background = visuals.faint_bg_color;

    // Pending run: consecutive lines with the same format are batched into
    // a single section to minimize section count.
    let mut pending_fmt: Option<FmtIdx> = None;
    let mut pending_start: usize = 0;
    let mut pending_end: usize = 0;

    let flush =
        |job: &mut egui::text::LayoutJob, fmt: &Option<FmtIdx>, start: usize, end: usize| {
            if let Some(idx) = *fmt {
                let format = resolve_format(idx, &base, &weak, &inline_code, &heading_formats);
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
                resolve_format(FmtIdx::Weak, &base, &weak, &inline_code, &heading_formats),
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

        let mut pos = line_start;
        let line_bytes = line.as_bytes();
        let mut i = 0;
        while let Some(tick_rel) = memchr::memchr(b'`', &line_bytes[i..]) {
            let tick_i = i + tick_rel;
            // Flush text before backtick.
            if pos < line_start + tick_i {
                push_section(&mut job, pos..line_start + tick_i, base.clone());
            }
            // Find closing backtick.
            if let Some(close) = memchr::memchr(b'`', &line_bytes[tick_i + 1..]) {
                let tick_start = line_start + tick_i;
                let code_start = tick_start + 1;
                let code_end = code_start + close;
                let tick_end = code_end + 1;
                push_section(&mut job, tick_start..code_start, weak.clone());
                push_section(&mut job, code_start..code_end, inline_code.clone());
                push_section(&mut job, code_end..tick_end, weak.clone());
                pos = tick_end;
                i = tick_i + 1 + close + 1;
            } else {
                // Unmatched backtick.
                push_section(
                    &mut job,
                    line_start + tick_i..line_start + tick_i + 1,
                    weak.clone(),
                );
                if line_start + tick_i + 1 < line_end {
                    push_section(&mut job, line_start + tick_i + 1..line_end, base.clone());
                }
                pos = line_end;
                i = line_bytes.len();
            }
        }
        if pos < line_end {
            push_section(&mut job, pos..line_end, base.clone());
        }
    }

    flush(&mut job, &pending_fmt, pending_start, pending_end);
    job
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
}
