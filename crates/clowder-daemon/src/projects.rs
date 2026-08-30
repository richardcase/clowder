// SPDX-License-Identifier: Apache-2.0

use crate::store::JsonStore;
use anyhow::{bail, Context, Result};
use clowder_workspace::{detect_kind, WorktreeLayout};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One registered project. `path` is canonical and is the record's identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub path: PathBuf,
    /// `"git"` or `"jj"` — `WorkspaceKind::as_str()` at the time it was added.
    pub kind: String,
}

/// The durable list of projects the user has added. Policy-free: it validates and persists,
/// but knows nothing about agents. "Refuse to remove while worktrees exist" lives on `Daemon`.
pub struct ProjectStore {
    store: JsonStore<ProjectRecord>,
    /// The same layout `Daemon` provisions with — constructed once in `Daemon::new_with_paths` and
    /// cloned in, so "where do worktrees live?" has exactly one answer across the daemon.
    layout: WorktreeLayout,
}

impl ProjectStore {
    pub fn new(path: PathBuf, layout: WorktreeLayout) -> Self {
        Self { store: JsonStore::new(path), layout }
    }

    /// `$CLOWDER_PROJECTS_FILE` › `$XDG_STATE_HOME/clowder/projects.json` ›
    /// `$HOME/.local/state/clowder/projects.json` — the same derivation as the agent registry.
    pub fn default_path() -> PathBuf {
        if let Ok(p) = std::env::var("CLOWDER_PROJECTS_FILE") {
            if !p.is_empty() { return PathBuf::from(p); }
        }
        let base = std::env::var("XDG_STATE_HOME").ok().filter(|s| !s.is_empty())
            .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.local/state")))
            .unwrap_or_else(|| "/tmp".to_string());
        PathBuf::from(base).join("clowder").join("projects.json")
    }

    pub fn list(&self) -> Vec<ProjectRecord> {
        self.store.load()
    }

    /// Is `canonical` a registered project? The caller must pass an already-canonical path.
    pub fn contains(&self, canonical: &Path) -> bool {
        self.store.load().iter().any(|r| r.path == canonical)
    }

    /// Register `path`. Idempotent: adding an already-registered project returns its record.
    /// Messages are user-facing — they surface in the app's error banner.
    pub fn add(&self, path: &Path) -> Result<ProjectRecord> {
        // Canonicalize FIRST: on macOS /tmp resolves to /private/tmp, and `spawn_agent`'s
        // registered-project check compares canonical paths. If only one side canonicalizes,
        // every spawn into such a project fails the check.
        let canonical = path
            .canonicalize()
            .with_context(|| format!("no such path: {}", path.display()))?;
        if !canonical.is_dir() {
            bail!("not a directory: {}", canonical.display());
        }
        let kind = detect_kind(&canonical)
            .ok_or_else(|| anyhow::anyhow!("not a git or jj repository: {}", canonical.display()))?;

        // Adding a clowder worktree as a project would nest branches inside branches. Check the
        // external base (where worktrees have lived since #65) AND the pre-#65 in-repo location:
        // existing worktrees are deliberately not migrated, so they must keep being rejected.
        //
        // Compare against a CANONICAL base: `canonical` is fully resolved, but the configured base
        // need not be (on macOS `/var` is a symlink to `/private/var`, and a user-set base may
        // contain symlinks of its own) — an uncanonical base would make every `starts_with` below
        // silently false. Best-effort, since the base legitimately may not exist yet.
        let layout = WorktreeLayout::new(
            self.layout.base().canonicalize().unwrap_or_else(|_| self.layout.base().to_path_buf()),
        );
        let legacy = Path::new(".clowder").join("worktrees");
        for rec in self.store.load() {
            if canonical.starts_with(layout.project_dir(&rec.path))
                || canonical.starts_with(rec.path.join(&legacy))
            {
                bail!(
                    "{} is a worktree of project {} — add the project, not its worktree",
                    canonical.display(),
                    rec.path.display()
                );
            }
        }
        // A worktree whose project is no longer registered is still not a project. This also stops
        // a base that happens to sit inside an unrelated repo from making every subdirectory look
        // addable — `detect_kind` walks ancestors, so it would find that outer repo's marker.
        if canonical.starts_with(layout.base()) {
            bail!(
                "{} is inside clowder's worktree directory — add the project it was created from",
                canonical.display()
            );
        }

        let rec = ProjectRecord { path: canonical, kind: kind.as_str().to_string() };
        let out = rec.clone();
        self.store.try_mutate(move |all| {
            if !all.iter().any(|r| r.path == rec.path) {
                all.push(rec);
            }
        })?;
        Ok(out)
    }

    /// Drop `path` from the list. Absent is not an error (idempotent).
    pub fn remove(&self, path: &Path) -> Result<()> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.store.try_mutate(|all| all.retain(|r| r.path != canonical))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A git repo (just the marker dir — `detect_kind` only looks for `.git`).
    fn git_dir() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        d
    }

    /// A store whose state file and worktree base both live under `state`.
    fn store_in(state: &Path) -> ProjectStore {
        ProjectStore::new(state.join("projects.json"), layout_in(state))
    }

    fn layout_in(state: &Path) -> WorktreeLayout {
        WorktreeLayout::new(state.join("worktrees"))
    }

    #[test]
    fn add_records_canonical_path_and_kind() {
        let repo = git_dir();
        let state = tempfile::tempdir().unwrap();
        let store = store_in(state.path());
        let rec = store.add(repo.path()).unwrap();
        // On macOS a tempdir path is NOT canonical (/var -> /private/var); the stored path must be.
        assert_eq!(rec.path, repo.path().canonicalize().unwrap());
        assert_eq!(rec.kind, "git");
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn add_is_idempotent() {
        let repo = git_dir();
        let state = tempfile::tempdir().unwrap();
        let store = store_in(state.path());
        store.add(repo.path()).unwrap();
        store.add(repo.path()).unwrap();
        assert_eq!(store.list().len(), 1, "adding twice must not duplicate");
    }

    #[test]
    fn add_matches_a_non_canonical_path_to_its_canonical_record() {
        // The whole point of canonicalizing: a caller passing an uncanonical path must
        // hit the SAME record, or spawn's registered-project check fails on macOS.
        let repo = git_dir();
        let state = tempfile::tempdir().unwrap();
        let store = store_in(state.path());
        let rec = store.add(repo.path()).unwrap();
        assert!(store.contains(&rec.path));
        let link_dir = tempfile::tempdir().unwrap();
        let link = link_dir.path().join("link");
        std::os::unix::fs::symlink(repo.path(), &link).unwrap();
        let via_link = store.add(&link).unwrap();
        assert_eq!(via_link.path, rec.path, "symlink must resolve to the same record");
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn add_rejects_a_non_repo() {
        let plain = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let store = store_in(state.path());
        let e = store.add(plain.path()).unwrap_err().to_string();
        assert!(e.contains("not a git or jj repository"), "unhelpful message: {e}");
    }

    #[test]
    fn add_rejects_a_missing_path_and_a_file() {
        let state = tempfile::tempdir().unwrap();
        let store = store_in(state.path());
        assert!(store.add(std::path::Path::new("/nope/does/not/exist")).is_err());
        let f = tempfile::NamedTempFile::new().unwrap();
        let e = store.add(f.path()).unwrap_err().to_string();
        assert!(e.contains("not a directory"), "unhelpful message: {e}");
    }

    #[test]
    fn add_rejects_a_path_inside_a_projects_legacy_in_repo_worktrees() {
        let repo = git_dir();
        let state = tempfile::tempdir().unwrap();
        let store = store_in(state.path());
        store.add(repo.path()).unwrap();
        let wt = repo.path().join(".clowder").join("worktrees").join("feat");
        std::fs::create_dir_all(&wt).unwrap();
        let e = store.add(&wt).unwrap_err().to_string();
        assert!(e.contains("worktree"), "unhelpful message: {e}");
    }

    #[test]
    fn add_rejects_a_path_inside_the_external_worktree_base() {
        // Since #65 a worktree lives at <base>/<slug>-<hash>/<name>, not inside the project.
        let repo = git_dir();
        let state = tempfile::tempdir().unwrap();
        let store = store_in(state.path());
        let rec = store.add(repo.path()).unwrap();

        let wt = layout_in(state.path()).worktree_path(&rec.path, "feat");
        std::fs::create_dir_all(wt.join(".git")).unwrap(); // looks like a repo, as a real worktree does
        let e = store.add(&wt).unwrap_err().to_string();
        assert!(e.contains("worktree"), "unhelpful message: {e}");
        assert!(e.contains(&rec.path.display().to_string()), "must name the project: {e}");
    }

    #[test]
    fn add_rejects_anything_under_the_base_even_for_an_unregistered_project() {
        // The project may have been removed, or the base may sit inside an unrelated repo whose
        // marker `detect_kind` finds by walking ancestors. Either way it is not a project.
        let state = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(state.path().join(".git")).unwrap(); // an enclosing repo
        let store = store_in(state.path());
        assert!(store.list().is_empty(), "no project registered");

        let stray = layout_in(state.path()).base().join("api-abc123def456").join("feat");
        std::fs::create_dir_all(&stray).unwrap();
        let e = store.add(&stray).unwrap_err().to_string();
        assert!(e.contains("clowder's worktree directory"), "unhelpful message: {e}");
    }

    #[test]
    fn remove_drops_the_record_and_is_ok_when_absent() {
        let repo = git_dir();
        let state = tempfile::tempdir().unwrap();
        let store = store_in(state.path());
        store.add(repo.path()).unwrap();
        store.remove(repo.path()).unwrap();
        assert!(store.list().is_empty());
        store.remove(repo.path()).unwrap(); // absent -> Ok, not an error
    }

    #[test]
    fn jj_wins_for_a_colocated_repo() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        std::fs::create_dir_all(d.path().join(".jj")).unwrap();
        let state = tempfile::tempdir().unwrap();
        let store = store_in(state.path());
        assert_eq!(store.add(d.path()).unwrap().kind, "jj");
    }
}
