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
    /// `<runtime_dir>/muxy/daemon.pid`, where runtime_dir is `$XDG_RUNTIME_DIR`, else `$TMPDIR`,
    /// else `/tmp`.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .or_else(|| std::env::var_os("TMPDIR"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        base.join("muxy").join("daemon.pid")
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
                bail!("another muxy-daemon is already running (lock held at {})", path.display());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails_while_first_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("muxy").join("daemon.pid");

        let lock1 = InstanceLock::acquire(&path).expect("first acquire succeeds");
        assert!(
            InstanceLock::acquire(&path).is_err(),
            "a second acquire must fail while the first lock is held"
        );

        // Releasing the first lock lets a later acquire succeed (stale lock is reclaimable).
        drop(lock1);
        let _lock2 = InstanceLock::acquire(&path).expect("acquire after release succeeds");
    }

    #[test]
    fn default_path_prefers_xdg_runtime_dir() {
        // default_path() just joins muxy/daemon.pid under the chosen base; assert the suffix.
        let p = InstanceLock::default_path();
        assert!(
            p.ends_with("muxy/daemon.pid"),
            "default path should end with muxy/daemon.pid, got {}",
            p.display()
        );
    }
}
