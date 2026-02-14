#![forbid(unsafe_code)]

use eframe::egui;

pub(crate) fn markdown_layout_job(ui: &egui::Ui, source: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();

    let base_font = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Monospace)
        .cloned()
        .unwrap_or_else(|| egui::FontId::monospace(14.0));

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
        font_id: base_font.clone(),
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
