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
    let mut alt = String::with_capacity(64);
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
                pulldown_cmark::CodeBlockKind::Fenced(l) if !l.is_empty() => l.as_ref().to_owned(),
                _ => String::new(),
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
    let mut alt = String::with_capacity(64);
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
    let mut code = String::with_capacity(256);
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
    let mut inner = Vec::with_capacity(4);
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
    let mut items = Vec::with_capacity(8);
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
                // Track whether the first paragraph has been fully consumed.
                // In loose lists, pulldown-cmark wraps each item's text in
                // Paragraph start/end events; subsequent paragraphs become
                // child blocks.
                let mut first_para_done = false;
                // Collect inline text for a secondary paragraph inside the
                // item, to be flushed as `Block::Paragraph` into `children`.
                let mut extra_para: Option<StyledText> = None;
                while consumed < events.len() {
                    match &events[consumed] {
                        Event::End(TagEnd::Item) => {
                            // Flush any trailing extra paragraph.
                            if let Some(ep) = extra_para.take()
                                && !ep.text.is_empty()
                            {
                                children.push(Block::Paragraph(ep));
                            }
                            consumed += 1;
                            break;
                        }
                        Event::Start(Tag::Paragraph) => {
                            consumed += 1;
                            if first_para_done {
                                // Start collecting a new paragraph into
                                // `extra_para`; it will be flushed on
                                // `End(Paragraph)` or `End(Item)`.
                                extra_para = Some(StyledText::default());
                                fmt.clear();
                            }
                        }
                        Event::End(TagEnd::Paragraph) => {
                            consumed += 1;
                            if let Some(ep) = extra_para.take()
                                && !ep.text.is_empty()
                            {
                                children.push(Block::Paragraph(ep));
                            }
                            first_para_done = true;
                        }
                        // Block-level children: delegate to `parse_block`.
                        Event::Start(
                            Tag::List(_)
                            | Tag::CodeBlock(_)
                            | Tag::BlockQuote(_)
                            | Tag::Heading { .. }
                            | Tag::Table(_)
                            | Tag::HtmlBlock,
                        ) => {
                            // Flush any in-progress extra paragraph first.
                            if let Some(ep) = extra_para.take()
                                && !ep.text.is_empty()
                            {
                                children.push(Block::Paragraph(ep));
                            }
                            let n = parse_block(&events[consumed..], &mut children, fmt);
                            consumed += n;
                        }
                        Event::Rule => {
                            if let Some(ep) = extra_para.take()
                                && !ep.text.is_empty()
                            {
                                children.push(Block::Paragraph(ep));
                            }
                            children.push(Block::ThematicBreak);
                            consumed += 1;
                        }
                        Event::TaskListMarker(is_checked) => {
                            checked = Some(*is_checked);
                            consumed += 1;
                        }
                        ev => {
                            // Inline content: route to current paragraph
                            // target (extra_para if active, else item_text).
                            if let Some(ref mut ep) = extra_para {
                                consume_inline(ev, ep, fmt);
                            } else {
                                consume_inline(ev, &mut item_text, fmt);
                            }
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
    let mut rows: Vec<Vec<StyledText>> = Vec::with_capacity(16);
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
            // Build bracket text in one shot to avoid multiple style clones.
            let style = state.style();
            let mut ref_text = String::with_capacity(label.len() + 2);
            ref_text.push('[');
            ref_text.push_str(label);
            ref_text.push(']');
            styled.push_text(&ref_text, style);
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
#[allow(clippy::panic, clippy::expect_used)]
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

    // ── Stress tests: edge-case correctness ───────────────────────────

    #[test]
    fn stress_table_more_data_cols_than_headers() {
        // pulldown-cmark truncates extra columns to match header count
        let md = "| A | B |\n|---|---|\n| 1 | 2 | 3 | 4 |\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Table(table) => {
                let TableData { header, rows, .. } = table.as_ref();
                assert_eq!(header.len(), 2);
                assert_eq!(rows.len(), 1);
                // Extra columns are trimmed by pulldown-cmark
                assert!(
                    rows[0].len() <= header.len() + 2,
                    "row cols ({}) should not be wildly larger than header ({})",
                    rows[0].len(),
                    header.len()
                );
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn stress_table_fewer_data_cols_than_headers() {
        let md = "| A | B | C | D |\n|---|---|---|---|\n| 1 |\n| x | y |\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Table(table) => {
                let TableData { header, rows, .. } = table.as_ref();
                assert_eq!(header.len(), 4);
                assert_eq!(rows.len(), 2);
                // Rows may have fewer cells than headers
                assert!(rows[0].len() <= 4);
                assert!(rows[1].len() <= 4);
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn stress_table_alignment_with_long_headers() {
        let long_a = "A".repeat(200);
        let long_b = "B".repeat(300);
        let md = format!("| {long_a} | {long_b} | Short |\n|:---|:---:|---:|\n| x | y | z |\n");
        let blocks = parse_markdown(&md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Table(table) => {
                let TableData {
                    header,
                    alignments,
                    rows,
                } = table.as_ref();
                assert_eq!(header.len(), 3);
                assert_eq!(header[0].text, long_a);
                assert_eq!(header[1].text, long_b);
                assert_eq!(alignments[0], Alignment::Left);
                assert_eq!(alignments[1], Alignment::Center);
                assert_eq!(alignments[2], Alignment::Right);
                assert_eq!(rows.len(), 1);
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn stress_table_cell_with_all_formatting() {
        let md = concat!(
            "| Cell |\n|---|\n",
            "| **bold** *italic* `code` [link](url) ~~strike~~ |\n"
        );
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Table(table) => {
                let TableData { rows, .. } = table.as_ref();
                assert_eq!(rows.len(), 1);
                let cell = &rows[0][0];
                assert!(cell.text.contains("bold"));
                assert!(cell.text.contains("italic"));
                assert!(cell.text.contains("code"));
                assert!(cell.text.contains("link"));
                assert!(cell.text.contains("strike"));
                assert!(cell.spans.iter().any(|s| s.style.strong()));
                assert!(cell.spans.iter().any(|s| s.style.emphasis()));
                assert!(cell.spans.iter().any(|s| s.style.code()));
                assert!(cell.spans.iter().any(|s| s.style.link.is_some()));
                assert!(cell.spans.iter().any(|s| s.style.strikethrough()));
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn stress_empty_table_headers_only() {
        let md = "| H1 | H2 | H3 |\n|---|---|---|\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Table(table) => {
                let TableData { header, rows, .. } = table.as_ref();
                assert_eq!(header.len(), 3);
                assert_eq!(header[0].text, "H1");
                assert_eq!(header[1].text, "H2");
                assert_eq!(header[2].text, "H3");
                assert!(rows.is_empty(), "no data rows expected");
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn stress_large_table_100_rows_20_cols() {
        let mut md = String::with_capacity(100_000);
        // Header
        md.push('|');
        for c in 0..20 {
            write!(md, " H{c} |").ok();
        }
        md.push('\n');
        // Separator
        md.push('|');
        for _ in 0..20 {
            md.push_str("---|");
        }
        md.push('\n');
        // 100 rows
        for r in 0..100 {
            md.push('|');
            for c in 0..20 {
                write!(md, " r{r}c{c} |").ok();
            }
            md.push('\n');
        }

        let blocks = parse_markdown(&md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Table(table) => {
                let TableData {
                    header,
                    rows,
                    alignments,
                } = table.as_ref();
                assert_eq!(header.len(), 20);
                assert_eq!(alignments.len(), 20);
                assert_eq!(rows.len(), 100);
                // Spot-check a few cells
                assert_eq!(rows[0][0].text, "r0c0");
                assert_eq!(rows[99][19].text, "r99c19");
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn stress_deeply_nested_lists_10_levels() {
        let mut md = String::with_capacity(512);
        for depth in 0..10 {
            let indent = "  ".repeat(depth);
            writeln!(md, "{indent}- level {depth}").ok();
        }
        let blocks = parse_markdown(&md);
        assert_eq!(blocks.len(), 1);

        // Walk the nesting chain
        fn count_depth(block: &Block) -> usize {
            match block {
                Block::UnorderedList(items) => {
                    if let Some(child) = items[0].children.first() {
                        1 + count_depth(child)
                    } else {
                        1
                    }
                }
                _ => 0,
            }
        }
        let depth = count_depth(&blocks[0]);
        assert!(
            depth >= 10,
            "expected at least 10 levels of nesting, got {depth}"
        );
    }

    #[test]
    fn stress_mixed_ordered_unordered_nesting() {
        let md = concat!(
            "- bullet A\n",
            "  1. ordered 1\n",
            "     - nested bullet\n",
            "       1. deep ordered\n",
            "  2. ordered 2\n",
            "- bullet B\n",
        );
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 2);
                // First item should have an ordered list child
                assert!(
                    items[0]
                        .children
                        .iter()
                        .any(|b| matches!(b, Block::OrderedList { .. })),
                    "expected ordered list child"
                );
                // Walk into the ordered list to find the nested bullet
                for child in &items[0].children {
                    if let Block::OrderedList {
                        items: ol_items, ..
                    } = child
                        && let Some(Block::UnorderedList(ul_items)) = ol_items[0]
                            .children
                            .iter()
                            .find(|b| matches!(b, Block::UnorderedList(_)))
                    {
                        // The nested bullet should have a deep ordered child
                        assert!(
                            ul_items[0]
                                .children
                                .iter()
                                .any(|b| matches!(b, Block::OrderedList { .. })),
                            "expected deep ordered list inside nested bullet"
                        );
                    }
                }
            }
            other => panic!("expected unordered list, got {other:?}"),
        }
    }

    #[test]
    fn stress_code_block_with_backtick_content() {
        // Use 4-backtick fence to allow triple backticks inside
        let md = "````\n```rust\nfn main() {}\n```\n````\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Code { code, .. } => {
                assert!(
                    code.contains("```rust"),
                    "inner triple backticks should be preserved as text"
                );
                assert!(code.contains("fn main()"));
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn stress_inline_code_with_backticks() {
        // Double backticks allow single backtick inside
        let md = "Use `` `backtick` `` in code";
        let blocks = parse_markdown(md);
        match &blocks[0] {
            Block::Paragraph(st) => {
                assert!(
                    st.text.contains('`'),
                    "inline code should preserve backtick: {:?}",
                    st.text
                );
                assert!(st.spans.iter().any(|s| s.style.code()));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn stress_link_special_chars_in_url() {
        let md = "[spaces](https://example.com/path%20with%20spaces)";
        let blocks = parse_markdown(md);
        match &blocks[0] {
            Block::Paragraph(st) => {
                let link_span = st
                    .spans
                    .iter()
                    .find(|s| s.style.link.is_some())
                    .expect("should have link span");
                let url = link_span.style.link.as_ref().expect("link url");
                assert!(
                    url.contains("spaces"),
                    "URL should contain percent-encoded spaces"
                );
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn stress_link_unicode_url() {
        let md = "[unicode](https://example.com/日本語)";
        let blocks = parse_markdown(md);
        match &blocks[0] {
            Block::Paragraph(st) => {
                let link_span = st
                    .spans
                    .iter()
                    .find(|s| s.style.link.is_some())
                    .expect("should have link span");
                let url = link_span.style.link.as_ref().expect("link url");
                assert!(url.contains("日本語"), "URL should preserve unicode");
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn stress_link_parentheses_in_url() {
        let md = "[parens](https://en.wikipedia.org/wiki/Rust_(programming_language))";
        let blocks = parse_markdown(md);
        match &blocks[0] {
            Block::Paragraph(st) => {
                assert!(
                    st.spans.iter().any(|s| s.style.link.is_some()),
                    "should parse link with parentheses in URL"
                );
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn stress_image_long_alt_with_formatting() {
        let long_alt = "A".repeat(500);
        let md = format!("![**bold** *italic* {long_alt}](img.png)");
        let blocks = parse_markdown(&md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Image { alt, url } => {
                assert_eq!(url, "img.png");
                assert!(alt.contains(&long_alt), "long alt text should be preserved");
                assert!(alt.contains("bold"), "bold text should be in alt");
                assert!(alt.contains("italic"), "italic text should be in alt");
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn stress_blockquote_containing_table() {
        let md = concat!("> | H1 | H2 |\n", "> |---|---|\n", "> | a | b |\n",);
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Quote(inner) => {
                assert!(
                    inner.iter().any(|b| matches!(b, Block::Table(_))),
                    "blockquote should contain a table"
                );
            }
            other => panic!("expected Quote, got {other:?}"),
        }
    }

    #[test]
    fn stress_blockquote_containing_code_and_list() {
        let md = concat!(
            "> ```python\n",
            "> print('hi')\n",
            "> ```\n",
            ">\n",
            "> - item 1\n",
            "> - item 2\n",
        );
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Quote(inner) => {
                assert!(
                    inner.iter().any(|b| matches!(b, Block::Code { .. })),
                    "blockquote should contain code"
                );
                assert!(
                    inner.iter().any(|b| matches!(b, Block::UnorderedList(_))),
                    "blockquote should contain list"
                );
            }
            other => panic!("expected Quote, got {other:?}"),
        }
    }

    #[test]
    fn stress_adjacent_tables_no_blank_line() {
        // Two tables back-to-back; pulldown-cmark may merge or separate them
        let md = concat!("| A |\n|---|\n| 1 |\n", "| B |\n|---|\n| 2 |\n",);
        let blocks = parse_markdown(md);
        // Should produce at least one table (may be merged by parser)
        let table_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::Table(_)))
            .count();
        assert!(
            table_count >= 1,
            "should have at least 1 table, got {table_count}"
        );
    }

    #[test]
    fn stress_huge_single_paragraph() {
        let text = "word ".repeat(20_000); // ~100KB
        let blocks = parse_markdown(&text);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Paragraph(st) => {
                assert!(
                    st.text.len() > 90_000,
                    "paragraph should contain the large text"
                );
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn stress_only_thematic_breaks() {
        let md = "---\n\n***\n\n___\n\n---\n\n***\n";
        let blocks = parse_markdown(md);
        let break_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::ThematicBreak))
            .count();
        assert_eq!(break_count, 5, "should have 5 thematic breaks");
    }

    #[test]
    fn stress_very_long_heading_text() {
        let long_text = "X".repeat(1200);
        let md = format!("# {long_text}\n");
        let blocks = parse_markdown(&md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 1);
                assert_eq!(text.text.len(), 1200);
            }
            other => panic!("expected heading, got {other:?}"),
        }
    }

    #[test]
    fn stress_heading_all_inline_formatting() {
        let md = "## **bold** *italic* `code` [link](url) ~~strike~~\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 2);
                assert!(text.spans.iter().any(|s| s.style.strong()));
                assert!(text.spans.iter().any(|s| s.style.emphasis()));
                assert!(text.spans.iter().any(|s| s.style.code()));
                assert!(text.spans.iter().any(|s| s.style.link.is_some()));
                assert!(text.spans.iter().any(|s| s.style.strikethrough()));
                assert_spans_cover_text(text);
            }
            other => panic!("expected heading, got {other:?}"),
        }
    }

    #[test]
    fn stress_task_lists_nested() {
        let md = concat!(
            "- [x] parent done\n",
            "  - [ ] child todo\n",
            "  - [x] child done\n",
            "- [ ] parent todo\n",
            "  - [ ] nested todo\n",
        );
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].checked, Some(true));
                assert_eq!(items[1].checked, Some(false));
                // Check nested children
                assert!(!items[0].children.is_empty());
                if let Some(Block::UnorderedList(nested)) = items[0].children.first() {
                    assert_eq!(nested.len(), 2);
                    assert_eq!(nested[0].checked, Some(false));
                    assert_eq!(nested[1].checked, Some(true));
                } else {
                    panic!("expected nested list in first item children");
                }
                assert!(!items[1].children.is_empty());
                if let Some(Block::UnorderedList(nested)) = items[1].children.first() {
                    assert_eq!(nested.len(), 1);
                    assert_eq!(nested[0].checked, Some(false));
                } else {
                    panic!("expected nested list in second item children");
                }
            }
            other => panic!("expected unordered list, got {other:?}"),
        }
    }

    #[test]
    fn stress_smart_punctuation() {
        // Smart punctuation converts quotes, dashes, ellipsis
        let md = "\"Hello\" -- world... 'single' --- em";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Paragraph(st) => {
                // Smart punctuation should convert:
                // "Hello" → \u{201c}Hello\u{201d}
                // -- → \u{2013} (en-dash)
                // --- → \u{2014} (em-dash)
                // ... → \u{2026} (ellipsis)
                // 'single' → \u{2018}single\u{2019}
                let t = &st.text;
                assert!(
                    t.contains('\u{201c}') || t.contains('\u{201d}') || t.contains('"'),
                    "should have smart or plain double quotes: {t:?}"
                );
                assert!(
                    t.contains('\u{2026}') || t.contains("..."),
                    "should have ellipsis: {t:?}"
                );
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn stress_adjacent_tables_separated_by_blank() {
        let md = "| A |\n|---|\n| 1 |\n\n| B |\n|---|\n| 2 |\n";
        let blocks = parse_markdown(md);
        let table_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::Table(_)))
            .count();
        assert_eq!(table_count, 2, "blank line should separate into 2 tables");
    }

    #[test]
    fn stress_code_block_with_many_backticks() {
        // 5-backtick fence with 3 and 4 backtick content
        let md = "`````\n```\n````\nsome code\n`````\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Code { code, .. } => {
                assert!(code.contains("```"), "should contain triple backticks");
                assert!(code.contains("````"), "should contain quad backticks");
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn stress_table_empty_cells() {
        let md = "| A | B | C |\n|---|---|---|\n|  |  |  |\n| x |  | z |\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Table(table) => {
                let TableData { rows, .. } = table.as_ref();
                assert_eq!(rows.len(), 2);
                // First row: all empty cells
                assert!(
                    rows[0].iter().all(|c| c.text.is_empty()),
                    "first row should have all empty cells"
                );
                // Second row: mixed
                assert_eq!(rows[1][0].text, "x");
                assert!(rows[1][1].text.is_empty());
                assert_eq!(rows[1][2].text, "z");
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn stress_deeply_nested_blockquote_with_content() {
        let md = concat!(
            "> level 1\n",
            ">> level 2\n",
            ">>> level 3\n",
            ">>>> level 4\n",
            ">>>>> level 5\n",
        );
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);

        fn max_quote_depth(blocks: &[Block]) -> usize {
            let mut max = 0;
            for b in blocks {
                if let Block::Quote(inner) = b {
                    let child_depth = max_quote_depth(inner);
                    if 1 + child_depth > max {
                        max = 1 + child_depth;
                    }
                }
            }
            max
        }

        let depth = max_quote_depth(&blocks);
        assert!(
            depth >= 5,
            "expected at least 5 levels of blockquote nesting, got {depth}"
        );
    }

    #[test]
    fn stress_list_items_with_paragraphs_and_code() {
        let md = concat!(
            "1. First item\n\n",
            "   ```rust\n",
            "   let x = 1;\n",
            "   ```\n\n",
            "2. Second item\n",
        );
        let blocks = parse_markdown(md);
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, Block::OrderedList { .. })),
            "should contain an ordered list"
        );
    }

    #[test]
    fn stress_heading_with_link_and_code() {
        let md = "### [`parse`](https://docs.rs) function\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 3);
                assert!(text.text.contains("parse"));
                assert!(text.spans.iter().any(|s| s.style.code()));
                assert!(text.spans.iter().any(|s| s.style.link.is_some()));
            }
            other => panic!("expected heading, got {other:?}"),
        }
    }

    #[test]
    fn stress_footnote_reference_inline() {
        let md = "Text with a footnote[^1].\n\n[^1]: The footnote content.\n";
        let blocks = parse_markdown(md);
        // Should not crash; footnote references render as bracketed text
        assert!(
            !blocks.is_empty(),
            "document with footnote should produce blocks"
        );
    }

    #[test]
    fn stress_html_inline_in_paragraph() {
        let md = "Text with <strong>html</strong> inline.\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Paragraph(st) => {
                assert!(st.text.contains("html"));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn stress_spans_cover_large_table_cells() {
        let md = concat!(
            "| **Bold** `code` | *it* ~~s~~ [lnk](u) |\n",
            "|---|---|\n",
            "| **x** *y* | `a` ~~b~~ |\n",
        );
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
    fn stress_multiple_images_standalone() {
        // Each image on its own line/paragraph should produce separate Image blocks
        let md = "![a](1.png)\n\n![b](2.png)\n\n![c](3.png)\n";
        let blocks = parse_markdown(md);
        let image_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::Image { .. }))
            .count();
        assert_eq!(image_count, 3, "should have 3 separate images");
    }

    #[test]
    fn stress_ordered_list_high_start() {
        let md = "42. answer\n43. next\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 42);
                assert_eq!(items.len(), 2);
            }
            other => panic!("expected ordered list, got {other:?}"),
        }
    }

    #[test]
    fn stress_only_newlines() {
        let md = "\n\n\n\n\n\n\n\n";
        let blocks = parse_markdown(md);
        assert!(blocks.is_empty(), "only newlines should produce no blocks");
    }

    #[test]
    fn stress_mixed_block_types_rapid_succession() {
        let md = concat!(
            "# Heading\n",
            "Paragraph text.\n\n",
            "---\n\n",
            "- list item\n\n",
            "> quote\n\n",
            "```\ncode\n```\n\n",
            "| T |\n|---|\n| v |\n\n",
            "![img](x.png)\n",
        );
        let blocks = parse_markdown(md);
        // Should have one of each type
        assert!(blocks.iter().any(|b| matches!(b, Block::Heading { .. })));
        assert!(blocks.iter().any(|b| matches!(b, Block::Paragraph(_))));
        assert!(blocks.iter().any(|b| matches!(b, Block::ThematicBreak)));
        assert!(blocks.iter().any(|b| matches!(b, Block::UnorderedList(_))));
        assert!(blocks.iter().any(|b| matches!(b, Block::Quote(_))));
        assert!(blocks.iter().any(|b| matches!(b, Block::Code { .. })));
        assert!(blocks.iter().any(|b| matches!(b, Block::Table(_))));
        assert!(blocks.iter().any(|b| matches!(b, Block::Image { .. })));
    }

    // ── Inline formatting stress tests ─────────────────────────────

    /// Validate invariants on a `StyledText`:
    /// - All spans have `start < end`
    /// - No span exceeds `text.len()`
    /// - Spans cover every byte (no gaps)
    /// - Spans don't overlap
    fn validate_styled_text(st: &StyledText) {
        let text_len = st.text.len() as u32;

        if st.text.is_empty() {
            assert!(st.spans.is_empty(), "empty text should have no spans");
            return;
        }

        assert!(!st.spans.is_empty(), "non-empty text should have spans");

        for (i, span) in st.spans.iter().enumerate() {
            assert!(
                span.start < span.end,
                "span {i}: start ({}) must be < end ({})",
                span.start,
                span.end
            );
            assert!(
                span.end <= text_len,
                "span {i}: end ({}) exceeds text len ({text_len})",
                span.end
            );
        }

        // Check contiguity: first span starts at 0, last ends at text_len,
        // and each span starts where the previous one ended.
        assert_eq!(
            st.spans[0].start, 0,
            "first span should start at 0, got {}",
            st.spans[0].start
        );
        assert_eq!(
            st.spans.last().expect("non-empty").end,
            text_len,
            "last span should end at text len ({text_len})"
        );
        for i in 1..st.spans.len() {
            assert_eq!(
                st.spans[i].start,
                st.spans[i - 1].end,
                "gap between span {} (end {}) and span {i} (start {})",
                i - 1,
                st.spans[i - 1].end,
                st.spans[i].start
            );
        }
    }

    /// Extract the first paragraph's `StyledText` from parsed markdown.
    fn parse_paragraph(md: &str) -> StyledText {
        let blocks = parse_markdown(md);
        match blocks.into_iter().next() {
            Some(Block::Paragraph(st)) => st,
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    // ── 1. Span merging ───────────────────────────────────────────

    #[test]
    fn inline_merge_adjacent_bold() {
        // push_text should merge adjacent spans of the same style.
        let mut st = StyledText::default();
        let mut bold = SpanStyle::plain();
        bold.set_strong();
        st.push_text("bold1", bold.clone());
        st.push_text("bold2", bold);
        assert_eq!(st.text, "bold1bold2");
        assert_eq!(
            st.spans.len(),
            1,
            "adjacent identical-style bold spans should merge into one"
        );
        assert!(st.spans[0].style.strong());
        validate_styled_text(&st);
    }

    #[test]
    fn inline_no_merge_different_styles() {
        // *italic*normal*italic* — styles differ so spans must not merge.
        let st = parse_paragraph("*italic*normal*italic*");
        assert_eq!(st.text, "italicnormalitalic");
        assert!(
            st.spans.len() >= 3,
            "different-style spans should not merge, got {} span(s)",
            st.spans.len()
        );
        assert!(st.spans[0].style.emphasis());
        assert!(!st.spans[1].style.emphasis());
        assert!(st.spans[2].style.emphasis());
        validate_styled_text(&st);
    }

    #[test]
    fn inline_merge_plain_fragments() {
        // Multiple adjacent plain text nodes should merge.
        let mut st = StyledText::default();
        st.push_text("aaa", SpanStyle::plain());
        st.push_text("bbb", SpanStyle::plain());
        st.push_text("ccc", SpanStyle::plain());
        assert_eq!(st.spans.len(), 1);
        assert_eq!(st.text, "aaabbbccc");
        assert_eq!(st.spans[0].start, 0);
        assert_eq!(st.spans[0].end, 9);
    }

    // ── 2. Deeply nested formatting ───────────────────────────────

    #[test]
    fn inline_bold_italic() {
        let st = parse_paragraph("***bold-italic***");
        assert_eq!(st.text, "bold-italic");
        assert_eq!(st.spans.len(), 1);
        assert!(st.spans[0].style.strong());
        assert!(st.spans[0].style.emphasis());
        validate_styled_text(&st);
    }

    #[test]
    fn inline_bold_italic_strikethrough() {
        let st = parse_paragraph("***~~bold-italic-strike~~***");
        assert_eq!(st.text, "bold-italic-strike");
        assert_eq!(st.spans.len(), 1);
        assert!(st.spans[0].style.strong());
        assert!(st.spans[0].style.emphasis());
        assert!(st.spans[0].style.strikethrough());
        validate_styled_text(&st);
    }

    #[test]
    fn inline_bold_italic_link() {
        let st = parse_paragraph("[***bold-italic link***](url)");
        assert_eq!(st.text, "bold-italic link");
        assert_eq!(st.spans.len(), 1);
        assert!(st.spans[0].style.strong());
        assert!(st.spans[0].style.emphasis());
        assert!(
            st.spans[0].style.link.is_some(),
            "expected link URL on span"
        );
        assert_eq!(st.spans[0].style.link.as_deref(), Some("url"));
        validate_styled_text(&st);
    }

    // ── 3. Code span isolation ────────────────────────────────────

    #[test]
    fn inline_code_inside_bold() {
        // **bold `code` bold** — code span inherits strong flag.
        let st = parse_paragraph("**bold `code` bold**");
        assert_eq!(st.text, "bold code bold");
        validate_styled_text(&st);

        let code_spans: Vec<_> = st.spans.iter().filter(|s| s.style.code()).collect();
        assert_eq!(code_spans.len(), 1, "expected exactly one code span");
        assert!(
            code_spans[0].style.strong(),
            "code inside bold should inherit strong flag"
        );
        let code_text = &st.text[code_spans[0].start as usize..code_spans[0].end as usize];
        assert_eq!(code_text, "code");
    }

    #[test]
    fn inline_backtick_sequence() {
        // `a`b`c` — pulldown_cmark treats first and third backtick pairs as code.
        let st = parse_paragraph("`a`b`c`");
        validate_styled_text(&st);
        let code_count = st.spans.iter().filter(|s| s.style.code()).count();
        assert!(
            code_count >= 1,
            "backtick sequence should produce at least one code span"
        );
    }

    #[test]
    fn inline_double_backtick_code() {
        // Double-backtick code spans allow internal backticks.
        let st = parse_paragraph("`` `inner` ``");
        validate_styled_text(&st);
        let code_spans: Vec<_> = st.spans.iter().filter(|s| s.style.code()).collect();
        assert_eq!(code_spans.len(), 1);
        let code_text = &st.text[code_spans[0].start as usize..code_spans[0].end as usize];
        assert!(
            code_text.contains('`'),
            "double-backtick code should preserve inner backtick"
        );
    }

    // ── 4. Link text formatting ───────────────────────────────────

    #[test]
    fn inline_link_with_formatted_text() {
        let st = parse_paragraph("[**bold** and *italic*](url)");
        assert_eq!(st.text, "bold and italic");
        validate_styled_text(&st);

        // All spans should have link set.
        for span in &st.spans {
            assert_eq!(
                span.style.link.as_deref(),
                Some("url"),
                "all spans in link should carry the URL"
            );
        }

        // Check formatting within the link.
        assert!(
            st.spans.iter().any(|s| s.style.strong()),
            "link should contain bold span"
        );
        assert!(
            st.spans.iter().any(|s| s.style.emphasis()),
            "link should contain italic span"
        );
    }

    #[test]
    fn inline_multiple_links_different_urls() {
        let st = parse_paragraph("[aaa](url1) [bbb](url2)");
        validate_styled_text(&st);

        let link_spans: Vec<_> = st.spans.iter().filter(|s| s.style.link.is_some()).collect();
        assert!(
            link_spans.len() >= 2,
            "expected at least 2 link spans, got {}",
            link_spans.len()
        );

        let urls: Vec<&str> = link_spans
            .iter()
            .map(|s| s.style.link.as_deref().expect("link"))
            .collect();
        assert!(urls.contains(&"url1"));
        assert!(urls.contains(&"url2"));
    }

    // ── 5. Span byte boundary validation ──────────────────────────

    #[test]
    fn inline_validate_plain_text() {
        validate_styled_text(&parse_paragraph("Hello world"));
    }

    #[test]
    fn inline_validate_bold_italic_code() {
        validate_styled_text(&parse_paragraph("**bold** and *italic* and `code` end"));
    }

    #[test]
    fn inline_validate_unicode() {
        validate_styled_text(&parse_paragraph("**你好** *世界* `🚀`"));
    }

    #[test]
    fn inline_validate_all_formatting_types() {
        validate_styled_text(&parse_paragraph(
            "plain **bold** *italic* ~~strike~~ `code` [link](url) ***bi*** end",
        ));
    }

    #[test]
    fn inline_validate_nested_formatting() {
        validate_styled_text(&parse_paragraph("***~~all~~***"));
    }

    // ── 6. Empty and edge cases ───────────────────────────────────

    #[test]
    fn inline_empty_bold() {
        // **** is empty bold — pulldown_cmark may not emit it or may emit empty text.
        let blocks = parse_markdown("****");
        // Whatever the parser does, it should not panic and spans should be valid.
        for block in &blocks {
            if let Block::Paragraph(st) = block {
                validate_styled_text(st);
            }
        }
    }

    #[test]
    fn inline_empty_emphasis_underscores() {
        let blocks = parse_markdown("__");
        for block in &blocks {
            if let Block::Paragraph(st) = block {
                validate_styled_text(st);
            }
        }
    }

    #[test]
    fn inline_empty_link() {
        let st = parse_paragraph("[](url)");
        // Empty link text — no text emitted, spans should be empty.
        if st.text.is_empty() {
            assert!(st.spans.is_empty());
        } else {
            validate_styled_text(&st);
        }
    }

    #[test]
    fn inline_unclosed_bold() {
        // ** without closing — treated as literal text by pulldown_cmark.
        let blocks = parse_markdown("**unclosed");
        assert!(!blocks.is_empty());
        if let Some(Block::Paragraph(st)) = blocks.first() {
            validate_styled_text(st);
        }
    }

    #[test]
    fn inline_unclosed_emphasis() {
        let blocks = parse_markdown("*unclosed");
        assert!(!blocks.is_empty());
        if let Some(Block::Paragraph(st)) = blocks.first() {
            validate_styled_text(st);
        }
    }

    #[test]
    fn inline_unclosed_code() {
        let blocks = parse_markdown("`unclosed");
        assert!(!blocks.is_empty());
        if let Some(Block::Paragraph(st)) = blocks.first() {
            validate_styled_text(st);
        }
    }

    #[test]
    fn inline_unclosed_strikethrough() {
        let blocks = parse_markdown("~~unclosed");
        assert!(!blocks.is_empty());
        if let Some(Block::Paragraph(st)) = blocks.first() {
            validate_styled_text(st);
        }
    }

    // ── 7. Long inline sequences ──────────────────────────────────

    #[test]
    fn inline_100_alternating_bold_normal() {
        let mut md = String::new();
        for i in 0..100 {
            if i % 2 == 0 {
                write!(md, "**bold{i}** ").ok();
            } else {
                write!(md, "normal{i} ").ok();
            }
        }
        let st = parse_paragraph(&md);
        validate_styled_text(&st);

        let bold_count = st.spans.iter().filter(|s| s.style.strong()).count();
        assert_eq!(bold_count, 50, "expected 50 bold spans");

        let plain_count = st.spans.iter().filter(|s| !s.style.strong()).count();
        assert!(plain_count >= 50, "expected at least 50 plain spans");
    }

    #[test]
    fn inline_50_links() {
        let mut md = String::new();
        for i in 0..50 {
            write!(md, "[link{i}](https://example.com/{i}) ").ok();
        }
        let st = parse_paragraph(&md);
        validate_styled_text(&st);

        let link_count = st.spans.iter().filter(|s| s.style.link.is_some()).count();
        assert!(
            link_count >= 50,
            "expected at least 50 link spans, got {link_count}"
        );
    }

    #[test]
    fn inline_100_code_spans() {
        let mut md = String::new();
        for i in 0..100 {
            write!(md, "`code{i}` ").ok();
        }
        let st = parse_paragraph(&md);
        validate_styled_text(&st);

        let code_count = st.spans.iter().filter(|s| s.style.code()).count();
        assert_eq!(code_count, 100, "expected 100 code spans");
    }

    #[test]
    fn inline_deeply_interleaved_formatting() {
        // Bold wrapping italic wrapping strikethrough wrapping code.
        let st = parse_paragraph("**bold *italic ~~strike `code` strike~~ italic* bold**");
        validate_styled_text(&st);
        assert!(
            st.spans.iter().any(|s| s.style.strong()),
            "should have bold"
        );
        assert!(
            st.spans.iter().any(|s| s.style.emphasis()),
            "should have italic"
        );
        assert!(
            st.spans.iter().any(|s| s.style.strikethrough()),
            "should have strikethrough"
        );
        assert!(st.spans.iter().any(|s| s.style.code()), "should have code");
    }

    #[test]
    fn inline_link_with_code() {
        let st = parse_paragraph("[`code` in link](url)");
        validate_styled_text(&st);
        assert!(
            st.spans
                .iter()
                .any(|s| s.style.code() && s.style.link.is_some()),
            "code inside link should have both code and link flags"
        );
    }

    #[test]
    fn inline_adjacent_different_links_no_merge() {
        let st = parse_paragraph("[a](u1)[b](u2)");
        validate_styled_text(&st);
        // Different URLs must not merge.
        let link_count = st.spans.iter().filter(|s| s.style.link.is_some()).count();
        assert!(
            link_count >= 2,
            "different link URLs should produce separate spans"
        );
    }

    #[test]
    fn inline_emphasis_across_softbreak() {
        // Emphasis spanning a soft break (line continuation).
        let st = parse_paragraph("*italic\nacross lines*");
        validate_styled_text(&st);
        assert!(
            st.spans.iter().any(|s| s.style.emphasis()),
            "emphasis should span soft break"
        );
    }

    #[test]
    fn inline_all_formatting_combined_in_link() {
        let st = parse_paragraph("[***~~all~~***](url)");
        validate_styled_text(&st);
        let span = &st.spans[0];
        assert!(span.style.strong());
        assert!(span.style.emphasis());
        assert!(span.style.strikethrough());
        assert!(span.style.link.is_some());
    }

    #[test]
    fn parse_list_with_code_block() {
        let md = "- Item with code:\n\n  ```rust\n  fn main() {}\n  ```\n\n- Next item\n";
        let blocks = parse_markdown(md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert!(!items.is_empty());
                assert!(
                    !items[0].children.is_empty(),
                    "code block should be in children"
                );
                assert!(matches!(&items[0].children[0], Block::Code { .. }));
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    #[test]
    fn parse_list_with_blockquote() {
        let md = "- Item with quote:\n\n  > Quoted text\n\n- Next item\n";
        let blocks = parse_markdown(md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert!(
                    !items[0].children.is_empty(),
                    "blockquote should be in children"
                );
                assert!(matches!(&items[0].children[0], Block::Quote(_)));
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    #[test]
    fn parse_multi_paragraph_list_item() {
        let md = "- First paragraph\n\n  Second paragraph\n\n- Another item\n";
        let blocks = parse_markdown(md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 2);
                // First item should have text in content and the second paragraph in children
                assert!(!items[0].content.text.is_empty());
                assert!(
                    !items[0].children.is_empty(),
                    "second paragraph should be in children"
                );
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    // ── Additional parsing coverage tests ──────────────────────────

    #[test]
    fn parse_heading_trailing_hashes() {
        let blocks = parse_markdown("## Title ##\n");
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 2);
                assert_eq!(text.text.trim(), "Title");
            }
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    #[test]
    fn parse_image_empty_url() {
        let blocks = parse_markdown("![alt text]()\n");
        match &blocks[0] {
            Block::Image { url, alt } => {
                assert!(url.is_empty());
                assert_eq!(alt, "alt text");
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn parse_triple_emphasis_bold_italic() {
        let blocks = parse_markdown("***bold and italic***\n");
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert!(!text.spans.is_empty());
                let span = &text.spans[0];
                assert!(span.style.strong(), "should be strong");
                assert!(span.style.emphasis(), "should be emphasis");
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_escaped_special_chars() {
        let blocks = parse_markdown("\\*not bold\\* and \\[not link\\]\n");
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert!(
                    text.text.contains("*not bold*"),
                    "escaped asterisks should be literal: {:?}",
                    text.text
                );
                assert!(
                    text.text.contains("[not link]"),
                    "escaped brackets should be literal: {:?}",
                    text.text
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_html_entities_decoded() {
        let blocks = parse_markdown("&amp; &lt; &gt; &#123;\n");
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert!(text.text.contains('&'), "should decode &amp;");
                assert!(text.text.contains('<'), "should decode &lt;");
                assert!(text.text.contains('>'), "should decode &gt;");
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_ordered_task_list() {
        let blocks = parse_markdown("1. [x] Done\n2. [ ] Todo\n3. Normal\n");
        match &blocks[0] {
            Block::OrderedList { start, items } => {
                assert_eq!(*start, 1);
                assert_eq!(items.len(), 3);
                assert_eq!(items[0].checked, Some(true));
                assert_eq!(items[1].checked, Some(false));
                assert_eq!(items[2].checked, None);
            }
            other => panic!("expected OrderedList, got {other:?}"),
        }
    }

    #[test]
    fn parse_table_escaped_pipe() {
        let blocks = parse_markdown("| A |\n|---|\n| a \\| b |\n");
        match &blocks[0] {
            Block::Table(table) => {
                assert_eq!(table.header.len(), 1, "should be 1-column table");
                assert!(
                    table.rows[0][0].text.contains("a | b")
                        || table.rows[0][0].text.contains("a \\| b"),
                    "cell should contain literal pipe: {:?}",
                    table.rows[0][0].text
                );
            }
            other => panic!("expected Table, got {other:?}"),
        }
    }

    #[test]
    fn parse_link_with_title() {
        let blocks = parse_markdown("[text](url \"title\")\n");
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert!(
                    text.spans.iter().any(|s| s.style.link.is_some()),
                    "should have a link span"
                );
                let link = text
                    .spans
                    .iter()
                    .find(|s| s.style.link.is_some())
                    .expect("link span");
                assert_eq!(link.style.link.as_deref(), Some("url"));
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_reference_link() {
        let blocks = parse_markdown("[text][ref]\n\n[ref]: https://example.com\n");
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert!(
                    text.spans.iter().any(|s| s.style.link.is_some()),
                    "should have a link span"
                );
                let link = text
                    .spans
                    .iter()
                    .find(|s| s.style.link.is_some())
                    .expect("link span");
                assert_eq!(link.style.link.as_deref(), Some("https://example.com"));
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_indented_code_block_two_lines() {
        let blocks = parse_markdown("    code line 1\n    code line 2\n");
        match &blocks[0] {
            Block::Code { language, code } => {
                assert!(language.is_empty(), "indented code block has no language");
                assert!(code.contains("code line 1"));
                assert!(code.contains("code line 2"));
            }
            other => panic!("expected Code block, got {other:?}"),
        }
    }

    #[test]
    fn parse_hard_line_break() {
        let blocks = parse_markdown("Line one  \nLine two\n");
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert!(
                    (text.text.contains('\n')
                        || text.text.contains("Line one") && text.text.contains("Line two")),
                    "hard line break should be preserved: {:?}",
                    text.text
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_soft_line_break() {
        let blocks = parse_markdown("Line one\nLine two\n");
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert!(
                    text.text.contains("Line one") && text.text.contains("Line two"),
                    "both lines should be present: {:?}",
                    text.text
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_list_with_heading_child() {
        let md = "- Item\n\n  ## Sub-heading\n\n- Next\n";
        let blocks = parse_markdown(md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert!(
                    !items[0].children.is_empty(),
                    "should have heading as child"
                );
                assert!(
                    matches!(&items[0].children[0], Block::Heading { .. }),
                    "child should be Heading"
                );
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    #[test]
    fn parse_list_with_thematic_break_child() {
        let md = "- Item\n\n  ---\n\n- Next\n";
        let blocks = parse_markdown(md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert!(
                    items[0]
                        .children
                        .iter()
                        .any(|b| matches!(b, Block::ThematicBreak)),
                    "should have ThematicBreak as child: {:?}",
                    items[0].children
                );
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    #[test]
    fn parse_list_with_table_child() {
        let md = "- Item\n\n  | A | B |\n  |---|---|\n  | 1 | 2 |\n\n- Next\n";
        let blocks = parse_markdown(md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert!(
                    items[0]
                        .children
                        .iter()
                        .any(|b| matches!(b, Block::Table(_))),
                    "should have Table as child: {:?}",
                    items[0].children
                );
            }
            other => panic!("expected UnorderedList, got {other:?}"),
        }
    }

    #[test]
    fn parse_gfm_strikethrough() {
        let blocks = parse_markdown("~~deleted~~\n");
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert!(!text.spans.is_empty());
                assert!(
                    text.spans[0].style.strikethrough(),
                    "should be strikethrough"
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parse_inline_code_with_backticks() {
        let blocks = parse_markdown("`` `inner` ``\n");
        match &blocks[0] {
            Block::Paragraph(text) => {
                assert!(!text.spans.is_empty());
                assert!(
                    text.spans.iter().any(|s| s.style.code()),
                    "should have code span"
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }
}
