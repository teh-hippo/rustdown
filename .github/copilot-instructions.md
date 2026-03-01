# Copilot instructions for rustdown

## Build, test, and lint commands
- Build: `cargo build -p rustdown`
- Run locally: `cargo run -p rustdown`
- Formatting check: `cargo fmt --all -- --check`
- Lint: `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- Full tests: `cargo test --workspace --locked`
- Run one test by exact name: `cargo test -p rustdown parse_launch_options_covers_modes_paths_and_diagnostics -- --exact`
- Run one module-scoped test: `cargo test -p rustdown live_merge::tests::merge_three_way_conflict_cases`

## High-level architecture
- This workspace currently has one package, `rustdown`, in `crates/rustdown-gui`; it is a native `eframe/egui` app, and `wasm32` builds are explicitly blocked.
- `crates/rustdown-gui/src/main.rs` owns the app shell (`RustdownApp`) and orchestrates UI modes, shortcuts, open/save/export flows, dirty-state prompts, search/replace, and status UI.
- Document state is centralized in `Document` (`text`, `base_text`, `disk_rev`, stats, preview cache flags, `edit_seq`), and the editor path uses `TrackedTextBuffer` + `EditorGalleyCache` to avoid expensive relayouts.
- Markdown rendering is split:
  - Editor highlighting: `highlight::markdown_layout_job` (headings/inline code/fenced code styling).
  - Preview rendering: `egui_commonmark::CommonMarkViewer` with `CommonMarkCache`.
- External file change handling spans multiple modules:
  - `main.rs` manages watcher/polling, async reload scheduling, and conflict dialogs.
  - `disk_io.rs` provides stable UTF-8 reads (`read_stable_utf8`), revision metadata (`disk_revision`), and atomic writes.
  - `live_merge.rs` performs 3-way merges for dirty buffers and returns clean or conflicted outcomes.
  - Conflicted "keep mine" flow can write a `.rustdown-merge*.md` sidecar via `next_merge_sidecar_path`.
- Fenced-code parsing logic is shared in `markdown_fence.rs` and reused by both formatter and highlighter.

## Key conventions
- Keep the app native-first: avoid webview/wasm assumptions in new code paths.
- Workspace lint policy is strict (`unsafe_code` denied; warnings denied; no `unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`/`dbg!` outside tests).
- Prefer low-allocation edits: document text is stored as `Arc<String>` and mutated via `Arc::make_mut`; when text changes, keep `edit_seq`, dirty flags, and stats/preview invalidation in sync.
- Preserve formatter semantics in `format.rs`: only `.editorconfig` keys `trim_trailing_whitespace`, `insert_final_newline`, and `end_of_line` are honored, with fenced block content intentionally preserved.
- If merge/conflict behavior changes, keep `live_merge.rs` tests and `main.rs` conflict-choice tests aligned; both conflict-marker and ours-wins outputs are intentional.
