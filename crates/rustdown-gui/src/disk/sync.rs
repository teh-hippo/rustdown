use std::{ffi::OsString, io, path::PathBuf, sync::mpsc, time::Instant};

use notify::RecommendedWatcher;

use crate::disk::io::DiskRevision;

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
    use std::{io, sync::mpsc, time::SystemTime};

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
    fn sync_state_mutation_and_defaults() {
        // All Option fields default to None, counters to zero.
        let s = DiskSyncState::default();
        assert_eq!(s.reload_nonce, 0);
        assert!(!s.reload_in_flight);
        for is_none in [
            s.watcher.is_none(),
            s.watch_root.is_none(),
            s.watch_target_name.is_none(),
            s.watch_rx.is_none(),
            s.poll_at.is_none(),
            s.pending_reload_at.is_none(),
            s.read_tx.is_none(),
            s.read_rx.is_none(),
            s.conflict.is_none(),
            s.merge_sidecar_path.is_none(),
        ] {
            assert!(is_none);
        }

        // Nonce, in-flight, conflict, watch fields, and channels.
        let mut s = DiskSyncState::default();
        s.reload_nonce += 1;
        assert_eq!(s.reload_nonce, 1);
        s.reload_in_flight = true;
        assert!(s.reload_in_flight);
        s.conflict = Some(DiskConflict {
            disk_text: "d".into(),
            disk_rev: dummy_rev(),
            conflict_marked: "cm".into(),
            ours_wins: "ow".into(),
        });
        assert!(s.conflict.is_some());
        s.conflict = None;
        assert!(s.conflict.is_none());
        s.watch_target_name = Some(OsString::from("README.md"));
        s.watch_root = Some(PathBuf::from("/tmp"));
        let (tx, rx) = mpsc::channel::<DiskReadMessage>();
        s.read_tx = Some(tx);
        s.read_rx = Some(rx);
        assert!(s.read_tx.is_some() && s.read_rx.is_some());
    }

    #[test]
    fn channel_round_trip_all_outcome_variants() {
        let outcomes: Vec<Result<DiskReloadOutcome, io::Error>> = vec![
            Ok(DiskReloadOutcome::Replace {
                disk_text: "new".into(),
                disk_rev: dummy_rev(),
            }),
            Ok(DiskReloadOutcome::MergeClean {
                merged_text: "merged".into(),
                disk_text: "disk".into(),
                disk_rev: dummy_rev(),
            }),
            Ok(DiskReloadOutcome::MergeConflict {
                disk_text: "disk".into(),
                disk_rev: dummy_rev(),
                conflict_marked: "<<<<<<< ours\nX\n=======\nY\n>>>>>>> theirs\n".into(),
                ours_wins: "X\n".into(),
            }),
            Err(io::Error::new(io::ErrorKind::NotFound, "gone")),
        ];
        for (i, outcome) in outcomes.into_iter().enumerate() {
            let is_err = outcome.is_err();
            let (tx, rx) = mpsc::channel();
            let nonce = (i + 1) as u64;
            tx.send(DiskReadMessage {
                path: PathBuf::from("/tmp/test.md"),
                nonce,
                edit_seq: 0,
                outcome,
            })
            .ok();
            let m = rx.try_recv().unwrap_or_else(|_| unreachable!());
            assert_eq!(m.nonce, nonce);
            assert_eq!(m.outcome.is_err(), is_err, "variant {i}");
        }
    }

    #[test]
    fn stale_nonce_and_debounce_patterns() {
        // Stale nonce detection.
        let mut s = DiskSyncState::default();
        s.reload_nonce = s.reload_nonce.wrapping_add(1);
        let stale = s.reload_nonce;
        s.reload_nonce = s.reload_nonce.wrapping_add(1);
        assert_ne!(stale, s.reload_nonce);
        assert_eq!(s.reload_nonce, s.reload_nonce);

        // Debounce: first event wins.
        let mut s = DiskSyncState::default();
        let t1 = Instant::now() + std::time::Duration::from_millis(200);
        s.pending_reload_at = Some(t1);
        if s.pending_reload_at.is_none() {
            s.pending_reload_at = Some(Instant::now());
        }
        assert_eq!(s.pending_reload_at, Some(t1));
    }

    #[test]
    fn disk_types_fields_clone_debug_and_copy() {
        // DiskReloadOutcome variant fields via table.
        let replace = DiskReloadOutcome::Replace {
            disk_text: "hello".into(),
            disk_rev: dummy_rev(),
        };
        let merge_clean = DiskReloadOutcome::MergeClean {
            merged_text: "m".into(),
            disk_text: "d".into(),
            disk_rev: dummy_rev(),
        };
        let merge_conflict = DiskReloadOutcome::MergeConflict {
            disk_text: "t".into(),
            disk_rev: dummy_rev(),
            conflict_marked: "<<<".into(),
            ours_wins: "o".into(),
        };
        assert!(matches!(replace, DiskReloadOutcome::Replace { .. }));
        assert!(matches!(merge_clean, DiskReloadOutcome::MergeClean { .. }));
        assert!(matches!(
            merge_conflict,
            DiskReloadOutcome::MergeConflict { .. }
        ));

        // DiskConflict clone and debug.
        let c = DiskConflict {
            disk_text: "d".into(),
            disk_rev: dummy_rev(),
            conflict_marked: "cm".into(),
            ours_wins: "ow".into(),
        };
        let c2 = c.clone();
        assert_eq!(
            (
                c2.disk_text.as_str(),
                c2.conflict_marked.as_str(),
                c2.ours_wins.as_str()
            ),
            ("d", "cm", "ow")
        );
        assert!(format!("{c:?}").contains("DiskConflict"));

        // ReloadKind: debug, Copy.
        for (kind, name) in [
            (ReloadKind::Clean, "Clean"),
            (ReloadKind::Merged, "Merged"),
            (ReloadKind::ConflictResolved, "ConflictResolved"),
        ] {
            assert!(format!("{kind:?}").contains(name));
            let k2 = kind;
            assert_eq!(format!("{kind:?}"), format!("{k2:?}"));
        }
    }
}
