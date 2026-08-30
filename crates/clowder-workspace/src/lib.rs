// SPDX-License-Identifier: Apache-2.0

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

mod layout;
pub use layout::{branch_name, jj_workspace_name, task_from_branch, WorktreeLayout};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceKind { Git, Jj }

impl WorkspaceKind {
    pub fn as_str(&self) -> &'static str {
        match self { WorkspaceKind::Git => "git", WorkspaceKind::Jj => "jj" }
    }
    pub fn from_str(s: &str) -> Option<WorkspaceKind> {
        match s { "git" => Some(WorkspaceKind::Git), "jj" => Some(WorkspaceKind::Jj), _ => None }
    }
}

#[derive(Clone, Debug)]
pub struct Workspace {
    pub path: PathBuf,
    pub branch: String,
    pub project: PathBuf,
    pub kind: WorkspaceKind,
}

pub trait WorkspaceDriver: Send + Sync {
    fn kind(&self) -> WorkspaceKind;
    /// Create an isolated working copy on a fresh branch of `project`'s repo, at the destination
    /// `layout` picks — which since #65 is OUTSIDE the project. `project` must be canonical
    /// (see `WorktreeLayout::project_dir`).
    fn provision(&self, layout: &WorktreeLayout, project: &Path, name: &str) -> Result<Workspace>;
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

    fn provision(&self, layout: &WorktreeLayout, project: &Path, name: &str) -> Result<Workspace> {
        let branch = branch_name(name);
        // `prepare` creates the parent chain (the base may not exist yet) but NOT the leaf, which
        // `git worktree add` insists on creating itself.
        let path = layout.prepare(project, name)?;
        let path_str = path.to_string_lossy().to_string();
        // `git worktree add <path> -b <branch>` creates the dir + a new branch off HEAD.
        Self::git(project, &["worktree", "add", &path_str, "-b", &branch])?;
        Ok(Workspace { path, branch, project: project.to_path_buf(), kind: WorkspaceKind::Git })
    }

    fn land(&self, ws: &Workspace) -> Result<()> {
        let task = task_from_branch(&ws.branch);
        // Commit any uncommitted work onto the branch (only if dirty).
        let status = Command::new("git").arg("-C").arg(&ws.path).args(["status", "--porcelain"])
            .output().with_context(|| "git status")?;
        if !status.stdout.is_empty() {
            Self::git(&ws.path, &["add", "-A"])?;
            Self::git(&ws.path, &["commit", "-m", &format!("clowder: {task}")])?;
        }
        // Remove the (now clean) worktree; KEEP the branch.
        let path_str = ws.path.to_string_lossy().to_string();
        Self::git(&ws.project, &["worktree", "remove", &path_str])?;
        let _ = Command::new("git").arg("-C").arg(&ws.project).args(["worktree", "prune"]).output();
        Ok(())
    }

    fn discard(&self, ws: &Workspace) -> Result<()> {
        // Discard means "throw this away", so an already-absent worktree or branch is the desired
        // end state, not a failure. Each step is best-effort; the END STATE is what we assert, which
        // is more robust than matching on git's stderr.
        if ws.path.exists() {
            let path_str = ws.path.to_string_lossy().to_string();
            let _ = Self::git(&ws.project, &["worktree", "remove", "--force", &path_str]);
        }
        let _ = Command::new("git").arg("-C").arg(&ws.project).args(["worktree", "prune"]).output();

        // A worktree provisioned in a repo with NO COMMITS sits on an UNBORN branch: `worktree add -b`
        // succeeds (git infers `--orphan`) but no ref exists until the first commit, so there is
        // nothing to delete. Deleting unconditionally is what made Discard fail on such repos.
        if branch_exists(&ws.project, &ws.branch) {
            let _ = Self::git(&ws.project, &["branch", "-D", &ws.branch]);   // unmerged: force
        }

        if ws.path.exists() {
            bail!("could not remove worktree {}", ws.path.display());
        }
        Ok(())
    }
}

/// Does `refs/heads/<branch>` exist? False for an **unborn** branch — which is what a worktree
/// provisioned in a repo with no commits is checked out to.
fn branch_exists(project: &Path, branch: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(project)
        .args(["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A jujutsu workspace driver: each agent gets its own `jj workspace` (a working copy
/// with its own working-copy commit `@`). Sibling to GitWorktreeDriver; shells out to `jj`.
pub struct JjDriver;

impl JjDriver {
    fn jj(repo: &Path, args: &[&str]) -> Result<()> {
        let out = Command::new("jj")
            .arg("-R")
            .arg(repo)
            .args(args)
            .output()
            .with_context(|| format!("failed to run jj {args:?}"))?;
        if !out.status.success() {
            bail!("jj {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        }
        Ok(())
    }
}

impl WorkspaceDriver for JjDriver {
    fn kind(&self) -> WorkspaceKind {
        WorkspaceKind::Jj
    }

    fn provision(&self, layout: &WorktreeLayout, project: &Path, name: &str) -> Result<Workspace> {
        let branch = branch_name(name);
        // `jj workspace add` will not create leading directories, hence `prepare` — which is now
        // shared with the git driver rather than being this driver's private step.
        let path = layout.prepare(project, name)?;
        let ws_name = jj_workspace_name(name);
        let path_str = path.to_string_lossy().to_string();
        // Create a jj workspace at `path` with a fresh working-copy commit.
        Self::jj(project, &["workspace", "add", "--name", &ws_name, &path_str])?;
        Ok(Workspace { path, branch, project: project.to_path_buf(), kind: WorkspaceKind::Jj })
    }

    fn land(&self, ws: &Workspace) -> Result<()> {
        let ws_name = jj_workspace_name(task_from_branch(&ws.branch));
        // jj auto-snapshots the working copy into `@` — nothing to add/commit. Pin the
        // work under a bookmark so it survives forgetting the workspace, then detach + remove.
        Self::jj(&ws.path, &["bookmark", "set", &ws.branch, "-r", "@"])?;
        Self::jj(&ws.project, &["workspace", "forget", &ws_name])?;
        // Best-effort removal: work is already pinned under the bookmark, so transient lock errors must not fail the operation.
        let _ = std::fs::remove_dir_all(&ws.path);
        Ok(())
    }

    fn discard(&self, ws: &Workspace) -> Result<()> {
        let ws_name = jj_workspace_name(task_from_branch(&ws.branch));
        // Drop the working-copy change (best-effort — an empty `@` needn't block cleanup),
        // then detach + remove the workspace. No bookmark was created, so nothing persists.
        // Every step is best-effort and the END STATE is asserted instead, mirroring the git
        // driver: an already-forgotten workspace is the desired outcome, not an error.
        let _ = Self::jj(&ws.path, &["abandon", "-r", "@"]);
        let _ = Self::jj(&ws.project, &["workspace", "forget", &ws_name]);
        let _ = std::fs::remove_dir_all(&ws.path);
        if ws.path.exists() {
            bail!("could not remove jj workspace {}", ws.path.display());
        }
        Ok(())
    }
}

/// The workspace kind for `project`, or `None` if it is not a repo at all.
/// Walks `project` and its ancestors; `.jj` wins over `.git` (colocated repos), matching
/// jj's own behaviour. Note `.git` is `.exists()` rather than `.is_dir()`: a linked git
/// worktree records `.git` as a FILE.
pub fn detect_kind(project: &Path) -> Option<WorkspaceKind> {
    let mut cur = Some(project);
    while let Some(dir) = cur {
        if dir.join(".jj").is_dir() {
            return Some(WorkspaceKind::Jj);
        }
        if dir.join(".git").exists() {
            return Some(WorkspaceKind::Git);
        }
        cur = dir.parent();
    }
    None
}

/// Validate a worktree/workspace name. The name becomes BOTH a git ref (`clowder/<name>`)
/// and the final path component of the worktree (see `WorktreeLayout`), so it must be safe as both.
/// Messages are user-facing — they surface directly in the app's error banner.
///
/// If you add or change a rule here, add a case to `docs/protocol/fixtures/worktree-names.json`
/// and mirror the rule in Swift's `NewWorktreeForm.nameError` (`macos/Sources/ClowderCore/SheetForms.swift`)
/// — that fixture is what `agrees_with_the_shared_name_cases` (below) and its Swift counterpart
/// both check the two implementations against.
pub fn validate_workspace_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("worktree name must not be empty");
    }
    let len = name.chars().count();
    if len > 64 {
        bail!("worktree name must be 64 characters or fewer (got {len})");
    }
    if name == "." || name == ".." {
        bail!("worktree name must not be {name:?}");
    }
    // Not redundant with the `contains("..")` check below despite `".."` matching both: this
    // exact-match check produces a clearer, more specific message for that one case — keep it.
    if name.contains("..") {
        bail!("worktree name must not contain '..'");
    }
    if name.ends_with(".lock") {
        bail!("worktree name must not end with '.lock' (git reserves that suffix)");
    }
    if name.ends_with('.') {
        bail!("worktree name must not end with '.' (git rejects it as a ref)");
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-'))
    {
        bail!("worktree name must contain only letters, digits, '.', '_' or '-' (found {c:?})");
    }
    // Checked last so the charset message wins for e.g. "  x": a leading '.' or '-' is legal
    // in a path but reads as a hidden file or a CLI flag.
    let first = name.chars().next().unwrap();
    if first == '.' || first == '-' {
        bail!("worktree name must not start with {first:?}");
    }
    Ok(())
}

/// Pick a workspace driver for `project`. Falls back to git when `project` is not a repo,
/// preserving the pre-M10 contract; callers that need to REJECT a non-repo use `detect_kind`.
pub fn driver_for(project: &Path) -> Arc<dyn WorkspaceDriver> {
    detect_kind(project).map(driver_for_kind).unwrap_or_else(|| Arc::new(GitWorktreeDriver))
}

/// The driver matching a provisioned workspace's kind — used to route land/discard.
pub fn driver_for_kind(kind: WorkspaceKind) -> Arc<dyn WorkspaceDriver> {
    match kind {
        WorkspaceKind::Git => Arc::new(GitWorktreeDriver),
        WorkspaceKind::Jj => Arc::new(JjDriver),
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

    /// A worktree base OUTSIDE any repo, plus its layout — the post-#65 arrangement.
    /// Bind both: dropping the `TempDir` deletes every worktree provisioned under it.
    fn base() -> (tempfile::TempDir, WorktreeLayout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = WorktreeLayout::new(dir.path());
        (dir, layout)
    }

    #[test]
    fn provision_creates_isolated_worktree_on_new_branch() {
        let repo = init_repo();
        let driver = GitWorktreeDriver;
        let (_base, lay) = base();
        let ws = driver.provision(&lay, repo.path(), "task-a").unwrap();

        assert!(ws.path.is_dir(), "worktree dir not created");
        assert_eq!(ws.branch, "clowder/task-a");
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
        let (_base, lay) = base();
        let ws = d.provision(&lay, repo.path(), "task-a").unwrap();
        std::fs::write(ws.path.join("work.txt"), b"agent output").unwrap();   // dirty
        d.land(&ws).unwrap();
        assert!(!ws.path.exists(), "worktree removed");
        assert!(branch_exists(repo.path(), "clowder/task-a"), "branch kept");
        let log = Command::new("git").arg("-C").arg(repo.path()).args(["log", "clowder/task-a", "--oneline"]).output().unwrap();
        assert!(String::from_utf8_lossy(&log.stdout).contains("clowder: task-a"), "dirty work committed");
    }

    #[test]
    fn land_clean_worktree_makes_no_extra_commit() {
        let repo = init_repo();
        let d = GitWorktreeDriver;
        let (_base, lay) = base();
        let ws = d.provision(&lay, repo.path(), "task-c").unwrap();          // no changes
        d.land(&ws).unwrap();
        assert!(branch_exists(repo.path(), "clowder/task-c"));
        let count = Command::new("git").arg("-C").arg(repo.path()).args(["rev-list", "--count", "clowder/task-c"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "1", "only the initial commit — no clowder: commit");
    }

    #[test]
    fn discard_removes_worktree_and_deletes_branch() {
        let repo = init_repo();
        let d = GitWorktreeDriver;
        let (_base, lay) = base();
        let ws = d.provision(&lay, repo.path(), "task-b").unwrap();
        d.discard(&ws).unwrap();
        assert!(!ws.path.exists(), "worktree removed");
        assert!(!branch_exists(repo.path(), "clowder/task-b"), "branch deleted");
    }

    /// A repo with NO commits: `git init` and nothing else. `HEAD` is an unborn `main`.
    fn init_empty_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let run = |args: &[&str]| {
            assert!(Command::new("git").arg("-C").arg(p).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        dir
    }

    #[test]
    fn discard_succeeds_when_the_branch_is_unborn() {
        // The reported bug: in a repo with no commits, `worktree add -b` succeeds (git infers
        // `--orphan`) but the branch ref never exists, so `branch -D` used to fail the whole discard.
        let repo = init_empty_repo();
        let (_base, lay) = base();
        let ws = GitWorktreeDriver.provision(&lay, repo.path(), "feat").unwrap();
        assert!(ws.path.is_dir(), "provision must still create the worktree on an empty repo");
        assert!(!branch_exists(repo.path(), &ws.branch), "the branch should be unborn");

        GitWorktreeDriver.discard(&ws).unwrap();
        assert!(!ws.path.exists(), "worktree removed");
    }

    #[test]
    fn discard_is_idempotent() {
        let repo = init_repo();
        let (_base, lay) = base();
        let ws = GitWorktreeDriver.provision(&lay, repo.path(), "task-i").unwrap();
        GitWorktreeDriver.discard(&ws).unwrap();
        // A second discard must not fail: the end state is already what discard wants.
        GitWorktreeDriver.discard(&ws).unwrap();
        assert!(!ws.path.exists());
    }

    #[test]
    fn discard_after_land_is_ok_and_removes_the_branch() {
        let repo = init_repo();
        let (_base, lay) = base();
        let ws = GitWorktreeDriver.provision(&lay, repo.path(), "task-al").unwrap();
        std::fs::write(ws.path.join("work.txt"), b"x").unwrap();
        GitWorktreeDriver.land(&ws).unwrap();            // removes the worktree, KEEPS the branch
        assert!(branch_exists(repo.path(), &ws.branch));
        GitWorktreeDriver.discard(&ws).unwrap();         // worktree already gone
        assert!(!branch_exists(repo.path(), &ws.branch), "discard still deletes the branch");
    }

    #[test]
    fn discard_reports_when_the_worktree_cannot_be_removed() {
        // A path that exists but is not a registered worktree: `worktree remove` cannot remove it,
        // so the end-state check must fail rather than silently reporting success.
        let repo = init_repo();
        let stray = repo.path().join("not-a-worktree");
        std::fs::create_dir_all(&stray).unwrap();
        let ws = Workspace {
            path: stray.clone(),
            branch: "clowder/nope".into(),
            project: repo.path().to_path_buf(),
            kind: WorkspaceKind::Git,
        };
        let e = GitWorktreeDriver.discard(&ws).unwrap_err().to_string();
        assert!(e.contains("could not remove worktree"), "unhelpful message: {e}");
        assert!(stray.exists(), "nothing was destroyed");
    }

    #[test]
    fn branch_exists_is_false_for_an_unborn_branch() {
        let repo = init_empty_repo();
        assert!(!branch_exists(repo.path(), "main"), "an unborn HEAD has no ref");
        let with_commits = init_repo();
        let head = Command::new("git").arg("-C").arg(with_commits.path())
            .args(["symbolic-ref", "--short", "HEAD"]).output().unwrap();
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
        assert!(branch_exists(with_commits.path(), &head), "a born branch is found");
    }

    #[test]
    fn provision_sets_git_kind() {
        let repo = init_repo();
        let (_base, lay) = base();
        let ws = GitWorktreeDriver.provision(&lay, repo.path(), "task-k").unwrap();
        assert_eq!(ws.kind, WorkspaceKind::Git);
        assert_eq!(GitWorktreeDriver.kind(), WorkspaceKind::Git);
    }

    #[test]
    fn git_provision_puts_the_worktree_outside_the_project() {
        // The point of #65: the project directory is left completely untouched.
        let repo = init_repo();
        let (_base, lay) = base();
        let ws = GitWorktreeDriver.provision(&lay, repo.path(), "feat").unwrap();

        assert!(ws.path.starts_with(lay.base()), "{:?} not under the base", ws.path);
        assert!(!ws.path.starts_with(repo.path()), "{:?} is inside the project", ws.path);
        assert!(!repo.path().join(".clowder").exists(), "nothing written into the project");
        assert_eq!(ws.path, lay.worktree_path(repo.path(), "feat"));
        // Still a real, populated worktree.
        assert!(ws.path.join("README.md").is_file());
    }

    #[test]
    fn git_land_works_on_a_worktree_outside_the_project() {
        // land/discard use the STORED ws.path, so crossing out of the project must not affect them.
        let repo = init_repo();
        let (_base, lay) = base();
        let ws = GitWorktreeDriver.provision(&lay, repo.path(), "feat").unwrap();
        assert!(!ws.path.starts_with(repo.path()));
        std::fs::write(ws.path.join("work.txt"), b"agent output").unwrap();
        GitWorktreeDriver.land(&ws).unwrap();
        assert!(!ws.path.exists());
        assert!(branch_exists(repo.path(), "clowder/feat"), "work landed on the branch");
    }

    #[test]
    fn provision_into_a_base_inside_another_repo_leaves_it_clean() {
        // The dotfiles hazard: $XDG_DATA_HOME can sit inside a repo someone version-controls.
        // `prepare`'s `.gitignore` is what stops the agent fleet showing up there (and, for a
        // colocated jj, being auto-snapshotted into its operation log).
        let outer = init_repo();
        let project = init_repo();
        let lay = WorktreeLayout::new(outer.path().join("data").join("clowder").join("worktrees"));

        let ws = GitWorktreeDriver.provision(&lay, project.path(), "feat").unwrap();
        assert!(ws.path.starts_with(outer.path()), "test must actually nest inside the outer repo");

        let out = Command::new("git").arg("-C").arg(outer.path())
            .args(["status", "--porcelain"]).output().unwrap();
        let status = String::from_utf8_lossy(&out.stdout);
        assert!(status.is_empty(), "outer repo polluted by the worktree base:\n{status}");
    }

    fn jj_available() -> bool {
        Command::new("jj").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    /// A fresh jj repo with one snapshotted file. Returns the TempDir (kept alive).
    fn init_jj_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let run = |args: &[&str]| {
            let ok = Command::new("jj").arg("-R").arg(p).args(args)
                .env("JJ_USER", "clowder-test").env("JJ_EMAIL", "clowder@test.invalid")
                .status().unwrap().success();
            assert!(ok, "jj {args:?} failed");
        };
        // `jj git init <p>` initialises in place (validate exact form in Step 0).
        let ok = Command::new("jj").args(["git", "init", &p.to_string_lossy()])
            .env("JJ_USER", "clowder-test").env("JJ_EMAIL", "clowder@test.invalid")
            .status().unwrap().success();
        assert!(ok, "jj git init failed");
        std::fs::write(p.join("README.md"), b"init").unwrap();
        run(&["status"]); // force a working-copy snapshot
        dir
    }

    fn jj_bookmark_exists(repo: &Path, name: &str) -> bool {
        let out = Command::new("jj").arg("-R").arg(repo).args(["bookmark", "list"])
            .env("JJ_USER", "clowder-test").env("JJ_EMAIL", "clowder@test.invalid")
            .output().unwrap();
        String::from_utf8_lossy(&out.stdout).contains(name)
    }

    fn jj_commit_has_file(repo: &Path, rev: &str, file: &str) -> bool {
        let out = Command::new("jj").arg("-R").arg(repo).args(["file", "list", "-r", rev])
            .env("JJ_USER", "clowder-test").env("JJ_EMAIL", "clowder@test.invalid")
            .output().unwrap();
        String::from_utf8_lossy(&out.stdout).contains(file)
    }

    #[test]
    fn jj_provision_creates_workspace_and_sets_jj_kind() {
        if !jj_available() { return; }
        let repo = init_jj_repo();
        let (_base, lay) = base();
        let ws = JjDriver.provision(&lay, repo.path(), "task-j").unwrap();
        assert!(ws.path.is_dir(), "jj workspace dir not created");
        assert_eq!(ws.branch, "clowder/task-j");
        assert_eq!(ws.kind, WorkspaceKind::Jj);
        assert_eq!(JjDriver.kind(), WorkspaceKind::Jj);
    }

    #[test]
    fn jj_land_sets_bookmark_forgets_workspace_removes_dir() {
        if !jj_available() { return; }
        let repo = init_jj_repo();
        let (_base, lay) = base();
        let ws = JjDriver.provision(&lay, repo.path(), "task-l").unwrap();
        std::fs::write(ws.path.join("work.txt"), b"agent output").unwrap();
        JjDriver.land(&ws).unwrap();
        assert!(!ws.path.exists(), "workspace dir should be removed after land");
        assert!(jj_bookmark_exists(repo.path(), "clowder/task-l"), "bookmark should be kept");
        assert!(jj_commit_has_file(repo.path(), "clowder/task-l", "work.txt"),
                "landed bookmark must contain the agent's work");
    }

    #[test]
    fn jj_discard_forgets_workspace_and_leaves_no_bookmark() {
        if !jj_available() { return; }
        let repo = init_jj_repo();
        let (_base, lay) = base();
        let ws = JjDriver.provision(&lay, repo.path(), "task-d").unwrap();
        std::fs::write(ws.path.join("work.txt"), b"throwaway").unwrap();
        JjDriver.discard(&ws).unwrap();
        assert!(!ws.path.exists(), "workspace dir should be removed after discard");
        assert!(!jj_bookmark_exists(repo.path(), "clowder/task-d"), "discard must not leave a bookmark");
    }

    #[test]
    fn jj_provision_puts_the_workspace_outside_the_project() {
        if !jj_available() { return; }
        let repo = init_jj_repo();
        let (_base, lay) = base();
        let ws = JjDriver.provision(&lay, repo.path(), "feat").unwrap();
        assert!(ws.path.starts_with(lay.base()), "{:?} not under the base", ws.path);
        assert!(!ws.path.starts_with(repo.path()), "{:?} is inside the project", ws.path);
        assert!(!repo.path().join(".clowder").exists(), "nothing written into the project");
    }

    #[test]
    fn driver_for_picks_git_for_a_git_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        assert_eq!(driver_for(dir.path()).kind(), WorkspaceKind::Git);
    }

    #[test]
    fn driver_for_picks_jj_when_dot_jj_present_even_with_git() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join(".jj")).unwrap();
        assert_eq!(driver_for(dir.path()).kind(), WorkspaceKind::Jj); // .jj wins
    }

    #[test]
    fn driver_for_finds_marker_in_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".jj")).unwrap();
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(driver_for(&nested).kind(), WorkspaceKind::Jj);
    }

    #[test]
    fn driver_for_kind_maps_both() {
        assert_eq!(driver_for_kind(WorkspaceKind::Git).kind(), WorkspaceKind::Git);
        assert_eq!(driver_for_kind(WorkspaceKind::Jj).kind(), WorkspaceKind::Jj);
    }

    #[test]
    fn workspace_kind_string_roundtrip() {
        for k in [WorkspaceKind::Git, WorkspaceKind::Jj] {
            assert_eq!(WorkspaceKind::from_str(k.as_str()), Some(k));
        }
        assert_eq!(WorkspaceKind::from_str("nope"), None);
    }

    #[test]
    fn detect_kind_none_when_not_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect_kind(dir.path()), None);
    }

    #[test]
    fn detect_kind_git_and_jj_with_jj_winning() {
        let git = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(git.path().join(".git")).unwrap();
        assert_eq!(detect_kind(git.path()), Some(WorkspaceKind::Git));

        let jj = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(jj.path().join(".jj")).unwrap();
        assert_eq!(detect_kind(jj.path()), Some(WorkspaceKind::Jj));

        // Colocated: .jj wins, matching jj's own behaviour.
        let both = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(both.path().join(".git")).unwrap();
        std::fs::create_dir_all(both.path().join(".jj")).unwrap();
        assert_eq!(detect_kind(both.path()), Some(WorkspaceKind::Jj));
    }

    #[test]
    fn detect_kind_finds_marker_in_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(detect_kind(&nested), Some(WorkspaceKind::Git));
    }

    #[test]
    fn detect_kind_treats_git_file_as_git() {
        // A linked worktree has `.git` as a FILE, not a dir — `.exists()` must accept both.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".git"), b"gitdir: /elsewhere").unwrap();
        assert_eq!(detect_kind(dir.path()), Some(WorkspaceKind::Git));
    }

    #[test]
    fn validate_workspace_name_accepts_reasonable_names() {
        for ok in ["a", "add-projects", "fix_bug", "v1.2", "M10a", "a-b_c.d"] {
            assert!(validate_workspace_name(ok).is_ok(), "should accept {ok:?}");
        }
    }

    #[test]
    fn validate_workspace_name_rejects_unsafe_names() {
        let too_long = "a".repeat(65);
        let cases: [&str; 12] = [
            "",              // empty
            &too_long,       // > 64 chars
            ".",             // path component
            "..",            // path component
            "a..b",          // traversal fragment
            "x.lock",        // git reserves the .lock suffix
            "my feature",    // space
            "feat/x",        // slash would nest the path AND the ref
            ".hidden",       // leading dot
            "-dash",         // leading dash reads as a flag
            "caf\u{e9}",     // non-ASCII
            "v1.",           // trailing dot — git rejects a ref ending in '.'
        ];
        for bad in cases {
            assert!(validate_workspace_name(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn validate_workspace_name_errors_name_the_problem() {
        // The message is user-facing (it surfaces in the app's error banner), so it must say
        // what is wrong, not just that something is.
        let e = validate_workspace_name("my feature").unwrap_err().to_string();
        assert!(e.contains("letters"), "unhelpful message: {e}");
        let e = validate_workspace_name(&"a".repeat(65)).unwrap_err().to_string();
        assert!(e.contains("64"), "unhelpful message: {e}");
    }

    #[test]
    fn agrees_with_the_shared_name_cases() {
        #[derive(serde::Deserialize)]
        struct Case { name: String, valid: bool }
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/protocol/fixtures/worktree-names.json");
        let raw = std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("missing {}: {e}", p.display()));
        for c in serde_json::from_str::<Vec<Case>>(&raw).unwrap() {
            assert_eq!(validate_workspace_name(&c.name).is_ok(), c.valid,
                       "disagreed on {:?} — if you changed a rule, update the shared cases and the Swift mirror",
                       c.name);
        }
    }
}
