//! Disk-watcher integration for `RustdownApp` — file watching, reload
//! debouncing, background reads, and 3-way merge conflict handling.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

use eframe::egui;
use notify::{Event, RecursiveMode, Watcher};

use crate::disk_io::{DiskRevision, disk_revision, read_stable_utf8};
use crate::disk_sync::{DiskConflict, DiskReadMessage, DiskReloadOutcome, ReloadKind};
use crate::document::DocumentStats;
use crate::live_merge::{Merge3Outcome, merge_three_way};

use super::{DISK_POLL_INTERVAL, DISK_RELOAD_DEBOUNCE, RustdownApp};

impl RustdownApp {
    fn clear_disk_watcher(&mut self) {
        self.disk.watcher = None;
        self.disk.watch_root = None;
        self.disk.watch_target_name = None;
        self.disk.watch_rx = None;
    }

    pub(crate) fn schedule_disk_reload(&mut self, now: Instant) {
        let due_at = now + DISK_RELOAD_DEBOUNCE;
        if self
            .disk
            .pending_reload_at
            .is_none_or(|existing| existing > due_at)
        {
            self.disk.pending_reload_at = Some(due_at);
        }
    }

    pub(crate) fn apply_disk_text_state(
        &mut self,
        text: Arc<String>,
        base_text: Arc<String>,
        disk_rev: DiskRevision,
        kind: ReloadKind,
    ) {
        self.doc.text = text;
        self.doc.base_text = base_text;
        self.doc.disk_rev = Some(disk_rev);
        self.bump_edit_seq();
        self.doc.stats = DocumentStats::from_text(self.doc.text.as_str());
        self.doc.stats_dirty = false;
        self.doc.preview_cache.clear();
        self.doc.preview_dirty = false;
        self.doc.dirty = !matches!(kind, ReloadKind::Clean);
        if matches!(kind, ReloadKind::Clean | ReloadKind::ConflictResolved) {
            self.doc.last_edit_at = None;
        }
        self.doc.editor_galley_cache = None;
        self.error = None;
    }

    fn set_disk_conflict(
        &mut self,
        disk_text: String,
        disk_rev: DiskRevision,
        conflict_marked: String,
        ours_wins: String,
    ) {
        self.disk.conflict = Some(DiskConflict {
            disk_text,
            disk_rev,
            conflict_marked,
            ours_wins,
        });
    }

    pub(crate) fn reset_disk_sync_state(&mut self) {
        self.disk.reload_nonce = self.disk.reload_nonce.wrapping_add(1);
        self.disk.poll_at = None;
        self.disk.pending_reload_at = None;
        self.disk.reload_in_flight = false;
        self.disk.conflict = None;
        self.clear_disk_watcher();
    }

    fn ensure_disk_read_channel(&mut self) {
        if self.disk.read_tx.is_some() {
            return;
        }

        let (tx, rx) = mpsc::channel();
        self.disk.read_tx = Some(tx);
        self.disk.read_rx = Some(rx);
    }

    fn ensure_disk_watcher(&mut self, ctx: &egui::Context, path: &Path) {
        let watch_root = path.parent().unwrap_or_else(|| Path::new("."));
        let target_name = path.file_name().map(ToOwned::to_owned);

        if self.disk.watcher.is_some() && self.disk.watch_root.as_deref() == Some(watch_root) {
            self.disk.watch_target_name = target_name;
            return;
        }

        self.clear_disk_watcher();

        let Some(target_name) = target_name else {
            return;
        };

        let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
        let ctx = ctx.clone();
        let handler = move |res| {
            let _ = tx.send(res);
            ctx.request_repaint();
        };

        let mut watcher = match notify::recommended_watcher(handler) {
            Ok(watcher) => watcher,
            Err(err) => {
                self.error
                    .get_or_insert_with(|| format!("Watch setup failed: {err}"));
                return;
            }
        };

        if let Err(err) = watcher.watch(watch_root, RecursiveMode::NonRecursive) {
            self.error
                .get_or_insert_with(|| format!("Watch start failed: {err}"));
            return;
        }

        self.disk.watcher = Some(watcher);
        self.disk.watch_root = Some(watch_root.to_path_buf());
        self.disk.watch_target_name = Some(target_name);
        self.disk.watch_rx = Some(rx);
        self.disk.poll_at = None;
    }

    fn drain_disk_watch_events(&mut self) -> bool {
        let Some(rx) = self.disk.watch_rx.as_ref() else {
            return false;
        };

        let target_name = self.disk.watch_target_name.as_deref();
        let mut saw_change = false;
        let mut watch_error = None;
        let mut disconnected = false;

        loop {
            match rx.try_recv() {
                Ok(Ok(event)) => {
                    if let Some(target) = target_name
                        && event
                            .paths
                            .iter()
                            .any(|path| path.file_name().is_some_and(|name| name == target))
                    {
                        saw_change = true;
                    }
                }
                Ok(Err(err)) => {
                    watch_error = Some(format!("Watch error: {err}"));
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if let Some(err) = watch_error {
            self.error.get_or_insert(err);
            self.clear_disk_watcher();
            return false;
        }
        if disconnected {
            self.clear_disk_watcher();
            return false;
        }
        saw_change
    }

    pub(crate) fn tick_disk_sync(&mut self, ctx: &egui::Context) {
        self.drain_disk_read_results();

        if self.disk.conflict.is_some() {
            return;
        }

        let Some(path) = self.doc.path.clone() else {
            self.reset_disk_sync_state();
            return;
        };

        self.ensure_disk_watcher(ctx, path.as_path());

        let now = Instant::now();
        if self.drain_disk_watch_events() {
            self.schedule_disk_reload(now);
        }

        if self.disk.watcher.is_none() && !self.disk.reload_in_flight {
            match self.disk.poll_at {
                Some(next) if now < next => {}
                _ => {
                    self.disk.poll_at = Some(now + DISK_POLL_INTERVAL);

                    match disk_revision(path.as_path()) {
                        Ok(rev) if Some(rev) != self.doc.disk_rev => self.schedule_disk_reload(now),
                        Ok(_) => {}
                        Err(err) => {
                            self.error
                                .get_or_insert_with(|| format!("Disk check failed: {err}"));
                        }
                    }
                }
            }
        } else {
            self.disk.poll_at = None;
        }

        if self.disk.reload_in_flight {
            return;
        }

        if let Some(due_at) = self.disk.pending_reload_at
            && now >= due_at
        {
            self.disk.pending_reload_at = None;
            self.start_disk_reload(ctx, path.clone());
        }

        let mut next_wake = self.disk.pending_reload_at;
        if self.disk.watcher.is_none() {
            next_wake = match (next_wake, self.disk.poll_at) {
                (Some(existing), Some(poll)) => Some(existing.min(poll)),
                (one, other) => one.or(other),
            };
        }

        if let Some(next) = next_wake
            && now < next
        {
            ctx.request_repaint_after(next - now);
        }
    }

    fn start_disk_reload(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.ensure_disk_read_channel();
        let Some(tx) = self.disk.read_tx.clone() else {
            return;
        };

        let edit_seq = self.doc.edit_seq;
        let dirty = self.doc.dirty;
        let base_text = dirty.then(|| self.doc.base_text.clone());
        let ours_text = dirty.then(|| self.doc.text.clone());

        self.disk.reload_nonce = self.disk.reload_nonce.wrapping_add(1);
        let nonce = self.disk.reload_nonce;

        self.disk.reload_in_flight = true;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let outcome = match read_stable_utf8(&path) {
                Ok((disk_text, disk_rev)) => {
                    if dirty {
                        match (base_text, ours_text) {
                            (Some(base_text), Some(ours_text)) => match merge_three_way(
                                base_text.as_str(),
                                ours_text.as_str(),
                                disk_text.as_str(),
                            ) {
                                Merge3Outcome::Clean(merged_text) => {
                                    Ok(DiskReloadOutcome::MergeClean {
                                        merged_text,
                                        disk_text,
                                        disk_rev,
                                    })
                                }
                                Merge3Outcome::Conflicted {
                                    conflict_marked,
                                    ours_wins,
                                } => Ok(DiskReloadOutcome::MergeConflict {
                                    disk_text,
                                    disk_rev,
                                    conflict_marked,
                                    ours_wins,
                                }),
                            },
                            _ => Err(io::Error::other("missing merge inputs")),
                        }
                    } else {
                        Ok(DiskReloadOutcome::Replace {
                            disk_text,
                            disk_rev,
                        })
                    }
                }
                Err(err) => Err(err),
            };

            let _ = tx.send(DiskReadMessage {
                path,
                nonce,
                edit_seq,
                outcome,
            });
            ctx.request_repaint();
        });
    }

    fn drain_disk_read_results(&mut self) {
        loop {
            let recv = match self.disk.read_rx.as_ref() {
                Some(rx) => rx.try_recv(),
                None => return,
            };
            match recv {
                Ok(msg) => {
                    if msg.nonce != self.disk.reload_nonce {
                        continue;
                    }
                    if self.doc.path.as_deref() != Some(msg.path.as_path()) {
                        continue;
                    }
                    self.disk.reload_in_flight = false;

                    if self.doc.edit_seq != msg.edit_seq {
                        self.schedule_disk_reload(Instant::now());
                        continue;
                    }

                    match msg.outcome {
                        Ok(DiskReloadOutcome::Replace {
                            disk_text,
                            disk_rev,
                        }) => {
                            let disk_text = Arc::new(disk_text);
                            self.apply_disk_text_state(
                                disk_text.clone(),
                                disk_text,
                                disk_rev,
                                ReloadKind::Clean,
                            );
                        }
                        Ok(DiskReloadOutcome::MergeClean {
                            merged_text,
                            disk_text,
                            disk_rev,
                        }) => {
                            self.apply_disk_text_state(
                                Arc::new(merged_text),
                                Arc::new(disk_text),
                                disk_rev,
                                ReloadKind::Merged,
                            );
                        }
                        Ok(DiskReloadOutcome::MergeConflict {
                            disk_text,
                            disk_rev,
                            conflict_marked,
                            ours_wins,
                        }) => {
                            self.set_disk_conflict(disk_text, disk_rev, conflict_marked, ours_wins);
                        }
                        Err(err) => {
                            self.error = Some(format!("Reload failed: {err}"));
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.disk.reload_in_flight = false;
                    self.disk.read_rx = None;
                    self.disk.read_tx = None;
                    return;
                }
            }
        }
    }

    pub(crate) fn incorporate_disk_text(&mut self, disk_text: String, disk_rev: DiskRevision) {
        if self.doc.disk_rev == Some(disk_rev) && disk_text == self.doc.base_text.as_str() {
            return;
        }

        if !self.doc.dirty {
            let disk_text = Arc::new(disk_text);
            self.apply_disk_text_state(disk_text.clone(), disk_text, disk_rev, ReloadKind::Clean);
            return;
        }

        match merge_three_way(
            self.doc.base_text.as_str(),
            self.doc.text.as_str(),
            disk_text.as_str(),
        ) {
            Merge3Outcome::Clean(merged) => {
                self.apply_disk_text_state(
                    Arc::new(merged),
                    Arc::new(disk_text),
                    disk_rev,
                    ReloadKind::Merged,
                );
            }
            Merge3Outcome::Conflicted {
                conflict_marked,
                ours_wins,
            } => {
                self.set_disk_conflict(disk_text, disk_rev, conflict_marked, ours_wins);
            }
        }
    }
}
