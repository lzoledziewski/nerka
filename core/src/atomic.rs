//! Writing user files so that a crash, a full disk or a killed process never
//! leaves one truncated: the new contents go to a temporary file in the same
//! folder, are flushed to disk, and replace the file by a rename (atomic on
//! one volume, on Windows and Unix alike). A reader sees the old file or the
//! new one, never half of it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Replace `path`'s contents with `contents`, atomically. A symbolic link
/// is followed: its target is replaced, the link stays.
pub fn write_atomic(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    let path = path.as_ref();
    let target = match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let tmp = temp_name(&target);
    let written = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_ref())?;
        f.sync_all()
    })();
    let result = written.and_then(|()| std::fs::rename(&tmp, &target));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// `<name>.<pid>-<n>.tmp` next to `path`: unique per process and call, so
/// two windows saving the same file don't share one.
fn temp_name(path: &Path) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}-{n}.tmp", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nerka-atomic-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn creates_and_replaces_without_leftovers() {
        let d = dir("replace");
        let f = d.join("prefs");
        write_atomic(&f, "one\n").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "one\n");
        write_atomic(&f, "two\n").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "two\n");
        let names: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, ["prefs"], "no temporary file is left");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_failed_write_keeps_the_old_file() {
        let d = dir("fail");
        let f = d.join("pack.nerka-pack");
        write_atomic(&f, "pack a\n").unwrap();
        // The rename can't replace a folder: the write fails, the file in
        // the way and the folder's other files stay as they were.
        let blocked = d.join("sub");
        std::fs::create_dir_all(blocked.join("x")).unwrap();
        assert!(write_atomic(&blocked, "new").is_err());
        assert!(blocked.is_dir());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "pack a\n");
        let mut names: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name()).collect();
        names.sort();
        assert_eq!(names, ["pack.nerka-pack", "sub"], "no temporary file is left");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
