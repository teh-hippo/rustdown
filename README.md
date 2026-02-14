# rustdown

A highly performant, minimalist Markdown editor written in Rust with a **native UI** (no webviews).

## Status
Early, but functional:
- Native GUI editor with tabs, editor/preview toggle, basic markdown rendering, and lightweight syntax highlighting.
- CLI `preview` mode (plain-text rendering).

## Quickstart

GUI:
```bash
cargo run -p rustdown-gui
```

CLI preview:
```bash
cargo run -p rustdown-cli -- preview README.md
```
