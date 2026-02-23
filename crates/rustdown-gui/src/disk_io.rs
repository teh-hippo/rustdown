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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DiskRevision {
    pub(crate) modified: SystemTime,
    pub(crate) len: u64,
    #[cfg(unix)]
    pub(crate) dev: u64,
    #[cfg(unix)]
    pub(crate) inode: u64,
}

pub(crate) fn disk_revision(path: &Path) -> io::Result<DiskRevision> {
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

pub(crate) fn read_stable_utf8(path: &Path) -> io::Result<(String, DiskRevision)> {
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

pub(crate) fn atomic_write_utf8(path: &Path, contents: &str) -> io::Result<()> {
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

    for attempt in 0..10u128 {
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

pub(crate) fn next_merge_sidecar_path(original: &Path) -> io::Result<PathBuf> {
    let dir = match original.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let stem = original
        .file_stem()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file stem"))?;

    let ext = original.extension();
    for n in 1..=100usize {
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

    fn make_temp_dir(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        dir.push(format!("{name}-{nanos}-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn disk_revision_reads_metadata() {
        let dir = make_temp_dir("rustdown-disk-rev-test");
        let path = dir.join("test.md");
        fs::write(&path, "hello").ok();

        let rev = disk_revision(&path);
        assert!(rev.is_ok());
        let rev = rev.ok().map(|r| r.len);
        assert_eq!(rev, Some(5));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn disk_revision_missing_file_returns_error() {
        let result = disk_revision(Path::new("/tmp/rustdown-nonexistent-12345.md"));
        assert!(result.is_err());
    }

    #[test]
    fn read_stable_utf8_reads_content_and_revision() {
        let dir = make_temp_dir("rustdown-stable-read-test");
        let path = dir.join("test.md");
        fs::write(&path, "content").ok();

        let result = read_stable_utf8(&path);
        assert!(result.is_ok(), "read_stable_utf8 failed: {result:?}");
        if let Ok((text, rev)) = result {
            assert_eq!(text, "content");
            assert_eq!(rev.len, 7);
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_creates_and_overwrites() {
        let dir = make_temp_dir("rustdown-atomic-test");
        let path = dir.join("test.md");

        assert!(atomic_write_utf8(&path, "first").is_ok());
        let content = fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(content, "first");

        assert!(atomic_write_utf8(&path, "second").is_ok());
        let content = fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(content, "second");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_rejects_missing_filename() {
        let result = atomic_write_utf8(Path::new("/"), "data");
        assert!(result.is_err());
    }

    #[test]
    fn next_merge_sidecar_path_first_candidate() {
        let dir = make_temp_dir("rustdown-sidecar-test");
        let original = dir.join("notes.md");

        let result = next_merge_sidecar_path(&original);
        assert!(result.is_ok());
        let sidecar = result.ok().unwrap_or_default();
        assert_eq!(
            sidecar.file_name().unwrap_or_default(),
            "notes.rustdown-merge.md"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_merge_sidecar_path_skips_existing() {
        let dir = make_temp_dir("rustdown-sidecar-skip-test");
        let original = dir.join("notes.md");

        // Create the first sidecar so it skips to -2
        let first = dir.join("notes.rustdown-merge.md");
        fs::write(&first, "").ok();

        let result = next_merge_sidecar_path(&original);
        assert!(result.is_ok());
        let sidecar = result.ok().unwrap_or_default();
        assert_eq!(
            sidecar.file_name().unwrap_or_default(),
            "notes.rustdown-merge-2.md"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_merge_sidecar_path_no_extension() {
        let dir = make_temp_dir("rustdown-sidecar-noext-test");
        let original = dir.join("README");

        let result = next_merge_sidecar_path(&original);
        assert!(result.is_ok());
        let sidecar = result.ok().unwrap_or_default();
        assert_eq!(
            sidecar.file_name().unwrap_or_default(),
            "README.rustdown-merge"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
