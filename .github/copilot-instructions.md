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
- Navigation panel (`nav_panel.rs`) provides a table-of-contents sidebar driven by heading extraction (`nav_outline.rs` via `pulldown_cmark::Parser`); headings are stored as byte offsets to avoid allocations.
- Fenced-code parsing logic is shared in `markdown_fence.rs` and reused by both formatter and highlighter.

## Key conventions
- Keep the app native-first: avoid webview/wasm assumptions in new code paths.
- Workspace lint policy is strict (`unsafe_code` denied; warnings denied; no `unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`/`dbg!` outside tests). The single `#[allow(unsafe_code)]` exception is the WSL workaround in `apply_wsl_workarounds()` (clearing `WAYLAND_DISPLAY` before threads spawn to avoid a smithay-clipboard crash).
- Prefer low-allocation edits: document text is stored as `Arc<String>` and mutated via `Arc::make_mut`; when text changes, keep `edit_seq`, dirty flags, and stats/preview invalidation in sync.
- Preserve formatter semantics in `format.rs`: only `.editorconfig` keys `trim_trailing_whitespace`, `insert_final_newline`, and `end_of_line` are honored, with fenced block content intentionally preserved.
- If merge/conflict behavior changes, keep `live_merge.rs` tests and `main.rs` conflict-choice tests aligned; both conflict-marker and ours-wins outputs are intentional.
- eframe dependency versions must stay aligned: eframe 0.31 pairs with egui_commonmark 0.20. Upgrading one requires upgrading the other. On Linux, both `wayland` and `x11` eframe features are enabled.
- CI runs with `--locked`, so `Cargo.lock` must be committed and up to date after any dependency change.
- The release workflow triggers on tag pushes matching `v*`. Tags containing `-` (e.g. `v0.3.0-alpha.1`) are marked as pre-releases.

## Releasing a new version

1. **Bump the version** in `Cargo.toml` (`[workspace.package] version`).
2. **Regenerate the lockfile**: `cargo generate-lockfile` (needed because CI uses `--locked`).
3. **Validate locally**: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings && cargo test --workspace --locked`
4. **Commit and push**: `git add -A && git commit -m "chore: bump to vX.Y.Z" && git push origin main`
5. **Tag and push**: `git tag vX.Y.Z && git push origin vX.Y.Z`
6. **Wait for CI**: The tag push triggers both the CI workflow and the Release workflow. The release workflow builds Linux and Windows binaries, creates a versioned GitHub Release, and updates the `latest` release (used by mise). Monitor with `gh run list --limit 5`.
7. **Update mise lockfile**: The `latest` GitHub Release now points to the new tag, but `~/.config/mise/mise.lock` caches old asset IDs and checksums. Fix it:
   - Remove the stale rustdown entry from `~/.config/mise/mise.lock`.
   - Get new checksums: `curl -sL "https://github.com/teh-hippo/rustdown/releases/download/vX.Y.Z/rustdown-linux-x86_64.tar.gz.sha256"` (and the windows `.zip.sha256`).
   - Get new asset IDs: `gh api repos/teh-hippo/rustdown/releases/tags/vX.Y.Z --jq '.assets[] | "\(.id) \(.name)"'`
   - Re-add the entry to `mise.lock` with the correct checksums and asset IDs.
8. **Install in WSL**: `mise install "github:teh-hippo/rustdown" --force && rustdown --version`
9. **Install in Windows**: `powershell.exe -NoProfile -Command 'mise install "github:teh-hippo/rustdown" --force'`
