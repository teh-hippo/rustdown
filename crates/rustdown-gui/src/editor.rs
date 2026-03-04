use eframe::egui;

/// Build a `(row_y, row_start_byte)` table from galley rows.
/// Computed once per galley rebuild; enables O(log n) scroll ↔ byte lookups.
/// Uses a single-pass O(n) char scan instead of O(n²) repeated `.nth()`.
pub(crate) fn build_row_byte_offsets(galley: &egui::Galley, text: &str) -> Vec<(f32, u32)> {
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

pub(crate) fn row_byte_offset_to_y(rows: &[(f32, u32)], byte_offset: usize) -> f32 {
    if rows.is_empty() {
        return 0.0;
    }
    let offset = byte_offset as u32;
    let idx = rows
        .partition_point(|(_, b)| *b <= offset)
        .saturating_sub(1);
    rows[idx].0
}

pub(crate) fn row_y_to_byte_offset(rows: &[(f32, u32)], y: f32) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let idx = rows
        .partition_point(|(row_y, _)| *row_y <= y)
        .saturating_sub(1);
    rows[idx].1 as usize
}

/// Convert a character index to a byte offset in `text`.
#[cfg(test)]
pub(crate) fn char_index_to_byte(text: &str, char_index: usize) -> usize {
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
        assert_eq!(row_byte_offset_to_y(&[], 50), 0.0);
    }

    #[test]
    fn row_y_to_byte_offset_binary_search() {
        let rows = vec![(0.0, 0u32), (20.0, 50), (40.0, 100), (60.0, 150)];
        assert_eq!(row_y_to_byte_offset(&rows, 0.0), 0);
        assert_eq!(row_y_to_byte_offset(&rows, 10.0), 0);
        assert_eq!(row_y_to_byte_offset(&rows, 25.0), 50);
        assert_eq!(row_y_to_byte_offset(&rows, 40.0), 100);
        assert_eq!(row_y_to_byte_offset(&rows, 99.0), 150);
        assert_eq!(row_y_to_byte_offset(&[], 50.0), 0);
    }
}
