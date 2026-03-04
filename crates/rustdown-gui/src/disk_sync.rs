use std::{ffi::OsString, io, path::PathBuf, sync::mpsc, time::Instant};

use notify::RecommendedWatcher;

use crate::disk_io::DiskRevision;

/// How the document should be flagged after applying disk text.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ReloadKind {
    /// Clean reload from disk: buffer is clean, clear last edit timestamp.
    Clean,
    /// Merge result: buffer is dirty (contains merged edits), keep editing.
    Merged,
    /// Conflict resolution choice: buffer is dirty, clear last edit.
    ConflictResolved,
}

#[derive(Debug)]
pub(crate) enum DiskReloadOutcome {
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
pub(crate) struct DiskReadMessage {
    pub(crate) path: PathBuf,
    pub(crate) nonce: u64,
    pub(crate) edit_seq: u64,
    pub(crate) outcome: io::Result<DiskReloadOutcome>,
}

#[derive(Clone, Debug)]
pub(crate) struct DiskConflict {
    pub(crate) disk_text: String,
    pub(crate) disk_rev: DiskRevision,
    pub(crate) conflict_marked: String,
    pub(crate) ours_wins: String,
}

/// Persistent state for the disk-synchronisation subsystem.
#[derive(Default)]
pub(crate) struct DiskSyncState {
    pub(crate) reload_nonce: u64,
    pub(crate) watcher: Option<RecommendedWatcher>,
    pub(crate) watch_root: Option<PathBuf>,
    pub(crate) watch_target_name: Option<OsString>,
    pub(crate) watch_rx: Option<mpsc::Receiver<notify::Result<notify::Event>>>,
    pub(crate) poll_at: Option<Instant>,
    pub(crate) pending_reload_at: Option<Instant>,
    pub(crate) reload_in_flight: bool,
    pub(crate) read_tx: Option<mpsc::Sender<DiskReadMessage>>,
    pub(crate) read_rx: Option<mpsc::Receiver<DiskReadMessage>>,
    pub(crate) conflict: Option<DiskConflict>,
    pub(crate) merge_sidecar_path: Option<PathBuf>,
}
