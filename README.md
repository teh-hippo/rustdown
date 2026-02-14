# rustdown

A highly performant, minimalist Markdown editor written in Rust with a **native UI** (no webviews).

## Status
Early, but functional:
- Native GUI editor with tabs, editor/preview toggle, basic markdown rendering, lightweight syntax highlighting, and basic Open/Save.
- CLI `preview` mode (plain-text rendering).

## Quickstart

GUI:
```bash
cargo run -p rustdown-gui
```

Keyboard shortcuts (Cmd on macOS, Ctrl elsewhere):
- Cmd/Ctrl+O: Open…
- Cmd/Ctrl+S: Save
- Cmd/Ctrl+N: New tab
- Cmd/Ctrl+W: Close tab
- Cmd/Ctrl+Enter: Toggle Edit/Preview

CLI preview:
```bash
cargo run -p rustdown-cli -- preview README.md
```
