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
