#![allow(clippy::cast_precision_loss)] // UI math — list depths and sizes are small

//! Shared layout metrics for render-time UI and height estimation.

const MIN_CONTENT_WIDTH: f32 = 40.0;
const BLOCK_SPACING_EM: f32 = 0.4;
const LIST_SPACING_EM: f32 = 0.2;
const LIST_ITEM_OVERHEAD_EM: f32 = 0.3;
const HEADING_TOP_SPACING_EM: f32 = 0.3;
const HEADING_BOTTOM_SPACING_EM: f32 = 0.15;
const HEADING_MIN_BOTTOM_BODY_EM: f32 = 0.3;
const CODE_FONT_SCALE: f32 = 0.9;
const CODE_BLOCK_INNER_MARGIN_PX: i8 = 6;
const CODE_BLOCK_HORIZONTAL_PADDING_PX: f32 = 12.0;
const LIST_INDENT_PX: f32 = 16.0;
const UNORDERED_BULLET_COLUMN_EM: f32 = 1.5;
const UNORDERED_GAP_PX: f32 = 2.0;
const ORDERED_GAP_PX: f32 = 4.0;
const BLOCKQUOTE_BAR_WIDTH_PX: f32 = 3.0;
const BLOCKQUOTE_BAR_MARGIN_EM: f32 = 0.4;
const BLOCKQUOTE_CONTENT_MARGIN_EM: f32 = 0.6;
const TABLE_AVG_CHAR_EM: f32 = 0.55;
const TABLE_MIN_COL_EM: f32 = 2.5;
const TABLE_MIN_COL_PX: f32 = 36.0;
const TABLE_CONTENT_PADDING_PX: f32 = 12.0;
const TABLE_SINGLE_COLUMN_MAX_FRACTION: f32 = 0.6;
const TABLE_ROW_HEIGHT_EM: f32 = 1.4;
const TABLE_ROW_SPACING_PX: f32 = 3.0;
const TABLE_SCROLLBAR_HEIGHT_PX: f32 = 14.0;
const THEMATIC_BREAK_HEIGHT_EM: f32 = 0.8;
const IMAGE_FALLBACK_HEIGHT_EM: f32 = 8.0;
const IMAGE_MAX_HEIGHT_FRACTION: f32 = 0.75;

/// Shared render/estimation measurements derived from the body font size.
#[derive(Clone, Copy, Debug)]
pub(super) struct RenderMetrics {
    body_size: f32,
    list_depth: usize,
}

impl RenderMetrics {
    pub(super) const fn new(body_size: f32) -> Self {
        Self {
            body_size,
            list_depth: 0,
        }
    }

    pub(super) const fn with_list_depth(self, list_depth: usize) -> Self {
        Self { list_depth, ..self }
    }

    pub(super) const fn nested_list(self) -> Self {
        self.with_list_depth(self.list_depth + 1)
    }

    pub(super) const fn body_size(self) -> f32 {
        self.body_size
    }

    pub(super) fn paragraph_spacing(self) -> f32 {
        self.body_size * BLOCK_SPACING_EM
    }

    pub(super) fn list_spacing(self) -> f32 {
        self.body_size * LIST_SPACING_EM
    }

    pub(super) fn list_item_overhead(self) -> f32 {
        self.body_size * LIST_ITEM_OVERHEAD_EM
    }

    pub(super) fn heading_top_spacing(heading_size: f32) -> f32 {
        heading_size * HEADING_TOP_SPACING_EM
    }

    pub(super) fn heading_bottom_spacing(self, heading_size: f32) -> f32 {
        (heading_size * HEADING_BOTTOM_SPACING_EM).max(self.body_size * HEADING_MIN_BOTTOM_BODY_EM)
    }

    pub(super) fn code_font_size(self) -> f32 {
        self.body_size * CODE_FONT_SCALE
    }

    pub(super) const fn code_block_inner_margin() -> i8 {
        CODE_BLOCK_INNER_MARGIN_PX
    }

    pub(super) const fn code_block_horizontal_padding() -> f32 {
        CODE_BLOCK_HORIZONTAL_PADDING_PX
    }

    pub(super) const fn bullet_text(self) -> &'static str {
        match self.list_depth {
            0 => "\u{2022}",
            1 => "\u{25E6}",
            _ => "\u{25AA}",
        }
    }

    pub(super) fn list_indent_px(self) -> f32 {
        LIST_INDENT_PX * self.list_depth as f32
    }

    pub(super) fn unordered_bullet_column_width(self) -> f32 {
        self.body_size * UNORDERED_BULLET_COLUMN_EM
    }

    pub(super) const fn unordered_gap_px() -> f32 {
        UNORDERED_GAP_PX
    }

    pub(super) const fn ordered_gap_px() -> f32 {
        ORDERED_GAP_PX
    }

    pub(super) const fn blockquote_bar_width() -> f32 {
        BLOCKQUOTE_BAR_WIDTH_PX
    }

    pub(super) fn blockquote_bar_margin(self) -> f32 {
        self.body_size * BLOCKQUOTE_BAR_MARGIN_EM
    }

    pub(super) fn blockquote_content_margin(self) -> f32 {
        self.body_size * BLOCKQUOTE_CONTENT_MARGIN_EM
    }

    pub(super) fn blockquote_reserved_width(self) -> f32 {
        self.blockquote_bar_margin()
            + Self::blockquote_bar_width()
            + self.blockquote_content_margin()
    }

    pub(super) fn blockquote_content_width(self, wrap_width: f32) -> f32 {
        (wrap_width - self.blockquote_reserved_width()).max(MIN_CONTENT_WIDTH)
    }

    pub(super) fn table_avg_char_width(self) -> f32 {
        self.body_size * TABLE_AVG_CHAR_EM
    }

    pub(super) fn table_min_col_width(self) -> f32 {
        (self.body_size * TABLE_MIN_COL_EM).max(TABLE_MIN_COL_PX)
    }

    pub(super) const fn table_content_padding() -> f32 {
        TABLE_CONTENT_PADDING_PX
    }

    pub(super) fn table_single_column_cap(usable: f32, content_est: f32) -> f32 {
        (usable * TABLE_SINGLE_COLUMN_MAX_FRACTION).max(content_est.min(usable))
    }

    pub(super) fn table_base_row_height(self) -> f32 {
        self.body_size * TABLE_ROW_HEIGHT_EM
    }

    pub(super) const fn table_row_spacing() -> f32 {
        TABLE_ROW_SPACING_PX
    }

    pub(super) const fn table_scrollbar_height() -> f32 {
        TABLE_SCROLLBAR_HEIGHT_PX
    }

    pub(super) fn thematic_break_height(self) -> f32 {
        self.body_size * THEMATIC_BREAK_HEIGHT_EM
    }

    pub(super) fn image_fallback_height(self) -> f32 {
        self.body_size * IMAGE_FALLBACK_HEIGHT_EM
    }

    pub(super) fn image_max_height(wrap_width: f32) -> f32 {
        wrap_width * IMAGE_MAX_HEIGHT_FRACTION
    }
}

/// Shared render-time state for nested block traversal.
#[derive(Clone, Copy, Debug)]
pub(super) struct RenderContext {
    indent: usize,
    metrics: RenderMetrics,
}

impl RenderContext {
    pub(super) fn root(ui: &egui::Ui) -> Self {
        Self {
            indent: 0,
            metrics: RenderMetrics::new(ui.text_style_height(&egui::TextStyle::Body)),
        }
    }

    pub(super) const fn indent(self) -> usize {
        self.indent
    }

    pub(super) const fn metrics(self) -> RenderMetrics {
        self.metrics
    }

    pub(super) const fn nested_list(self) -> Self {
        Self {
            indent: self.indent + 1,
            metrics: self.metrics.nested_list(),
        }
    }

    pub(super) const fn quote_inner(self) -> Self {
        Self {
            indent: self.indent + 1,
            metrics: self.metrics.with_list_depth(0),
        }
    }
}
