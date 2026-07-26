use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug)]
pub struct Workspace {
    pub path: PathBuf,
    pub branch: String,
    pub project: PathBuf,
}

pub trait WorkspaceDriver: Send + Sync {
    /// Create an isolated working copy on a fresh branch under `project`'s repo.
    fn provision(&self, project: &Path, name: &str) -> Result<Workspace>;
    /// Remove the working copy (best-effort prune of stale registrations).
    fn teardown(&self, ws: &Workspace) -> Result<()>;
}

pub struct GitWorktreeDriver;

impl GitWorktreeDriver {
    fn git(project: &Path, args: &[&str]) -> Result<()> {
        let out = Command::new("git")
            .arg("-C")
            .arg(project)
            .args(args)
            .output()
            .with_context(|| format!("failed to run git {args:?}"))?;
        if !out.status.success() {
            bail!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(())
    }
}

impl WorkspaceDriver for GitWorktreeDriver {
    fn provision(&self, project: &Path, name: &str) -> Result<Workspace> {
        let branch = format!("muxy/{name}");
        let path = project.join(".muxy").join("worktrees").join(name);
        let path_str = path.to_string_lossy().to_string();
        // `git worktree add <path> -b <branch>` creates the dir + a new branch off HEAD.
        Self::git(project, &["worktree", "add", &path_str, "-b", &branch])?;
        Ok(Workspace { path, branch, project: project.to_path_buf() })
    }

    fn teardown(&self, ws: &Workspace) -> Result<()> {
        let path_str = ws.path.to_string_lossy().to_string();
        Self::git(&ws.project, &["worktree", "remove", &path_str, "--force"])?;
        // prune stale registrations from the main repo (best-effort).
        let _ = Command::new("git")
            .arg("-C")
            .arg(&ws.project)
            .args(["worktree", "prune"])
            .output();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a temp git repo with one commit so `worktree add` has a valid HEAD.
    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let run = |args: &[&str]| {
            let ok = Command::new("git").arg("-C").arg(p).args(args).status().unwrap().success();
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(p.join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        dir
    }

    #[test]
    fn provision_creates_isolated_worktree_on_new_branch() {
        let repo = init_repo();
        let driver = GitWorktreeDriver;
        let ws = driver.provision(repo.path(), "task-a").unwrap();

        assert!(ws.path.is_dir(), "worktree dir not created");
        assert_eq!(ws.branch, "muxy/task-a");
        // README from the initial commit is present in the isolated copy.
        assert!(ws.path.join("README.md").is_file());
        // A file created only in the worktree is NOT in the main working copy.
        std::fs::write(ws.path.join("only_here.txt"), b"x").unwrap();
        assert!(!repo.path().join("only_here.txt").exists());
    }

    #[test]
    fn teardown_removes_worktree() {
        let repo = init_repo();
        let driver = GitWorktreeDriver;
        let ws = driver.provision(repo.path(), "task-b").unwrap();
        assert!(ws.path.is_dir());
        driver.teardown(&ws).unwrap();
        assert!(!ws.path.exists(), "worktree dir still present after teardown");
    }
}
