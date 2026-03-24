//! Markdown preview renderer for egui.
//!
//! Renders parsed Markdown (via `pulldown-cmark`) directly into egui widgets,
//! supporting configurable heading colours/sizes and viewport-culled scrolling.

mod parse;
pub(crate) mod render;
#[cfg(test)]
mod stress;
mod style;

#[cfg(test)]
mod bench;

#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::float_cmp
)]
mod snapshot_tests;

pub(crate) use parse::heading_level_to_u8;
pub(crate) use render::{MarkdownCache, MarkdownViewer, bytecount_newlines};
pub(crate) use style::{
    DARK_HEADING_COLORS, HEADING_FONT_SCALES, LIGHT_HEADING_COLORS, MarkdownStyle,
};
