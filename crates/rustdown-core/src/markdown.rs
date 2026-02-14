use pulldown_cmark::{Event, Options, Parser, TagEnd};

fn options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options
}

/// Create a `pulldown-cmark` parser with our default options enabled.
pub fn parser(source: &str) -> Parser<'_> {
    Parser::new_ext(source, options())
}

/// Render markdown to a simple plain-text representation.
///
/// This is used for tests and as a fallback for CLI preview output.
pub fn plain_text(source: &str) -> String {
    let mut out = String::new();
    let mut last_was_newline = true;

    let push_newline = |out: &mut String, last_was_newline: &mut bool| {
        if !*last_was_newline {
            out.push('\n');
            *last_was_newline = true;
        }
    };

    for event in parser(source) {
        match event {
            Event::Text(text) | Event::Code(text) => {
                out.push_str(text.as_ref());
                last_was_newline = false;
            }
            Event::SoftBreak | Event::HardBreak => push_newline(&mut out, &mut last_was_newline),
            Event::Rule => {
                push_newline(&mut out, &mut last_was_newline);
                out.push_str("---");
                last_was_newline = false;
                push_newline(&mut out, &mut last_was_newline);
            }
            Event::End(end) => match end {
                TagEnd::Paragraph
                | TagEnd::Heading { .. }
                | TagEnd::BlockQuote(_)
                | TagEnd::CodeBlock
                | TagEnd::Item
                | TagEnd::List(_)
                | TagEnd::Table
                | TagEnd::TableHead
                | TagEnd::TableRow => push_newline(&mut out, &mut last_was_newline),
                TagEnd::TableCell => {
                    if !last_was_newline {
                        out.push('\t');
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    out
}
