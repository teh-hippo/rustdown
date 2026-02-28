# rustdown

A highly performant, minimalist Markdown editor written in Rust with a **native UI** (no webviews, no wasm/web build).

## Status
Early, but functional:
- Native GUI editor with Edit/Preview/Side-by-side modes, native markdown preview (lists/task lists/quotes/tables/code/strike), lightweight syntax highlighting, drag-and-drop markdown open, live writing stats, HTML export, and Open/Save/Save As + unsaved-changes confirmation.

## Quickstart

GUI:
```bash
cargo run -p rustdown
```

Open a file directly:
```bash
cargo run -p rustdown -- README.md
```
When a markdown path is provided at launch, rustdown opens in **Preview** mode by default.

Start in Preview mode:
```bash
cargo run -p rustdown -- -p
```

Start in Side-by-side mode:
```bash
cargo run -p rustdown -- -s
```

Run profiling diagnostics for markdown load/render pipelines:
```bash
cargo run -p rustdown -- --diagnostics-open README.md --diag-iterations=120
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
- Cmd/Ctrl+F: Find
- Cmd/Ctrl+Shift+F: Find + Replace all
- Cmd/Ctrl+Alt+F: Format document
- Cmd/Ctrl+E: Export HTML…
- Cmd/Ctrl+Enter: Cycle Edit/Preview/Side-by-side
- Cmd/Ctrl++: Increase font size
- Cmd/Ctrl+-: Decrease font size
- Ctrl/Cmd + mouse wheel (or pinch gesture): Zoom text
- Experimental: toggle **Color headings (exp)** in the status bar for thematic heading colors in the editor
  
Tip: the mode indicator in the bottom bar is clickable.
Tip: you can also drag and drop `.md`/`.markdown` files into the window to open them.

Formatting is intentionally simple; if a `.editorconfig` file is present, rustdown will use a small subset:
`trim_trailing_whitespace`, `insert_final_newline`, and `end_of_line` (lf/crlf).

## Supported platforms

| Platform | Architecture | Status |
|----------|-------------|--------|
| Linux | x86_64 | ✅ Built + tested in CI |
| Windows | x86_64 | ✅ Built + tested in CI |

Pre-built binaries are available on the [Releases](https://github.com/teh-hippo/rustdown/releases) page.
