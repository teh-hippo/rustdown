#![forbid(unsafe_code)]

use eframe::egui;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Tag, TagEnd};

#[derive(Clone, Debug)]
pub(crate) struct PreviewDoc {
    blocks: Vec<Block>,
}

#[derive(Clone, Debug)]
enum Block {
    QuoteStart,
    QuoteEnd,
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph {
        spans: Vec<Span>,
    },
    ListItem {
        depth: usize,
        task: Option<bool>,
        spans: Vec<Span>,
    },
    Code {
        language: Option<String>,
        code: String,
    },
    Table {
        rows: Vec<TableRow>,
    },
    Rule,
}

#[derive(Clone, Debug)]
struct TableRow {
    header: bool,
    cells: Vec<Vec<Span>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SpanStyle {
    emphasis: bool,
    code: bool,
    strikethrough: bool,
    link: Option<String>,
}

#[derive(Clone, Debug)]
struct Span {
    text: String,
    style: SpanStyle,
}

#[derive(Clone, Copy, Debug)]
enum BlockKind {
    Heading(u8),
    Paragraph,
    ListItem { depth: usize },
}

/// Parse markdown into a cached preview representation (no egui types).
pub(crate) fn parse(source: &str) -> PreviewDoc {
    let mut blocks = Vec::<Block>::new();

    let mut kind: Option<BlockKind> = None;
    let mut spans = Vec::<Span>::new();

    let mut list_depth: usize = 0;
    let mut emphasis_depth: usize = 0;
    let mut strikethrough_depth: usize = 0;
    let mut link_stack: Vec<String> = Vec::new();
    let mut task_marker: Option<bool> = None;

    let mut in_code_block = false;
    let mut code_block_language: Option<String> = None;
    let mut code_block_text = String::new();
    let mut code_block_in_table = false;

    let mut in_table = false;
    let mut in_table_head = false;
    let mut in_table_cell = false;
    let mut table_rows = Vec::<TableRow>::new();
    let mut table_row_cells = Vec::<Vec<Span>>::new();
    let mut table_cell_spans = Vec::<Span>::new();

    for event in rustdown_core::markdown::parser(source) {
        match event {
            Event::Start(tag) => match tag {
                Tag::BlockQuote(_) => blocks.push(Block::QuoteStart),
                Tag::List(_) => list_depth = list_depth.saturating_add(1),
                Tag::Item => {
                    if !in_table {
                        kind = Some(BlockKind::ListItem { depth: list_depth });
                        spans.clear();
                        task_marker = None;
                    }
                }
                Tag::Paragraph => {
                    if !in_table && kind.is_none() {
                        kind = Some(BlockKind::Paragraph);
                        spans.clear();
                    }
                }
                Tag::Heading {
                    level,
                    id: _,
                    classes: _,
                    attrs: _,
                } => {
                    if !in_table {
                        kind = Some(BlockKind::Heading(heading_level(level)));
                        spans.clear();
                    }
                }
                Tag::Emphasis => emphasis_depth = emphasis_depth.saturating_add(1),
                Tag::Strikethrough => {
                    strikethrough_depth = strikethrough_depth.saturating_add(1);
                }
                Tag::Link {
                    link_type: _,
                    dest_url,
                    title: _,
                    id: _,
                } => link_stack.push(dest_url.to_string()),
                Tag::CodeBlock(code_kind) => {
                    if in_table {
                        in_code_block = true;
                        code_block_in_table = true;
                    } else {
                        in_code_block = true;
                        code_block_text.clear();
                        code_block_language = match code_kind {
                            CodeBlockKind::Fenced(lang) => {
                                let lang = lang.trim();
                                (!lang.is_empty()).then(|| lang.to_owned())
                            }
                            CodeBlockKind::Indented => None,
                        };
                    }
                }
                Tag::Table(_) => {
                    in_table = true;
                    table_rows.clear();
                }
                Tag::TableHead => in_table_head = true,
                Tag::TableRow => {
                    if in_table {
                        table_row_cells.clear();
                    }
                }
                Tag::TableCell => {
                    if in_table {
                        in_table_cell = true;
                        table_cell_spans.clear();
                    }
                }
                _ => {}
            },
            Event::End(end) => match end {
                TagEnd::BlockQuote(_) => blocks.push(Block::QuoteEnd),
                TagEnd::List(_) => list_depth = list_depth.saturating_sub(1),
                TagEnd::Emphasis => emphasis_depth = emphasis_depth.saturating_sub(1),
                TagEnd::Strikethrough => {
                    strikethrough_depth = strikethrough_depth.saturating_sub(1);
                }
                TagEnd::Link => {
                    let _ = link_stack.pop();
                }
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    if code_block_in_table {
                        code_block_in_table = false;
                    } else {
                        blocks.push(Block::Code {
                            language: code_block_language.take(),
                            code: std::mem::take(&mut code_block_text),
                        });
                    }
                }
                TagEnd::Heading(_) => {
                    if let Some(BlockKind::Heading(level)) = kind.take() {
                        blocks.push(Block::Heading {
                            level,
                            spans: std::mem::take(&mut spans),
                        });
                    }
                }
                TagEnd::Paragraph => {
                    if matches!(kind, Some(BlockKind::Paragraph)) {
                        kind = None;
                        blocks.push(Block::Paragraph {
                            spans: std::mem::take(&mut spans),
                        });
                    }
                }
                TagEnd::Item => {
                    if let Some(BlockKind::ListItem { depth }) = kind.take() {
                        blocks.push(Block::ListItem {
                            depth,
                            task: task_marker.take(),
                            spans: std::mem::take(&mut spans),
                        });
                    }
                }
                TagEnd::TableHead => in_table_head = false,
                TagEnd::TableCell => {
                    if in_table {
                        in_table_cell = false;
                        table_row_cells.push(std::mem::take(&mut table_cell_spans));
                    }
                }
                TagEnd::TableRow => {
                    if in_table {
                        table_rows.push(TableRow {
                            header: in_table_head,
                            cells: std::mem::take(&mut table_row_cells),
                        });
                    }
                }
                TagEnd::Table => {
                    in_table = false;
                    blocks.push(Block::Table {
                        rows: std::mem::take(&mut table_rows),
                    });
                }
                _ => {}
            },
            Event::TaskListMarker(checked) => {
                if !in_table {
                    task_marker = Some(checked);
                }
            }
            Event::Text(text) => {
                if in_code_block {
                    if code_block_in_table && in_table_cell {
                        push_span(
                            &mut table_cell_spans,
                            text.as_ref(),
                            SpanStyle {
                                emphasis: false,
                                code: true,
                                strikethrough: false,
                                link: None,
                            },
                        );
                    } else {
                        code_block_text.push_str(text.as_ref());
                    }
                } else if in_table && in_table_cell {
                    push_span(
                        &mut table_cell_spans,
                        text.as_ref(),
                        SpanStyle {
                            emphasis: emphasis_depth > 0,
                            code: false,
                            strikethrough: strikethrough_depth > 0,
                            link: link_stack.last().cloned(),
                        },
                    );
                } else {
                    push_span(
                        &mut spans,
                        text.as_ref(),
                        SpanStyle {
                            emphasis: emphasis_depth > 0,
                            code: false,
                            strikethrough: strikethrough_depth > 0,
                            link: link_stack.last().cloned(),
                        },
                    );
                }
            }
            Event::Code(text) => {
                if in_table && in_table_cell {
                    push_span(
                        &mut table_cell_spans,
                        text.as_ref(),
                        SpanStyle {
                            emphasis: false,
                            code: true,
                            strikethrough: false,
                            link: None,
                        },
                    );
                } else if in_code_block {
                    code_block_text.push_str(text.as_ref());
                } else {
                    push_span(
                        &mut spans,
                        text.as_ref(),
                        SpanStyle {
                            emphasis: false,
                            code: true,
                            strikethrough: false,
                            link: None,
                        },
                    );
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if in_code_block {
                    code_block_text.push('\n');
                } else if in_table && in_table_cell {
                    push_span(
                        &mut table_cell_spans,
                        "\n",
                        SpanStyle {
                            emphasis: emphasis_depth > 0,
                            code: false,
                            strikethrough: strikethrough_depth > 0,
                            link: link_stack.last().cloned(),
                        },
                    );
                } else {
                    push_span(
                        &mut spans,
                        "\n",
                        SpanStyle {
                            emphasis: emphasis_depth > 0,
                            code: false,
                            strikethrough: strikethrough_depth > 0,
                            link: link_stack.last().cloned(),
                        },
                    );
                }
            }
            Event::Rule => {
                blocks.push(Block::Rule);
            }
            _ => {}
        }
    }

    PreviewDoc { blocks }
}

pub(crate) fn show(ui: &mut egui::Ui, doc: &PreviewDoc) {
    let mut quote_depth: usize = 0;

    for (block_idx, block) in doc.blocks.iter().enumerate() {
        match block {
            Block::QuoteStart => {
                quote_depth = quote_depth.saturating_add(1);
            }
            Block::QuoteEnd => {
                quote_depth = quote_depth.saturating_sub(1);
            }
            _ => with_quote(ui, quote_depth, |ui| match block {
                Block::Heading { level, spans } => {
                    let font = heading_font(ui, *level);
                    ui.add(egui::Label::new(spans_layout_job(ui, spans, font)).wrap());
                    ui.add_space(4.0);
                }
                Block::Paragraph { spans } => {
                    let font = ui
                        .style()
                        .text_styles
                        .get(&egui::TextStyle::Body)
                        .cloned()
                        .unwrap_or_else(|| egui::FontId::proportional(16.0));
                    ui.add(egui::Label::new(spans_layout_job(ui, spans, font)).wrap());
                    ui.add_space(6.0);
                }
                Block::ListItem { depth, task, spans } => {
                    let font = ui
                        .style()
                        .text_styles
                        .get(&egui::TextStyle::Body)
                        .cloned()
                        .unwrap_or_else(|| egui::FontId::proportional(16.0));

                    ui.horizontal_wrapped(|ui| {
                        ui.add_space(*depth as f32 * 12.0);
                        if let Some(checked) = task {
                            let mut checked = *checked;
                            ui.add_enabled(false, egui::Checkbox::new(&mut checked, ""));
                        } else {
                            ui.label("•");
                        }
                        ui.add(egui::Label::new(spans_layout_job(ui, spans, font)).wrap());
                    });
                    ui.add_space(4.0);
                }
                Block::Code { language, code } => {
                    if let Some(lang) = language.as_deref() {
                        ui.label(egui::RichText::new(lang).weak());
                    }

                    let frame = egui::Frame::group(ui.style())
                        .fill(ui.visuals().faint_bg_color)
                        .inner_margin(egui::Margin::same(8.0));

                    frame.show(ui, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(code).monospace())
                                .wrap()
                                .selectable(true),
                        );
                    });
                    ui.add_space(6.0);
                }
                Block::Table { rows } => {
                    let font = ui
                        .style()
                        .text_styles
                        .get(&egui::TextStyle::Body)
                        .cloned()
                        .unwrap_or_else(|| egui::FontId::proportional(16.0));

                    let cols = rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
                    let grid_id = ui.id().with(("table", block_idx));

                    egui::Grid::new(grid_id).striped(true).show(ui, |ui| {
                        for row in rows {
                            for cell in &row.cells {
                                let mut job = spans_layout_job(ui, cell, font.clone());
                                if row.header {
                                    for section in &mut job.sections {
                                        section.format.underline =
                                            egui::Stroke::new(1.0, ui.visuals().weak_text_color());
                                    }
                                }
                                ui.add(egui::Label::new(job).wrap());
                            }
                            for _ in row.cells.len()..cols {
                                ui.label("");
                            }
                            ui.end_row();
                        }
                    });
                    ui.add_space(6.0);
                }
                Block::Rule => {
                    ui.separator();
                    ui.add_space(6.0);
                }
                Block::QuoteStart | Block::QuoteEnd => {}
            }),
        }
    }
}

fn with_quote(ui: &mut egui::Ui, depth: usize, add_contents: impl FnOnce(&mut egui::Ui)) {
    if depth == 0 {
        add_contents(ui);
        return;
    }

    ui.horizontal(|ui| {
        ui.add_space((depth - 1) as f32 * 12.0);
        ui.colored_label(ui.visuals().weak_text_color(), "|");
        ui.add_space(4.0);
        ui.vertical(add_contents);
    });
}

fn push_span(spans: &mut Vec<Span>, text: &str, style: SpanStyle) {
    if text.is_empty() {
        return;
    }

    match spans.last_mut() {
        Some(last) if last.style == style => last.text.push_str(text),
        _ => spans.push(Span {
            text: text.to_owned(),
            style,
        }),
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn heading_font(ui: &egui::Ui, level: u8) -> egui::FontId {
    let base = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Heading)
        .cloned()
        .unwrap_or_else(|| egui::FontId::proportional(22.0));

    let scale = match level {
        1 => 1.20,
        2 => 1.10,
        3 => 1.05,
        _ => 1.0,
    };

    egui::FontId {
        size: base.size * scale,
        family: base.family,
    }
}

fn spans_layout_job(
    ui: &egui::Ui,
    spans: &[Span],
    base_font: egui::FontId,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();

    for span in spans {
        let mut format = egui::text::TextFormat {
            font_id: if span.style.code {
                ui.style()
                    .text_styles
                    .get(&egui::TextStyle::Monospace)
                    .cloned()
                    .unwrap_or_else(|| egui::FontId::monospace(base_font.size))
            } else {
                base_font.clone()
            },
            color: ui.visuals().text_color(),
            ..Default::default()
        };

        if span.style.emphasis {
            format.italics = true;
        }

        if span.style.strikethrough {
            format.strikethrough = egui::Stroke::new(1.0, format.color);
        }

        if span.style.link.is_some() {
            format.underline = egui::Stroke::new(1.0, ui.visuals().hyperlink_color);
            format.color = ui.visuals().hyperlink_color;
        }

        job.append(&span.text, 0.0, format);
    }

    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_blocks() {
        let md = "# Title\n\nHello *world* ~~gone~~.\n\n> quoted\n\n- [ ] a\n- [x] b\n\n| a | b |\n| - | - |\n| c | d |\n\n```rs\nlet x = 1;\n```\n";
        let doc = parse(md);

        assert!(matches!(doc.blocks[0], Block::Heading { .. }));
        assert!(matches!(doc.blocks[1], Block::Paragraph { .. }));
        assert!(matches!(doc.blocks[2], Block::QuoteStart));
        assert!(matches!(doc.blocks[3], Block::Paragraph { .. }));
        assert!(matches!(doc.blocks[4], Block::QuoteEnd));
        let Block::ListItem { task, .. } = &doc.blocks[5] else {
            panic!("expected list item");
        };
        assert_eq!(*task, Some(false));

        let Block::ListItem { task, .. } = &doc.blocks[6] else {
            panic!("expected list item");
        };
        assert_eq!(*task, Some(true));
        assert!(matches!(doc.blocks[7], Block::Table { .. }));
        assert!(matches!(doc.blocks[8], Block::Code { .. }));

        let Block::Paragraph { spans } = &doc.blocks[1] else {
            panic!("expected paragraph");
        };
        assert!(spans.iter().any(|s| s.style.strikethrough));
    }
}
