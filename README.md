# rustdown

A highly performant, minimalist Markdown editor written in Rust with a **native UI** (no webviews).

## Status
Early, but functional:
- Native GUI editor with tabs, edit/preview toggle, native markdown preview (lists/task lists/quotes/tables/code/strike), lightweight syntax highlighting, and Open/Save/Save As/Save All + confirm-close.
- CLI `preview` mode (plain-text rendering).

## Quickstart

GUI:
```bash
cargo run -p rustdown-gui
```

Open a file directly:
```bash
cargo run -p rustdown-gui -- README.md
```

Keyboard shortcuts (Cmd on macOS, Ctrl elsewhere):
- Cmd/Ctrl+O: Open…
- Cmd/Ctrl+S: Save
- Cmd/Ctrl+Shift+S: Save As…
- Cmd/Ctrl+N: New tab
- Cmd/Ctrl+W: Close tab
- Cmd/Ctrl+Enter: Toggle Edit/Preview

Tip: right-click a tab for Save/Close options.
Tip: use the "Save All" button before closing lots of tabs.

CLI preview:
```bash
cargo run -p rustdown-cli -- preview README.md
```
