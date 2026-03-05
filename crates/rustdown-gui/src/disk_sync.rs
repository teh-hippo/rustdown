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
mod tests {
    use super::*;
    use std::{io, path::Path, sync::mpsc, time::SystemTime};

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
    fn sync_state_defaults_are_inactive() {
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
    fn channel_round_trip_replace() {
        let (tx, rx) = mpsc::channel();
        let msg = DiskReadMessage {
            path: PathBuf::from("/tmp/test.md"),
            nonce: 42,
            edit_seq: 7,
            outcome: Ok(DiskReloadOutcome::Replace {
                disk_text: "new content".into(),
                disk_rev: dummy_rev(),
            }),
        };
        tx.send(msg).ok();
        let received = rx.try_recv();
        assert!(received.is_ok());
        let m = received.unwrap_or_else(|_| unreachable!());
        assert_eq!(m.nonce, 42);
        assert_eq!(m.edit_seq, 7);
        assert!(m.outcome.is_ok());
    }

    #[test]
    fn channel_round_trip_merge_clean() {
        let (tx, rx) = mpsc::channel();
        let msg = DiskReadMessage {
            path: PathBuf::from("/tmp/test.md"),
            nonce: 5,
            edit_seq: 3,
            outcome: Ok(DiskReloadOutcome::MergeClean {
                merged_text: "merged".into(),
                disk_text: "disk".into(),
                disk_rev: dummy_rev(),
            }),
        };
        tx.send(msg).ok();
        let received = rx.try_recv();
        assert!(received.is_ok());
    }

    #[test]
    fn channel_round_trip_merge_conflict() {
        let (tx, rx) = mpsc::channel();
        let msg = DiskReadMessage {
            path: PathBuf::from("/tmp/test.md"),
            nonce: 10,
            edit_seq: 8,
            outcome: Ok(DiskReloadOutcome::MergeConflict {
                disk_text: "disk".into(),
                disk_rev: dummy_rev(),
                conflict_marked: "<<<<<<< ours\nX\n=======\nY\n>>>>>>> theirs\n".into(),
                ours_wins: "X\n".into(),
            }),
        };
        tx.send(msg).ok();
        let received = rx.try_recv();
        assert!(received.is_ok());
    }

    #[test]
    fn channel_round_trip_error() {
        let (tx, rx) = mpsc::channel();
        let msg = DiskReadMessage {
            path: PathBuf::from("/tmp/test.md"),
            nonce: 1,
            edit_seq: 0,
            outcome: Err(io::Error::new(io::ErrorKind::NotFound, "gone")),
        };
        tx.send(msg).ok();
        let received = rx.try_recv();
        assert!(received.is_ok());
        let m = received.unwrap_or_else(|_| unreachable!());
        assert!(m.outcome.is_err());
    }

    #[test]
    fn stale_nonce_detection_pattern() {
        // Simulates the nonce-matching logic from drain_disk_read_results:
        // messages with an old nonce should be skippable.
        let mut s = DiskSyncState::default();
        s.reload_nonce = s.reload_nonce.wrapping_add(1);
        let stale_nonce = s.reload_nonce;
        s.reload_nonce = s.reload_nonce.wrapping_add(1);
        let current_nonce = s.reload_nonce;

        assert_ne!(stale_nonce, current_nonce);
        // A message with stale_nonce should be skipped.
        assert_ne!(stale_nonce, s.reload_nonce);
        // A message with current_nonce should be accepted.
        assert_eq!(current_nonce, s.reload_nonce);
    }

    #[test]
    fn pending_reload_debounce_pattern() {
        // Simulates the debounce pattern: multiple events coalesce into one
        // pending_reload_at, and only the first one is kept.
        let mut s = DiskSyncState::default();
        assert!(s.pending_reload_at.is_none());

        let t1 = Instant::now() + std::time::Duration::from_millis(200);
        s.pending_reload_at = Some(t1);
        assert_eq!(s.pending_reload_at, Some(t1));

        // Second event arrives — should NOT overwrite the earlier debounce.
        let t2 = Instant::now() + std::time::Duration::from_millis(300);
        if s.pending_reload_at.is_none() {
            s.pending_reload_at = Some(t2);
        }
        // Original timestamp preserved.
        assert_eq!(s.pending_reload_at, Some(t1));
    }

    // ── ReloadKind ──────────────────────────────────────────────────

    #[test]
    fn reload_kind_debug_representations() {
        let clean = format!("{:?}", ReloadKind::Clean);
        let merged = format!("{:?}", ReloadKind::Merged);
        let resolved = format!("{:?}", ReloadKind::ConflictResolved);
        assert!(clean.contains("Clean"));
        assert!(merged.contains("Merged"));
        assert!(resolved.contains("ConflictResolved"));
    }

    #[test]
    fn reload_kind_copy_semantics() {
        let a = ReloadKind::Clean;
        let b = a; // Copy
        assert!(matches!(a, ReloadKind::Clean));
        assert!(matches!(b, ReloadKind::Clean));
    }

    // ── DiskReloadOutcome ───────────────────────────────────────────

    #[test]
    fn disk_reload_outcome_replace_fields() {
        let rev = dummy_rev();
        let outcome = DiskReloadOutcome::Replace {
            disk_text: "hello".into(),
            disk_rev: rev,
        };
        if let DiskReloadOutcome::Replace {
            disk_text,
            disk_rev,
        } = outcome
        {
            assert_eq!(disk_text, "hello");
            assert_eq!(disk_rev.len, 42);
        } else {
            unreachable!();
        }
    }

    #[test]
    fn disk_reload_outcome_merge_clean_fields() {
        let outcome = DiskReloadOutcome::MergeClean {
            merged_text: "merged".into(),
            disk_text: "disk".into(),
            disk_rev: dummy_rev(),
        };
        if let DiskReloadOutcome::MergeClean {
            merged_text,
            disk_text,
            ..
        } = outcome
        {
            assert_eq!(merged_text, "merged");
            assert_eq!(disk_text, "disk");
        } else {
            unreachable!();
        }
    }

    #[test]
    fn disk_reload_outcome_merge_conflict_fields() {
        let outcome = DiskReloadOutcome::MergeConflict {
            disk_text: "theirs".into(),
            disk_rev: dummy_rev(),
            conflict_marked: "<<<".into(),
            ours_wins: "ours".into(),
        };
        if let DiskReloadOutcome::MergeConflict {
            conflict_marked,
            ours_wins,
            ..
        } = outcome
        {
            assert_eq!(conflict_marked, "<<<");
            assert_eq!(ours_wins, "ours");
        } else {
            unreachable!();
        }
    }

    // ── DiskConflict ────────────────────────────────────────────────

    #[test]
    fn disk_conflict_clone() {
        let c = DiskConflict {
            disk_text: "d".into(),
            disk_rev: dummy_rev(),
            conflict_marked: "cm".into(),
            ours_wins: "ow".into(),
        };
        let c2 = c.clone();
        assert_eq!(c2.disk_text, "d");
        assert_eq!(c2.conflict_marked, "cm");
        assert_eq!(c2.ours_wins, "ow");
        assert_eq!(c2.disk_rev.len, c.disk_rev.len);
    }

    #[test]
    fn disk_conflict_debug_output() {
        let c = DiskConflict {
            disk_text: "d".into(),
            disk_rev: dummy_rev(),
            conflict_marked: "cm".into(),
            ours_wins: "ow".into(),
        };
        let dbg = format!("{c:?}");
        assert!(dbg.contains("DiskConflict"));
    }

    // ── DiskSyncState extended ──────────────────────────────────────

    #[test]
    fn sync_state_watch_target_name() {
        let mut s = DiskSyncState::default();
        assert!(s.watch_target_name.is_none());
        s.watch_target_name = Some(OsString::from("README.md"));
        assert_eq!(
            s.watch_target_name.as_deref(),
            Some(std::ffi::OsStr::new("README.md"))
        );
    }

    #[test]
    fn sync_state_watch_root() {
        let mut s = DiskSyncState::default();
        assert!(s.watch_root.is_none());
        s.watch_root = Some(PathBuf::from("/tmp"));
        assert_eq!(s.watch_root.as_deref(), Some(Path::new("/tmp")));
    }

    #[test]
    fn sync_state_merge_sidecar_path() {
        let mut s = DiskSyncState::default();
        assert!(s.merge_sidecar_path.is_none());
        s.merge_sidecar_path = Some(PathBuf::from("/tmp/.rustdown-merge0.md"));
        assert!(s.merge_sidecar_path.is_some());
    }

    #[test]
    fn sync_state_channel_pair() {
        let mut s = DiskSyncState::default();
        let (tx, rx) = mpsc::channel::<DiskReadMessage>();
        s.read_tx = Some(tx);
        s.read_rx = Some(rx);
        assert!(s.read_tx.is_some());
        assert!(s.read_rx.is_some());
    }

    #[test]
    fn disk_read_message_debug() {
        let msg = DiskReadMessage {
            path: PathBuf::from("test.md"),
            nonce: 1,
            edit_seq: 0,
            outcome: Ok(DiskReloadOutcome::Replace {
                disk_text: "x".into(),
                disk_rev: dummy_rev(),
            }),
        };
        let dbg = format!("{msg:?}");
        assert!(dbg.contains("DiskReadMessage"));
        assert!(dbg.contains("test.md"));
    }
}
