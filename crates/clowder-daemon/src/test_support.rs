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

/// A temp git repo with **no commits** — `HEAD` is an unborn branch. `git worktree add -b`
/// still succeeds here (git infers `--orphan`), but the new branch has no ref until something
/// is committed, which is the state that used to break Discard.
pub(crate) fn init_empty_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let run = |args: &[&str]| {
        assert!(Command::new("git").arg("-C").arg(p).args(args).status().unwrap().success(),
                "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@t.test"]);
    run(&["config", "user.name", "t"]);
    dir
}

/// Make every `git commit` in `repo` fail, deterministically, via a `pre-commit` hook that exits 1.
/// Used to drive `land` into its failure path without racing anything.
pub(crate) fn install_failing_precommit_hook(repo: &std::path::Path) {
    let hooks = repo.join(".githooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("pre-commit");
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(Command::new("git").arg("-C").arg(repo)
        .args(["config", "core.hooksPath", &hooks.to_string_lossy()])
        .status().unwrap().success());
}
