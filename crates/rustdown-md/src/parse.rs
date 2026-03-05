#![forbid(unsafe_code)]
//! Markdown parsing: converts source text into a flat list of render blocks.

use std::rc::Rc;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// A single renderable block produced by parsing.
///
/// Large variants (`Table`) are boxed to keep the enum compact (~56 bytes
/// instead of ~72), improving cache locality during block iteration.
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
    Table(Box<TableData>),
    Image {
        url: String,
        alt: String,
    },
}

/// Table block data, boxed inside `Block::Table` to keep enum size down.
#[derive(Clone, Debug)]
pub struct TableData {
    pub header: Vec<StyledText>,
    pub alignments: Vec<Alignment>,
    pub rows: Vec<Vec<StyledText>>,
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
    #[must_use]
    pub const fn plain() -> Self {
        Self {
            flags: 0,
            link: None,
        }
    }

    #[must_use]
    pub const fn strong(&self) -> bool {
        self.flags & FLAG_STRONG != 0
    }

    pub const fn set_strong(&mut self) {
        self.flags |= FLAG_STRONG;
    }

    #[must_use]
    pub const fn emphasis(&self) -> bool {
        self.flags & FLAG_EMPHASIS != 0
    }

    pub const fn set_emphasis(&mut self) {
        self.flags |= FLAG_EMPHASIS;
    }

    #[must_use]
    pub const fn strikethrough(&self) -> bool {
        self.flags & FLAG_STRIKETHROUGH != 0
    }

    pub const fn set_strikethrough(&mut self) {
        self.flags |= FLAG_STRIKETHROUGH;
    }

    #[must_use]
    pub const fn code(&self) -> bool {
        self.flags & FLAG_CODE != 0
    }

    pub const fn set_code(&mut self) {
        self.flags |= FLAG_CODE;
    }
}

/// An inline formatting span within a `StyledText`.
///
/// Uses `u32` offsets to keep the struct compact (32 bytes instead of 40).
/// Documents larger than 4 GiB are unsupported.
#[derive(Clone, Debug)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub style: SpanStyle,
}

impl StyledText {
    fn push_text(&mut self, s: &str, style: SpanStyle) {
        let start = self.text.len() as u32;
        self.text.push_str(s);
        let end = self.text.len() as u32;
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

/// Parse markdown source into blocks (convenience wrapper for tests).
#[cfg(test)]
pub fn parse_markdown(source: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    parse_markdown_into(source, &mut blocks);
    blocks
}

/// Parse markdown source, appending blocks to an existing `Vec`.
/// Reuses the existing allocation when possible.
pub fn parse_markdown_into(source: &str, blocks: &mut Vec<Block>) {
    let opts = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_SMART_PUNCTUATION
        | Options::ENABLE_GFM;
    let parser = Parser::new_ext(source, opts);
    // Collect into Vec — required for our indexed recursive descent.
    // Pre-allocate based on source size heuristic.
    let events: Vec<Event<'_>> = {
        let capacity = source.len() / 20 + 16;
        let mut v = Vec::with_capacity(capacity);
        v.extend(parser);
        v
    };
    blocks.reserve(events.len() / 4 + 4);
    let mut fmt = InlineState::new();
    let mut i = 0;
    while i < events.len() {
        i += parse_block(&events[i..], blocks, &mut fmt);
    }
}

/// Collect alt text from inline events following a `Start(Image)`.
///
/// Scans events starting at `offset`, consuming `Text`, `Code`, and break
/// events until `End(Image)` is found.  Returns `(alt_text, events_consumed)`.
fn collect_image_alt(events: &[Event<'_>], offset: usize) -> (String, usize) {
    let mut alt = String::new();
    let mut i = offset;
    while i < events.len() {
        match &events[i] {
            Event::End(TagEnd::Image) => {
                i += 1;
                break;
            }
            Event::Text(t) => {
                alt.push_str(t);
                i += 1;
            }
            Event::Code(c) => {
                alt.push_str(c);
                i += 1;
            }
            Event::SoftBreak | Event::HardBreak => {
                alt.push(' ');
                i += 1;
            }
            _ => i += 1,
        }
    }
    (alt, i)
}

fn parse_block(events: &[Event<'_>], blocks: &mut Vec<Block>, fmt: &mut InlineState) -> usize {
    match &events[0] {
        Event::Start(Tag::Heading { level, .. }) => parse_heading(events, *level, blocks, fmt),
        Event::Start(Tag::Paragraph) => parse_paragraph(events, blocks, fmt),
        Event::Start(Tag::CodeBlock(kind)) => {
            let lang = match kind {
                pulldown_cmark::CodeBlockKind::Fenced(l) => l.to_string(),
                pulldown_cmark::CodeBlockKind::Indented => String::new(),
            };
            parse_code_block(events, lang, blocks)
        }
        Event::Start(Tag::BlockQuote(_)) => parse_blockquote(events, blocks, fmt),
        Event::Start(Tag::List(start)) => parse_list(events, *start, blocks, fmt),
        Event::Start(Tag::Table(aligns)) => parse_table(events, aligns, blocks, fmt),
        Event::Start(Tag::Image { dest_url, .. }) => {
            let (alt, end) = collect_image_alt(events, 1);
            blocks.push(Block::Image {
                url: dest_url.to_string(),
                alt,
            });
            end
        }
        Event::Rule => {
            blocks.push(Block::ThematicBreak);
            1
        }
        // Skip events not handled at block level (e.g. stray End tags,
        // FootnoteDefinition, metadata blocks).  Consuming 1 event advances
        // the cursor past the unknown token.
        _ => 1,
    }
}

fn parse_heading(
    events: &[Event<'_>],
    level: HeadingLevel,
    blocks: &mut Vec<Block>,
    fmt: &mut InlineState,
) -> usize {
    let lvl = heading_level_to_u8(level);
    let mut styled = StyledText::default();
    let mut consumed = 1;
    fmt.clear();
    while consumed < events.len() {
        match &events[consumed] {
            Event::End(TagEnd::Heading(_)) => {
                consumed += 1;
                break;
            }
            ev => {
                consume_inline(ev, &mut styled, fmt);
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

fn parse_paragraph(events: &[Event<'_>], blocks: &mut Vec<Block>, fmt: &mut InlineState) -> usize {
    // Check if this paragraph is a standalone image (the only inline content
    // inside the paragraph is a single Image tag). If so, emit Block::Image
    // instead of a paragraph containing alt text.
    if let Some(consumed) = try_parse_standalone_image(events, blocks) {
        return consumed;
    }

    let mut styled = StyledText::default();
    let mut consumed = 1;
    fmt.clear();
    while consumed < events.len() {
        match &events[consumed] {
            Event::End(TagEnd::Paragraph) => {
                consumed += 1;
                break;
            }
            ev => {
                consume_inline(ev, &mut styled, fmt);
                consumed += 1;
            }
        }
    }
    blocks.push(Block::Paragraph(styled));
    consumed
}

/// If the paragraph's *only* child is a single `Image` tag, emit
/// `Block::Image` and return the number of events consumed (including
/// the opening `Start(Paragraph)` and closing `End(Paragraph)`).
fn try_parse_standalone_image(events: &[Event<'_>], blocks: &mut Vec<Block>) -> Option<usize> {
    // events[0] is Start(Paragraph). Expect:
    //   [0] Start(Paragraph)
    //   [1] Start(Image { dest_url, .. })
    //   ... inline text events (alt text) ...
    //   [k] End(Image)
    //   [k+1] End(Paragraph)
    if events.len() < 4 {
        return None;
    }
    let dest_url = match &events[1] {
        Event::Start(Tag::Image { dest_url, .. }) => dest_url.to_string(),
        _ => return None,
    };

    // Use shared alt-text collector, but reject if any unexpected events appear
    // (which would mean this isn't a standalone image paragraph).
    let mut alt = String::new();
    let mut i = 2;
    while i < events.len() {
        match &events[i] {
            Event::End(TagEnd::Image) => {
                i += 1;
                break;
            }
            Event::Text(t) => {
                alt.push_str(t);
                i += 1;
            }
            Event::Code(c) => {
                alt.push_str(c);
                i += 1;
            }
            Event::SoftBreak | Event::HardBreak => {
                alt.push(' ');
                i += 1;
            }
            // Formatting tags inside alt text — skip the tag but keep scanning
            Event::Start(Tag::Strong | Tag::Emphasis | Tag::Strikethrough)
            | Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough) => {
                i += 1;
            }
            _ => return None, // unexpected event → not a standalone image
        }
    }

    // The very next event must be End(Paragraph).
    if i >= events.len() || !matches!(&events[i], Event::End(TagEnd::Paragraph)) {
        return None;
    }
    i += 1; // consume End(Paragraph)

    blocks.push(Block::Image { url: dest_url, alt });
    Some(i)
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

fn parse_blockquote(events: &[Event<'_>], blocks: &mut Vec<Block>, fmt: &mut InlineState) -> usize {
    let mut inner = Vec::new();
    let mut consumed = 1;
    while consumed < events.len() {
        if let Event::End(TagEnd::BlockQuote(_)) = &events[consumed] {
            consumed += 1;
            break;
        }
        let n = parse_block(&events[consumed..], &mut inner, fmt);
        consumed += n;
    }
    blocks.push(Block::Quote(inner));
    consumed
}

fn parse_list(
    events: &[Event<'_>],
    start: Option<u64>,
    blocks: &mut Vec<Block>,
    fmt: &mut InlineState,
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
                fmt.clear();
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
                            let n = parse_block(&events[consumed..], &mut children, fmt);
                            consumed += n;
                        }
                        Event::TaskListMarker(is_checked) => {
                            checked = Some(*is_checked);
                            consumed += 1;
                        }
                        ev => {
                            consume_inline(ev, &mut item_text, fmt);
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
    fmt: &mut InlineState,
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

    let num_cols = aligns.len();
    let mut header = Vec::with_capacity(num_cols);
    let mut rows: Vec<Vec<StyledText>> = Vec::new();
    let mut in_head = false;
    let mut current_row: Vec<StyledText> = Vec::with_capacity(num_cols);
    let mut current_cell = StyledText::default();
    fmt.clear();
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
                fmt.clear();
                consumed += 1;
            }
            Event::End(TagEnd::TableCell) => {
                current_row.push(std::mem::take(&mut current_cell));
                consumed += 1;
            }
            ev => {
                consume_inline(ev, &mut current_cell, fmt);
                consumed += 1;
            }
        }
    }

    blocks.push(Block::Table(Box::new(TableData {
        header,
        alignments,
        rows,
    })));
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

/// Maintains the inline formatting stack and a cached `SpanStyle` that
/// is updated incrementally on push/pop, making style queries O(1).
struct InlineState {
    stack: Vec<InlineFlag>,
    /// Cached flags bitfield (strong | emphasis | strikethrough).
    cached_flags: u8,
    /// Cached link URL (last link on stack, or None).
    cached_link: Option<Rc<str>>,
}

impl InlineState {
    fn new() -> Self {
        Self {
            stack: Vec::with_capacity(4),
            cached_flags: 0,
            cached_link: None,
        }
    }

    fn clear(&mut self) {
        self.stack.clear();
        self.cached_flags = 0;
        self.cached_link = None;
    }

    /// Current `SpanStyle` — O(1), no stack traversal.
    fn style(&self) -> SpanStyle {
        SpanStyle {
            flags: self.cached_flags,
            link: self.cached_link.clone(),
        }
    }

    /// Current `SpanStyle` with the code flag set.
    fn style_with_code(&self) -> SpanStyle {
        SpanStyle {
            flags: self.cached_flags | FLAG_CODE,
            link: self.cached_link.clone(),
        }
    }

    fn push(&mut self, flag: InlineFlag) {
        match &flag {
            InlineFlag::Strong => self.cached_flags |= FLAG_STRONG,
            InlineFlag::Emphasis => self.cached_flags |= FLAG_EMPHASIS,
            InlineFlag::Strikethrough => self.cached_flags |= FLAG_STRIKETHROUGH,
            InlineFlag::Link(url) => self.cached_link = Some(Rc::clone(url)),
        }
        self.stack.push(flag);
    }

    fn pop(&mut self, flag: &InlineFlag) {
        if let Some(pos) = self.stack.iter().rposition(|k| k == flag) {
            // swap_remove is O(1); order doesn't matter because the cache is
            // rebuilt from the full remaining set (bitwise OR of all flags).
            self.stack.swap_remove(pos);
            self.rebuild_cache();
        }
    }

    fn pop_link(&mut self) {
        if let Some(pos) = self
            .stack
            .iter()
            .rposition(|k| matches!(k, InlineFlag::Link(_)))
        {
            // See pop() above — order is irrelevant for flag accumulation.
            self.stack.swap_remove(pos);
            self.rebuild_cache();
        }
    }

    /// Rebuild cached flags from the stack (only needed on pop).
    fn rebuild_cache(&mut self) {
        self.cached_flags = 0;
        self.cached_link = None;
        for flag in &self.stack {
            match flag {
                InlineFlag::Strong => self.cached_flags |= FLAG_STRONG,
                InlineFlag::Emphasis => self.cached_flags |= FLAG_EMPHASIS,
                InlineFlag::Strikethrough => self.cached_flags |= FLAG_STRIKETHROUGH,
                InlineFlag::Link(url) => self.cached_link = Some(Rc::clone(url)),
            }
        }
    }
}

fn consume_inline(event: &Event<'_>, styled: &mut StyledText, state: &mut InlineState) {
    match event {
        Event::Text(t) => {
            let style = state.style();
            styled.push_text(t, style);
        }
        Event::Code(c) => {
            let style = state.style_with_code();
            styled.push_text(c, style);
        }
        Event::SoftBreak => {
            let style = state.style();
            styled.push_text(" ", style);
        }
        Event::HardBreak => {
            let style = state.style();
            styled.push_text("\n", style);
        }
        Event::Start(Tag::Strong) => state.push(InlineFlag::Strong),
        Event::End(TagEnd::Strong) => state.pop(&InlineFlag::Strong),
        Event::Start(Tag::Emphasis) => state.push(InlineFlag::Emphasis),
        Event::End(TagEnd::Emphasis) => state.pop(&InlineFlag::Emphasis),
        Event::Start(Tag::Strikethrough) => state.push(InlineFlag::Strikethrough),
        Event::End(TagEnd::Strikethrough) => state.pop(&InlineFlag::Strikethrough),
        Event::Start(Tag::Link { dest_url, .. }) => {
            state.push(InlineFlag::Link(Rc::from(dest_url.as_ref())));
        }
        Event::End(TagEnd::Link) => state.pop_link(),
        // Render footnote references as bracketed text.
        Event::FootnoteReference(label) => {
            let style = state.style();
            styled.push_text("[", style.clone());
            styled.push_text(label, style.clone());
            styled.push_text("]", style);
        }
        // Render inline HTML as plain text.
        Event::InlineHtml(html) | Event::Html(html) => {
            let style = state.style_with_code();
            styled.push_text(html, style);
        }
        // Math events (not enabled by default, but handle gracefully).
        Event::InlineMath(math) | Event::DisplayMath(math) => {
            let style = state.style_with_code();
            styled.push_text(math, style);
        }
        _ => {}
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
            Block::Table(table) => {
                let TableData {
                    header,
                    rows,
                    alignments,
                } = table.as_ref();
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
            Block::Table(table) => {
                let TableData { alignments, .. } = table.as_ref();
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
            Block::Table(table) => {
                let TableData { header, rows, .. } = table.as_ref();
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
        // Standalone images (only child of a paragraph) should produce Block::Image.
        let blocks = parse_markdown("![alt text](https://img.png \"title\")");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Image { url, alt } => {
                assert_eq!(url, "https://img.png");
                assert_eq!(alt, "alt text");
            }
            other => panic!("expected Image block, got {other:?}"),
        }
    }

    #[test]
    fn parse_image_without_alt() {
        let blocks = parse_markdown("![](image.png)");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Image { url, alt } => {
                assert_eq!(url, "image.png");
                assert!(alt.is_empty());
            }
            other => panic!("expected Image block, got {other:?}"),
        }
    }

    #[test]
    fn parse_image_inline_with_text() {
        // Image mixed with text in the same paragraph stays as Paragraph.
        let blocks = parse_markdown("See this: ![pic](img.png) in text.");
        assert_eq!(blocks.len(), 1);
        assert!(
            matches!(&blocks[0], Block::Paragraph(_)),
            "image mixed with text should stay as paragraph"
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

    /// Verify that all bytes in styled text are covered by spans (no gaps).
    fn assert_spans_cover_text(st: &StyledText) {
        if st.text.is_empty() {
            assert!(st.spans.is_empty(), "empty text should have no spans");
            return;
        }
        assert!(!st.spans.is_empty(), "non-empty text should have spans");
        assert_eq!(st.spans[0].start, 0, "first span should start at 0");
        for window in st.spans.windows(2) {
            assert_eq!(
                window[0].end, window[1].start,
                "spans should be contiguous: {:?} then {:?}",
                window[0], window[1]
            );
        }
        assert_eq!(
            st.spans.last().map(|s| s.end),
            Some(st.text.len() as u32),
            "last span should end at text length"
        );
    }

    #[test]
    fn spans_cover_all_paragraph_bytes() {
        let cases = [
            "Hello world",
            "Hello **bold** world",
            "**bold** *italic* ~~strike~~ `code`",
            "A [link](https://x.com) here",
            "**bold *bold-italic* bold**",
            "Mixed **bold** and *italic* with `code` and [link](url)",
        ];
        for md in &cases {
            let blocks = parse_markdown(md);
            for block in &blocks {
                if let Block::Paragraph(st) = block {
                    assert_spans_cover_text(st);
                }
            }
        }
    }

    #[test]
    fn spans_cover_all_heading_bytes() {
        let md = "# Simple\n## **Bold** heading\n### `Code` in heading";
        let blocks = parse_markdown(md);
        for block in &blocks {
            if let Block::Heading { text, .. } = block {
                assert_spans_cover_text(text);
            }
        }
    }

    #[test]
    fn spans_cover_list_item_bytes() {
        let md = "- Item with **bold**\n- Item with `code`\n- [Link](url) item";
        let blocks = parse_markdown(md);
        for block in &blocks {
            if let Block::UnorderedList(items) = block {
                for item in items {
                    assert_spans_cover_text(&item.content);
                }
            }
        }
    }

    #[test]
    fn spans_cover_table_cell_bytes() {
        let md = "| **Bold** | `Code` | [Link](url) |\n|---|---|---|\n| a | b | c |";
        let blocks = parse_markdown(md);
        for block in &blocks {
            if let Block::Table(table) = block {
                let TableData { header, rows, .. } = table.as_ref();
                for cell in header {
                    assert_spans_cover_text(cell);
                }
                for row in rows {
                    for cell in row {
                        assert_spans_cover_text(cell);
                    }
                }
            }
        }
    }

    #[test]
    fn inline_state_pop_without_push() {
        let mut state = InlineState::new();
        // Pop Strong without any preceding push — must not crash.
        state.pop(&InlineFlag::Strong);
        assert!(state.stack.is_empty());
        assert_eq!(state.cached_flags, 0);
        assert!(state.cached_link.is_none());
    }

    #[test]
    fn parse_empty_code_block() {
        let blocks = parse_markdown("```\n```\n");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Code { language, code } => {
                assert!(language.is_empty());
                assert!(code.is_empty());
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn parse_table_column_mismatch() {
        let md = "| A | B | C |\n|---|---|---|\n| 1 | 2 |\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Table(table) => {
                let TableData {
                    header,
                    rows,
                    alignments,
                } = table.as_ref();
                assert_eq!(header.len(), 3);
                assert_eq!(alignments.len(), 3);
                assert_eq!(rows.len(), 1);
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn parse_empty_list_items() {
        let blocks = parse_markdown("- \n- text\n");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 2);
                assert!(items[0].content.text.is_empty());
                assert_eq!(items[1].content.text, "text");
            }
            other => panic!("expected unordered list, got {other:?}"),
        }
    }

    #[test]
    fn parse_ordered_list_starting_at_zero() {
        let blocks = parse_markdown("0. zero\n1. one\n");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 0);
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].content.text, "zero");
                assert_eq!(items[1].content.text, "one");
            }
            other => panic!("expected ordered list, got {other:?}"),
        }
    }

    #[test]
    fn parse_deeply_nested_blockquotes() {
        let blocks = parse_markdown("> > > deep\n");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Quote(level1) => {
                assert!(
                    level1.iter().any(|b| matches!(b, Block::Quote(_))),
                    "expected second level of nesting"
                );
                // Find the inner Quote and check for a third level.
                for b in level1 {
                    if let Block::Quote(level2) = b {
                        assert!(
                            level2.iter().any(|b2| matches!(b2, Block::Quote(_))),
                            "expected third level of nesting"
                        );
                    }
                }
            }
            other => panic!("expected quote, got {other:?}"),
        }
    }

    #[test]
    fn parse_code_block_without_closing_fence() {
        let blocks = parse_markdown("```rust\ncode\n");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Code { language, code } => {
                assert_eq!(language, "rust");
                assert!(code.contains("code"));
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn parse_heading_with_mixed_formatting() {
        let blocks = parse_markdown("# **bold** and *italic*\n");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 1);
                assert!(text.text.contains("bold"));
                assert!(text.text.contains("italic"));
                assert!(
                    text.spans.iter().any(|s| s.style.strong()),
                    "expected a strong span in heading"
                );
                assert!(
                    text.spans.iter().any(|s| s.style.emphasis()),
                    "expected an emphasis span in heading"
                );
            }
            other => panic!("expected heading, got {other:?}"),
        }
    }

    #[test]
    fn parse_empty_table_no_data_rows() {
        let md = "| X | Y |\n|---|---|\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Table(table) => {
                let TableData { header, rows, .. } = table.as_ref();
                assert_eq!(header.len(), 2);
                assert!(rows.is_empty());
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn parse_multiple_consecutive_headings() {
        let blocks = parse_markdown("# H1\n## H2\n### H3\n");
        assert_eq!(blocks.len(), 3);
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 1);
                assert_eq!(text.text, "H1");
            }
            other => panic!("expected heading, got {other:?}"),
        }
        match &blocks[1] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 2);
                assert_eq!(text.text, "H2");
            }
            other => panic!("expected heading, got {other:?}"),
        }
        match &blocks[2] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 3);
                assert_eq!(text.text, "H3");
            }
            other => panic!("expected heading, got {other:?}"),
        }
    }

    // ── Round 9: Edge case parsing tests ──────────────────────────────

    #[test]
    fn parse_whitespace_only_input() {
        let blocks = parse_markdown("   \n\n   \n");
        assert!(
            blocks.is_empty(),
            "whitespace-only should produce no blocks"
        );
    }

    #[test]
    fn parse_triple_emphasis() {
        let blocks = parse_markdown("***bold and italic***");
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert_eq!(text.text, "bold and italic");
                assert!(
                    text.spans
                        .iter()
                        .any(|s| s.style.strong() && s.style.emphasis()),
                    "should have span with both strong and emphasis"
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_strikethrough_with_code() {
        let blocks = parse_markdown("~~deleted `code` deleted~~");
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert!(text.text.contains("code"), "should contain code text");
                assert!(
                    text.spans.iter().any(|s| s.style.strikethrough()),
                    "should have strikethrough span"
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_code_block_inside_blockquote() {
        let md = "> ```rust\n> fn main() {}\n> ```\n";
        let blocks = parse_markdown(md);
        match &blocks[0] {
            Block::Quote(inner) => {
                assert!(
                    inner.iter().any(|b| matches!(b, Block::Code { .. })),
                    "blockquote should contain a code block"
                );
            }
            other => panic!("expected Quote, got {other:?}"),
        }
    }

    #[test]
    fn parse_multiple_blank_lines() {
        let md = "para1\n\n\n\n\npara2";
        let blocks = parse_markdown(md);
        let para_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::Paragraph(_)))
            .count();
        assert_eq!(
            para_count, 2,
            "should have exactly 2 paragraphs regardless of blank lines"
        );
    }

    #[test]
    fn parse_escaped_characters() {
        let md = "\\# Not a heading\n\n\\* Not a bullet\n";
        let blocks = parse_markdown(md);
        assert!(
            blocks.iter().all(|b| matches!(b, Block::Paragraph(_))),
            "escaped markdown should parse as plain paragraphs"
        );
    }

    #[test]
    fn parse_indented_code_two_lines() {
        let md = "    indented code\n    second line\n";
        let blocks = parse_markdown(md);
        assert!(
            blocks.iter().any(|b| matches!(b, Block::Code { .. })),
            "4-space indented text should parse as code block"
        );
    }

    #[test]
    fn parse_angle_bracket_autolink() {
        let md = "Visit <https://example.com> for more.";
        let blocks = parse_markdown(md);
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert!(
                    text.spans.iter().any(|s| s.style.link.is_some()),
                    "angle-bracket URL should be auto-linked: spans={:?}",
                    text.spans
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_setext_headings() {
        let md = "H1 Heading\n==========\n\nH2 Heading\n----------\n";
        let blocks = parse_markdown(md);
        let headings: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Heading { level, text } => Some((*level, text.text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(headings.len(), 2, "should parse 2 setext headings");
        assert_eq!(headings[0].0, 1, "first should be H1");
        assert_eq!(headings[1].0, 2, "second should be H2");
    }

    #[test]
    fn parse_image_alt_text_from_inline_events() {
        let md = "![alt text](img.png)";
        let blocks = parse_markdown(md);
        match &blocks[0] {
            Block::Image { alt, .. } => {
                assert_eq!(alt, "alt text", "alt text should come from brackets");
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }
}
