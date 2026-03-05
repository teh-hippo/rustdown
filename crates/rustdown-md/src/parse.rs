#![forbid(unsafe_code)]
//! Markdown parsing: converts source text into a flat list of render blocks.

use std::rc::Rc;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// A single renderable block produced by parsing.
///
/// Large variants (`Table`) are boxed to keep the enum compact (~56 bytes
/// instead of ~72), improving cache locality during block iteration.
///
/// Immutable string fields use `Box<str>` to avoid the 8-byte capacity
/// overhead of `String` — these values are never modified after construction.
#[derive(Clone, Debug)]
pub enum Block {
    Heading {
        level: u8,
        text: StyledText,
    },
    Paragraph(StyledText),
    Code {
        /// Language tag from fenced code blocks (e.g. "rust", "python").
        language: Box<str>,
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
        url: Box<str>,
        alt: Box<str>,
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
    #[cfg(test)]
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

    #[cfg(test)]
    pub const fn set_strong(&mut self) {
        self.flags |= FLAG_STRONG;
    }

    #[must_use]
    pub const fn emphasis(&self) -> bool {
        self.flags & FLAG_EMPHASIS != 0
    }

    #[cfg(test)]
    pub const fn set_emphasis(&mut self) {
        self.flags |= FLAG_EMPHASIS;
    }

    #[must_use]
    pub const fn strikethrough(&self) -> bool {
        self.flags & FLAG_STRIKETHROUGH != 0
    }

    #[cfg(test)]
    #[allow(dead_code)] // API symmetry with set_strong / set_emphasis / set_code
    pub const fn set_strikethrough(&mut self) {
        self.flags |= FLAG_STRIKETHROUGH;
    }

    #[must_use]
    pub const fn code(&self) -> bool {
        self.flags & FLAG_CODE != 0
    }

    #[cfg(test)]
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
            let lang: Box<str> = match kind {
                pulldown_cmark::CodeBlockKind::Fenced(l) if !l.is_empty() => l.as_ref().into(),
                _ => Box::from(""),
            };
            parse_code_block(events, lang, blocks)
        }
        Event::Start(Tag::BlockQuote(_)) => parse_blockquote(events, blocks, fmt),
        Event::Start(Tag::List(start)) => parse_list(events, *start, blocks, fmt),
        Event::Start(Tag::Table(aligns)) => parse_table(events, aligns, blocks, fmt),
        Event::Start(Tag::Image { dest_url, .. }) => {
            let (alt, end) = collect_image_alt(events, 1);
            blocks.push(Block::Image {
                url: dest_url.as_ref().into(),
                alt: alt.into_boxed_str(),
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
    let dest_url: Box<str> = match &events[1] {
        Event::Start(Tag::Image { dest_url, .. }) => dest_url.as_ref().into(),
        _ => return None,
    };

    // Reuse shared alt-text collector starting after Start(Image).
    let (alt, end_idx) = collect_image_alt(events, 2);

    // The very next event must be End(Paragraph).
    if end_idx >= events.len() || !matches!(&events[end_idx], Event::End(TagEnd::Paragraph)) {
        return None;
    }

    blocks.push(Block::Image {
        url: dest_url,
        alt: alt.into_boxed_str(),
    });
    Some(end_idx + 1) // +1 to consume End(Paragraph)
}

fn parse_code_block(events: &[Event<'_>], language: Box<str>, blocks: &mut Vec<Block>) -> usize {
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

/// Maintains the inline formatting stack with counter-based cache rebuild.
///
/// Instead of rescanning the full stack on every pop (O(n)), we track
/// reference counts for each flag type.  This makes push, pop, and
/// style queries all O(1).
struct InlineState {
    stack: Vec<InlineFlag>,
    /// Per-flag reference counts — avoids O(n) rebuild on pop.
    strong_count: u8,
    emphasis_count: u8,
    strikethrough_count: u8,
    /// Number of Link entries on the stack.
    link_count: u8,
    /// Cached link URL (topmost link on stack, or None).
    cached_link: Option<Rc<str>>,
}

impl InlineState {
    fn new() -> Self {
        Self {
            stack: Vec::with_capacity(4),
            strong_count: 0,
            emphasis_count: 0,
            strikethrough_count: 0,
            link_count: 0,
            cached_link: None,
        }
    }

    fn clear(&mut self) {
        self.stack.clear();
        self.strong_count = 0;
        self.emphasis_count = 0;
        self.strikethrough_count = 0;
        self.link_count = 0;
        self.cached_link = None;
    }

    /// Compute the flags bitfield from counters — O(1).
    const fn flags(&self) -> u8 {
        let mut f = 0u8;
        if self.strong_count > 0 {
            f |= FLAG_STRONG;
        }
        if self.emphasis_count > 0 {
            f |= FLAG_EMPHASIS;
        }
        if self.strikethrough_count > 0 {
            f |= FLAG_STRIKETHROUGH;
        }
        f
    }

    /// Current `SpanStyle` — O(1), no stack traversal.
    fn style(&self) -> SpanStyle {
        SpanStyle {
            flags: self.flags(),
            link: self.cached_link.clone(),
        }
    }

    /// Current `SpanStyle` with the code flag set.
    fn style_with_code(&self) -> SpanStyle {
        SpanStyle {
            flags: self.flags() | FLAG_CODE,
            link: self.cached_link.clone(),
        }
    }

    fn push(&mut self, flag: InlineFlag) {
        match &flag {
            InlineFlag::Strong => self.strong_count += 1,
            InlineFlag::Emphasis => self.emphasis_count += 1,
            InlineFlag::Strikethrough => self.strikethrough_count += 1,
            InlineFlag::Link(url) => {
                self.link_count += 1;
                self.cached_link = Some(Rc::clone(url));
            }
        }
        self.stack.push(flag);
    }

    fn pop(&mut self, flag: &InlineFlag) {
        if let Some(pos) = self.stack.iter().rposition(|k| k == flag) {
            let removed = self.stack.swap_remove(pos);
            self.decrement(&removed);
        }
    }

    fn pop_link(&mut self) {
        if let Some(pos) = self
            .stack
            .iter()
            .rposition(|k| matches!(k, InlineFlag::Link(_)))
        {
            let removed = self.stack.swap_remove(pos);
            self.decrement(&removed);
        }
    }

    /// Decrement the counter for a removed flag and refresh the link cache
    /// only when necessary.
    fn decrement(&mut self, flag: &InlineFlag) {
        match flag {
            InlineFlag::Strong => self.strong_count = self.strong_count.saturating_sub(1),
            InlineFlag::Emphasis => self.emphasis_count = self.emphasis_count.saturating_sub(1),
            InlineFlag::Strikethrough => {
                self.strikethrough_count = self.strikethrough_count.saturating_sub(1);
            }
            InlineFlag::Link(_) => {
                self.link_count = self.link_count.saturating_sub(1);
                if self.link_count == 0 {
                    self.cached_link = None;
                } else {
                    // Find the new topmost link.
                    self.cached_link = self.stack.iter().rev().find_map(|f| match f {
                        InlineFlag::Link(url) => Some(Rc::clone(url)),
                        _ => None,
                    });
                }
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

#[must_use]
pub const fn heading_level_to_u8(level: HeadingLevel) -> u8 {
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

    fn validate_styled_text(st: &StyledText) {
        let text_len = st.text.len() as u32;
        if st.text.is_empty() {
            assert!(st.spans.is_empty(), "empty text should have no spans");
            return;
        }
        assert!(!st.spans.is_empty(), "non-empty text should have spans");
        for (i, span) in st.spans.iter().enumerate() {
            assert!(span.start < span.end, "span {i}: start >= end");
            assert!(span.end <= text_len, "span {i}: end exceeds text len");
        }
        assert_eq!(st.spans[0].start, 0, "first span should start at 0");
        assert_eq!(
            st.spans.last().expect("non-empty").end,
            text_len,
            "last span should end at text len"
        );
        for i in 1..st.spans.len() {
            assert_eq!(
                st.spans[i].start,
                st.spans[i - 1].end,
                "gap between span {} and {i}",
                i - 1
            );
        }
    }

    fn parse_paragraph(md: &str) -> StyledText {
        let blocks = parse_markdown(md);
        match blocks.into_iter().next() {
            Some(Block::Paragraph(st)) => st,
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    // ── Heading parsing ──────────────────────────────────────────

    #[test]
    fn heading_parsing() {
        for (label, md, expected) in [
            ("simple", "# Hello World", vec![(1_u8, "Hello World")]),
            (
                "levels_1_to_6",
                "# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6\n",
                vec![
                    (1, "H1"),
                    (2, "H2"),
                    (3, "H3"),
                    (4, "H4"),
                    (5, "H5"),
                    (6, "H6"),
                ],
            ),
            (
                "unicode",
                "# 你好世界\n## 🚀 Rocket\n",
                vec![(1, "你好世界"), (2, "🚀 Rocket")],
            ),
            (
                "consecutive",
                "# H1\n## H2\n### H3\n",
                vec![(1, "H1"), (2, "H2"), (3, "H3")],
            ),
            ("trailing_hashes", "## Title ##\n", vec![(2, "Title")]),
        ] {
            let blocks = parse_markdown(md);
            let headings: Vec<_> = blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Heading { level, text } => Some((*level, text.text.as_str())),
                    _ => None,
                })
                .collect();
            assert_eq!(headings.len(), expected.len(), "{label}: count");
            for (i, ((gl, gt), (el, et))) in headings.iter().zip(expected.iter()).enumerate() {
                assert_eq!(gl, el, "{label}[{i}]: level");
                assert!(
                    gt.trim().contains(et),
                    "{label}[{i}]: text {gt:?} missing {et:?}"
                );
            }
        }
    }

    #[test]
    fn heading_with_formatting() {
        for (label, md, checks) in [
            (
                "mixed",
                "# **bold** and *italic*\n",
                &["strong", "emphasis"] as &[&str],
            ),
            (
                "all_inline",
                "## **bold** *italic* `code` [link](url) ~~strike~~\n",
                &["strong", "emphasis", "code", "link", "strikethrough"],
            ),
            (
                "link_and_code",
                "### [`parse`](https://docs.rs) function\n",
                &["code", "link"],
            ),
        ] {
            let blocks = parse_markdown(md);
            match &blocks[0] {
                Block::Heading { text, .. } => {
                    for check in checks {
                        match *check {
                            "strong" => assert!(
                                text.spans.iter().any(|s| s.style.strong()),
                                "{label}: strong"
                            ),
                            "emphasis" => assert!(
                                text.spans.iter().any(|s| s.style.emphasis()),
                                "{label}: emphasis"
                            ),
                            "code" => {
                                assert!(text.spans.iter().any(|s| s.style.code()), "{label}: code");
                            }
                            "link" => assert!(
                                text.spans.iter().any(|s| s.style.link.is_some()),
                                "{label}: link"
                            ),
                            "strikethrough" => assert!(
                                text.spans.iter().any(|s| s.style.strikethrough()),
                                "{label}: strike"
                            ),
                            _ => {}
                        }
                    }
                    validate_styled_text(text);
                }
                other => panic!("{label}: expected heading, got {other:?}"),
            }
        }
    }

    // ── Inline formatting ────────────────────────────────────────

    #[test]
    fn inline_formatting_parsing() {
        // (label, md, text_sub, strong, emphasis, strikethrough, strong_and_emph)
        for (label, md, text_sub, strong, emph, strike, combined) in [
            (
                "emphasis_and_bold",
                "Hello **world** and *italic*",
                "world",
                true,
                true,
                false,
                false,
            ),
            (
                "strikethrough",
                "This is ~~deleted~~ text",
                "deleted",
                false,
                false,
                true,
                false,
            ),
            (
                "triple_emphasis",
                "***bold and italic***",
                "bold and italic",
                true,
                true,
                false,
                true,
            ),
            (
                "strike_with_code",
                "~~deleted `code` deleted~~",
                "code",
                false,
                false,
                true,
                false,
            ),
            (
                "gfm_strike",
                "~~deleted~~\n",
                "deleted",
                false,
                false,
                true,
                false,
            ),
            (
                "triple_bold_italic",
                "***bold and italic***\n",
                "bold and italic",
                true,
                true,
                false,
                true,
            ),
        ] {
            let blocks = parse_markdown(md);
            match &blocks[0] {
                Block::Paragraph(st) => {
                    assert!(st.text.contains(text_sub), "{label}: text");
                    if strong {
                        assert!(st.spans.iter().any(|s| s.style.strong()), "{label}: strong");
                    }
                    if emph {
                        assert!(st.spans.iter().any(|s| s.style.emphasis()), "{label}: emph");
                    }
                    if strike {
                        assert!(
                            st.spans.iter().any(|s| s.style.strikethrough()),
                            "{label}: strike"
                        );
                    }
                    if combined {
                        assert!(
                            st.spans
                                .iter()
                                .any(|s| s.style.strong() && s.style.emphasis()),
                            "{label}: combined"
                        );
                    }
                }
                other => panic!("{label}: expected paragraph, got {other:?}"),
            }
        }
    }

    // ── Code block parsing ───────────────────────────────────────

    #[test]
    fn code_block_parsing() {
        for (label, md, lang, code_sub) in [
            (
                "fenced_rust",
                "```rust\nfn main() {}\n```",
                "rust",
                "fn main()",
            ),
            ("indented", "    fn foo() {}\n    bar()\n", "", "fn foo()"),
            ("empty_fenced", "```\n```\n", "", ""),
            ("unclosed", "```rust\ncode\n", "rust", "code"),
            (
                "indented_two",
                "    code line 1\n    code line 2\n",
                "",
                "code line 1",
            ),
        ] {
            let blocks = parse_markdown(md);
            assert_eq!(blocks.len(), 1, "{label}");
            match &blocks[0] {
                Block::Code { language, code } => {
                    assert_eq!(&**language, lang, "{label}: lang");
                    if code_sub.is_empty() {
                        assert!(code.is_empty(), "{label}: empty");
                    } else {
                        assert!(code.contains(code_sub), "{label}: code");
                    }
                }
                other => panic!("{label}: expected Code, got {other:?}"),
            }
        }
    }

    // ── List parsing ─────────────────────────────────────────────

    #[test]
    fn list_parsing_unordered() {
        for (label, md, count, first) in [
            ("basic", "- one\n- two\n- three", 3, "one"),
            ("empty_items", "- \n- text\n", 2, ""),
        ] {
            let blocks = parse_markdown(md);
            match &blocks[0] {
                Block::UnorderedList(items) => {
                    assert_eq!(items.len(), count, "{label}: count");
                    assert_eq!(items[0].content.text, first, "{label}: first");
                }
                other => panic!("{label}: expected UL, got {other:?}"),
            }
        }
    }

    #[test]
    fn list_parsing_ordered() {
        for (label, md, start, count, first) in [
            ("basic", "1. first\n2. second", 1_u64, 2, "first"),
            ("start_zero", "0. zero\n1. one\n", 0, 2, "zero"),
            ("high_start", "42. answer\n43. next\n", 42, 2, "answer"),
        ] {
            let blocks = parse_markdown(md);
            match &blocks[0] {
                Block::OrderedList { start: s, items } => {
                    assert_eq!(*s, start, "{label}: start");
                    assert_eq!(items.len(), count, "{label}: count");
                    assert_eq!(items[0].content.text, first, "{label}: first");
                }
                other => panic!("{label}: expected OL, got {other:?}"),
            }
        }
    }

    #[test]
    fn list_nesting() {
        for (label, md) in [
            ("nested_ul", "- parent\n  - child\n  - child2\n- sibling"),
            (
                "mixed",
                "- bullet\n  1. ordered a\n  2. ordered b\n- bullet2\n",
            ),
        ] {
            let blocks = parse_markdown(md);
            match &blocks[0] {
                Block::UnorderedList(items) => {
                    assert_eq!(items.len(), 2, "{label}");
                    assert!(!items[0].children.is_empty(), "{label}: children");
                }
                other => panic!("{label}: expected UL, got {other:?}"),
            }
        }
    }

    // ── Image parsing ────────────────────────────────────────────

    #[test]
    fn image_parsing() {
        for (label, md, url, alt) in [
            (
                "full",
                "![alt text](https://img.png \"title\")",
                "https://img.png",
                "alt text",
            ),
            ("no_alt", "![](image.png)", "image.png", ""),
            (
                "from_brackets",
                "![alt text](img.png)",
                "img.png",
                "alt text",
            ),
            ("empty_url", "![alt text]()\n", "", "alt text"),
        ] {
            let blocks = parse_markdown(md);
            match &blocks[0] {
                Block::Image { url: u, alt: a } => {
                    assert_eq!(&**u, url, "{label}: url");
                    assert_eq!(&**a, alt, "{label}: alt");
                }
                other => panic!("{label}: expected Image, got {other:?}"),
            }
        }
        // Inline with text stays as paragraph
        assert!(matches!(
            &parse_markdown("See ![pic](img.png) text.")[0],
            Block::Paragraph(_)
        ));
        // Multiple standalone images
        let imgs = parse_markdown("![a](1.png)\n\n![b](2.png)\n\n![c](3.png)\n")
            .iter()
            .filter(|b| matches!(b, Block::Image { .. }))
            .count();
        assert_eq!(imgs, 3);
    }

    // ── Link parsing ─────────────────────────────────────────────

    #[test]
    fn link_parsing() {
        for (label, md, url) in [
            (
                "basic",
                "[link](https://example.com)",
                "https://example.com",
            ),
            ("with_title", "[text](url \"title\")\n", "url"),
            (
                "reference",
                "[text][ref]\n\n[ref]: https://example.com\n",
                "https://example.com",
            ),
            (
                "autolink",
                "Visit <https://example.com> for more.",
                "https://example.com",
            ),
        ] {
            let blocks = parse_markdown(md);
            match &blocks[0] {
                Block::Paragraph(st) => {
                    let has = st
                        .spans
                        .iter()
                        .any(|s| s.style.link.as_deref() == Some(url));
                    assert!(has, "{label}: no span with URL {url:?}");
                }
                other => panic!("{label}: expected paragraph, got {other:?}"),
            }
        }
        // Multiple links
        let blocks = parse_markdown("Visit [a](https://a.com) and [b](https://b.com) today.");
        match &blocks[0] {
            Block::Paragraph(st) => {
                let n = st.spans.iter().filter(|s| s.style.link.is_some()).count();
                assert!(n >= 2, "expected >=2 links, got {n}");
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    // ── Table parsing ────────────────────────────────────────────

    #[test]
    fn table_parsing() {
        for (label, md, hdr, rows, aligns) in [
            (
                "basic",
                "| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |",
                2,
                2,
                vec![Alignment::None, Alignment::None],
            ),
            (
                "alignment",
                "| L | C | R |\n|:---|:---:|---:|\n| a | b | c |\n",
                3,
                1,
                vec![Alignment::Left, Alignment::Center, Alignment::Right],
            ),
            (
                "header_only",
                "| A | B |\n|---|---|\n",
                2,
                0,
                vec![Alignment::None, Alignment::None],
            ),
            (
                "col_mismatch",
                "| A | B | C |\n|---|---|---|\n| 1 | 2 |\n",
                3,
                1,
                vec![Alignment::None, Alignment::None, Alignment::None],
            ),
            (
                "empty_data",
                "| X | Y |\n|---|---|\n",
                2,
                0,
                vec![Alignment::None, Alignment::None],
            ),
        ] {
            let blocks = parse_markdown(md);
            match &blocks[0] {
                Block::Table(t) => {
                    assert_eq!(t.header.len(), hdr, "{label}: hdr");
                    assert_eq!(t.rows.len(), rows, "{label}: rows");
                    assert_eq!(t.alignments.len(), aligns.len(), "{label}: aligns len");
                    for (i, (g, e)) in t.alignments.iter().zip(aligns.iter()).enumerate() {
                        assert_eq!(g, e, "{label}: align[{i}]");
                    }
                }
                other => panic!("{label}: expected table, got {other:?}"),
            }
        }
        // Escaped pipe
        let blocks = parse_markdown("| A |\n|---|\n| a \\| b |\n");
        match &blocks[0] {
            Block::Table(t) => {
                assert!(
                    t.rows[0][0].text.contains("a | b") || t.rows[0][0].text.contains("a \\| b")
                );
            }
            other => panic!("expected Table, got {other:?}"),
        }
    }

    // ── Blockquote parsing ───────────────────────────────────────

    #[test]
    fn blockquote_parsing() {
        // Simple
        assert!(matches!(&parse_markdown("> quoted")[0], Block::Quote(_)));
        // Nested: 2 levels
        match &parse_markdown("> outer\n>> inner\n")[0] {
            Block::Quote(outer) => assert!(outer.iter().any(|b| matches!(b, Block::Quote(_)))),
            other => panic!("expected Quote, got {other:?}"),
        }
        // 3 levels
        match &parse_markdown("> > > deep\n")[0] {
            Block::Quote(l1) => {
                for b in l1 {
                    if let Block::Quote(l2) = b {
                        assert!(l2.iter().any(|b2| matches!(b2, Block::Quote(_))));
                    }
                }
            }
            other => panic!("expected Quote, got {other:?}"),
        }
        // Inner blocks
        for (label, md) in [
            ("code", "> ```rust\n> fn main() {}\n> ```\n"),
            ("table", "> | H1 | H2 |\n> |---|---|\n> | a | b |\n"),
            (
                "code_and_list",
                "> ```python\n> print('hi')\n> ```\n>\n> - item 1\n> - item 2\n",
            ),
        ] {
            match &parse_markdown(md)[0] {
                Block::Quote(inner) => assert!(!inner.is_empty(), "{label}"),
                other => panic!("{label}: expected Quote, got {other:?}"),
            }
        }
    }

    // ── Span coverage ────────────────────────────────────────────

    #[test]
    fn spans_cover_all_block_types() {
        for md in [
            "Hello world",
            "Hello **bold** world",
            "**bold** *italic* ~~strike~~ `code`",
            "A [link](https://x.com) here",
            "**bold *bold-italic* bold**",
            "Mixed **bold** and *italic* with `code` and [link](url)",
        ] {
            for block in &parse_markdown(md) {
                if let Block::Paragraph(st) = block {
                    validate_styled_text(st);
                }
            }
        }
        for block in &parse_markdown("# Simple\n## **Bold** heading\n### `Code` in heading") {
            if let Block::Heading { text, .. } = block {
                validate_styled_text(text);
            }
        }
        for block in &parse_markdown("- Item with **bold**\n- Item with `code`\n- [Link](url) item")
        {
            if let Block::UnorderedList(items) = block {
                for item in items {
                    validate_styled_text(&item.content);
                }
            }
        }
        let table_md = "| **Bold** | `Code` | [Link](url) |\n|---|---|---|\n| a | b | c |";
        for block in &parse_markdown(table_md) {
            if let Block::Table(t) = block {
                for cell in &t.header {
                    validate_styled_text(cell);
                }
                for row in &t.rows {
                    for cell in row {
                        validate_styled_text(cell);
                    }
                }
            }
        }
        // Large table cells with formatting
        let md2 =
            "| **Bold** `code` | *it* ~~s~~ [lnk](u) |\n|---|---|\n| **x** *y* | `a` ~~b~~ |\n";
        for block in &parse_markdown(md2) {
            if let Block::Table(t) = block {
                for cell in &t.header {
                    validate_styled_text(cell);
                }
                for row in &t.rows {
                    for cell in row {
                        validate_styled_text(cell);
                    }
                }
            }
        }
    }

    // ── Edge cases ───────────────────────────────────────────────

    #[test]
    fn parse_edge_cases() {
        for (label, md, empty) in [
            ("empty", "", true),
            ("whitespace", "   \n\n   \n", true),
            ("newlines", "\n\n\n\n\n\n\n\n", true),
        ] {
            assert_eq!(parse_markdown(md).is_empty(), empty, "{label}");
        }
        // CRLF
        let b = parse_markdown("# Hello\r\n\r\nParagraph\r\n");
        assert!(matches!(&b[0], Block::Heading { level: 1, .. }));
        assert!(matches!(&b[1], Block::Paragraph(_)));
        // Multiple blank lines
        let p = parse_markdown("para1\n\n\n\n\npara2")
            .iter()
            .filter(|b| matches!(b, Block::Paragraph(_)))
            .count();
        assert_eq!(p, 2);
    }

    #[test]
    fn parse_inline_code() {
        for (label, md) in [("single", "Use `code` here"), ("double", "`` `inner` ``\n")] {
            match &parse_markdown(md)[0] {
                Block::Paragraph(st) => assert!(st.spans.iter().any(|s| s.style.code()), "{label}"),
                other => panic!("{label}: expected paragraph, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_large_document_perf() {
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
        for _ in 0..100_u32 {
            assert!(!parse_markdown(&doc).is_empty());
        }
        let per_iter = start.elapsed() / 100;
        if cfg!(not(debug_assertions)) {
            assert!(per_iter.as_millis() < 5, "too slow: {per_iter:?}");
        }
    }

    #[test]
    fn parse_task_lists() {
        // Unordered
        match &parse_markdown("- [x] checked\n- [ ] unchecked\n- normal\n")[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items[0].checked, Some(true));
                assert_eq!(items[1].checked, Some(false));
                assert_eq!(items[2].checked, None);
            }
            other => panic!("expected UL, got {other:?}"),
        }
        // Ordered
        match &parse_markdown("1. [x] Done\n2. [ ] Todo\n3. Normal\n")[0] {
            Block::OrderedList { items, .. } => {
                assert_eq!(items[0].checked, Some(true));
                assert_eq!(items[1].checked, Some(false));
                assert_eq!(items[2].checked, None);
            }
            other => panic!("expected OL, got {other:?}"),
        }
    }

    #[test]
    fn parse_misc_block_types() {
        // Thematic break
        assert!(matches!(&parse_markdown("---")[0], Block::ThematicBreak));
        // Setext headings
        let h: Vec<_> = parse_markdown("H1\n===\n\nH2\n---\n")
            .iter()
            .filter_map(|b| {
                if let Block::Heading { level, .. } = b {
                    Some(*level)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(h, vec![1, 2]);
        // Escaped characters
        for md in [
            "\\# Not a heading\n\n\\* Not a bullet\n",
            "\\*not bold\\* and \\[not link\\]\n",
        ] {
            assert!(
                parse_markdown(md)
                    .iter()
                    .all(|b| matches!(b, Block::Paragraph(_)))
            );
        }
        // Line breaks
        for md in ["Line one  \nLine two\n", "Line one\nLine two\n"] {
            match &parse_markdown(md)[0] {
                Block::Paragraph(t) => {
                    assert!(t.text.contains("Line one") && t.text.contains("Line two"));
                }
                other => panic!("expected Paragraph, got {other:?}"),
            }
        }
        // HTML entities
        match &parse_markdown("&amp; &lt; &gt; &#123;\n")[0] {
            Block::Paragraph(t) => {
                assert!(t.text.contains('&') && t.text.contains('<') && t.text.contains('>'));
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
        // Inline HTML
        match &parse_markdown("Text with <strong>html</strong> inline.\n")[0] {
            Block::Paragraph(t) => assert!(t.text.contains("html")),
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn inline_state_pop_without_push() {
        let mut state = InlineState::new();
        state.pop(&InlineFlag::Strong);
        assert!(state.stack.is_empty());
        assert_eq!(state.flags(), 0);
        assert!(state.cached_link.is_none());
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
    fn parse_list_with_child_blocks() {
        for (md, label) in [
            (
                "- Item:\n\n  ```rust\n  fn main() {}\n  ```\n\n- Next\n",
                "Code",
            ),
            ("- Item:\n\n  > Quoted text\n\n- Next\n", "Quote"),
            ("- First para\n\n  Second para\n\n- Another\n", "Paragraph"),
            ("- Item\n\n  ## Sub-heading\n\n- Next\n", "Heading"),
            ("- Item\n\n  ---\n\n- Next\n", "ThematicBreak"),
            (
                "- Item\n\n  | A | B |\n  |---|---|\n  | 1 | 2 |\n\n- Next\n",
                "Table",
            ),
            (
                "1. First item\n\n   ```rust\n   let x = 1;\n   ```\n\n2. Second item\n",
                "OL+Code",
            ),
        ] {
            let blocks = parse_markdown(md);
            let has_children = match &blocks[0] {
                Block::UnorderedList(items) | Block::OrderedList { items, .. } => {
                    !items[0].children.is_empty()
                }
                _ => false,
            };
            assert!(has_children, "{label}: should have children");
        }
    }

    // ── Inline merge and nesting ─────────────────────────────────

    #[test]
    fn inline_merge_behavior() {
        // Adjacent bold merges
        let mut st = StyledText::default();
        let mut bold = SpanStyle::plain();
        bold.set_strong();
        st.push_text("bold1", bold.clone());
        st.push_text("bold2", bold);
        assert_eq!(st.spans.len(), 1);
        assert!(st.spans[0].style.strong());
        validate_styled_text(&st);

        // Different styles don't merge
        let st = parse_paragraph("*italic*normal*italic*");
        assert!(st.spans.len() >= 3);
        assert!(st.spans[0].style.emphasis());
        assert!(!st.spans[1].style.emphasis());
        assert!(st.spans[2].style.emphasis());
        validate_styled_text(&st);

        // Plain fragments merge
        let mut st = StyledText::default();
        st.push_text("aaa", SpanStyle::plain());
        st.push_text("bbb", SpanStyle::plain());
        st.push_text("ccc", SpanStyle::plain());
        assert_eq!(st.spans.len(), 1);
        assert_eq!(st.text, "aaabbbccc");
    }

    #[test]
    fn inline_deep_nesting() {
        for (label, md, text, strong, emph, strike, link) in [
            (
                "bold_italic",
                "***bold-italic***",
                "bold-italic",
                true,
                true,
                false,
                None,
            ),
            (
                "bold_italic_strike",
                "***~~bold-italic-strike~~***",
                "bold-italic-strike",
                true,
                true,
                true,
                None,
            ),
            (
                "bold_italic_link",
                "[***bold-italic link***](url)",
                "bold-italic link",
                true,
                true,
                false,
                Some("url"),
            ),
            (
                "all_in_link",
                "[***~~all~~***](url)",
                "all",
                true,
                true,
                true,
                Some("url"),
            ),
        ] {
            let st = parse_paragraph(md);
            assert_eq!(st.text, text, "{label}");
            assert_eq!(st.spans.len(), 1, "{label}: span count");
            let s = &st.spans[0];
            assert_eq!(s.style.strong(), strong, "{label}: strong");
            assert_eq!(s.style.emphasis(), emph, "{label}: emph");
            assert_eq!(s.style.strikethrough(), strike, "{label}: strike");
            assert_eq!(s.style.link.as_deref(), link, "{label}: link");
            validate_styled_text(&st);
        }
    }

    #[test]
    fn inline_code_in_context() {
        // Code inside bold inherits strong
        let st = parse_paragraph("**bold `code` bold**");
        validate_styled_text(&st);
        let code: Vec<_> = st.spans.iter().filter(|s| s.style.code()).collect();
        assert_eq!(code.len(), 1);
        assert!(code[0].style.strong());
        assert_eq!(
            &st.text[code[0].start as usize..code[0].end as usize],
            "code"
        );

        // Backtick sequences
        for md in ["`a`b`c`", "`` `inner` ``"] {
            let st = parse_paragraph(md);
            validate_styled_text(&st);
            assert!(st.spans.iter().any(|s| s.style.code()));
        }
    }

    #[test]
    fn inline_link_formatting() {
        // Formatted text in link
        let st = parse_paragraph("[**bold** and *italic*](url)");
        validate_styled_text(&st);
        for span in &st.spans {
            assert_eq!(span.style.link.as_deref(), Some("url"));
        }
        assert!(st.spans.iter().any(|s| s.style.strong()));
        assert!(st.spans.iter().any(|s| s.style.emphasis()));

        // Multiple links
        let st = parse_paragraph("[aaa](url1) [bbb](url2)");
        validate_styled_text(&st);
        let urls: Vec<_> = st
            .spans
            .iter()
            .filter_map(|s| s.style.link.as_deref())
            .collect();
        assert!(urls.contains(&"url1") && urls.contains(&"url2"));

        // Code in link
        let st = parse_paragraph("[`code` in link](url)");
        validate_styled_text(&st);
        assert!(
            st.spans
                .iter()
                .any(|s| s.style.code() && s.style.link.is_some())
        );

        // Adjacent different links don't merge
        let st = parse_paragraph("[a](u1)[b](u2)");
        validate_styled_text(&st);
        assert!(st.spans.iter().filter(|s| s.style.link.is_some()).count() >= 2);

        // Emphasis across softbreak
        let st = parse_paragraph("*italic\nacross lines*");
        validate_styled_text(&st);
        assert!(st.spans.iter().any(|s| s.style.emphasis()));
    }

    #[test]
    fn inline_deeply_interleaved_formatting() {
        let st = parse_paragraph("**bold *italic ~~strike `code` strike~~ italic* bold**");
        validate_styled_text(&st);
        assert!(st.spans.iter().any(|s| s.style.strong()));
        assert!(st.spans.iter().any(|s| s.style.emphasis()));
        assert!(st.spans.iter().any(|s| s.style.strikethrough()));
        assert!(st.spans.iter().any(|s| s.style.code()));
    }

    #[test]
    fn inline_validate_various_inputs() {
        for input in [
            "Hello world",
            "**bold** and *italic* and `code` end",
            "**你好** *世界* `🚀`",
            "plain **bold** *italic* ~~strike~~ `code` [link](url) ***bi*** end",
            "***~~all~~***",
        ] {
            validate_styled_text(&parse_paragraph(input));
        }
    }

    #[test]
    fn inline_empty_and_unclosed() {
        for md in ["****", "__", "[](url)"] {
            for block in &parse_markdown(md) {
                if let Block::Paragraph(st) = block {
                    validate_styled_text(st);
                }
            }
        }
        for md in ["**unclosed", "*unclosed", "`unclosed", "~~unclosed"] {
            let blocks = parse_markdown(md);
            assert!(!blocks.is_empty(), "{md:?}");
            if let Some(Block::Paragraph(st)) = blocks.first() {
                validate_styled_text(st);
            }
        }
    }

    #[test]
    fn inline_long_sequences() {
        // 100 alternating bold/normal
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
        assert_eq!(st.spans.iter().filter(|s| s.style.strong()).count(), 50);

        // 50 links
        md.clear();
        for i in 0..50 {
            write!(md, "[link{i}](https://example.com/{i}) ").ok();
        }
        let st = parse_paragraph(&md);
        validate_styled_text(&st);
        assert!(st.spans.iter().filter(|s| s.style.link.is_some()).count() >= 50);

        // 100 code spans
        md.clear();
        for i in 0..100 {
            write!(md, "`code{i}` ").ok();
        }
        let st = parse_paragraph(&md);
        validate_styled_text(&st);
        assert_eq!(st.spans.iter().filter(|s| s.style.code()).count(), 100);
    }

    // ── Stress tests ─────────────────────────────────────────────

    #[test]
    fn stress_table_structural() {
        // Extra cols
        match &parse_markdown("| A | B |\n|---|---|\n| 1 | 2 | 3 | 4 |\n")[0] {
            Block::Table(t) => {
                assert_eq!(t.header.len(), 2);
                assert_eq!(t.rows.len(), 1);
            }
            other => panic!("expected table, got {other:?}"),
        }
        // Fewer cols
        match &parse_markdown("| A | B | C | D |\n|---|---|---|---|\n| 1 |\n| x | y |\n")[0] {
            Block::Table(t) => {
                assert_eq!(t.header.len(), 4);
                assert_eq!(t.rows.len(), 2);
            }
            other => panic!("expected table, got {other:?}"),
        }
        // Empty cells
        match &parse_markdown("| A | B | C |\n|---|---|---|\n|  |  |  |\n| x |  | z |\n")[0] {
            Block::Table(t) => {
                assert!(t.rows[0].iter().all(|c| c.text.is_empty()));
                assert_eq!(t.rows[1][0].text, "x");
                assert_eq!(t.rows[1][2].text, "z");
            }
            other => panic!("expected table, got {other:?}"),
        }
        // Headers only
        match &parse_markdown("| H1 | H2 | H3 |\n|---|---|---|\n")[0] {
            Block::Table(t) => {
                assert_eq!(
                    t.header.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
                    vec!["H1", "H2", "H3"]
                );
                assert!(t.rows.is_empty());
            }
            other => panic!("expected table, got {other:?}"),
        }
        // Adjacent tables
        assert!(
            parse_markdown("| A |\n|---|\n| 1 |\n| B |\n|---|\n| 2 |\n")
                .iter()
                .any(|b| matches!(b, Block::Table(_)))
        );
        assert_eq!(
            parse_markdown("| A |\n|---|\n| 1 |\n\n| B |\n|---|\n| 2 |\n")
                .iter()
                .filter(|b| matches!(b, Block::Table(_)))
                .count(),
            2
        );
    }

    #[test]
    fn stress_table_alignment_with_long_headers() {
        let (la, lb) = ("A".repeat(200), "B".repeat(300));
        let md = format!("| {la} | {lb} | Short |\n|:---|:---:|---:|\n| x | y | z |\n");
        match &parse_markdown(&md)[0] {
            Block::Table(t) => {
                assert_eq!(t.header[0].text, la);
                assert_eq!(t.header[1].text, lb);
                assert_eq!(
                    t.alignments,
                    vec![Alignment::Left, Alignment::Center, Alignment::Right]
                );
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn stress_table_cell_with_all_formatting() {
        let md = "| Cell |\n|---|\n| **bold** *italic* `code` [link](url) ~~strike~~ |\n";
        match &parse_markdown(md)[0] {
            Block::Table(t) => {
                let c = &t.rows[0][0];
                assert!(c.spans.iter().any(|s| s.style.strong()));
                assert!(c.spans.iter().any(|s| s.style.emphasis()));
                assert!(c.spans.iter().any(|s| s.style.code()));
                assert!(c.spans.iter().any(|s| s.style.link.is_some()));
                assert!(c.spans.iter().any(|s| s.style.strikethrough()));
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn stress_large_table_100_rows_20_cols() {
        let mut md = String::with_capacity(100_000);
        md.push('|');
        for c in 0..20 {
            write!(md, " H{c} |").ok();
        }
        md.push('\n');
        md.push('|');
        for _ in 0..20 {
            md.push_str("---|");
        }
        md.push('\n');
        for r in 0..100 {
            md.push('|');
            for c in 0..20 {
                write!(md, " r{r}c{c} |").ok();
            }
            md.push('\n');
        }
        match &parse_markdown(&md)[0] {
            Block::Table(t) => {
                assert_eq!(t.header.len(), 20);
                assert_eq!(t.rows.len(), 100);
                assert_eq!(t.rows[0][0].text, "r0c0");
                assert_eq!(t.rows[99][19].text, "r99c19");
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
        fn count_depth(block: &Block) -> usize {
            match block {
                Block::UnorderedList(items) => {
                    items[0].children.first().map_or(1, |c| 1 + count_depth(c))
                }
                _ => 0,
            }
        }
        assert!(count_depth(&blocks[0]) >= 10);
    }

    #[test]
    fn stress_mixed_ordered_unordered_nesting() {
        let md = "- bullet A\n  1. ordered 1\n     - nested bullet\n       1. deep ordered\n  2. ordered 2\n- bullet B\n";
        let blocks = parse_markdown(md);
        match &blocks[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items.len(), 2);
                assert!(
                    items[0]
                        .children
                        .iter()
                        .any(|b| matches!(b, Block::OrderedList { .. }))
                );
                for child in &items[0].children {
                    if let Block::OrderedList { items: ol, .. } = child
                        && let Some(Block::UnorderedList(ul)) = ol[0]
                            .children
                            .iter()
                            .find(|b| matches!(b, Block::UnorderedList(_)))
                    {
                        assert!(
                            ul[0]
                                .children
                                .iter()
                                .any(|b| matches!(b, Block::OrderedList { .. }))
                        );
                    }
                }
            }
            other => panic!("expected UL, got {other:?}"),
        }
    }

    #[test]
    fn stress_code_block_variations() {
        let b = parse_markdown("````\n```rust\nfn main() {}\n```\n````\n");
        match &b[0] {
            Block::Code { code, .. } => {
                assert!(code.contains("```rust"));
                assert!(code.contains("fn main()"));
            }
            other => panic!("expected Code, got {other:?}"),
        }
        let b = parse_markdown("`````\n```\n````\nsome code\n`````\n");
        match &b[0] {
            Block::Code { code, .. } => {
                assert!(code.contains("```"));
                assert!(code.contains("````"));
            }
            other => panic!("expected Code, got {other:?}"),
        }
        // Inline backticks preserved
        match &parse_markdown("Use `` `backtick` `` in code")[0] {
            Block::Paragraph(st) => {
                assert!(st.text.contains('`'));
                assert!(st.spans.iter().any(|s| s.style.code()));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn stress_link_url_edge_cases() {
        for (md, frag) in [
            (
                "[spaces](https://example.com/path%20with%20spaces)",
                "spaces",
            ),
            ("[unicode](https://example.com/日本語)", "日本語"),
            (
                "[parens](https://en.wikipedia.org/wiki/Rust_(programming_language))",
                "Rust_",
            ),
        ] {
            match &parse_markdown(md)[0] {
                Block::Paragraph(st) => {
                    let url = st
                        .spans
                        .iter()
                        .find(|s| s.style.link.is_some())
                        .unwrap_or_else(|| panic!("link span for {md:?}"))
                        .style
                        .link
                        .as_ref()
                        .expect("url");
                    assert!(
                        url.contains(frag),
                        "URL should contain {frag:?}, got {url:?}"
                    );
                }
                other => panic!("expected paragraph, got {other:?}"),
            }
        }
    }

    #[test]
    fn stress_image_long_alt() {
        let long_alt = "A".repeat(500);
        let md = format!("![**bold** *italic* {long_alt}](img.png)");
        match &parse_markdown(&md)[0] {
            Block::Image { alt, url } => {
                assert_eq!(&**url, "img.png");
                assert!(alt.contains(&long_alt) && alt.contains("bold") && alt.contains("italic"));
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn stress_blockquote_deep_and_content() {
        let md = "> level 1\n>> level 2\n>>> level 3\n>>>> level 4\n>>>>> level 5\n";
        fn max_depth(blocks: &[Block]) -> usize {
            blocks
                .iter()
                .map(|b| {
                    if let Block::Quote(inner) = b {
                        1 + max_depth(inner)
                    } else {
                        0
                    }
                })
                .max()
                .unwrap_or(0)
        }
        assert!(max_depth(&parse_markdown(md)) >= 5);
    }

    #[test]
    fn stress_task_lists_nested() {
        let md = "- [x] parent done\n  - [ ] child todo\n  - [x] child done\n- [ ] parent todo\n  - [ ] nested todo\n";
        match &parse_markdown(md)[0] {
            Block::UnorderedList(items) => {
                assert_eq!(items[0].checked, Some(true));
                assert_eq!(items[1].checked, Some(false));
                if let Some(Block::UnorderedList(n)) = items[0].children.first() {
                    assert_eq!(n[0].checked, Some(false));
                    assert_eq!(n[1].checked, Some(true));
                } else {
                    panic!("nested list");
                }
                if let Some(Block::UnorderedList(n)) = items[1].children.first() {
                    assert_eq!(n[0].checked, Some(false));
                } else {
                    panic!("nested list");
                }
            }
            other => panic!("expected UL, got {other:?}"),
        }
    }

    #[test]
    fn stress_smart_punctuation() {
        match &parse_markdown("\"Hello\" -- world... 'single' --- em")[0] {
            Block::Paragraph(st) => {
                let t = &st.text;
                assert!(t.contains('\u{201c}') || t.contains('\u{201d}') || t.contains('"'));
                assert!(t.contains('\u{2026}') || t.contains("..."));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn stress_parse_no_panic() {
        for (label, md) in [
            (
                "footnote",
                "Text with a footnote[^1].\n\n[^1]: The footnote content.\n".to_string(),
            ),
            ("huge_para", "word ".repeat(20_000)),
            (
                "thematic_breaks",
                "---\n\n***\n\n___\n\n---\n\n***\n".to_string(),
            ),
            ("long_heading", format!("# {}\n", "X".repeat(1200))),
        ] {
            assert!(!parse_markdown(&md).is_empty(), "{label}");
        }
        assert_eq!(
            parse_markdown("---\n\n***\n\n___\n\n---\n\n***\n")
                .iter()
                .filter(|b| matches!(b, Block::ThematicBreak))
                .count(),
            5
        );
    }

    #[test]
    fn stress_mixed_block_types() {
        let md = "# Heading\nParagraph.\n\n---\n\n- list\n\n> quote\n\n```\ncode\n```\n\n| T |\n|---|\n| v |\n\n![img](x.png)\n";
        let b = parse_markdown(md);
        assert!(b.iter().any(|b| matches!(b, Block::Heading { .. })));
        assert!(b.iter().any(|b| matches!(b, Block::Paragraph(_))));
        assert!(b.iter().any(|b| matches!(b, Block::ThematicBreak)));
        assert!(b.iter().any(|b| matches!(b, Block::UnorderedList(_))));
        assert!(b.iter().any(|b| matches!(b, Block::Quote(_))));
        assert!(b.iter().any(|b| matches!(b, Block::Code { .. })));
        assert!(b.iter().any(|b| matches!(b, Block::Table(_))));
        assert!(b.iter().any(|b| matches!(b, Block::Image { .. })));
    }
}
