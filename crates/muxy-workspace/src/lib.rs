use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceKind { Git, Jj }

#[derive(Clone, Debug)]
pub struct Workspace {
    pub path: PathBuf,
    pub branch: String,
    pub project: PathBuf,
    pub kind: WorkspaceKind,
}

pub trait WorkspaceDriver: Send + Sync {
    fn kind(&self) -> WorkspaceKind;
    /// Create an isolated working copy on a fresh branch under `project`'s repo.
    fn provision(&self, project: &Path, name: &str) -> Result<Workspace>;
    /// Finalize: commit any uncommitted work, remove the working copy, KEEP the branch.
    fn land(&self, ws: &Workspace) -> Result<()>;
    /// Throw away: remove the working copy and DELETE the branch.
    fn discard(&self, ws: &Workspace) -> Result<()>;
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
    fn kind(&self) -> WorkspaceKind { WorkspaceKind::Git }

    fn provision(&self, project: &Path, name: &str) -> Result<Workspace> {
        let branch = format!("muxy/{name}");
        let path = project.join(".muxy").join("worktrees").join(name);
        let path_str = path.to_string_lossy().to_string();
        // `git worktree add <path> -b <branch>` creates the dir + a new branch off HEAD.
        Self::git(project, &["worktree", "add", &path_str, "-b", &branch])?;
        Ok(Workspace { path, branch, project: project.to_path_buf(), kind: WorkspaceKind::Git })
    }

    fn land(&self, ws: &Workspace) -> Result<()> {
        let task = ws.branch.strip_prefix("muxy/").unwrap_or(&ws.branch);
        // Commit any uncommitted work onto the branch (only if dirty).
        let status = Command::new("git").arg("-C").arg(&ws.path).args(["status", "--porcelain"])
            .output().with_context(|| "git status")?;
        if !status.stdout.is_empty() {
            Self::git(&ws.path, &["add", "-A"])?;
            Self::git(&ws.path, &["commit", "-m", &format!("muxy: {task}")])?;
        }
        // Remove the (now clean) worktree; KEEP the branch.
        let path_str = ws.path.to_string_lossy().to_string();
        Self::git(&ws.project, &["worktree", "remove", &path_str])?;
        let _ = Command::new("git").arg("-C").arg(&ws.project).args(["worktree", "prune"]).output();
        Ok(())
    }

    fn discard(&self, ws: &Workspace) -> Result<()> {
        let path_str = ws.path.to_string_lossy().to_string();
        Self::git(&ws.project, &["worktree", "remove", "--force", &path_str])?;
        let _ = Command::new("git").arg("-C").arg(&ws.project).args(["worktree", "prune"]).output();
        Self::git(&ws.project, &["branch", "-D", &ws.branch])?;   // force-delete the unmerged branch
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

    fn branch_exists(repo: &Path, name: &str) -> bool {
        let out = Command::new("git").arg("-C").arg(repo).args(["branch", "--list", name]).output().unwrap();
        !out.stdout.is_empty()
    }

    #[test]
    fn land_commits_dirty_removes_worktree_keeps_branch() {
        let repo = init_repo();
        let d = GitWorktreeDriver;
        let ws = d.provision(repo.path(), "task-a").unwrap();
        std::fs::write(ws.path.join("work.txt"), b"agent output").unwrap();   // dirty
        d.land(&ws).unwrap();
        assert!(!ws.path.exists(), "worktree removed");
        assert!(branch_exists(repo.path(), "muxy/task-a"), "branch kept");
        let log = Command::new("git").arg("-C").arg(repo.path()).args(["log", "muxy/task-a", "--oneline"]).output().unwrap();
        assert!(String::from_utf8_lossy(&log.stdout).contains("muxy: task-a"), "dirty work committed");
    }

    #[test]
    fn land_clean_worktree_makes_no_extra_commit() {
        let repo = init_repo();
        let d = GitWorktreeDriver;
        let ws = d.provision(repo.path(), "task-c").unwrap();          // no changes
        d.land(&ws).unwrap();
        assert!(branch_exists(repo.path(), "muxy/task-c"));
        let count = Command::new("git").arg("-C").arg(repo.path()).args(["rev-list", "--count", "muxy/task-c"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "1", "only the initial commit — no muxy: commit");
    }

    #[test]
    fn discard_removes_worktree_and_deletes_branch() {
        let repo = init_repo();
        let d = GitWorktreeDriver;
        let ws = d.provision(repo.path(), "task-b").unwrap();
        d.discard(&ws).unwrap();
        assert!(!ws.path.exists(), "worktree removed");
        assert!(!branch_exists(repo.path(), "muxy/task-b"), "branch deleted");
    }

    #[test]
    fn provision_sets_git_kind() {
        let repo = init_repo();
        let ws = GitWorktreeDriver.provision(repo.path(), "task-k").unwrap();
        assert_eq!(ws.kind, WorkspaceKind::Git);
        assert_eq!(GitWorktreeDriver.kind(), WorkspaceKind::Git);
    }
}
