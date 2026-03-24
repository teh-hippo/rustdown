// Visual rendering integration tests for the Markdown preview renderer.
//
// Each test renders markdown headlessly through egui, then verifies
// structural properties (block types, heights, positions, spans) that
// correspond to correct visual output.
//
// ## Reference images
//
// Each test section is paired with a `.md` file in `test-assets/visual-refs/`.
// To generate reference screenshots:
//
//   1. `cargo run -p rustdown`
//   2. Open the `.md` file (Ctrl+O or drag-and-drop)
//   3. Switch to Preview mode (Ctrl+Enter)
//   4. Take a screenshot and save alongside the `.md` file
//
// Reference images serve as visual documentation for humans and agents.
// The automated tests verify the underlying structural correctness.
//
// ## Running
//
//   cargo test -p rustdown-md --test snapshot_tests
