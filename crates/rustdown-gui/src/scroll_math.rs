use eframe::egui;

use crate::nav::outline::HeadingEntry;

/// Compute the scroll-area [`egui::Id`] used by the code editor.
pub(crate) fn editor_scroll_id() -> egui::Id {
    egui::Id::new("editor").with("editor_scroll")
}

/// Convert `byte_offset` to an estimated preview scroll-y value using
/// piecewise-linear interpolation between heading waypoints.
/// Returns `0.0` when the outline is empty or all headings are at offset 0.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn preview_byte_to_scroll_y(
    outline: &[HeadingEntry],
    byte_offset: usize,
    total_height: f32,
) -> f32 {
    if total_height <= 0.0 {
        return 0.0;
    }
    let max_byte = match outline.last() {
        Some(h) if h.byte_offset > 0 => h.byte_offset,
        _ => return 0.0,
    };

    if outline.len() < 2 {
        return (byte_offset as f32 / max_byte as f32 * total_height).clamp(0.0, total_height);
    }

    let n = outline.len();
    let after_idx = outline.partition_point(|h| h.byte_offset <= byte_offset);

    if after_idx == 0 {
        return 0.0;
    }

    if after_idx >= n {
        let last = &outline[n - 1];
        let last_y = total_height * ((n - 1) as f32 / n as f32);
        let remaining_bytes = max_byte.saturating_sub(last.byte_offset) as f32;
        if remaining_bytes <= 0.0 {
            return last_y;
        }
        let frac = (byte_offset - last.byte_offset) as f32 / remaining_bytes;
        return last_y + frac * (total_height - last_y);
    }

    let before = &outline[after_idx - 1];
    let after = &outline[after_idx];
    let before_y = total_height * ((after_idx - 1) as f32 / n as f32);
    let after_y = total_height * (after_idx as f32 / n as f32);

    let byte_range = (after.byte_offset - before.byte_offset) as f32;
    if byte_range <= 0.0 {
        return before_y;
    }
    let frac = (byte_offset - before.byte_offset) as f32 / byte_range;
    frac.mul_add(after_y - before_y, before_y)
}

/// Convert a preview scroll-y value to an estimated byte offset using
/// the inverse piecewise-linear mapping.
/// Returns `0` when the outline is empty.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub(crate) fn preview_scroll_y_to_byte(
    outline: &[HeadingEntry],
    scroll_y: f32,
    total_height: f32,
) -> usize {
    if total_height <= 0.0 {
        return 0;
    }
    let max_byte = match outline.last() {
        Some(h) if h.byte_offset > 0 => h.byte_offset,
        _ => return 0,
    };

    if outline.len() < 2 {
        return (scroll_y / total_height * max_byte as f32).clamp(0.0, max_byte as f32) as usize;
    }

    let n = outline.len();
    let scroll_y = scroll_y.clamp(0.0, total_height);
    let slot = scroll_y / total_height * n as f32;
    let before_idx = (slot as usize).min(n - 1);

    if before_idx >= n - 1 {
        let last = &outline[n - 1];
        let last_y = total_height * ((n - 1) as f32 / n as f32);
        let band_height = total_height - last_y;
        if band_height <= 0.0 {
            return last.byte_offset;
        }
        let frac = ((scroll_y - last_y) / band_height).clamp(0.0, 1.0);
        let remaining_bytes = max_byte.saturating_sub(last.byte_offset);
        return last.byte_offset + (frac * remaining_bytes as f32) as usize;
    }

    let before = &outline[before_idx];
    let after = &outline[before_idx + 1];
    let before_y = total_height * (before_idx as f32 / n as f32);
    let after_y = total_height * ((before_idx + 1) as f32 / n as f32);
    let band_height = after_y - before_y;
    if band_height <= 0.0 {
        return before.byte_offset;
    }
    let frac = ((scroll_y - before_y) / band_height).clamp(0.0, 1.0);
    let byte_range = after.byte_offset - before.byte_offset;
    before.byte_offset + (frac * byte_range as f32) as usize
}
