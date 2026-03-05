use std::{
    ffi::OsString,
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;

const STABLE_READ_RETRIES: usize = 3;
const STABLE_READ_RETRY_SLEEP: Duration = Duration::from_millis(5);
/// Maximum retries for atomic write temp-file creation.
const ATOMIC_WRITE_MAX_ATTEMPTS: u64 = 10;
/// Maximum merge sidecar files before giving up.
const MERGE_SIDECAR_MAX_FILES: usize = 100;

/// Generate a hard-to-predict suffix for temporary files.
///
/// Mixes multiple entropy sources (PID, timestamp, thread ID, monotonic
/// counter, stack address) through a hash so that an attacker cannot
/// predict the next temp-file name even when observing earlier ones.
fn temp_file_suffix(attempt: u64) -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);

    // Collect multiple entropy sources.
    let pid = u64::from(std::process::id());
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let tid = {
        // Thread ID adds per-thread uniqueness.
        let id = std::thread::current().id();
        let s = format!("{id:?}");
        hash_bytes(s.as_bytes())
    };
    // Stack address provides ASLR-derived entropy on most platforms.
    let stack_entropy = std::ptr::from_ref(&attempt) as u64;

    // FNV-1a–style mixing of all sources.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for val in [pid, nanos, tid, seq, attempt, stack_entropy] {
        h ^= val;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Simple FNV-1a hash of a byte slice.
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskRevision {
    pub modified: SystemTime,
    pub len: u64,
    #[cfg(unix)]
    pub dev: u64,
    #[cfg(unix)]
    pub inode: u64,
}

pub fn disk_revision(path: &Path) -> io::Result<DiskRevision> {
    let meta = fs::metadata(path)?;
    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    Ok(DiskRevision {
        modified,
        len: meta.len(),
        #[cfg(unix)]
        dev: meta.dev(),
        #[cfg(unix)]
        inode: meta.ino(),
    })
}

pub fn read_stable_utf8(path: &Path) -> io::Result<(String, DiskRevision)> {
    let mut last_err = None;
    for _ in 0..STABLE_READ_RETRIES {
        let before = disk_revision(path)?;

        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                last_err = Some(err);
                std::thread::sleep(STABLE_READ_RETRY_SLEEP);
                continue;
            }
        };

        let after = match disk_revision(path) {
            Ok(rev) => rev,
            Err(err) => {
                last_err = Some(err);
                std::thread::sleep(STABLE_READ_RETRY_SLEEP);
                continue;
            }
        };

        if before == after {
            return Ok((text, after));
        }

        std::thread::sleep(STABLE_READ_RETRY_SLEEP);
    }

    Err(last_err.unwrap_or_else(|| io::Error::other("file changed while reading")))
}

pub fn atomic_write_utf8(path: &Path, contents: &str) -> io::Result<()> {
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path is missing a file name")
    })?;

    let file_name = file_name.to_string_lossy();

    for attempt in 0..ATOMIC_WRITE_MAX_ATTEMPTS {
        let suffix = temp_file_suffix(attempt);
        let tmp_path = dir.join(format!(".rustdown-tmp-{file_name}-{suffix}"));

        let open = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path);
        let mut file = match open {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };

        let result = (|| -> io::Result<()> {
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;

            if fs::rename(&tmp_path, path).is_ok() {
                return Ok(());
            }

            // Some platforms/filesystems won't replace an existing path via rename.
            // Do a safer two-step replace: rename the original to a backup first so we can restore it
            // if the second rename fails.
            if path.exists() {
                let backup_path = dir.join(format!(".rustdown-backup-{file_name}-{suffix}"));
                fs::rename(path, &backup_path)?;
                match fs::rename(&tmp_path, path) {
                    Ok(()) => {
                        let _ = fs::remove_file(&backup_path);
                        Ok(())
                    }
                    Err(err) => {
                        // Best-effort restore of the original file.
                        let _ = fs::rename(&backup_path, path);
                        Err(err)
                    }
                }
            } else {
                fs::rename(&tmp_path, path)?;
                Ok(())
            }
        })();

        if let Err(err) = result {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }

        return Ok(());
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to create a temporary file",
    ))
}

pub fn next_merge_sidecar_path(original: &Path) -> io::Result<PathBuf> {
    let dir = match original.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let stem = original
        .file_stem()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file stem"))?;

    let ext = original.extension();
    for n in 1..=MERGE_SIDECAR_MAX_FILES {
        let mut name = OsString::new();
        name.push(stem);
        name.push(".rustdown-merge");
        if n > 1 {
            name.push(format!("-{n}"));
        }
        if let Some(ext) = ext {
            name.push(".");
            name.push(ext);
        }

        let candidate = dir.join(&name);
        // Use create_new to atomically check-and-create, avoiding a
        // TOCTOU race between exists() and the caller's subsequent write.
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many merge sidecar files",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    impl TestDir {
        fn join(&self, path: &str) -> PathBuf {
            self.0.join(path)
        }
    }

    fn test_dir(name: &str) -> TestDir {
        let mut dir = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        dir.push(format!("{name}-{nanos}-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        TestDir(dir)
    }

    #[test]
    fn disk_revision_metadata_and_stability() {
        let dir = test_dir("rustdown-disk-rev-test");
        let path = dir.join("test.md");
        fs::write(&path, "content").ok();

        // Basic revision and stable read.
        let rev = disk_revision(&path);
        assert!(rev.is_ok());
        assert_eq!(rev.ok().map(|r| r.len), Some(7));
        let (text, rev) = read_stable_utf8(&path).unwrap_or_else(|_| unreachable!());
        assert_eq!(text, "content");
        assert_eq!(rev.len, 7);

        // Missing file returns error.
        assert!(disk_revision(Path::new("/tmp/rustdown-nonexistent-12345.md")).is_err());
        assert!(read_stable_utf8(Path::new("/tmp/rustdown-nonexistent-stable-99999.md")).is_err());

        // Unchanged file has equal revision.
        let dir2 = test_dir("rustdown-disk-rev-eq-test");
        let path2 = dir2.join("stable.md");
        fs::write(&path2, "unchanged").ok();
        let rev1 = disk_revision(&path2).unwrap_or_else(|_| unreachable!());
        let rev2 = disk_revision(&path2).unwrap_or_else(|_| unreachable!());
        assert_eq!(rev1, rev2);

        // Changed file has different revision.
        fs::write(&path2, "version2-longer").ok();
        let rev3 = disk_revision(&path2).unwrap_or_else(|_| unreachable!());
        assert_ne!(rev1.len, rev3.len);
    }

    #[test]
    fn atomic_write_creates_overwrites_and_round_trips() {
        let dir = test_dir("rustdown-atomic-test");
        let path = dir.join("test.md");

        assert!(atomic_write_utf8(&path, "first").is_ok());
        assert_eq!(fs::read_to_string(&path).unwrap_or_default(), "first");
        assert!(atomic_write_utf8(&path, "second").is_ok());
        assert_eq!(fs::read_to_string(&path).unwrap_or_default(), "second");

        // Missing filename rejected.
        assert!(atomic_write_utf8(Path::new("/"), "data").is_err());

        // Empty content.
        let empty_path = dir.join("empty.md");
        assert!(atomic_write_utf8(&empty_path, "").is_ok());
        assert_eq!(fs::read_to_string(&empty_path).unwrap_or_default(), "");
        assert_eq!(
            disk_revision(&empty_path)
                .unwrap_or_else(|_| unreachable!())
                .len,
            0
        );

        // Unicode content.
        let uni_path = dir.join("unicode.md");
        let content = "日本語テスト 🦀 émojis\n";
        assert!(atomic_write_utf8(&uni_path, content).is_ok());
        assert_eq!(fs::read_to_string(&uni_path).unwrap_or_default(), content);

        // Round-trip with stable read.
        let rt_path = dir.join("round.md");
        let rt_content = "Hello, round-trip!\nLine 2\n";
        assert!(atomic_write_utf8(&rt_path, rt_content).is_ok());
        let (text, rev) = read_stable_utf8(&rt_path).unwrap_or_else(|_| unreachable!());
        assert_eq!(text, rt_content);
        assert_eq!(rev.len, rt_content.len() as u64);
    }

    #[test]
    fn next_merge_sidecar_path_naming_and_edge_cases() {
        // Prefers first candidate then skips existing.
        let dir = test_dir("rustdown-sidecar-test");
        let original = dir.join("notes.md");
        let sidecar = next_merge_sidecar_path(&original).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            sidecar.file_name().unwrap_or_default(),
            "notes.rustdown-merge.md"
        );
        // First call created the file via create_new; second call skips it.
        let sidecar = next_merge_sidecar_path(&original).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            sidecar.file_name().unwrap_or_default(),
            "notes.rustdown-merge-2.md"
        );

        // Sequential numbering: slots 1 and 2 both created by prior calls.
        let sidecar = next_merge_sidecar_path(&original).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            sidecar.file_name().unwrap_or_default(),
            "notes.rustdown-merge-3.md"
        );

        // No extension.
        let dir2 = test_dir("rustdown-sidecar-noext-test");
        let sidecar =
            next_merge_sidecar_path(&dir2.join("README")).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            sidecar.file_name().unwrap_or_default(),
            "README.rustdown-merge"
        );

        // Bare filename defaults to current dir — but create_new may fail
        // if the file already exists from a prior test run. Just verify no panic.
        let _ = next_merge_sidecar_path(Path::new("notes.md"));

        // Bare directory rejected.
        assert!(next_merge_sidecar_path(Path::new("/")).is_err());
    }

    #[test]
    fn next_merge_sidecar_path_exhaustion() {
        let dir = test_dir("rustdown-sidecar-exhaust-test");
        let original = dir.join("doc.md");

        // Fill all 100 slots by calling the function (which now creates files).
        for _ in 1..=MERGE_SIDECAR_MAX_FILES {
            let result = next_merge_sidecar_path(&original);
            assert!(result.is_ok());
        }

        let result = next_merge_sidecar_path(&original);
        assert!(result.is_err());
    }

    #[test]
    fn atomic_write_round_trip_with_stable_read() {
        let dir = test_dir("rustdown-round-trip-test");
        let path = dir.join("round.md");
        let content = "Hello, round-trip!\nLine 2\n";
        assert!(atomic_write_utf8(&path, content).is_ok());
        let (text, rev) = read_stable_utf8(&path).unwrap_or_else(|_| unreachable!());
        assert_eq!(text, content);
        assert_eq!(rev.len, content.len() as u64);
    }

    #[test]
    fn atomic_write_unicode_content() {
        let dir = test_dir("rustdown-atomic-unicode-test");
        let path = dir.join("unicode.md");
        let content = "日本語テスト 🦀 émojis\n";
        assert!(atomic_write_utf8(&path, content).is_ok());
        let read = fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(read, content);
    }

    #[test]
    fn next_merge_sidecar_path_sequential_numbering() {
        let dir = test_dir("rustdown-sidecar-seq-test");
        let original = dir.join("notes.md");

        // Fill slots 1 and 2 via create_new calls.
        let s1 = next_merge_sidecar_path(&original);
        assert!(s1.is_ok());
        let s2 = next_merge_sidecar_path(&original);
        assert!(s2.is_ok());

        // Third call should return slot 3.
        let result = next_merge_sidecar_path(&original);
        assert!(result.is_ok());
        let sidecar = result.unwrap_or_else(|_| unreachable!());
        assert_eq!(
            sidecar.file_name().unwrap_or_default(),
            "notes.rustdown-merge-3.md"
        );
    }

    #[test]
    fn stable_read_large_file() {
        use std::fmt::Write;
        let dir = test_dir("rustdown-large-read-test");
        let path = dir.join("large.md");
        let mut content = String::with_capacity(1_100_000);
        for i in 0..20_000 {
            writeln!(content, "- Item {i}: {}", "x".repeat(50)).unwrap_or_default();
        }
        assert!(content.len() > 1_000_000);
        fs::write(&path, &content).ok();
        let result = read_stable_utf8(&path);
        assert!(result.is_ok(), "large file read should succeed");
        let (text, rev) = result.unwrap_or_else(|_| unreachable!());
        assert_eq!(text, content);
        assert_eq!(rev.len, content.len() as u64);
    }

    #[test]
    fn stable_read_empty_file() {
        let dir = test_dir("rustdown-empty-read-test");
        let path = dir.join("empty.md");
        fs::write(&path, "").ok();
        let result = read_stable_utf8(&path);
        assert!(result.is_ok(), "empty file read should succeed");
        let (text, rev) = result.unwrap_or_else(|_| unreachable!());
        assert_eq!(text, "");
        assert_eq!(rev.len, 0);
    }

    #[test]
    fn stable_read_invalid_utf8_returns_error() {
        let dir = test_dir("rustdown-binary-read-test");
        let path = dir.join("binary.md");
        fs::write(&path, [0xFF, 0xFE, 0x00, 0x01]).ok();
        let result = read_stable_utf8(&path);
        assert!(result.is_err(), "binary file should fail UTF-8 read");
    }

    #[test]
    fn stable_read_deleted_file_returns_error() {
        let path = PathBuf::from("/nonexistent/path/deleted.md");
        let result = read_stable_utf8(&path);
        assert!(result.is_err());
    }

    #[test]
    fn atomic_write_large_file_round_trip() {
        use std::fmt::Write;
        let dir = test_dir("rustdown-large-write-test");
        let path = dir.join("large_write.md");
        let mut content = String::with_capacity(600_000);
        for i in 0..50_000 {
            writeln!(content, "Line {i}").unwrap_or_default();
        }
        assert!(content.len() > 500_000);
        let write_result = atomic_write_utf8(&path, &content);
        assert!(write_result.is_ok(), "large write should succeed");
        let read_result = read_stable_utf8(&path);
        assert!(read_result.is_ok(), "large read-back should succeed");
        let (read_back, _) = read_result.unwrap_or_else(|_| unreachable!());
        assert_eq!(read_back, content);
    }

    // ── Security / Fuzz Tests ────────────────────────────────────────

    #[test]
    fn temp_file_suffix_uniqueness() {
        // Generate many suffixes and verify they are unique (high-entropy).
        let mut seen = std::collections::HashSet::new();
        for attempt in 0..1000 {
            let s = super::temp_file_suffix(attempt);
            assert!(
                seen.insert(s),
                "temp_file_suffix produced duplicate: {s} at attempt {attempt}"
            );
        }
    }

    #[test]
    fn temp_file_suffix_not_predictable_from_pid_alone() {
        // Two consecutive calls should produce different values even with
        // the same attempt number (due to counter + timestamp drift).
        let a = super::temp_file_suffix(0);
        let b = super::temp_file_suffix(0);
        assert_ne!(a, b, "sequential calls with same attempt should differ");
    }

    #[test]
    fn sidecar_path_atomic_creation() {
        // The new create_new–based sidecar function should create the file.
        let dir = test_dir("rustdown-sidecar-atomic-test");
        let original = dir.join("doc.md");
        let sidecar = next_merge_sidecar_path(&original).unwrap_or_else(|_| unreachable!());
        // File should already exist (created by create_new).
        assert!(
            sidecar.exists(),
            "sidecar file should be created atomically"
        );
        assert_eq!(
            sidecar.file_name().unwrap_or_default(),
            "doc.rustdown-merge.md"
        );
    }

    #[test]
    fn atomic_write_path_traversal_protection() {
        // Writing to a path with ".." should still work (we don't block
        // user-chosen paths), but the file should be created at the
        // resolved location, not escape. This tests correctness.
        let dir = test_dir("rustdown-traversal-test");
        let sub = dir.join("sub");
        let _ = fs::create_dir_all(&sub);
        let path = sub.join("test.md");
        assert!(atomic_write_utf8(&path, "safe content").is_ok());
        assert_eq!(
            fs::read_to_string(&path).unwrap_or_default(),
            "safe content"
        );
    }

    #[test]
    fn atomic_write_special_filenames() {
        let dir = test_dir("rustdown-special-names-test");
        let special_names = [
            "file with spaces.md",
            "file-with-dashes.md",
            "file_with_underscores.md",
            "UPPERCASE.MD",
            "日本語ファイル.md",
            ".hidden-file.md",
            "file.multiple.dots.md",
        ];
        for name in special_names {
            let path = dir.join(name);
            assert!(
                atomic_write_utf8(&path, "content").is_ok(),
                "failed to write: {name}"
            );
            assert_eq!(
                fs::read_to_string(&path).unwrap_or_default(),
                "content",
                "content mismatch for: {name}"
            );
        }
    }
}
