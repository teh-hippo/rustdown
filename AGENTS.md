# AGENTS

This repo is a native (non-webview) Markdown editor in Rust (no wasm/web build).

## Project structure
- `crates/rustdown-gui`: eframe/egui native GUI app
  - `src/main.rs` — app shell, shortcuts, UI modes, open/save/export
  - `src/preferences.rs` — user settings persistence (`~/.config/rustdown/settings.toml`)
  - `src/bundled/` — embedded demo and verification markdown files
  - `src/nav_panel.rs` / `src/nav_outline.rs` — navigation panel and heading extraction
  - `src/highlight.rs` — editor syntax highlighting
  - `src/format.rs` — `.editorconfig`-aware formatter
  - `src/disk_io.rs` / `src/disk_sync.rs` / `src/disk_watcher.rs` — file I/O and live reload
  - `src/live_merge.rs` — 3-way merge for external changes
- `crates/rustdown-md`: Markdown parsing and rendering library (egui widgets)

## Dev commands
```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Quickstart
```bash
cargo run -p rustdown
```

## Conventions
- Prefer simple, explicit code.
- No `unsafe` (enforced). The two exceptions are the WSL workaround in `apply_wsl_workarounds()` and the Windows `AttachConsole` FFI in `attach_parent_console()`.
- Keep allocations/cloning minimal; favor borrowing.
- Avoid `unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`/`dbg!` in non-test code (enforced via workspace clippy lints).
- CI runs with `--locked`; keep `Cargo.lock` committed and up to date.
- User preferences are persisted via `preferences.rs` using `dirs`+`toml`+`serde`. Extend `UserPreferences` for new settings.
