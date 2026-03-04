use std::{ffi::OsString, io, path::PathBuf, sync::mpsc, time::Instant};

use notify::RecommendedWatcher;

use crate::disk_io::DiskRevision;

/// How the document should be flagged after applying disk text.
#[derive(Clone, Copy, Debug)]
pub enum ReloadKind {
    /// Clean reload from disk: buffer is clean, clear last edit timestamp.
    Clean,
    /// Merge result: buffer is dirty (contains merged edits), keep editing.
    Merged,
    /// Conflict resolution choice: buffer is dirty, clear last edit.
    ConflictResolved,
}

#[derive(Debug)]
pub enum DiskReloadOutcome {
    Replace {
        disk_text: String,
        disk_rev: DiskRevision,
    },
    MergeClean {
        merged_text: String,
        disk_text: String,
        disk_rev: DiskRevision,
    },
    MergeConflict {
        disk_text: String,
        disk_rev: DiskRevision,
        conflict_marked: String,
        ours_wins: String,
    },
}

#[derive(Debug)]
pub struct DiskReadMessage {
    pub path: PathBuf,
    pub nonce: u64,
    pub edit_seq: u64,
    pub outcome: io::Result<DiskReloadOutcome>,
}

#[derive(Clone, Debug)]
pub struct DiskConflict {
    pub disk_text: String,
    pub disk_rev: DiskRevision,
    pub conflict_marked: String,
    pub ours_wins: String,
}

/// Persistent state for the disk-synchronisation subsystem.
#[derive(Default)]
pub struct DiskSyncState {
    pub reload_nonce: u64,
    pub watcher: Option<RecommendedWatcher>,
    pub watch_root: Option<PathBuf>,
    pub watch_target_name: Option<OsString>,
    pub watch_rx: Option<mpsc::Receiver<notify::Result<notify::Event>>>,
    pub poll_at: Option<Instant>,
    pub pending_reload_at: Option<Instant>,
    pub reload_in_flight: bool,
    pub read_tx: Option<mpsc::Sender<DiskReadMessage>>,
    pub read_rx: Option<mpsc::Receiver<DiskReadMessage>>,
    pub conflict: Option<DiskConflict>,
    pub merge_sidecar_path: Option<PathBuf>,
}
