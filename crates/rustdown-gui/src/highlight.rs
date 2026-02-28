#![forbid(unsafe_code)]

use eframe::egui;

use crate::markdown_fence::{FenceState, consume_fence_delimiter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunKind {
    Base,
    InlineCode,
    Heading(usize),
}

#[derive(Clone, Copy, Debug, Default)]
struct PendingRun {
    kind: Option<RunKind>,
    start: usize,
    end: usize,
}

fn flush_pending(
    job: &mut egui::text::LayoutJob,
    source: &str,
    pending: &mut PendingRun,
    base: &egui::TextFormat,
    inline_code: &egui::TextFormat,
    heading_formats: &[egui::TextFormat; 6],
) {
    let Some(kind) = pending.kind else {
        return;
    };
    if pending.start >= pending.end || pending.end > source.len() {
        pending.kind = None;
        return;
    }

    let format = match kind {
        RunKind::Base => base.clone(),
        RunKind::InlineCode => inline_code.clone(),
        RunKind::Heading(level) => heading_formats[level - 1].clone(),
    };

    job.append(&source[pending.start..pending.end], 0.0, format);
    pending.kind = None;
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

#[must_use]
pub(crate) fn markdown_layout_job(
    style: &egui::Style,
    visuals: &egui::Visuals,
    source: &str,
    heading_color_mode: bool,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob {
        text: String::with_capacity(source.len()),
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

    let mut in_fence: Option<FenceState> = None;
    let mut offset = 0usize;
    let mut pending = PendingRun::default();
    for line in source.split_inclusive('\n') {
        let line_start = offset;
        let line_end = line_start + line.len();
        offset = line_end;

        if consume_fence_delimiter(line, &mut in_fence) {
            flush_pending(
                &mut job,
                source,
                &mut pending,
                &base,
                &inline_code,
                &heading_formats,
            );
            job.append(line, 0.0, weak.clone());
            continue;
        }
        if in_fence.is_some() {
            let kind = RunKind::InlineCode;
            match pending.kind {
                Some(existing) if existing == kind => pending.end = line_end,
                _ => {
                    flush_pending(
                        &mut job,
                        source,
                        &mut pending,
                        &base,
                        &inline_code,
                        &heading_formats,
                    );
                    pending.kind = Some(kind);
                    pending.start = line_start;
                    pending.end = line_end;
                }
            }
            continue;
        }

        let trimmed = line.trim_start();
        let level = trimmed.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&level) && trimmed.as_bytes().get(level) == Some(&b' ') {
            let kind = RunKind::Heading(level);
            match pending.kind {
                Some(existing) if existing == kind => pending.end = line_end,
                _ => {
                    flush_pending(
                        &mut job,
                        source,
                        &mut pending,
                        &base,
                        &inline_code,
                        &heading_formats,
                    );
                    pending.kind = Some(kind);
                    pending.start = line_start;
                    pending.end = line_end;
                }
            }
            continue;
        }

        if !line.contains('`') {
            let kind = RunKind::Base;
            match pending.kind {
                Some(existing) if existing == kind => pending.end = line_end,
                _ => {
                    flush_pending(
                        &mut job,
                        source,
                        &mut pending,
                        &base,
                        &inline_code,
                        &heading_formats,
                    );
                    pending.kind = Some(kind);
                    pending.start = line_start;
                    pending.end = line_end;
                }
            }
            continue;
        }

        flush_pending(
            &mut job,
            source,
            &mut pending,
            &base,
            &inline_code,
            &heading_formats,
        );
        let mut rest = line;
        while let Some(start) = rest.find('`') {
            let (before, after_tick) = rest.split_at(start);
            job.append(before, 0.0, base.clone());
            let after_tick = &after_tick[1..];
            if let Some(end) = after_tick.find('`') {
                let (code, after_code) = after_tick.split_at(end);
                job.append("`", 0.0, weak.clone());
                job.append(code, 0.0, inline_code.clone());
                job.append("`", 0.0, weak.clone());
                rest = &after_code[1..];
            } else {
                job.append("`", 0.0, weak.clone());
                job.append(after_tick, 0.0, base.clone());
                rest = "";
                break;
            }
        }
        if !rest.is_empty() {
            job.append(rest, 0.0, base.clone());
        }
    }

    flush_pending(
        &mut job,
        source,
        &mut pending,
        &base,
        &inline_code,
        &heading_formats,
    );
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
    fn markdown_layout_job_tilde_fences_mark_code_content_as_code() {
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "~~~azurecli\naz aks list\n~~~\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
        let code_section = section_for_snippet(&job, "az aks list");
        assert_eq!(code_section.format.background, visuals.faint_bg_color);
        assert_eq!(
            code_section.format.font_id,
            egui::TextStyle::Monospace.resolve(&style)
        );
    }

    #[test]
    fn markdown_layout_job_marks_fence_delimiters_as_weak_text() {
        let style = egui::Style::default();
        let visuals = egui::Visuals::dark();
        let source = "~~~bash\necho hi\n~~~\n";
        let job = markdown_layout_job(&style, &visuals, source, false);
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
