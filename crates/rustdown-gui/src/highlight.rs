#![forbid(unsafe_code)]

use eframe::egui;

pub(crate) fn markdown_layout_job(ui: &egui::Ui, source: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();

    let base_font = egui::TextStyle::Body.resolve(ui.style());
    let code_font = egui::TextStyle::Monospace.resolve(ui.style());

    let base = egui::TextFormat::simple(base_font.clone(), ui.visuals().text_color());
    let weak = egui::TextFormat::simple(base_font, ui.visuals().weak_text_color());

    let mut inline_code = base.clone();
    inline_code.font_id = code_font;
    inline_code.background = ui.visuals().faint_bg_color;

    let mut heading = base.clone();
    heading.font_id.size *= 1.05;
    heading.color = ui.visuals().hyperlink_color;

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

        let level = trimmed.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&level) && trimmed.as_bytes().get(level) == Some(&b' ') {
            let mut heading_format = heading.clone();
            heading_format.font_id.size *= 1.0 + (6 - level) as f32 * 0.02;
            job.append(line, 0.0, heading_format);
            continue;
        }

        append_inline_code(&mut job, line, &base, &weak, &inline_code);
    }

    job
}

fn append_inline_code(
    job: &mut egui::text::LayoutJob,
    line: &str,
    base: &egui::TextFormat,
    weak: &egui::TextFormat,
    inline_code: &egui::TextFormat,
) {
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
            return;
        }
    }

    job.append(rest, 0.0, base.clone());
}
