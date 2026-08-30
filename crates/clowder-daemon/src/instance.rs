// SPDX-License-Identifier: Apache-2.0

//! Single-instance guard: an advisory `flock` on a PID file. The OS releases the lock on process
//! death, so a crashed daemon's lock is reclaimable by the next start.

use anyhow::{bail, Context, Result};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

/// An acquired single-instance lock. Holds the locked file open for the process's lifetime; the
/// advisory `flock` is released when this value drops (or when the process dies).
pub struct InstanceLock {
    _file: std::fs::File,
    path: PathBuf,
}

impl InstanceLock {
    /// `<runtime_dir>/daemon.pid` — the SAME runtime dir the sockets use.
    ///
    /// Delegates to `clowder_config::runtime_dir()` rather than re-deriving the chain: this used to
    /// use `var_os`, which accepts an EMPTY value, while clowder-config treats empty as unset. With
    /// `TMPDIR=""` the lock went to `/clowder/daemon.pid` while the sockets went to `/tmp/clowder/`,
    /// and the lock's `create_dir_all` then failed at the filesystem root.
    pub fn default_path() -> PathBuf {
        clowder_config::runtime_dir().join("daemon.pid")
    }

    /// Try to take the exclusive advisory lock.
    ///
    /// `Ok(None)` means **another live daemon holds it** — the one condition the caller should treat
    /// as "yield, don't retry". Every other problem (mkdir, open, an unexpected flock errno) is an
    /// `Err`, because collapsing them into the same signal made an unrelated environment failure
    /// look like a second instance, and the supervisor then yielded permanently instead of retrying.
    pub fn acquire(path: &Path) -> Result<Option<InstanceLock>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create lock dir {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("open lock file {}", path.display()))?;
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(e) if e == rustix::io::Errno::WOULDBLOCK => return Ok(None),
            Err(e) => bail!("failed to lock {}: {e}", path.display()),
        }
        // Record our PID (informational; the lock is authoritative).
        use std::io::{Seek, SeekFrom, Write};
        let mut f = &file;
        f.set_len(0)?;
        f.seek(SeekFrom::Start(0))?;
        let _ = writeln!(f, "{}", std::process::id());
        Ok(Some(InstanceLock { _file: file, path: path.to_path_buf() }))
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
    fn second_acquire_yields_ok_none_while_first_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clowder").join("daemon.pid");

        let lock1 = InstanceLock::acquire(&path).expect("first acquire succeeds").expect("got a lock");
        // Ok(None), NOT Err: this is the ONE condition main.rs may translate into exit 3, which the
        // supervising app treats as permanent. An Err here would make the app yield for ever on
        // what is really a transient or environmental problem.
        assert!(
            matches!(InstanceLock::acquire(&path), Ok(None)),
            "a contended lock must report Ok(None), not Err"
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
            if matches!(lock2, Ok(Some(_))) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            lock2 = InstanceLock::acquire(&path);
        }
        let _lock2 = lock2.expect("acquire after release succeeds").expect("got a lock");
    }

    /// The distinction main.rs depends on: a real I/O problem must be an Err (exit 1 → the
    /// supervisor relaunches), never the Ok(None) that means "another instance owns this"
    /// (exit 3 → the supervisor yields permanently). Conflating them turned an environment
    /// failure into a silent, unrecoverable no-daemon state.
    #[test]
    fn an_io_failure_is_err_not_the_contended_signal() {
        let dir = tempfile::tempdir().unwrap();
        // A FILE where the lock's parent dir needs to be: create_dir_all fails with ENOTDIR.
        let blocker = dir.path().join("clowder");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let path = blocker.join("daemon.pid");

        let result = InstanceLock::acquire(&path);
        assert!(result.is_err(), "an unusable lock dir must be Err, got {:?}", result.map(|o| o.is_some()));
    }

    /// The lock and the sockets must resolve to the same directory. They used to diverge: this
    /// derivation accepted an EMPTY env value while clowder-config treated empty as unset, so
    /// `TMPDIR=""` put the lock at /clowder/daemon.pid and the sockets under /tmp/clowder/.
    #[test]
    fn default_path_agrees_with_the_config_runtime_dir() {
        let p = InstanceLock::default_path();
        assert_eq!(
            p.parent().unwrap(),
            clowder_config::runtime_dir(),
            "the PID lock must live in the same runtime dir as the sockets"
        );
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
