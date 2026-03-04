#![forbid(unsafe_code)]
//! `rustdown-md` — a custom Markdown preview renderer for egui.
//!
//! Renders parsed Markdown (via `pulldown-cmark`) directly into egui widgets,
//! supporting configurable heading colours/sizes and viewport-culled scrolling.

mod parse;
mod render;
mod style;

pub use render::{MarkdownCache, MarkdownViewer};
pub use style::MarkdownStyle;
