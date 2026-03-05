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
const ATOMIC_WRITE_MAX_ATTEMPTS: u128 = 10;
/// Maximum merge sidecar files before giving up.
const MERGE_SIDECAR_MAX_FILES: usize = 100;

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
    let pid = u128::from(std::process::id());
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());

    for attempt in 0..ATOMIC_WRITE_MAX_ATTEMPTS {
        let suffix = pid ^ nanos ^ attempt;
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
        if !candidate.exists() {
            return Ok(candidate);
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
    fn disk_revision_and_stable_read_report_expected_metadata() {
        let dir = test_dir("rustdown-disk-rev-test");
        let path = dir.join("test.md");
        fs::write(&path, "content").ok();

        let rev = disk_revision(&path);
        assert!(rev.is_ok());
        let rev = rev.ok().map(|r| r.len);
        assert_eq!(rev, Some(7));

        let result = read_stable_utf8(&path);
        assert!(result.is_ok(), "read_stable_utf8 failed: {result:?}");
        let (text, rev) = result.unwrap_or_else(|_| unreachable!());
        assert_eq!(text, "content");
        assert_eq!(rev.len, 7);
    }

    #[test]
    fn disk_revision_missing_file_returns_error() {
        let result = disk_revision(Path::new("/tmp/rustdown-nonexistent-12345.md"));
        assert!(result.is_err());
    }

    #[test]
    fn atomic_write_creates_and_overwrites() {
        let dir = test_dir("rustdown-atomic-test");
        let path = dir.join("test.md");

        assert!(atomic_write_utf8(&path, "first").is_ok());
        let content = fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(content, "first");

        assert!(atomic_write_utf8(&path, "second").is_ok());
        let content = fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(content, "second");
    }

    #[test]
    fn atomic_write_rejects_missing_filename() {
        let result = atomic_write_utf8(Path::new("/"), "data");
        assert!(result.is_err());
    }

    #[test]
    fn next_merge_sidecar_path_prefers_first_candidate_then_skips_existing() {
        let dir = test_dir("rustdown-sidecar-test");
        let original = dir.join("notes.md");

        let result = next_merge_sidecar_path(&original);
        assert!(result.is_ok());
        let sidecar = result.unwrap_or_else(|_| unreachable!());
        assert_eq!(
            sidecar.file_name().unwrap_or_default(),
            "notes.rustdown-merge.md"
        );
        let first = dir.join("notes.rustdown-merge.md");
        fs::write(&first, "").ok();

        let result = next_merge_sidecar_path(&original);
        assert!(result.is_ok());
        let sidecar = result.unwrap_or_else(|_| unreachable!());
        assert_eq!(
            sidecar.file_name().unwrap_or_default(),
            "notes.rustdown-merge-2.md"
        );
    }

    #[test]
    fn next_merge_sidecar_path_no_extension() {
        let dir = test_dir("rustdown-sidecar-noext-test");
        let original = dir.join("README");

        let result = next_merge_sidecar_path(&original);
        assert!(result.is_ok());
        let sidecar = result.unwrap_or_else(|_| unreachable!());
        assert_eq!(
            sidecar.file_name().unwrap_or_default(),
            "README.rustdown-merge"
        );
    }

    #[test]
    fn next_merge_sidecar_path_defaults_to_current_dir_when_no_parent() {
        // A bare filename with no parent directory should default to ".".
        let original = Path::new("notes.md");
        let result = next_merge_sidecar_path(original);
        assert!(result.is_ok());
        let sidecar = result.unwrap_or_else(|_| unreachable!());
        assert_eq!(
            sidecar.file_name().unwrap_or_default(),
            "notes.rustdown-merge.md"
        );
        assert_eq!(
            sidecar.parent().unwrap_or_else(|| unreachable!()),
            Path::new(".")
        );
    }

    #[test]
    fn next_merge_sidecar_path_rejects_bare_directory() {
        let result = next_merge_sidecar_path(Path::new("/"));
        assert!(result.is_err());
    }

    #[test]
    fn atomic_write_creates_parent_dir_file() {
        let dir = test_dir("rustdown-atomic-parent-test");
        let path = dir.join("subdir-file.md");
        assert!(atomic_write_utf8(&path, "hello").is_ok());
        let content = fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(content, "hello");
    }

    #[test]
    fn read_stable_utf8_returns_consistent_content() {
        let dir = test_dir("rustdown-stable-read-test");
        let path = dir.join("stable.md");
        fs::write(&path, "stable content").ok();

        let result = read_stable_utf8(&path);
        assert!(result.is_ok());
        let (text, rev) = result.unwrap_or_else(|_| unreachable!());
        assert_eq!(text, "stable content");
        assert_eq!(rev.len, 14);
    }

    #[test]
    fn disk_revision_reports_correct_length() {
        let dir = test_dir("rustdown-disk-rev-len-test");
        let path = dir.join("sized.md");
        fs::write(&path, "12345").ok();
        let rev = disk_revision(&path).unwrap_or_else(|_| unreachable!());
        assert_eq!(rev.len, 5);
    }

    #[test]
    fn atomic_write_empty_content() {
        let dir = test_dir("rustdown-atomic-empty-test");
        let path = dir.join("empty.md");
        assert!(atomic_write_utf8(&path, "").is_ok());
        let content = fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(content, "");
        let rev = disk_revision(&path).unwrap_or_else(|_| unreachable!());
        assert_eq!(rev.len, 0);
    }

    #[test]
    fn read_stable_utf8_missing_file_returns_error() {
        let result = read_stable_utf8(Path::new("/tmp/rustdown-nonexistent-stable-99999.md"));
        assert!(result.is_err());
    }

    #[test]
    fn disk_revision_unchanged_file_is_equal() {
        let dir = test_dir("rustdown-disk-rev-eq-test");
        let path = dir.join("stable.md");
        fs::write(&path, "unchanged").ok();
        let rev1 = disk_revision(&path).unwrap_or_else(|_| unreachable!());
        let rev2 = disk_revision(&path).unwrap_or_else(|_| unreachable!());
        assert_eq!(rev1, rev2);
    }

    #[test]
    fn disk_revision_changed_file_differs() {
        let dir = test_dir("rustdown-disk-rev-diff-test");
        let path = dir.join("changing.md");
        fs::write(&path, "v1").ok();
        let rev1 = disk_revision(&path).unwrap_or_else(|_| unreachable!());
        // Write different content (different length guarantees different revision).
        fs::write(&path, "version2-longer").ok();
        let rev2 = disk_revision(&path).unwrap_or_else(|_| unreachable!());
        assert_ne!(rev1.len, rev2.len);
    }

    #[test]
    fn next_merge_sidecar_path_exhaustion() {
        let dir = test_dir("rustdown-sidecar-exhaust-test");
        let original = dir.join("doc.md");

        // Fill all 100 slots.
        for n in 1..=MERGE_SIDECAR_MAX_FILES {
            let name = if n == 1 {
                "doc.rustdown-merge.md".to_owned()
            } else {
                format!("doc.rustdown-merge-{n}.md")
            };
            fs::write(dir.join(&name), "").ok();
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

        // Fill slots 1 and 2, verify it returns slot 3.
        fs::write(dir.join("notes.rustdown-merge.md"), "").ok();
        fs::write(dir.join("notes.rustdown-merge-2.md"), "").ok();

        let result = next_merge_sidecar_path(&original);
        assert!(result.is_ok());
        let sidecar = result.unwrap_or_else(|_| unreachable!());
        assert_eq!(
            sidecar.file_name().unwrap_or_default(),
            "notes.rustdown-merge-3.md"
        );
    }
}
