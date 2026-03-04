#![forbid(unsafe_code)]
//! Markdown parsing: converts source text into a flat list of render blocks.

use std::rc::Rc;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// A single renderable block produced by parsing.
#[derive(Clone, Debug)]
pub enum Block {
    Heading {
        level: u8,
        text: StyledText,
    },
    Paragraph(StyledText),
    Code {
        /// Language tag from fenced code blocks (e.g. "rust", "python").
        language: String,
        code: String,
    },
    Quote(Vec<Self>),
    UnorderedList(Vec<ListItem>),
    OrderedList {
        start: u64,
        items: Vec<ListItem>,
    },
    ThematicBreak,
    Table {
        header: Vec<StyledText>,
        alignments: Vec<Alignment>,
        rows: Vec<Vec<StyledText>>,
    },
    Image {
        url: String,
        alt: String,
    },
}

/// Alignment for table columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alignment {
    None,
    Left,
    Center,
    Right,
}

/// A single list item (may contain nested blocks).
#[derive(Clone, Debug)]
pub struct ListItem {
    pub content: StyledText,
    pub children: Vec<Block>,
    /// Task-list checkbox state: `Some(true)` = checked, `Some(false)` = unchecked, `None` = normal item.
    pub checked: Option<bool>,
}

/// Styled text: a string with inline formatting spans.
#[derive(Clone, Debug, Default)]
pub struct StyledText {
    pub text: String,
    pub spans: Vec<Span>,
}

/// Inline formatting flags that can be combined (e.g., bold + italic).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpanStyle {
    /// Bitfield: bit 0 = strong, 1 = emphasis, 2 = strikethrough, 3 = code.
    flags: u8,
    pub link: Option<Rc<str>>,
}

const FLAG_STRONG: u8 = 1;
const FLAG_EMPHASIS: u8 = 2;
const FLAG_STRIKETHROUGH: u8 = 4;
const FLAG_CODE: u8 = 8;

impl SpanStyle {
    pub const fn plain() -> Self {
        Self {
            flags: 0,
            link: None,
        }
    }

    pub const fn strong(&self) -> bool {
        self.flags & FLAG_STRONG != 0
    }

    pub const fn set_strong(&mut self) {
        self.flags |= FLAG_STRONG;
    }

    pub const fn emphasis(&self) -> bool {
        self.flags & FLAG_EMPHASIS != 0
    }

    pub const fn set_emphasis(&mut self) {
        self.flags |= FLAG_EMPHASIS;
    }

    pub const fn strikethrough(&self) -> bool {
        self.flags & FLAG_STRIKETHROUGH != 0
    }

    pub const fn set_strikethrough(&mut self) {
        self.flags |= FLAG_STRIKETHROUGH;
    }

    pub const fn code(&self) -> bool {
        self.flags & FLAG_CODE != 0
    }

    pub const fn set_code(&mut self) {
        self.flags |= FLAG_CODE;
    }

    #[allow(dead_code)]
    pub const fn is_plain(&self) -> bool {
        self.flags == 0 && self.link.is_none()
    }
}

/// An inline formatting span within a `StyledText`.
#[derive(Clone, Debug)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub style: SpanStyle,
}

impl StyledText {
    fn push_text(&mut self, s: &str, style: SpanStyle) {
        let start = self.text.len();
        self.text.push_str(s);
        let end = self.text.len();
        if start < end {
            // Merge adjacent spans of the same style.
            if let Some(last) = self.spans.last_mut()
                && last.end == start
                && last.style == style
            {
                last.end = end;
                return;
            }
            self.spans.push(Span { start, end, style });
        }
    }
}

/// Parse markdown source into blocks.
pub fn parse_markdown(source: &str) -> Vec<Block> {
    let opts = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(source, opts);
    // Collect into Vec — required for our indexed recursive descent.
    // Pre-allocate based on source size heuristic.
    let events: Vec<Event<'_>> = {
        let capacity = source.len() / 20 + 16;
        let mut v = Vec::with_capacity(capacity);
        v.extend(parser);
        v
    };
    let mut blocks = Vec::with_capacity(events.len() / 4 + 4);
    let mut fmt_stack = Vec::new();
    let mut i = 0;
    while i < events.len() {
        i += parse_block(&events[i..], &mut blocks, &mut fmt_stack);
    }
    blocks
}

fn parse_block(
    events: &[Event<'_>],
    blocks: &mut Vec<Block>,
    fmt_buf: &mut Vec<InlineFlag>,
) -> usize {
    match &events[0] {
        Event::Start(Tag::Heading { level, .. }) => parse_heading(events, *level, blocks, fmt_buf),
        Event::Start(Tag::Paragraph) => parse_paragraph(events, blocks, fmt_buf),
        Event::Start(Tag::CodeBlock(kind)) => {
            let lang = match kind {
                pulldown_cmark::CodeBlockKind::Fenced(l) => l.to_string(),
                pulldown_cmark::CodeBlockKind::Indented => String::new(),
            };
            parse_code_block(events, lang, blocks)
        }
        Event::Start(Tag::BlockQuote(_)) => parse_blockquote(events, blocks, fmt_buf),
        Event::Start(Tag::List(start)) => parse_list(events, *start, blocks, fmt_buf),
        Event::Start(Tag::Table(aligns)) => parse_table(events, aligns, blocks, fmt_buf),
        Event::Start(Tag::Image {
            dest_url, title, ..
        }) => {
            blocks.push(Block::Image {
                url: dest_url.to_string(),
                alt: title.to_string(),
            });
            // Skip to end of image tag.
            let mut consumed = 1;
            while consumed < events.len() {
                if matches!(events[consumed], Event::End(TagEnd::Image)) {
                    return consumed + 1;
                }
                consumed += 1;
            }
            consumed
        }
        Event::Rule => {
            blocks.push(Block::ThematicBreak);
            1
        }
        _ => 1,
    }
}

fn parse_heading(
    events: &[Event<'_>],
    level: HeadingLevel,
    blocks: &mut Vec<Block>,
    fmt_buf: &mut Vec<InlineFlag>,
) -> usize {
    let lvl = heading_level_to_u8(level);
    let mut styled = StyledText::default();
    let mut consumed = 1;
    fmt_buf.clear();
    while consumed < events.len() {
        match &events[consumed] {
            Event::End(TagEnd::Heading(_)) => {
                consumed += 1;
                break;
            }
            ev => {
                consume_inline(ev, &mut styled, fmt_buf);
                consumed += 1;
            }
        }
    }
    blocks.push(Block::Heading {
        level: lvl,
        text: styled,
    });
    consumed
}

fn parse_paragraph(
    events: &[Event<'_>],
    blocks: &mut Vec<Block>,
    fmt_buf: &mut Vec<InlineFlag>,
) -> usize {
    let mut styled = StyledText::default();
    let mut consumed = 1;
    fmt_buf.clear();
    while consumed < events.len() {
        match &events[consumed] {
            Event::End(TagEnd::Paragraph) => {
                consumed += 1;
                break;
            }
            ev => {
                consume_inline(ev, &mut styled, fmt_buf);
                consumed += 1;
            }
        }
    }
    blocks.push(Block::Paragraph(styled));
    consumed
}

fn parse_code_block(events: &[Event<'_>], language: String, blocks: &mut Vec<Block>) -> usize {
    let mut code = String::new();
    let mut consumed = 1;
    while consumed < events.len() {
        match &events[consumed] {
            Event::End(TagEnd::CodeBlock) => {
                consumed += 1;
                break;
            }
            Event::Text(t) => {
                code.push_str(t);
                consumed += 1;
            }
            _ => consumed += 1,
        }
    }
    blocks.push(Block::Code { language, code });
    consumed
}

fn parse_blockquote(
    events: &[Event<'_>],
    blocks: &mut Vec<Block>,
    fmt_buf: &mut Vec<InlineFlag>,
) -> usize {
    let mut inner = Vec::new();
    let mut consumed = 1;
    while consumed < events.len() {
        if let Event::End(TagEnd::BlockQuote(_)) = &events[consumed] {
            consumed += 1;
            break;
        }
        let n = parse_block(&events[consumed..], &mut inner, fmt_buf);
        consumed += n;
    }
    blocks.push(Block::Quote(inner));
    consumed
}

fn parse_list(
    events: &[Event<'_>],
    start: Option<u64>,
    blocks: &mut Vec<Block>,
    fmt_buf: &mut Vec<InlineFlag>,
) -> usize {
    let mut items = Vec::new();
    let mut consumed = 1;
    while consumed < events.len() {
        match &events[consumed] {
            Event::End(TagEnd::List(_)) => {
                consumed += 1;
                break;
            }
            Event::Start(Tag::Item) => {
                consumed += 1;
                let mut item_text = StyledText::default();
                let mut children = Vec::new();
                fmt_buf.clear();
                let mut checked: Option<bool> = None;
                while consumed < events.len() {
                    match &events[consumed] {
                        Event::End(TagEnd::Item) => {
                            consumed += 1;
                            break;
                        }
                        Event::Start(Tag::Paragraph) => {
                            consumed += 1; // skip paragraph open in list item
                        }
                        Event::End(TagEnd::Paragraph) => {
                            consumed += 1; // skip paragraph close in list item
                        }
                        Event::Start(Tag::List(_)) => {
                            let n = parse_block(&events[consumed..], &mut children, fmt_buf);
                            consumed += n;
                        }
                        Event::TaskListMarker(is_checked) => {
                            checked = Some(*is_checked);
                            consumed += 1;
                        }
                        ev => {
                            consume_inline(ev, &mut item_text, fmt_buf);
                            consumed += 1;
                        }
                    }
                }
                items.push(ListItem {
                    content: item_text,
                    children,
                    checked,
                });
            }
            _ => consumed += 1,
        }
    }
    if let Some(s) = start {
        blocks.push(Block::OrderedList { start: s, items });
    } else {
        blocks.push(Block::UnorderedList(items));
    }
    consumed
}

fn parse_table(
    events: &[Event<'_>],
    aligns: &[pulldown_cmark::Alignment],
    blocks: &mut Vec<Block>,
    fmt_buf: &mut Vec<InlineFlag>,
) -> usize {
    let alignments: Vec<Alignment> = aligns
        .iter()
        .map(|a| match a {
            pulldown_cmark::Alignment::None => Alignment::None,
            pulldown_cmark::Alignment::Left => Alignment::Left,
            pulldown_cmark::Alignment::Center => Alignment::Center,
            pulldown_cmark::Alignment::Right => Alignment::Right,
        })
        .collect();

    let mut header = Vec::new();
    let mut rows: Vec<Vec<StyledText>> = Vec::new();
    let mut in_head = false;
    let mut current_row: Vec<StyledText> = Vec::new();
    let mut current_cell = StyledText::default();
    fmt_buf.clear();
    let mut consumed = 1;

    while consumed < events.len() {
        match &events[consumed] {
            Event::End(TagEnd::Table) => {
                consumed += 1;
                break;
            }
            Event::Start(Tag::TableHead) => {
                in_head = true;
                consumed += 1;
            }
            Event::End(TagEnd::TableHead) => {
                in_head = false;
                header = std::mem::take(&mut current_row);
                consumed += 1;
            }
            Event::Start(Tag::TableRow) => {
                current_row.clear();
                consumed += 1;
            }
            Event::End(TagEnd::TableRow) => {
                if in_head {
                    current_row.clear();
                } else {
                    rows.push(std::mem::take(&mut current_row));
                }
                consumed += 1;
            }
            Event::Start(Tag::TableCell) => {
                current_cell = StyledText::default();
                fmt_buf.clear();
                consumed += 1;
            }
            Event::End(TagEnd::TableCell) => {
                current_row.push(std::mem::take(&mut current_cell));
                consumed += 1;
            }
            ev => {
                consume_inline(ev, &mut current_cell, fmt_buf);
                consumed += 1;
            }
        }
    }

    blocks.push(Block::Table {
        header,
        alignments,
        rows,
    });
    consumed
}

/// Formatting flag for the inline stack.
#[derive(Clone, Debug, PartialEq, Eq)]
enum InlineFlag {
    Strong,
    Emphasis,
    Strikethrough,
    Link(Rc<str>),
}

fn consume_inline(event: &Event<'_>, styled: &mut StyledText, fmt_stack: &mut Vec<InlineFlag>) {
    match event {
        Event::Text(t) => {
            let style = build_span_style(fmt_stack);
            styled.push_text(t, style);
        }
        Event::Code(c) => {
            let mut style = build_span_style(fmt_stack);
            style.set_code();
            styled.push_text(c, style);
        }
        Event::SoftBreak => {
            let style = build_span_style(fmt_stack);
            styled.push_text(" ", style);
        }
        Event::HardBreak => {
            let style = build_span_style(fmt_stack);
            styled.push_text("\n", style);
        }
        Event::Start(Tag::Strong) => fmt_stack.push(InlineFlag::Strong),
        Event::End(TagEnd::Strong) => {
            pop_flag(fmt_stack, &InlineFlag::Strong);
        }
        Event::Start(Tag::Emphasis) => fmt_stack.push(InlineFlag::Emphasis),
        Event::End(TagEnd::Emphasis) => {
            pop_flag(fmt_stack, &InlineFlag::Emphasis);
        }
        Event::Start(Tag::Strikethrough) => fmt_stack.push(InlineFlag::Strikethrough),
        Event::End(TagEnd::Strikethrough) => {
            pop_flag(fmt_stack, &InlineFlag::Strikethrough);
        }
        Event::Start(Tag::Link { dest_url, .. }) => {
            fmt_stack.push(InlineFlag::Link(Rc::from(dest_url.as_ref())));
        }
        Event::End(TagEnd::Link) => {
            if let Some(pos) = fmt_stack
                .iter()
                .rposition(|k| matches!(k, InlineFlag::Link(_)))
            {
                fmt_stack.swap_remove(pos);
            }
        }
        _ => {}
    }
}

/// Build a composite `SpanStyle` from the full formatting stack.
fn build_span_style(fmt_stack: &[InlineFlag]) -> SpanStyle {
    let mut style = SpanStyle::plain();
    for flag in fmt_stack {
        match flag {
            InlineFlag::Strong => style.set_strong(),
            InlineFlag::Emphasis => style.set_emphasis(),
            InlineFlag::Strikethrough => style.set_strikethrough(),
            InlineFlag::Link(url) => style.link = Some(url.clone()),
        }
    }
    style
}

fn pop_flag(fmt_stack: &mut Vec<InlineFlag>, flag: &InlineFlag) {
    if let Some(pos) = fmt_stack.iter().rposition(|k| k == flag) {
        fmt_stack.swap_remove(pos);
    }
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
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use std::fmt::Write;

    #[test]
    fn parse_heading_simple() {
        let blocks = parse_markdown("# Hello World");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 1);
                assert_eq!(text.text, "Hello World");
            }
            other => panic!("expected heading, got {other:?}"),
        }
    }

    #[test]
    fn parse_paragraph_with_emphasis() {
        let blocks = parse_markdown("Hello **world** and *italic*");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Paragraph(st) => {
                assert_eq!(st.text, "Hello world and italic");
                assert!(st.spans.iter().any(|s| s.style.strong()));
                assert!(st.spans.iter().any(|s| s.style.emphasis()));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_code_block_fenced() {
        let blocks = parse_markdown("```rust\nfn main() {}\n```");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Code { language, code } => {
                assert_eq!(language, "rust");
                assert!(code.contains("fn main()"));
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn parse_unordered_list() {
        let blocks = parse_markdown("- one\n- two\n- three");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0].content.text, "one");
            }
            other => panic!("expected unordered list, got {other:?}"),
        }
    }

    #[test]
    fn parse_ordered_list() {
        let blocks = parse_markdown("1. first\n2. second");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 1);
                assert_eq!(items.len(), 2);
            }
            other => panic!("expected ordered list, got {other:?}"),
        }
    }

    #[test]
    fn parse_blockquote() {
        let blocks = parse_markdown("> quoted text");
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], Block::Quote(_)));
    }

    #[test]
    fn parse_thematic_break() {
        let blocks = parse_markdown("---");
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], Block::ThematicBreak));
    }

    #[test]
    fn parse_table() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Table {
                header,
                rows,
                alignments,
            } => {
                assert_eq!(header.len(), 2);
                assert_eq!(rows.len(), 2);
                assert_eq!(alignments.len(), 2);
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn parse_inline_code() {
        let blocks = parse_markdown("Use `code` here");
        match &blocks[0] {
            Block::Paragraph(st) => {
                assert!(st.spans.iter().any(|s| s.style.code()));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_link() {
        let blocks = parse_markdown("[link](https://example.com)");
        match &blocks[0] {
            Block::Paragraph(st) => {
                assert!(st.spans.iter().any(
                    |s| matches!(&s.style.link, Some(url) if url.as_ref() == "https://example.com")
                ));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_nested_list() {
        let md = "- parent\n  - child\n  - child2\n- sibling";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 2);
                // First item should have nested children
                assert!(!items[0].children.is_empty());
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn styled_text_merges_adjacent() {
        let mut st = StyledText::default();
        st.push_text("hello", SpanStyle::plain());
        st.push_text(" world", SpanStyle::plain());
        assert_eq!(st.spans.len(), 1);
        assert_eq!(st.spans[0].end, 11);
    }

    #[test]
    fn parse_empty_input() {
        let blocks = parse_markdown("");
        assert_eq!(blocks.len(), 0);
    }

    #[test]
    fn parse_indented_code_block() {
        let blocks = parse_markdown("    fn foo() {}\n    bar()\n");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Code { language, code } => {
                assert!(language.is_empty());
                assert!(code.contains("fn foo()"));
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn parse_heading_levels_1_to_6() {
        let md = "# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 6);
        for (i, block) in blocks.iter().enumerate() {
            match block {
                Block::Heading { level, .. } => {
                    assert_eq!(usize::from(*level), i + 1);
                }
                other => panic!("expected heading at index {i}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_nested_blockquote() {
        let blocks = parse_markdown("> outer\n>> inner\n");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Quote(outer) => {
                assert!(!outer.is_empty());
                // The inner blockquote should appear as a nested Quote.
                assert!(
                    outer.iter().any(|b| matches!(b, Block::Quote(_))),
                    "expected a nested blockquote"
                );
            }
            other => panic!("expected quote, got {other:?}"),
        }
    }

    #[test]
    fn parse_table_alignment() {
        let md = "| L | C | R |\n|:---|:---:|---:|\n| a | b | c |\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Table { alignments, .. } => {
                assert_eq!(alignments.len(), 3);
                assert_eq!(alignments[0], Alignment::Left);
                assert_eq!(alignments[1], Alignment::Center);
                assert_eq!(alignments[2], Alignment::Right);
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn parse_header_only_table() {
        let md = "| A | B |\n|---|---|\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Table { header, rows, .. } => {
                assert_eq!(header.len(), 2);
                assert!(rows.is_empty());
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn parse_mixed_list_nesting() {
        let md = "- bullet\n  1. ordered a\n  2. ordered b\n- bullet2\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 2);
                assert!(
                    items[0]
                        .children
                        .iter()
                        .any(|b| matches!(b, Block::OrderedList { .. })),
                    "expected ordered list nested inside unordered list"
                );
            }
            other => panic!("expected unordered list, got {other:?}"),
        }
    }

    #[test]
    fn parse_multiple_links_in_paragraph() {
        let md = "Visit [a](https://a.com) and [b](https://b.com) today.";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Paragraph(st) => {
                let link_count = st.spans.iter().filter(|s| s.style.link.is_some()).count();
                assert!(
                    link_count >= 2,
                    "expected at least 2 links, got {link_count}"
                );
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_strikethrough() {
        let blocks = parse_markdown("This is ~~deleted~~ text");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Paragraph(st) => {
                assert!(
                    st.spans.iter().any(|s| s.style.strikethrough()),
                    "expected a strikethrough span"
                );
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_image() {
        // pulldown_cmark wraps images in paragraphs at the inline level;
        // verify the parser handles them without crashing and produces a block.
        let blocks = parse_markdown("![alt text](https://img.png \"title\")");
        assert_eq!(blocks.len(), 1);
        // The image is represented as a Paragraph (inline image) or Image block.
        assert!(
            matches!(&blocks[0], Block::Paragraph(_) | Block::Image { .. }),
            "expected paragraph or image block"
        );
    }

    #[test]
    fn parse_crlf_line_endings() {
        let md = "# Hello\r\n\r\nParagraph\r\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], Block::Heading { level: 1, .. }));
        assert!(matches!(&blocks[1], Block::Paragraph(_)));
    }

    #[test]
    fn parse_unicode_headings() {
        let md = "# 你好世界\n## 🚀 Rocket\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 1);
                assert_eq!(text.text, "你好世界");
            }
            other => panic!("expected heading, got {other:?}"),
        }
        match &blocks[1] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 2);
                assert!(text.text.contains('🚀'));
            }
            other => panic!("expected heading, got {other:?}"),
        }
    }

    #[test]
    fn parse_large_document_perf() {
        // Build a ~50KB document with various block types.
        let mut doc = String::with_capacity(50_000);
        for i in 0..200 {
            write!(doc, "## Heading {i}\n\n").ok();
            doc.push_str("Lorem ipsum dolor sit amet, consectetur adipiscing elit. ");
            doc.push_str("Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\n\n");
            if i % 5 == 0 {
                doc.push_str("```rust\nfn example() { /* code */ }\n```\n\n");
            }
            if i % 3 == 0 {
                doc.push_str("- item one\n- item two\n- item three\n\n");
            }
        }
        let start = std::time::Instant::now();
        let iterations = 100;
        for _ in 0..iterations {
            let blocks = parse_markdown(&doc);
            assert!(!blocks.is_empty());
        }
        let elapsed = start.elapsed();
        let per_iter = elapsed / iterations;
        // Should complete in < 5ms per parse in release; debug is 10-50x slower.
        if cfg!(not(debug_assertions)) {
            assert!(
                per_iter.as_millis() < 5,
                "parse too slow: {per_iter:?} per iteration for {}KB",
                doc.len() / 1024
            );
        }
    }

    #[test]
    fn parse_task_list_checked() {
        let blocks = parse_markdown("- [x] done");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].checked, Some(true));
                assert_eq!(items[0].content.text, "done");
            }
            other => panic!("expected unordered list, got {other:?}"),
        }
    }

    #[test]
    fn parse_task_list_unchecked() {
        let blocks = parse_markdown("- [ ] todo");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].checked, Some(false));
                assert_eq!(items[0].content.text, "todo");
            }
            other => panic!("expected unordered list, got {other:?}"),
        }
    }

    #[test]
    fn parse_task_list_mixed() {
        let md = "- [x] checked\n- [ ] unchecked\n- normal\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0].checked, Some(true));
                assert_eq!(items[1].checked, Some(false));
                assert_eq!(items[2].checked, None);
            }
            other => panic!("expected unordered list, got {other:?}"),
        }
    }
}
