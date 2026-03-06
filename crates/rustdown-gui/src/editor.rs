use eframe::egui;

/// Build a `(row_y, row_start_byte)` table from galley rows.
/// Computed once per galley rebuild; enables O(log n) scroll ↔ byte lookups.
/// Uses a single-pass O(n) char scan instead of O(n²) repeated `.nth()`.
#[allow(clippy::cast_possible_truncation)] // byte offsets are < 4GB for any realistic markdown file
pub fn build_row_byte_offsets(galley: &egui::Galley, text: &str) -> Vec<(f32, u32)> {
    let mut result = Vec::with_capacity(galley.rows.len());
    let mut char_iter = text.char_indices().peekable();
    let mut chars_consumed = 0usize;
    let mut target_char = 0usize;

    for row in &galley.rows {
        while chars_consumed < target_char {
            if char_iter.next().is_none() {
                break;
            }
            chars_consumed += 1;
        }
        let byte_offset = char_iter.peek().map_or(text.len(), |&(i, _)| i);
        result.push((row.rect().min.y, byte_offset as u32));

        target_char += row.glyphs.len();
        if row.ends_with_newline {
            target_char += 1;
        }
    }
    result
}

/// Map a byte offset to the Y coordinate of the row containing it.
/// O(log n) binary search.
#[inline]
#[allow(clippy::cast_possible_truncation)] // byte offsets are < 4GB
pub fn row_byte_offset_to_y(rows: &[(f32, u32)], byte_offset: usize) -> f32 {
    if rows.is_empty() {
        return 0.0;
    }
    let offset = byte_offset as u32;
    let idx = rows
        .partition_point(|(_, b)| *b <= offset)
        .saturating_sub(1);
    rows[idx].0
}

/// Map a Y coordinate to the byte offset at the start of the row at that
/// position.  O(log n) binary search.
#[inline]
pub fn row_y_to_byte_offset(rows: &[(f32, u32)], y: f32) -> usize {
    if rows.is_empty() || !y.is_finite() {
        return 0;
    }
    let idx = rows
        .partition_point(|(row_y, _)| *row_y <= y)
        .saturating_sub(1);
    rows[idx].1 as usize
}

/// Convert a character index to a byte offset in `text`.
#[cfg(test)]
pub fn char_index_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(i, _)| i)
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn char_index_to_byte_handles_ascii_and_multibyte() {
        assert_eq!(char_index_to_byte("hello", 0), 0);
        assert_eq!(char_index_to_byte("hello", 3), 3);
        assert_eq!(char_index_to_byte("hello", 5), 5);
        // Beyond end clamps to text.len()
        assert_eq!(char_index_to_byte("hello", 100), 5);
        // Multi-byte: 'é' is 2 bytes in UTF-8
        assert_eq!(char_index_to_byte("h\u{00e9}llo", 0), 0);
        assert_eq!(char_index_to_byte("h\u{00e9}llo", 1), 1);
        assert_eq!(char_index_to_byte("h\u{00e9}llo", 2), 3);
    }

    #[test]
    fn row_byte_offset_to_y_binary_search() {
        let rows = vec![(0.0, 0u32), (20.0, 50), (40.0, 100), (60.0, 150)];
        assert_eq!(row_byte_offset_to_y(&rows, 0), 0.0);
        assert_eq!(row_byte_offset_to_y(&rows, 75), 20.0);
        assert_eq!(row_byte_offset_to_y(&rows, 100), 40.0);
        assert_eq!(row_byte_offset_to_y(&rows, 200), 60.0);
        assert_eq!(row_byte_offset_to_y(&[], 0), 0.0);
        assert_eq!(row_byte_offset_to_y(&[], 42), 0.0);
    }

    #[test]
    fn row_y_to_byte_offset_binary_search() {
        let rows = vec![(0.0, 0u32), (20.0, 50), (40.0, 100), (60.0, 150)];
        assert_eq!(row_y_to_byte_offset(&rows, 0.0), 0);
        assert_eq!(row_y_to_byte_offset(&rows, 10.0), 0);
        assert_eq!(row_y_to_byte_offset(&rows, 25.0), 50);
        assert_eq!(row_y_to_byte_offset(&rows, 40.0), 100);
        assert_eq!(row_y_to_byte_offset(&rows, 99.0), 150);
        assert_eq!(row_y_to_byte_offset(&[], 0.0), 0);
        assert_eq!(row_y_to_byte_offset(&[], 42.0), 0);
    }

    #[test]
    fn row_byte_offset_to_y_boundaries() {
        let rows = vec![(0.0, 0u32), (15.0, 30), (30.0, 60)];
        // Exact match on first row.
        assert_eq!(row_byte_offset_to_y(&rows, 0), 0.0);
        // Exact match on last row.
        assert_eq!(row_byte_offset_to_y(&rows, 60), 30.0);
        // Beyond last row byte offset clamps to last row y.
        assert_eq!(row_byte_offset_to_y(&rows, 999), 30.0);
        // Between first and second row maps to first row.
        assert_eq!(row_byte_offset_to_y(&rows, 15), 0.0);
    }

    // ── Single-row (long line / no newlines) ────────────────────────

    #[test]
    fn row_byte_offset_to_y_single_row() {
        let rows = vec![(5.0, 0u32)];
        assert_eq!(row_byte_offset_to_y(&rows, 0), 5.0);
        assert_eq!(row_byte_offset_to_y(&rows, 500), 5.0);
        assert_eq!(row_byte_offset_to_y(&rows, usize::MAX), 5.0);
    }

    #[test]
    fn row_y_to_byte_offset_single_row() {
        let rows = vec![(5.0, 0u32)];
        assert_eq!(row_y_to_byte_offset(&rows, 0.0), 0);
        assert_eq!(row_y_to_byte_offset(&rows, 5.0), 0);
        assert_eq!(row_y_to_byte_offset(&rows, 999.0), 0);
    }

    // ── Multi-byte UTF-8 aligned offsets ────────────────────────────

    #[test]
    fn row_byte_offset_to_y_multibyte_offsets() {
        // Simulates rows where boundaries fall on multi-byte char edges.
        // "café\n" is 6 bytes: c(1) a(1) f(1) é(2) \n(1)
        let rows = vec![(0.0, 0u32), (20.0, 6), (40.0, 12)];
        assert_eq!(row_byte_offset_to_y(&rows, 0), 0.0);
        assert_eq!(row_byte_offset_to_y(&rows, 3), 0.0); // inside first row
        assert_eq!(row_byte_offset_to_y(&rows, 6), 20.0); // exact second row start
        assert_eq!(row_byte_offset_to_y(&rows, 9), 20.0); // inside second row
        assert_eq!(row_byte_offset_to_y(&rows, 12), 40.0);
    }

    #[test]
    fn row_y_to_byte_offset_multibyte_offsets() {
        let rows = vec![(0.0, 0u32), (20.0, 6), (40.0, 12)];
        assert_eq!(row_y_to_byte_offset(&rows, 0.0), 0);
        assert_eq!(row_y_to_byte_offset(&rows, 20.0), 6);
        assert_eq!(row_y_to_byte_offset(&rows, 40.0), 12);
        assert_eq!(row_y_to_byte_offset(&rows, 30.0), 6); // between rows 2 and 3
    }

    // ── char_index_to_byte extended ─────────────────────────────────

    #[test]
    fn char_index_to_byte_edge_cases() {
        // Empty string
        assert_eq!(char_index_to_byte("", 0), 0);
        assert_eq!(char_index_to_byte("", 5), 0);
        // CJK + emoji: '日' is 3 bytes, '🦀' is 4 bytes
        let text = "日🦀x";
        assert_eq!(char_index_to_byte(text, 0), 0);
        assert_eq!(char_index_to_byte(text, 1), 3);
        assert_eq!(char_index_to_byte(text, 2), 7);
        assert_eq!(char_index_to_byte(text, 3), 8);
    }

    // ── Chaos tests ──────────────────────────────────────────────────

    #[test]
    fn row_y_to_byte_offset_nan_inf() {
        let rows = vec![(0.0, 0u32), (20.0, 50), (40.0, 100)];
        // NaN and Inf should not panic — return 0
        assert_eq!(row_y_to_byte_offset(&rows, f32::NAN), 0);
        assert_eq!(row_y_to_byte_offset(&rows, f32::INFINITY), 0);
        assert_eq!(row_y_to_byte_offset(&rows, f32::NEG_INFINITY), 0);
    }

    #[test]
    fn row_byte_offset_to_y_extreme_values() {
        let rows = vec![(0.0, 0u32), (20.0, 50)];
        // usize::MAX should not panic — clamps to last row
        assert_eq!(row_byte_offset_to_y(&rows, usize::MAX), 20.0);
    }

    #[test]
    fn row_y_to_byte_offset_negative() {
        let rows = vec![(0.0, 0u32), (20.0, 50)];
        // Negative y should return first row's byte offset
        assert_eq!(row_y_to_byte_offset(&rows, -100.0), 0);
    }
}
