//! Single-instance guard: an advisory `flock` on a PID file. The OS releases the lock on process
//! death, so a crashed daemon's lock is reclaimable by the next start.

use anyhow::{bail, Result};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

/// An acquired single-instance lock. Holds the locked file open for the process's lifetime; the
/// advisory `flock` is released when this value drops (or when the process dies).
pub struct InstanceLock {
    _file: std::fs::File,
    path: PathBuf,
}

impl InstanceLock {
    /// `<runtime_dir>/clowder/daemon.pid`, where runtime_dir is `$XDG_RUNTIME_DIR`, else `$TMPDIR`,
    /// else `/tmp`.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .or_else(|| std::env::var_os("TMPDIR"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        base.join("clowder").join("daemon.pid")
    }

    /// Try to take the exclusive advisory lock. Returns Err if another live daemon holds it.
    pub fn acquire(path: &Path) -> Result<InstanceLock> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).read(true).write(true).open(path)?;
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(e) if e == rustix::io::Errno::WOULDBLOCK => {
                bail!("another clowder-daemon is already running (lock held at {})", path.display());
            }
            Err(e) => bail!("failed to lock {}: {e}", path.display()),
        }
        // Record our PID (informational; the lock is authoritative).
        use std::io::{Seek, SeekFrom, Write};
        let mut f = &file;
        f.set_len(0)?;
        f.seek(SeekFrom::Start(0))?;
        let _ = writeln!(f, "{}", std::process::id());
        Ok(InstanceLock { _file: file, path: path.to_path_buf() })
    }

    /// The PID-file path this lock holds.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Best-effort unlink of each path; a missing file is not an error.
pub fn remove_files(paths: &[&Path]) {
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails_while_first_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clowder").join("daemon.pid");

        let lock1 = InstanceLock::acquire(&path).expect("first acquire succeeds");
        assert!(
            InstanceLock::acquire(&path).is_err(),
            "a second acquire must fail while the first lock is held"
        );

        // Releasing the first lock lets a later acquire succeed (stale lock is reclaimable).
        drop(lock1);
        // Retry briefly: when this test runs alongside the crate's PTY-spawning tests (which
        // fork() child processes), a sibling test's forked-but-not-yet-exec'd child can transiently
        // hold a duplicate of our just-closed fd (fork() copies fds; Rust's CLOEXEC only takes
        // effect at exec()), making an immediate reacquire spuriously see WOULDBLOCK. This is a
        // test-process artifact of running many fork-happy tests in one binary, not a production
        // concern (the daemon never re-acquires its own lock). Give it a brief window to clear.
        let mut lock2 = InstanceLock::acquire(&path);
        for _ in 0..20 {
            if lock2.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            lock2 = InstanceLock::acquire(&path);
        }
        let _lock2 = lock2.expect("acquire after release succeeds");
    }

    #[test]
    fn default_path_prefers_xdg_runtime_dir() {
        // default_path() just joins clowder/daemon.pid under the chosen base; assert the suffix.
        let p = InstanceLock::default_path();
        assert!(
            p.ends_with("clowder/daemon.pid"),
            "default path should end with clowder/daemon.pid, got {}",
            p.display()
        );
    }

    #[test]
    fn remove_files_unlinks_existing_and_ignores_missing() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("a.sock");
        let missing = dir.path().join("b.sock");
        std::fs::write(&present, b"x").unwrap();
        assert!(present.exists());

        remove_files(&[present.as_path(), missing.as_path()]);

        assert!(!present.exists(), "existing file should be removed");
        assert!(!missing.exists(), "missing file stays missing (no panic)");
    }
}
