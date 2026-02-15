# rustdown

A highly performant, minimalist Markdown editor written in Rust with a **native UI** (no webviews, no wasm/web build).

## Status
Early, but functional:
- Native GUI editor with Edit/Preview/Side-by-side modes, native markdown preview (lists/task lists/quotes/tables/code/strike), lightweight syntax highlighting, and Open/Save/Save As + unsaved-changes confirmation.

## Quickstart

GUI:
```bash
cargo run -p rustdown
```

Open a file directly:
```bash
cargo run -p rustdown -- README.md
```

Start in Preview mode:
```bash
cargo run -p rustdown -- -p
```

Start in Side-by-side mode:
```bash
cargo run -p rustdown -- -s
```

rustdown loads a single system UI font at startup. Override with `RUSTDOWN_FONT_PATH=/path/to/font.ttf`.
On Linux it checks:
`/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf`,
`/usr/share/fonts/TTF/DejaVuSans.ttf`, or
`/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf`.

Keyboard shortcuts (Cmd on macOS, Ctrl elsewhere):
- Cmd/Ctrl+O: Open…
- Cmd/Ctrl+S: Save
- Cmd/Ctrl+Shift+S: Save As…
- Cmd/Ctrl+N: New document
- Cmd/Ctrl+Shift+F: Format document
- Cmd/Ctrl+Enter: Cycle Edit/Preview/Side-by-side
- Cmd/Ctrl++: Increase font size
- Cmd/Ctrl+-: Decrease font size
  
Tip: the mode indicator in the bottom bar is clickable.

Formatting is intentionally simple; if a `.editorconfig` file is present, rustdown will use a small subset:
`trim_trailing_whitespace`, `insert_final_newline`, and `end_of_line` (lf/crlf).
