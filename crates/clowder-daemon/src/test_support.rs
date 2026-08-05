//! Shared helpers for this crate's tests. Compiled only under `cfg(test)`.

use std::process::Command;

/// A temp git repo with one commit, so `git worktree add` has a valid HEAD.
pub(crate) fn init_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let run = |args: &[&str]| {
        assert!(Command::new("git").arg("-C").arg(p).args(args).status().unwrap().success(),
                "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@t.test"]);
    run(&["config", "user.name", "t"]);
    std::fs::write(p.join("README.md"), b"hi").unwrap();
    run(&["add", "."]);
    run(&["commit", "-qm", "init"]);
    dir
}
