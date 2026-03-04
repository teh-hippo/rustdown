#![forbid(unsafe_code)]
//! `rustdown-md` — a custom Markdown preview renderer for egui.
//!
//! Renders parsed Markdown (via `pulldown-cmark`) directly into egui widgets,
//! supporting configurable heading colours/sizes and viewport-culled scrolling.

mod parse;
mod render;
#[cfg(test)]
mod stress;
mod style;

#[cfg(test)]
mod bench;

pub use render::{MarkdownCache, MarkdownViewer};
pub use style::{
    DARK_HEADING_COLORS, HEADING_FONT_SCALES, HeadingStyle, LIGHT_HEADING_COLORS, MarkdownStyle,
};
