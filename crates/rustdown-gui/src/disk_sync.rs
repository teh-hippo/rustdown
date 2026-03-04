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

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::SystemTime;

    fn dummy_rev() -> DiskRevision {
        DiskRevision {
            modified: SystemTime::UNIX_EPOCH,
            len: 42,
            #[cfg(unix)]
            dev: 1,
            #[cfg(unix)]
            inode: 999,
        }
    }

    // ---- ReloadKind ----------------------------------------------------

    #[test]
    fn reload_kind_clean_variant() {
        let k = ReloadKind::Clean;
        assert!(matches!(k, ReloadKind::Clean));
    }

    #[test]
    fn reload_kind_merged_variant() {
        let k = ReloadKind::Merged;
        assert!(matches!(k, ReloadKind::Merged));
    }

    #[test]
    fn reload_kind_conflict_resolved_variant() {
        let k = ReloadKind::ConflictResolved;
        assert!(matches!(k, ReloadKind::ConflictResolved));
    }

    #[test]
    fn reload_kind_is_copy() {
        let a = ReloadKind::Merged;
        let b = a; // Copy
        assert!(matches!(a, ReloadKind::Merged));
        assert!(matches!(b, ReloadKind::Merged));
    }

    #[test]
    fn reload_kind_debug_impl() {
        let dbg = format!("{:?}", ReloadKind::Clean);
        assert_eq!(dbg, "Clean");
        assert_eq!(format!("{:?}", ReloadKind::Merged), "Merged");
        assert_eq!(
            format!("{:?}", ReloadKind::ConflictResolved),
            "ConflictResolved"
        );
    }

    // ---- DiskReloadOutcome ---------------------------------------------

    #[test]
    fn outcome_replace() {
        let o = DiskReloadOutcome::Replace {
            disk_text: "hello".into(),
            disk_rev: dummy_rev(),
        };
        match o {
            DiskReloadOutcome::Replace {
                disk_text,
                disk_rev,
            } => {
                assert_eq!(disk_text, "hello");
                assert_eq!(disk_rev.len, 42);
            }
            _ => panic!("expected Replace"),
        }
    }

    #[test]
    fn outcome_merge_clean() {
        let o = DiskReloadOutcome::MergeClean {
            merged_text: "merged".into(),
            disk_text: "disk".into(),
            disk_rev: dummy_rev(),
        };
        match o {
            DiskReloadOutcome::MergeClean {
                merged_text,
                disk_text,
                disk_rev,
            } => {
                assert_eq!(merged_text, "merged");
                assert_eq!(disk_text, "disk");
                assert_eq!(disk_rev.modified, SystemTime::UNIX_EPOCH);
            }
            _ => panic!("expected MergeClean"),
        }
    }

    #[test]
    fn outcome_merge_conflict() {
        let o = DiskReloadOutcome::MergeConflict {
            disk_text: "disk".into(),
            disk_rev: dummy_rev(),
            conflict_marked: "<<<< ours\n====\n>>>> theirs".into(),
            ours_wins: "ours".into(),
        };
        match o {
            DiskReloadOutcome::MergeConflict {
                disk_text,
                disk_rev,
                conflict_marked,
                ours_wins,
            } => {
                assert_eq!(disk_text, "disk");
                assert_eq!(disk_rev.len, 42);
                assert!(conflict_marked.contains("<<<<"));
                assert_eq!(ours_wins, "ours");
            }
            _ => panic!("expected MergeConflict"),
        }
    }

    #[test]
    fn outcome_debug_impl() {
        let o = DiskReloadOutcome::Replace {
            disk_text: String::new(),
            disk_rev: dummy_rev(),
        };
        let dbg = format!("{o:?}");
        assert!(dbg.contains("Replace"));
    }

    // ---- DiskReloadOutcome edge cases: empty strings --------------------

    #[test]
    fn outcome_replace_empty_text() {
        let o = DiskReloadOutcome::Replace {
            disk_text: String::new(),
            disk_rev: dummy_rev(),
        };
        match o {
            DiskReloadOutcome::Replace { disk_text, .. } => assert!(disk_text.is_empty()),
            _ => panic!("expected Replace"),
        }
    }

    #[test]
    fn outcome_merge_clean_empty_strings() {
        let o = DiskReloadOutcome::MergeClean {
            merged_text: String::new(),
            disk_text: String::new(),
            disk_rev: dummy_rev(),
        };
        match o {
            DiskReloadOutcome::MergeClean {
                merged_text,
                disk_text,
                ..
            } => {
                assert!(merged_text.is_empty());
                assert!(disk_text.is_empty());
            }
            _ => panic!("expected MergeClean"),
        }
    }

    #[test]
    fn outcome_merge_conflict_empty_strings() {
        let o = DiskReloadOutcome::MergeConflict {
            disk_text: String::new(),
            disk_rev: dummy_rev(),
            conflict_marked: String::new(),
            ours_wins: String::new(),
        };
        match o {
            DiskReloadOutcome::MergeConflict {
                disk_text,
                conflict_marked,
                ours_wins,
                ..
            } => {
                assert!(disk_text.is_empty());
                assert!(conflict_marked.is_empty());
                assert!(ours_wins.is_empty());
            }
            _ => panic!("expected MergeConflict"),
        }
    }

    // ---- DiskReadMessage -----------------------------------------------

    #[test]
    fn disk_read_message_ok() {
        let msg = DiskReadMessage {
            path: PathBuf::from("/tmp/test.md"),
            nonce: 7,
            edit_seq: 3,
            outcome: Ok(DiskReloadOutcome::Replace {
                disk_text: "content".into(),
                disk_rev: dummy_rev(),
            }),
        };
        assert_eq!(msg.path, PathBuf::from("/tmp/test.md"));
        assert_eq!(msg.nonce, 7);
        assert_eq!(msg.edit_seq, 3);
        assert!(msg.outcome.is_ok());
    }

    #[test]
    fn disk_read_message_err() {
        let msg = DiskReadMessage {
            path: PathBuf::from("missing.md"),
            nonce: 0,
            edit_seq: 0,
            outcome: Err(io::Error::new(io::ErrorKind::NotFound, "gone")),
        };
        match &msg.outcome {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
            Ok(_) => panic!("expected Err"),
        }
    }

    #[test]
    fn disk_read_message_debug() {
        let msg = DiskReadMessage {
            path: PathBuf::from("a.md"),
            nonce: 1,
            edit_seq: 2,
            outcome: Ok(DiskReloadOutcome::Replace {
                disk_text: String::new(),
                disk_rev: dummy_rev(),
            }),
        };
        let dbg = format!("{msg:?}");
        assert!(dbg.contains("DiskReadMessage"));
    }

    // ---- DiskConflict --------------------------------------------------

    #[test]
    fn disk_conflict_construction() {
        let c = DiskConflict {
            disk_text: "theirs".into(),
            disk_rev: dummy_rev(),
            conflict_marked: "marked".into(),
            ours_wins: "ours".into(),
        };
        assert_eq!(c.disk_text, "theirs");
        assert_eq!(c.conflict_marked, "marked");
        assert_eq!(c.ours_wins, "ours");
        assert_eq!(c.disk_rev.len, 42);
    }

    #[test]
    fn disk_conflict_clone() {
        let c = DiskConflict {
            disk_text: "a".into(),
            disk_rev: dummy_rev(),
            conflict_marked: "b".into(),
            ours_wins: "c".into(),
        };
        let c2 = c.clone();
        assert_eq!(c.disk_text, c2.disk_text);
        assert_eq!(c.conflict_marked, c2.conflict_marked);
        assert_eq!(c.ours_wins, c2.ours_wins);
        assert_eq!(c.disk_rev, c2.disk_rev);
    }

    #[test]
    fn disk_conflict_debug() {
        let c = DiskConflict {
            disk_text: String::new(),
            disk_rev: dummy_rev(),
            conflict_marked: String::new(),
            ours_wins: String::new(),
        };
        let dbg = format!("{c:?}");
        assert!(dbg.contains("DiskConflict"));
    }

    // ---- DiskSyncState -------------------------------------------------

    #[test]
    fn sync_state_default() {
        let s = DiskSyncState::default();
        assert_eq!(s.reload_nonce, 0);
        assert!(s.watcher.is_none());
        assert!(s.watch_root.is_none());
        assert!(s.watch_target_name.is_none());
        assert!(s.watch_rx.is_none());
        assert!(s.poll_at.is_none());
        assert!(s.pending_reload_at.is_none());
        assert!(!s.reload_in_flight);
        assert!(s.read_tx.is_none());
        assert!(s.read_rx.is_none());
        assert!(s.conflict.is_none());
        assert!(s.merge_sidecar_path.is_none());
    }

    #[test]
    fn sync_state_nonce_increment() {
        let mut s = DiskSyncState::default();
        s.reload_nonce += 1;
        assert_eq!(s.reload_nonce, 1);
        s.reload_nonce += 1;
        assert_eq!(s.reload_nonce, 2);
    }

    #[test]
    fn sync_state_reload_in_flight_toggle() {
        let mut s = DiskSyncState::default();
        assert!(!s.reload_in_flight);
        s.reload_in_flight = true;
        assert!(s.reload_in_flight);
        s.reload_in_flight = false;
        assert!(!s.reload_in_flight);
    }

    #[test]
    fn sync_state_conflict_set_and_clear() {
        let mut s = DiskSyncState::default();
        assert!(s.conflict.is_none());
        s.conflict = Some(DiskConflict {
            disk_text: "d".into(),
            disk_rev: dummy_rev(),
            conflict_marked: "cm".into(),
            ours_wins: "ow".into(),
        });
        assert!(s.conflict.is_some());
        s.conflict = None;
        assert!(s.conflict.is_none());
    }

    #[test]
    fn sync_state_watch_fields() {
        let s = DiskSyncState {
            watch_root: Some(PathBuf::from("/docs")),
            watch_target_name: Some(OsString::from("README.md")),
            ..Default::default()
        };
        assert_eq!(s.watch_root.as_deref(), Some(Path::new("/docs")));
        assert_eq!(
            s.watch_target_name.as_deref(),
            Some(std::ffi::OsStr::new("README.md"))
        );
    }

    #[test]
    fn sync_state_poll_and_pending_reload() {
        let mut s = DiskSyncState::default();
        let now = Instant::now();
        s.poll_at = Some(now);
        s.pending_reload_at = Some(now);
        assert!(s.poll_at.is_some());
        assert!(s.pending_reload_at.is_some());
    }

    #[test]
    fn sync_state_channel_fields() {
        let mut s = DiskSyncState::default();
        let (tx, rx) = mpsc::channel::<DiskReadMessage>();
        s.read_tx = Some(tx);
        s.read_rx = Some(rx);
        assert!(s.read_tx.is_some());
        assert!(s.read_rx.is_some());
    }

    #[test]
    fn sync_state_merge_sidecar_path() {
        let s = DiskSyncState {
            merge_sidecar_path: Some(PathBuf::from("/tmp/.rustdown-merge0.md")),
            ..Default::default()
        };
        assert_eq!(
            s.merge_sidecar_path.as_deref(),
            Some(Path::new("/tmp/.rustdown-merge0.md"))
        );
    }
}
