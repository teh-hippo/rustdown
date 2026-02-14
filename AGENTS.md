# AGENTS

This repo is a native (non-webview) Markdown editor in Rust.

## Project structure
- `crates/rustdown-core`: shared types + markdown parsing/rendering logic
- `crates/rustdown-cli`: CLI wrapper (preview-only mode)
- `crates/rustdown-gui`: eframe/egui native GUI

## Dev commands
```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Conventions
- Prefer simple, explicit code.
- No `unsafe` (enforced).
- Keep allocations/cloning minimal; favor borrowing.
