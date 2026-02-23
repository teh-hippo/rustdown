#![forbid(unsafe_code)]

use eframe::egui;

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

#[must_use]
pub(crate) fn markdown_layout_job(
    style: &egui::Style,
    visuals: &egui::Visuals,
    source: &str,
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
    let heading_formats = heading_scales.map(|scale| {
        let mut format = base.clone();
        format.font_id.size *= scale;
        format.color = visuals.hyperlink_color;
        format
    });

    let mut inline_code = base.clone();
    inline_code.font_id = code_font;
    inline_code.background = visuals.faint_bg_color;

    let mut in_fence = false;
    let mut offset = 0usize;
    let mut pending = PendingRun::default();
    for line in source.split_inclusive('\n') {
        let line_start = offset;
        let line_end = line_start + line.len();
        offset = line_end;

        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            flush_pending(
                &mut job,
                source,
                &mut pending,
                &base,
                &inline_code,
                &heading_formats,
            );
            in_fence = !in_fence;
            job.append(line, 0.0, weak.clone());
            continue;
        }
        if in_fence {
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
