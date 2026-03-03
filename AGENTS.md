# AGENTS

This repo is a native (non-webview) Markdown editor in Rust (no wasm/web build).

## Project structure
- `crates/rustdown-gui`: eframe/egui native GUI app

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
- No `unsafe` (enforced). The single exception is the WSL workaround in `apply_wsl_workarounds()`.
- Keep allocations/cloning minimal; favor borrowing.
- Avoid `unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`/`dbg!` in non-test code (enforced via workspace clippy lints).
- CI runs with `--locked`; keep `Cargo.lock` committed and up to date.
