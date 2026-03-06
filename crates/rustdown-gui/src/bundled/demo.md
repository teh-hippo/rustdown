# ✨ Rustdown Feature Demo

> A compact showcase of every supported Markdown feature.
> Open with **Ctrl+Shift+F11**.

---

## Headings

All six levels are supported:

# Heading 1
## Heading 2
### Heading 3
#### Heading 4
##### Heading 5
###### Heading 6

---

## Inline Styles

| Style          | Syntax              | Rendered                     |
|----------------|---------------------|------------------------------|
| Bold           | `**text**`          | **Bold text**                |
| Italic         | `*text*`            | *Italic text*                |
| Bold + Italic  | `***text***`        | ***Bold and italic***        |
| Strikethrough  | `~~text~~`          | ~~Struck through~~           |
| Inline Code    | `` `code` ``        | `inline code`                |
| Link           | `[text](url)`       | [Rust](https://rust-lang.org)|

Combining them: **bold**, *italic*, ~~strike~~, `code`, and [links](https://github.com/teh-hippo/rustdown) all in one paragraph.

---

## Smart Punctuation

"Double quotes" become curly. 'Single quotes' too.

Em-dash---like this. En-dash: 2020--2025. Ellipsis...

---

## Block Quotes

> A simple block quote.

> Nested quotes:
>
> > Second level.
> >
> > > Third level.

> Quote with **styled** content, `code`, and a [link](https://example.com).

---

## Lists

### Unordered

- First item
- Second item
  - Nested A
  - Nested B
    - Deep nested
- Third item

### Ordered

1. First
2. Second
   1. Sub-item
   2. Sub-item
3. Third

### Task Lists

- [x] Completed task
- [ ] Pending task
- [x] Another done
  - [ ] Nested pending
  - [x] Nested done

---

## Code Blocks

### Rust

```rust
fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

fn main() {
    println!("{}", greet("Rustdown"));
}
```

### Python

```python
def fibonacci(n: int) -> list[int]:
    a, b = 0, 1
    result = []
    for _ in range(n):
        result.append(a)
        a, b = b, a + b
    return result

print(fibonacci(10))
```

### JSON

```json
{
  "editor": {
    "theme": "dark",
    "fontSize": 14,
    "wordWrap": true
  }
}
```

### Plain (No Language)

```
Plain fenced code block.
No language tag — just monospace text.
```

---

## Tables

### Simple

| Name    | Role       | Status |
|---------|------------|--------|
| Alice   | Developer  | Active |
| Bob     | Designer   | Active |
| Charlie | Manager    | Away   |

### Column Alignment

| Left         |   Center   |     Right |
|:-------------|:----------:|----------:|
| Left-aligned |  Centered  |     Right |
| Text         |   Text     | 1,234.56  |

### Wide Table

| A | B | C | D | E | F | G | H |
|---|---|---|---|---|---|---|---|
| 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 |

---

## Images

A small test image:

![Tiny test image](https://raw.githubusercontent.com/teh-hippo/rustdown/main/test-assets/tiny.png)

A medium test image:

![Medium test image](https://raw.githubusercontent.com/teh-hippo/rustdown/main/test-assets/small.png)

---

## Horizontal Rules

---

***

___

---

## Mixed Content

Here is a technical paragraph with **bold terms**, *emphasized concepts*, and
`code references` all flowing together. See the [documentation](https://doc.rust-lang.org/)
for details.

> **Tip**: Use `Ctrl+Enter` to cycle between Edit, Preview, and Side-by-Side modes.

A summary table:

| Shortcut          | Action              |
|-------------------|---------------------|
| `Ctrl+O`          | Open file           |
| `Ctrl+S`          | Save                |
| `Ctrl+N`          | New document        |
| `Ctrl+Enter`      | Cycle mode          |
| `Ctrl+F`          | Find                |
| `Ctrl+Shift+F`    | Replace             |
| `Ctrl+Alt+F`      | Format              |
| `Ctrl+Shift+T`    | Toggle nav panel    |
| `Ctrl+Shift+F11`  | Open demo file      |
| `Ctrl+Shift+F12`  | Open verification   |

---

## End

That covers all supported features! 🎉
