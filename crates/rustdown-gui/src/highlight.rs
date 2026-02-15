#![forbid(unsafe_code)]

use eframe::egui;

pub(crate) fn markdown_layout_job(ui: &egui::Ui, source: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.text.reserve(source.len());

    let base_font = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Body)
        .cloned()
        .unwrap_or_else(|| egui::FontId::proportional(16.0));

    let code_font = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Monospace)
        .cloned()
        .unwrap_or_else(|| egui::FontId::monospace(base_font.size));

    let base = egui::text::TextFormat {
        font_id: base_font.clone(),
        color: ui.visuals().text_color(),
        ..Default::default()
    };

    let weak = egui::text::TextFormat {
        font_id: base_font.clone(),
        color: ui.visuals().weak_text_color(),
        ..Default::default()
    };

    let inline_code = egui::text::TextFormat {
        font_id: code_font.clone(),
        color: ui.visuals().text_color(),
        background: ui.visuals().faint_bg_color,
        ..Default::default()
    };

    let heading = egui::text::TextFormat {
        font_id: egui::FontId {
            size: base_font.size * 1.05,
            family: base_font.family.clone(),
        },
        color: ui.visuals().hyperlink_color,
        ..Default::default()
    };

    let mut in_fence = false;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            job.append(line, 0.0, weak.clone());
            continue;
        }

        if in_fence {
            job.append(line, 0.0, inline_code.clone());
            continue;
        }

        // headings: `# ` .. `###### `
        if let Some(level) = heading_level(trimmed) {
            let ws_len = line.len().saturating_sub(trimmed.len());
            if ws_len > 0 {
                job.append(&line[..ws_len], 0.0, base.clone());
            }

            // keep the leading `###` slightly muted
            let hashes_len = trimmed
                .bytes()
                .take_while(|b| *b == b'#')
                .count()
                .min(trimmed.len());
            job.append(&trimmed[..hashes_len], 0.0, weak.clone());

            // rest of the line gets heading style
            let rest = &trimmed[hashes_len..];
            let mut heading_format = heading.clone();
            heading_format.font_id.size *= 1.0 + (6 - level) as f32 * 0.02;
            job.append(rest, 0.0, heading_format);
            continue;
        }

        append_inline_code(&mut job, line, &base, &weak, &inline_code);
    }

    job
}

fn heading_level(line: &str) -> Option<u8> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }

    // markdown headings must have a space after the hashes
    line.as_bytes()
        .get(hashes)
        .is_some_and(|b| *b == b' ')
        .then_some(hashes as u8)
}

fn append_inline_code(
    job: &mut egui::text::LayoutJob,
    line: &str,
    base: &egui::text::TextFormat,
    weak: &egui::text::TextFormat,
    inline_code: &egui::text::TextFormat,
) {
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        let (before, after_tick) = rest.split_at(start);
        job.append(before, 0.0, base.clone());

        // find closing tick
        let after_tick = &after_tick[1..];
        if let Some(end) = after_tick.find('`') {
            let (code, after_code) = after_tick.split_at(end);
            job.append("`", 0.0, weak.clone());
            job.append(code, 0.0, inline_code.clone());
            job.append("`", 0.0, weak.clone());
            rest = &after_code[1..];
        } else {
            // unmatched; treat the rest as normal text
            job.append("`", 0.0, weak.clone());
            job.append(after_tick, 0.0, base.clone());
            return;
        }
    }

    job.append(rest, 0.0, base.clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn joined_text(job: &egui::text::LayoutJob) -> String {
        let mut out = String::new();
        for section in &job.sections {
            if let Some(text) = job.text.get(section.byte_range.clone()) {
                out.push_str(text);
            }
        }
        out
    }

    #[test]
    fn heading_level_requires_space() {
        assert_eq!(heading_level("# Title"), Some(1));
        assert_eq!(heading_level("## Title"), Some(2));
        assert_eq!(heading_level("###Title"), None);
        assert_eq!(heading_level("####### Too many"), None);
    }

    #[test]
    fn inline_code_round_trip() {
        let mut job = egui::text::LayoutJob::default();
        let fmt = egui::text::TextFormat::default();
        append_inline_code(&mut job, "a `b` c\n", &fmt, &fmt, &fmt);
        assert_eq!(joined_text(&job), "a `b` c\n");
    }

    #[test]
    fn inline_code_unmatched_tick() {
        let mut job = egui::text::LayoutJob::default();
        let fmt = egui::text::TextFormat::default();
        append_inline_code(&mut job, "a `b c\n", &fmt, &fmt, &fmt);
        assert_eq!(joined_text(&job), "a `b c\n");
    }

    #[test]
    #[ignore]
    fn perf_highlight_layout_job() {
        let ctx = egui::Context::default();
        let iters = 10u32;

        let chunk = "# Heading\nSome text with `inline code` and **bold**.\n\n- item a\n- item b\n\n```rs\nlet x = 1;   \n```\n\n";
        for target_bytes in [32 * 1024usize, 96 * 1024, 160 * 1024] {
            let mut source = String::with_capacity(target_bytes + chunk.len());
            while source.len() < target_bytes {
                source.push_str(chunk);
            }

            let mut total_job = Duration::ZERO;
            let mut total_layout = Duration::ZERO;

            for _ in 0..iters {
                let _ = ctx.run(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let t0 = Instant::now();
                        let mut job = markdown_layout_job(ui, &source);
                        job.wrap.max_width = 700.0;
                        total_job += t0.elapsed();

                        let t1 = Instant::now();
                        ui.fonts(|fonts| {
                            let _ = fonts.layout_job(job);
                        });
                        total_layout += t1.elapsed();
                    });
                });
            }

            eprintln!(
                "highlight: bytes={} job_avg={:?} layout_avg={:?}",
                source.len(),
                total_job / iters,
                total_layout / iters
            );
        }
    }
}
