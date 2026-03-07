# 🔬 Rustdown Verification Document

> **Purpose**: This bundled document exercises every supported Markdown feature
> at scale. Use it to verify rendering, navigation, and performance.
> Open with **Ctrl+Shift+F12**.

---

# 1 — Heading Hierarchy

All six ATX heading levels, demonstrating colour differentiation and nav-panel nesting.

## 1.1 — Second-Level Heading

### 1.1.1 — Third-Level Heading

#### 1.1.1.1 — Fourth-Level Heading

##### 1.1.1.1.1 — Fifth-Level Heading

###### 1.1.1.1.1.1 — Sixth-Level Heading

Paragraph after the deepest heading. Regular text should have no heading colour.

## 1.2 — Multiple Headings at Each Level

### Alpha Section

Content under Alpha.

### Beta Section

Content under Beta.

### Gamma Section

Content under Gamma.

#### Gamma Sub-A

#### Gamma Sub-B

##### Gamma Sub-B Deep

###### Gamma Sub-B Deepest

Back to normal paragraph text.

---

# 2 — Inline Styles

## 2.1 — Emphasis and Strong

This sentence has *italic text* in the middle.

This sentence has **bold text** in the middle.

This sentence has ***bold italic*** combined.

This sentence has ~~strikethrough~~ text.

This sentence combines **bold**, *italic*, ~~strikethrough~~, and `inline code` together.

Here is a longer paragraph that mixes **multiple bold phrases** with *italic words* and
even some ~~struck-through content~~ to ensure the inline style engine handles transitions
between styled and unstyled runs correctly across line wraps.

## 2.2 — Inline Code

Use `cargo build` to compile the project.

Backtick spans: ``code with `backtick` inside``.

A longer inline code span: `fn main() -> Result<(), Box<dyn std::error::Error>>`.

## 2.3 — Links

Visit [the Rust website](https://www.rust-lang.org/) for more information.

A bare autolink: https://github.com/teh-hippo/rustdown

Multiple links in one line: [Alpha](https://example.com/a), [Beta](https://example.com/b), [Gamma](https://example.com/c).

## 2.4 — Smart Punctuation

"Smart double quotes" and 'smart single quotes' should render with curly quotes.

An em-dash appears here---like this. An en-dash: 1--10. Ellipsis: ...

---

# 3 — Block Quotes

## 3.1 — Simple Block Quote

> This is a simple block quote. It should render with a left-side bar indicator
> and indented text.

## 3.2 — Multi-Paragraph Block Quote

> First paragraph of the block quote. Lorem ipsum dolor sit amet, consectetur
> adipiscing elit.
>
> Second paragraph. Sed do eiusmod tempor incididunt ut labore et dolore magna
> aliqua.

## 3.3 — Nested Block Quotes

> Level one.
>
> > Level two — nested inside level one.
> >
> > > Level three — deeply nested.
> > >
> > > > Level four — even deeper. The bar indicators should stack.

## 3.4 — Block Quote with Styled Content

> **Bold text** inside a block quote, plus *italic* and `code`.
>
> - A list inside a block quote
> - Second item
>
> ```rust
> fn quoted_code() {
>     println!("code inside a blockquote");
> }
> ```

---

# 4 — Lists

## 4.1 — Unordered Lists

- Item one
- Item two
- Item three
  - Nested item A
  - Nested item B
    - Deep nested item
    - Another deep item
  - Back to second level
- Item four

## 4.2 — Ordered Lists

1. First item
2. Second item
3. Third item
   1. Sub-item 3a
   2. Sub-item 3b
      1. Sub-sub-item
      2. Another sub-sub
   3. Sub-item 3c
4. Fourth item

## 4.3 — Task Lists

- [x] Completed task
- [x] Another completed task
- [ ] Uncompleted task
- [ ] Another uncompleted task
  - [x] Nested completed subtask
  - [ ] Nested uncompleted subtask

## 4.4 — Mixed Nested Lists

1. Ordered parent
   - Unordered child A
   - Unordered child B
     1. Re-ordered grandchild
     2. Another grandchild
2. Second ordered parent
   - [x] Task-list child
   - [ ] Another task child

## 4.5 — Long List (50 Items)

1. Item 001 — Lorem ipsum dolor sit amet
2. Item 002 — Consectetur adipiscing elit
3. Item 003 — Sed do eiusmod tempor incididunt
4. Item 004 — Ut labore et dolore magna aliqua
5. Item 005 — Ut enim ad minim veniam
6. Item 006 — Quis nostrud exercitation ullamco
7. Item 007 — Laboris nisi ut aliquip ex ea
8. Item 008 — Commodo consequat duis aute irure
9. Item 009 — Dolor in reprehenderit in voluptate
10. Item 010 — Velit esse cillum dolore eu fugiat
11. Item 011 — Nulla pariatur excepteur sint occaecat
12. Item 012 — Cupidatat non proident sunt in culpa
13. Item 013 — Qui officia deserunt mollit anim id
14. Item 014 — Est laborum sed ut perspiciatis unde
15. Item 015 — Omnis iste natus error sit voluptatem
16. Item 016 — Accusantium doloremque laudantium totam
17. Item 017 — Rem aperiam eaque ipsa quae ab illo
18. Item 018 — Inventore veritatis et quasi architecto
19. Item 019 — Beatae vitae dicta sunt explicabo
20. Item 020 — Nemo enim ipsam voluptatem quia voluptas
21. Item 021 — Sit aspernatur aut odit aut fugit
22. Item 022 — Sed quia consequuntur magni dolores eos
23. Item 023 — Qui ratione voluptatem sequi nesciunt
24. Item 024 — Neque porro quisquam est qui dolorem
25. Item 025 — Ipsum quia dolor sit amet consectetur
26. Item 026 — Adipisci velit sed quia non numquam
27. Item 027 — Eius modi tempora incidunt ut labore
28. Item 028 — Et dolore magnam aliquam quaerat voluptatem
29. Item 029 — Ut enim ad minima veniam quis nostrum
30. Item 030 — Exercitationem ullam corporis suscipit
31. Item 031 — Laboriosam nisi ut aliquid ex ea commodi
32. Item 032 — Consequatur quis autem vel eum iure
33. Item 033 — Reprehenderit qui in ea voluptate velit
34. Item 034 — Esse quam nihil molestiae consequatur
35. Item 035 — Vel illum qui dolorem eum fugiat quo
36. Item 036 — Voluptas nulla pariatur at vero eos
37. Item 037 — Et accusamus et iusto odio dignissimos
38. Item 038 — Ducimus qui blanditiis praesentium
39. Item 039 — Voluptatum deleniti atque corrupti quos
40. Item 040 — Dolores et quas molestias excepturi sint
41. Item 041 — Occaecati cupiditate non provident
42. Item 042 — Similique sunt in culpa qui officia
43. Item 043 — Deserunt mollitia animi id est laborum
44. Item 044 — Et harum quidem rerum facilis est et
45. Item 045 — Expedita distinctio nam libero tempore
46. Item 046 — Cum soluta nobis est eligendi optio
47. Item 047 — Cumque nihil impedit quo minus id quod
48. Item 048 — Maxime placeat facere possimus omnis
49. Item 049 — Voluptas assumenda est omnis dolor
50. Item 050 — Repellendus temporibus autem quibusdam

---

# 5 — Code Blocks

## 5.1 — Rust

```rust
use std::collections::HashMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut scores: HashMap<&str, i32> = HashMap::new();
    scores.insert("Alice", 100);
    scores.insert("Bob", 85);

    for (name, score) in &scores {
        println!("{name}: {score}");
    }

    let total: i32 = scores.values().sum();
    println!("Total: {total}");
    Ok(())
}
```

## 5.2 — Python

```python
from dataclasses import dataclass
from typing import Optional

@dataclass
class User:
    name: str
    email: str
    age: Optional[int] = None

    def greet(self) -> str:
        return f"Hello, {self.name}!"

users = [User("Alice", "alice@example.com", 30), User("Bob", "bob@example.com")]
for user in users:
    print(user.greet())
```

## 5.3 — JSON

```json
{
  "name": "rustdown",
  "version": "1.0.0",
  "features": [
    "markdown-preview",
    "syntax-highlighting",
    "live-reload"
  ],
  "config": {
    "theme": "dark",
    "fontSize": 14,
    "wordWrap": true
  }
}
```

## 5.4 — Bash / Shell

```bash
#!/usr/bin/env bash
set -euo pipefail

echo "Building rustdown..."
cargo build --release

BINARY="target/release/rustdown"
if [[ -f "$BINARY" ]]; then
    echo "Build succeeded: $(du -h "$BINARY" | cut -f1)"
    "$BINARY" --version
else
    echo "Build failed!" >&2
    exit 1
fi
```

## 5.5 — JavaScript

```javascript
class EventEmitter {
  #listeners = new Map();

  on(event, callback) {
    if (!this.#listeners.has(event)) {
      this.#listeners.set(event, []);
    }
    this.#listeners.get(event).push(callback);
    return this;
  }

  emit(event, ...args) {
    for (const cb of this.#listeners.get(event) ?? []) {
      cb(...args);
    }
  }
}

const emitter = new EventEmitter();
emitter.on("data", (msg) => console.log(`Received: ${msg}`));
emitter.emit("data", "Hello, World!");
```

## 5.6 — SQL

```sql
SELECT
    u.name,
    u.email,
    COUNT(o.id) AS order_count,
    COALESCE(SUM(o.total), 0) AS lifetime_spend
FROM users u
LEFT JOIN orders o ON o.user_id = u.id
WHERE u.created_at >= '2024-01-01'
GROUP BY u.id, u.name, u.email
HAVING COUNT(o.id) > 0
ORDER BY lifetime_spend DESC
LIMIT 20;
```

## 5.7 — TOML

```toml
[package]
name = "rustdown"
version = "1.0.0"
edition = "2021"

[dependencies]
eframe = { version = "0.31", features = ["wayland", "x11"] }
pulldown-cmark = { version = "0.12", default-features = false }

[profile.release]
opt-level = 3
lto = true
strip = true
```

## 5.8 — No Language Tag

```
This is a plain fenced code block without a language tag.
It should render in monospace with a background, but no
language label above it.
    Indentation is preserved.
```

## 5.9 — Long Lines (Horizontal Scroll)

```text
This is a very long line that should trigger horizontal scrolling in the code block viewer. It goes on and on and on and on and on and on and on and on and on and on and on and on and on and on and on and on and on and on and on until it wraps around or scrolls.
Another long line: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
```

---

# 6 — Tables

## 6.1 — Simple Table

| Name    | Role       | Status  |
|---------|------------|---------|
| Alice   | Developer  | Active  |
| Bob     | Designer   | Active  |
| Charlie | Manager    | On Leave|

## 6.2 — Column Alignment

| Left-Aligned | Center-Aligned | Right-Aligned |
|:-------------|:--------------:|--------------:|
| Left 1       | Center 1       | Right 1       |
| Left 2       | Center 2       | Right 2       |
| Left 3       | Center 3       | Right 3       |
| A longer left cell | Centered | 1,234,567.89 |

## 6.3 — Wide Table (10 Columns)

| Col 1 | Col 2 | Col 3 | Col 4 | Col 5 | Col 6 | Col 7 | Col 8 | Col 9 | Col 10 |
|-------|-------|-------|-------|-------|-------|-------|-------|-------|--------|
| A1    | A2    | A3    | A4    | A5    | A6    | A7    | A8    | A9    | A10    |
| B1    | B2    | B3    | B4    | B5    | B6    | B7    | B8    | B9    | B10    |
| C1    | C2    | C3    | C4    | C5    | C6    | C7    | C8    | C9    | C10    |
| D1    | D2    | D3    | D4    | D5    | D6    | D7    | D8    | D9    | D10    |
| E1    | E2    | E3    | E4    | E5    | E6    | E7    | E8    | E9    | E10    |

## 6.4 — Styled Content in Table Cells

| Feature        | Syntax              | Example                           |
|----------------|---------------------|-----------------------------------|
| Bold           | `**text**`          | **Bold text**                     |
| Italic         | `*text*`            | *Italic text*                     |
| Strikethrough  | `~~text~~`          | ~~Struck through~~                |
| Code           | `` `code` ``        | `inline code`                     |
| Link           | `[text](url)`       | [Example](https://example.com)    |
| Combined       | `***bold italic***` | ***Bold and italic***             |

## 6.5 — Large Table (50 Rows)

| #  | ID         | Name           | Category   | Value   | Status   | Priority | Tags          |
|----|------------|----------------|------------|---------|----------|----------|---------------|
| 1  | ID-001     | Widget Alpha   | Hardware   | $12.50  | Active   | High     | new, urgent   |
| 2  | ID-002     | Widget Beta    | Software   | $25.00  | Pending  | Medium   | review        |
| 3  | ID-003     | Widget Gamma   | Hardware   | $8.75   | Active   | Low      | stock         |
| 4  | ID-004     | Widget Delta   | Service    | $150.00 | Active   | High     | premium       |
| 5  | ID-005     | Widget Epsilon | Software   | $30.00  | Archived | Low      | deprecated    |
| 6  | ID-006     | Widget Zeta    | Hardware   | $45.00  | Active   | Medium   | popular       |
| 7  | ID-007     | Widget Eta     | Service    | $200.00 | Pending  | High     | enterprise    |
| 8  | ID-008     | Widget Theta   | Software   | $15.00  | Active   | Low      | free-tier     |
| 9  | ID-009     | Widget Iota    | Hardware   | $92.50  | Active   | Medium   | bulk          |
| 10 | ID-010     | Widget Kappa   | Service    | $75.00  | Active   | High     | support       |
| 11 | ID-011     | Widget Lambda  | Software   | $55.00  | Pending  | Medium   | beta          |
| 12 | ID-012     | Widget Mu      | Hardware   | $18.25  | Active   | Low      | clearance     |
| 13 | ID-013     | Widget Nu      | Service    | $300.00 | Active   | High     | vip           |
| 14 | ID-014     | Widget Xi      | Software   | $42.00  | Archived | Medium   | legacy        |
| 15 | ID-015     | Widget Omicron | Hardware   | $67.50  | Active   | High     | trending      |
| 16 | ID-016     | Widget Pi      | Service    | $125.00 | Pending  | Medium   | trial         |
| 17 | ID-017     | Widget Rho     | Software   | $20.00  | Active   | Low      | starter       |
| 18 | ID-018     | Widget Sigma   | Hardware   | $88.00  | Active   | High     | featured      |
| 19 | ID-019     | Widget Tau     | Service    | $250.00 | Active   | High     | annual        |
| 20 | ID-020     | Widget Upsilon | Software   | $35.00  | Pending  | Medium   | update        |
| 21 | ID-021     | Widget Phi     | Hardware   | $14.00  | Active   | Low      | basic         |
| 22 | ID-022     | Widget Chi     | Service    | $180.00 | Active   | High     | managed       |
| 23 | ID-023     | Widget Psi     | Software   | $60.00  | Active   | Medium   | pro           |
| 24 | ID-024     | Widget Omega   | Hardware   | $105.00 | Archived | Low      | discontinued  |
| 25 | ID-025     | Gadget Alpha   | Service    | $95.00  | Active   | High     | new           |
| 26 | ID-026     | Gadget Beta    | Software   | $22.00  | Pending  | Medium   | preview       |
| 27 | ID-027     | Gadget Gamma   | Hardware   | $50.00  | Active   | Low      | value         |
| 28 | ID-028     | Gadget Delta   | Service    | $175.00 | Active   | High     | premium       |
| 29 | ID-029     | Gadget Epsilon | Software   | $28.00  | Active   | Medium   | standard      |
| 30 | ID-030     | Gadget Zeta    | Hardware   | $38.00  | Pending  | Low      | budget        |
| 31 | ID-031     | Gadget Eta     | Service    | $220.00 | Active   | High     | platinum      |
| 32 | ID-032     | Gadget Theta   | Software   | $48.00  | Active   | Medium   | team          |
| 33 | ID-033     | Gadget Iota    | Hardware   | $72.00  | Active   | High     | industrial    |
| 34 | ID-034     | Gadget Kappa   | Service    | $130.00 | Archived | Medium   | sunset        |
| 35 | ID-035     | Gadget Lambda  | Software   | $16.00  | Active   | Low      | lite          |
| 36 | ID-036     | Gadget Mu      | Hardware   | $85.00  | Pending  | High     | preorder      |
| 37 | ID-037     | Gadget Nu      | Service    | $160.00 | Active   | Medium   | business      |
| 38 | ID-038     | Gadget Xi      | Software   | $32.00  | Active   | Low      | personal      |
| 39 | ID-039     | Gadget Omicron | Hardware   | $110.00 | Active   | High     | flagship      |
| 40 | ID-040     | Gadget Pi      | Service    | $90.00  | Active   | Medium   | monthly       |
| 41 | ID-041     | Gadget Rho     | Software   | $24.00  | Pending  | Low      | beta          |
| 42 | ID-042     | Gadget Sigma   | Hardware   | $58.00  | Active   | Medium   | midrange      |
| 43 | ID-043     | Gadget Tau     | Service    | $275.00 | Active   | High     | unlimited     |
| 44 | ID-044     | Gadget Upsilon | Software   | $40.00  | Active   | Medium   | advanced      |
| 45 | ID-045     | Gadget Phi     | Hardware   | $20.00  | Archived | Low      | eol           |
| 46 | ID-046     | Gadget Chi     | Service    | $145.00 | Active   | High     | priority      |
| 47 | ID-047     | Gadget Psi     | Software   | $52.00  | Pending  | Medium   | staging       |
| 48 | ID-048     | Gadget Omega   | Hardware   | $95.00  | Active   | High     | limited       |
| 49 | ID-049     | Module Alpha   | Service    | $350.00 | Active   | High     | custom        |
| 50 | ID-050     | Module Beta    | Software   | $65.00  | Active   | Medium   | modular       |

## 6.6 — Single-Column Table

| Status Messages              |
|------------------------------|
| System initializing...       |
| Loading configuration        |
| Connecting to database       |
| Migration complete           |
| Server listening on :8080    |

---

# 7 — Images

> **Note**: Images are loaded from GitHub over HTTP. Network access is required.

## 7.1 — Tiny Image (50×50)

![Tiny test image](https://raw.githubusercontent.com/teh-hippo/rustdown/main/test-assets/tiny.png)

Text continues after the image.

## 7.2 — Small Image (200×150)

![Small test image](https://raw.githubusercontent.com/teh-hippo/rustdown/main/test-assets/small.png)

## 7.3 — Medium Image (800×400)

![Medium test image](https://raw.githubusercontent.com/teh-hippo/rustdown/main/test-assets/medium.png)

## 7.4 — Large Image (1920×1080)

![Large full-HD test image](https://raw.githubusercontent.com/teh-hippo/rustdown/main/test-assets/large.png)

## 7.5 — Tall Narrow Image (100×800)

![Tall narrow test image](https://raw.githubusercontent.com/teh-hippo/rustdown/main/test-assets/tall_narrow.png)

---

# 8 — Horizontal Rules

Three different horizontal rule syntaxes — all should render identically:

---

***

___

Text between horizontal rules should be clearly separated.

---

# 9 — Long Paragraphs

## 9.1 — Text Reflow

Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.

Curabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra, est eros bibendum elit, nec luctus magna felis sollicitudin mauris. Integer in mauris eu nibh euismod gravida. Duis ac tellus et risus vulputate vehicula. Donec lobortis risus a elit. Etiam tempor. Ut ullamcorper, ligula ut dictum pharetra, nisi nunc fringilla magna, in commodo elit erat nec turpis. Ut pharetra augue nec augue. Nam elit agna, endrerit sit amet, tincidunt ac, viverra sed, nulla.

## 9.2 — Paragraph with Inline Styles

This paragraph contains **bold segments** interspersed with *italic segments* and even some `inline code` and ~~struck-through words~~ to test how the renderer handles frequent style transitions within flowing text. It also includes a [hyperlink to Rust docs](https://doc.rust-lang.org/) and ensures that all these elements work together seamlessly when the text wraps across multiple lines in the viewport.

---

# 10 — Deeply Nested Content

## 10.1 — Nested List with Block Content

1. First top-level item

   This is a paragraph inside a list item. It should be indented with the list.

   ```rust
   // Code block inside a list item
   fn nested_code() -> bool {
       true
   }
   ```

   > A block quote inside a list item.

2. Second top-level item
   - Sub-item with **bold**
   - Sub-item with *italic*
     1. Numbered sub-sub-item
     2. Another numbered item

        Paragraph inside a deeply nested item.

---

# 11 — Edge Cases

## 11.1 — Empty Heading Content

The heading below is intentionally empty (just the `##` marker):

##

## 11.2 — Heading with Inline Code

### The `parse_markdown()` function

## 11.3 — Heading with Link

### Visit [Rustdown](https://github.com/teh-hippo/rustdown) on GitHub

## 11.4 — Adjacent Code Blocks

```rust
fn first_block() {}
```

```python
def second_block():
    pass
```

```bash
echo "third block"
```

## 11.5 — Single-Row Table

| Only | One | Row |
|------|-----|-----|
| A    | B   | C   |

## 11.6 — Table Immediately After Heading

### Data Summary
| Metric | Value |
|--------|-------|
| Users  | 1,234 |
| Events | 56,789|

## 11.7 — Very Long Heading Text

### This is an extremely long heading that is designed to test how the navigation panel and heading renderer handle text that exceeds the typical viewport width

---

# 12 — Mixed Complex Content

## 12.1 — Technical Documentation Style

The `RustdownApp` struct is the main application shell. It manages:

1. **Document state** — via the `Document` struct
2. **UI modes** — Edit, Preview, SideBySide
3. **File I/O** — open, save, export with dirty-state prompts
4. **Live reload** — file watcher + 3-way merge for external changes

Configuration is stored in the following format:

```toml
[editor]
font_size = 14
word_wrap = true
theme = "dark"

[preview]
sync_scroll = true
image_loading = "lazy"
```

> **Note**: The preview renderer uses viewport culling — only visible blocks
> are rendered each frame, giving O(visible) cost regardless of document size.

Key performance characteristics:

| Operation         | Complexity | Notes                          |
|-------------------|------------|--------------------------------|
| Text editing      | O(1)       | `Arc::make_mut` copy-on-write  |
| Preview render    | O(visible) | Viewport culling               |
| Nav outline       | O(n)       | Full rescan on edit             |
| Heading scroll    | O(log n)   | Binary search on byte offsets   |
| File save         | O(n)       | Atomic write via temp file      |

## 12.2 — Feature Checklist

- [x] ATX headings (H1–H6)
- [x] Inline styles (bold, italic, strikethrough, code)
- [x] Links and autolinks
- [x] Images (relative and absolute URLs)
- [x] Fenced code blocks with language tags
- [x] Block quotes (nested)
- [x] Ordered and unordered lists (nested)
- [x] Task lists / checkboxes
- [x] Tables with alignment
- [x] Horizontal rules
- [x] Smart punctuation
- [x] GFM extensions

---

# 13 — Additional Table Variants

## 13.1 — Narrow Two-Column

| K | V |
|---|---|
| a | 1 |
| b | 2 |
| c | 3 |

## 13.2 — Table with Long Cell Content

| Description                                                                                   | Status  |
|-----------------------------------------------------------------------------------------------|---------|
| This cell contains a very long description that tests how the table renderer handles overflow  | OK      |
| Short                                                                                         | OK      |
| Another long cell: Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod    | Warning |

## 13.3 — Table with Code in Cells

| Command               | Description                    |
|-----------------------|--------------------------------|
| `cargo build`         | Compile the project            |
| `cargo test`          | Run all tests                  |
| `cargo clippy`        | Run the linter                 |
| `cargo fmt --check`   | Check formatting               |
| `cargo run -- -v`     | Print version                  |

---

# 14 — More Headings for Navigation

## Section A

### A.1

### A.2

### A.3

## Section B

### B.1

### B.2

#### B.2.1

#### B.2.2

### B.3

## Section C

### C.1

#### C.1.1

##### C.1.1.1

###### C.1.1.1.1

### C.2

### C.3

## Section D

Content under Section D.

## Section E

Content under Section E.

---

# 15 — Final Stress: Repeated Blocks

## 15.1 — Multiple Code Blocks in Succession

```rust
fn block_1() { }
```

```rust
fn block_2() { }
```

```rust
fn block_3() { }
```

```rust
fn block_4() { }
```

```rust
fn block_5() { }
```

## 15.2 — Alternating Block Quotes and Paragraphs

> Quote one.

Paragraph between quotes.

> Quote two.

Paragraph between quotes.

> Quote three.

Paragraph between quotes.

> Quote four.

Final paragraph.

## 15.3 — Dense Inline Styling

**Bold** *italic* ~~strike~~ `code` **Bold** *italic* ~~strike~~ `code`
**Bold** *italic* ~~strike~~ `code` **Bold** *italic* ~~strike~~ `code`
**Bold** *italic* ~~strike~~ `code` **Bold** *italic* ~~strike~~ `code`
**Bold** *italic* ~~strike~~ `code` **Bold** *italic* ~~strike~~ `code`

---

# 16 — End of Verification Document

This is the final paragraph. If you can see this text rendered correctly,
along with all the headings in the navigation panel, then the verification
document has loaded and rendered successfully. 🎉
