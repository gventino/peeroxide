//! Taking the verified package apart and putting the new executable in place.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::UpdateError;

/// The executable inside a release package.
pub const EXE_NAME: &str = if cfg!(windows) {
    "peeroxide.exe"
} else {
    "peeroxide"
};
const EXE_LIMIT: u64 = 256 * 1024 * 1024;
const LOCK_NAME: &str = "peeroxide.update.lock";
/// A lock older than this was left by a crash and is ignored.
const STALE_LOCK: Duration = Duration::from_secs(10 * 60);

/// Writes the package's executable to `dest`. The package's own paths are never used for
/// writing, so a crafted archive can't place files anywhere else.
pub fn extract_exe(zip_path: &Path, dest: &Path) -> Result<(), UpdateError> {
    let bad = |e: &dyn std::fmt::Display| UpdateError::Package(e.to_string());
    let file = File::open(zip_path).map_err(|e| bad(&e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| bad(&e))?;
    let matches: Vec<usize> = (0..archive.len())
        .filter(|&i| {
            archive.by_index(i).is_ok_and(|f| {
                f.is_file()
                    && f.name()
                        .rsplit(['/', '\\'])
                        .next()
                        .is_some_and(|n| n.eq_ignore_ascii_case(EXE_NAME))
            })
        })
        .collect();
    let index = match matches.as_slice() {
        [i] => *i,
        [] => return Err(UpdateError::Package(format!("no {EXE_NAME} inside"))),
        _ => {
            return Err(UpdateError::Package(format!(
                "more than one {EXE_NAME} inside"
            )));
        }
    };
    let entry = archive.by_index(index).map_err(|e| bad(&e))?;
    if entry.size() > EXE_LIMIT {
        return Err(UpdateError::TooLarge);
    }
    let mut out = File::create(dest).map_err(|e| writable_error(&e))?;
    let written = io::copy(&mut entry.take(EXE_LIMIT + 1), &mut out).map_err(|e| bad(&e))?;
    if written > EXE_LIMIT {
        return Err(UpdateError::TooLarge);
    }
    Ok(())
}

pub(crate) fn writable_error(e: &io::Error) -> UpdateError {
    match e.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => {
            UpdateError::NotWritable(e.to_string())
        }
        _ => UpdateError::Install(e.to_string()),
    }
}

/// Puts `new_exe` where the running executable is. The running process keeps working; the new
/// version runs from the next start.
pub(crate) fn replace_running_exe(new_exe: &Path) -> Result<(), UpdateError> {
    self_replace::self_replace(new_exe).map_err(|e| writable_error(&e))
}

/// Starts `exe` with `args`, e.g. the updated app with the options it was started with.
pub fn relaunch(exe: &Path, args: &[OsString]) -> io::Result<()> {
    std::process::Command::new(exe).args(args).spawn().map(drop)
}

/// Makes sure only one instance updates the app at a time. Released when dropped.
pub struct UpdateLock(Option<PathBuf>);

impl UpdateLock {
    /// `None` if another instance holds the lock. In a folder we can't write to, the "lock" is
    /// a no-op: nothing can be installed there anyway, but the check still runs.
    pub fn acquire(dir: &Path) -> Option<Self> {
        let path = dir.join(LOCK_NAME);
        for _ in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Some(Self(Some(path))),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| SystemTime::now().duration_since(t).ok())
                        .is_some_and(|age| age > STALE_LOCK);
                    if !stale {
                        return None;
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(_) => return Some(Self(None)),
            }
        }
        None
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use zip::write::SimpleFileOptions;

    use super::*;

    /// A zip with `(path, contents)` entries.
    pub(crate) fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, data) in entries {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn extract(entries: &[(&str, &[u8])]) -> (tempfile::TempDir, Result<Vec<u8>, UpdateError>) {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("p.zip");
        std::fs::write(&zip, zip_with(entries)).unwrap();
        let dest = dir.path().join("out.exe");
        let result = extract_exe(&zip, &dest).map(|()| std::fs::read(&dest).unwrap());
        (dir, result)
    }

    #[test]
    fn extracts_the_executable_from_the_release_folder() {
        let exe = format!("peeroxide-0.5.0-windows-x64/{EXE_NAME}");
        let (_d, r) = extract(&[
            ("peeroxide-0.5.0-windows-x64/QUICKSTART.txt", b"read me"),
            (&exe, b"new exe"),
        ]);
        assert_eq!(r.unwrap(), b"new exe");
    }

    #[test]
    fn needs_exactly_one_executable() {
        let (_d, r) = extract(&[("QUICKSTART.txt", b"read me")]);
        assert!(matches!(r, Err(UpdateError::Package(_))));
        let (a, b) = (format!("a/{EXE_NAME}"), format!("b/{EXE_NAME}"));
        let (_d, r) = extract(&[(&a, b"1"), (&b, b"2")]);
        assert!(matches!(r, Err(UpdateError::Package(_))));
        let (_d, r) = extract(&[]);
        assert!(r.is_err());
    }

    #[test]
    fn a_path_escaping_entry_only_ever_lands_in_the_chosen_file() {
        let evil = format!("../../{EXE_NAME}");
        let (dir, r) = extract(&[(&evil, b"evil")]);
        assert_eq!(r.unwrap(), b"evil");
        let parent = dir.path().parent().unwrap();
        assert!(!parent.join(EXE_NAME).exists());
    }

    #[test]
    fn not_a_zip_is_a_package_error() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("p.zip");
        std::fs::write(&zip, b"<html>not a zip</html>").unwrap();
        assert!(matches!(
            extract_exe(&zip, &dir.path().join("x")),
            Err(UpdateError::Package(_))
        ));
    }

    #[test]
    fn only_one_updater_at_a_time_and_stale_locks_expire() {
        let dir = tempfile::tempdir().unwrap();
        let first = UpdateLock::acquire(dir.path()).unwrap();
        assert!(UpdateLock::acquire(dir.path()).is_none());
        drop(first);
        let again = UpdateLock::acquire(dir.path()).unwrap();
        std::mem::forget(again); // left behind as if the app crashed
        let lock = dir.path().join(LOCK_NAME);
        File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(SystemTime::now() - STALE_LOCK - Duration::from_secs(1))
            .unwrap();
        assert!(UpdateLock::acquire(dir.path()).is_some());
    }
}
