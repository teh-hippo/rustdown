use std::{
    borrow::Cow,
    cell::Cell,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui;
use rustdown_md::MarkdownCache;

use crate::disk_io::DiskRevision;

pub struct Document {
    pub path: Option<PathBuf>,
    pub image_uri_scheme: String,
    pub text: Arc<String>,
    pub base_text: Arc<String>,
    pub disk_rev: Option<DiskRevision>,
    pub stats: DocumentStats,
    pub stats_dirty: bool,
    pub preview_dirty: bool,
    pub dirty: bool,
    pub preview_cache: MarkdownCache,
    pub last_edit_at: Option<Instant>,
    pub edit_seq: u64,
    pub editor_galley_cache: Option<EditorGalleyCache>,
}

impl Default for Document {
    fn default() -> Self {
        let text = Arc::new(String::new());
        Self {
            path: None,
            image_uri_scheme: crate::default_image_uri_scheme(None),
            text: text.clone(),
            base_text: text,
            disk_rev: None,
            stats: DocumentStats::default(),
            stats_dirty: false,
            preview_dirty: false,
            dirty: false,
            preview_cache: MarkdownCache::default(),
            last_edit_at: None,
            edit_seq: 0,
            editor_galley_cache: None,
        }
    }
}

impl Document {
    #[must_use]
    pub fn debounce_remaining(&self, debounce: Duration) -> Option<Duration> {
        let last = self.last_edit_at?;
        let since = last.elapsed();
        debounce.checked_sub(since)
    }

    #[must_use]
    pub fn title(&self) -> Cow<'_, str> {
        self.path
            .as_ref()
            .and_then(|path| path.file_name())
            .map_or_else(|| Cow::Borrowed("Untitled"), |name| name.to_string_lossy())
    }

    #[must_use]
    pub fn path_label(&self) -> Cow<'_, str> {
        self.path
            .as_ref()
            .map_or_else(|| Cow::Borrowed("Unsaved"), |path| path.to_string_lossy())
    }

    #[must_use]
    pub const fn stats(&self) -> DocumentStats {
        self.stats
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentStats {
    pub lines: usize,
}

impl DocumentStats {
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        let lines = if text.is_empty() {
            1
        } else {
            1 + bytecount_newlines(text)
        };
        Self { lines }
    }
}

impl Default for DocumentStats {
    fn default() -> Self {
        Self { lines: 1 }
    }
}

pub fn bytecount_newlines(text: &str) -> usize {
    memchr::memchr_iter(b'\n', text.as_bytes()).count()
}

#[derive(Clone)]
pub struct EditorGalleyCache {
    pub content_seq: u64,
    pub content_color_mode: bool,
    pub wrap_width_bits: u32,
    pub zoom_factor_bits: u32,
    pub layout_job: egui::text::LayoutJob,
    pub galley: Arc<egui::Galley>,
    pub row_byte_offsets: Vec<(f32, u32)>,
}

pub struct TrackedTextBuffer<'a, 'b> {
    pub text: &'a mut Arc<String>,
    pub seq: &'b Cell<u64>,
}

impl egui::TextBuffer for TrackedTextBuffer<'_, '_> {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        self.text.as_str()
    }

    fn insert_text(&mut self, text: &str, char_index: usize) -> usize {
        let inserted = egui::TextBuffer::insert_text(Arc::make_mut(self.text), text, char_index);
        if inserted != 0 {
            self.seq.set(self.seq.get().wrapping_add(1));
        }
        inserted
    }

    fn delete_char_range(&mut self, char_range: std::ops::Range<usize>) {
        if char_range.start < char_range.end {
            self.seq.set(self.seq.get().wrapping_add(1));
        }
        egui::TextBuffer::delete_char_range(Arc::make_mut(self.text), char_range);
    }

    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<TrackedTextBuffer<'static, 'static>>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::TextBuffer as _;

    // ── DocumentStats::from_text ──────────────────────────────────────

    #[test]
    fn stats_empty_string_is_one_line() {
        assert_eq!(DocumentStats::from_text("").lines, 1);
    }

    #[test]
    fn stats_no_newline_is_one_line() {
        assert_eq!(DocumentStats::from_text("hello").lines, 1);
    }

    #[test]
    fn stats_single_newline_is_two_lines() {
        assert_eq!(DocumentStats::from_text("a\nb").lines, 2);
    }

    #[test]
    fn stats_trailing_newline() {
        assert_eq!(DocumentStats::from_text("a\n").lines, 2);
    }

    #[test]
    fn stats_multiple_newlines() {
        assert_eq!(DocumentStats::from_text("a\nb\nc\n").lines, 4);
    }

    #[test]
    fn stats_crlf_counts_one_per_pair() {
        assert_eq!(DocumentStats::from_text("a\r\nb\r\n").lines, 3);
    }

    #[test]
    fn stats_bare_cr_not_counted() {
        // Only \n is counted; bare \r is not a line break.
        assert_eq!(DocumentStats::from_text("a\rb").lines, 1);
    }

    #[test]
    fn stats_mixed_newlines() {
        assert_eq!(DocumentStats::from_text("a\nb\r\nc\rd").lines, 3);
    }

    #[test]
    fn stats_cjk_text() {
        assert_eq!(DocumentStats::from_text("你好\n世界").lines, 2);
    }

    #[test]
    fn stats_only_newlines() {
        assert_eq!(DocumentStats::from_text("\n\n\n").lines, 4);
    }

    #[test]
    fn stats_default_matches_empty() {
        assert_eq!(DocumentStats::default(), DocumentStats::from_text(""));
    }

    // ── bytecount_newlines ────────────────────────────────────────────

    #[test]
    fn bytecount_empty() {
        assert_eq!(bytecount_newlines(""), 0);
    }

    #[test]
    fn bytecount_no_newlines() {
        assert_eq!(bytecount_newlines("hello world"), 0);
    }

    #[test]
    fn bytecount_several() {
        assert_eq!(bytecount_newlines("a\nb\nc\n"), 3);
    }

    // ── TrackedTextBuffer ─────────────────────────────────────────────

    #[test]
    fn tracked_buffer_insert_bumps_seq() {
        let mut text = Arc::new(String::from("hello"));
        let seq = Cell::new(0_u64);
        let mut buf = TrackedTextBuffer {
            text: &mut text,
            seq: &seq,
        };
        let inserted = egui::TextBuffer::insert_text(&mut buf, " world", 5);
        assert_eq!(inserted, 6);
        assert_eq!(seq.get(), 1);
        assert_eq!(buf.as_str(), "hello world");
    }

    #[test]
    fn tracked_buffer_insert_empty_does_not_bump_seq() {
        let mut text = Arc::new(String::from("hello"));
        let seq = Cell::new(0_u64);
        let mut buf = TrackedTextBuffer {
            text: &mut text,
            seq: &seq,
        };
        let inserted = egui::TextBuffer::insert_text(&mut buf, "", 0);
        assert_eq!(inserted, 0);
        assert_eq!(seq.get(), 0);
    }

    #[test]
    fn tracked_buffer_delete_bumps_seq() {
        let mut text = Arc::new(String::from("hello"));
        let seq = Cell::new(0_u64);
        let mut buf = TrackedTextBuffer {
            text: &mut text,
            seq: &seq,
        };
        egui::TextBuffer::delete_char_range(&mut buf, 0..3);
        assert_eq!(seq.get(), 1);
        assert_eq!(buf.as_str(), "lo");
    }

    #[test]
    fn tracked_buffer_delete_empty_range_does_not_bump_seq() {
        let mut text = Arc::new(String::from("hello"));
        let seq = Cell::new(0_u64);
        let mut buf = TrackedTextBuffer {
            text: &mut text,
            seq: &seq,
        };
        egui::TextBuffer::delete_char_range(&mut buf, 2..2);
        assert_eq!(seq.get(), 0);
        assert_eq!(buf.as_str(), "hello");
    }

    #[test]
    fn tracked_buffer_is_mutable() {
        let mut text = Arc::new(String::new());
        let seq = Cell::new(0_u64);
        let buf = TrackedTextBuffer {
            text: &mut text,
            seq: &seq,
        };
        assert!(egui::TextBuffer::is_mutable(&buf));
    }

    #[test]
    fn tracked_buffer_seq_wraps_at_max() {
        let mut text = Arc::new(String::from("x"));
        let seq = Cell::new(u64::MAX);
        let mut buf = TrackedTextBuffer {
            text: &mut text,
            seq: &seq,
        };
        let _ = egui::TextBuffer::insert_text(&mut buf, "y", 0);
        assert_eq!(seq.get(), 0);
    }

    // ── Document defaults & helpers ───────────────────────────────────

    #[test]
    fn document_default_is_clean() {
        let doc = Document::default();
        assert!(!doc.dirty);
        assert!(!doc.stats_dirty);
        assert!(!doc.preview_dirty);
        assert_eq!(doc.edit_seq, 0);
        assert!(doc.last_edit_at.is_none());
        assert!(doc.path.is_none());
        assert!(Arc::ptr_eq(&doc.text, &doc.base_text));
    }

    #[test]
    fn document_title_without_path() {
        let doc = Document::default();
        assert_eq!(doc.title(), "Untitled");
    }

    #[test]
    fn document_title_with_path() {
        let doc = Document {
            path: Some(PathBuf::from("/tmp/readme.md")),
            ..Default::default()
        };
        assert_eq!(doc.title(), "readme.md");
    }

    #[test]
    fn document_path_label_without_path() {
        let doc = Document::default();
        assert_eq!(doc.path_label(), "Unsaved");
    }

    #[test]
    fn document_path_label_with_path() {
        let doc = Document {
            path: Some(PathBuf::from("/tmp/readme.md")),
            ..Default::default()
        };
        assert_eq!(doc.path_label(), "/tmp/readme.md");
    }

    #[test]
    fn debounce_remaining_none_when_no_edit() {
        let doc = Document::default();
        assert!(doc.debounce_remaining(Duration::from_millis(500)).is_none());
    }

    #[test]
    fn debounce_remaining_some_after_recent_edit() {
        let doc = Document {
            last_edit_at: Some(Instant::now()),
            ..Default::default()
        };
        let remaining = doc.debounce_remaining(Duration::from_secs(10));
        assert!(remaining.is_some());
    }

    #[test]
    fn debounce_remaining_none_after_expired() {
        let doc = Document {
            last_edit_at: Instant::now().checked_sub(Duration::from_secs(5)),
            ..Default::default()
        };
        let remaining = doc.debounce_remaining(Duration::from_millis(100));
        assert!(remaining.is_none());
    }
}
