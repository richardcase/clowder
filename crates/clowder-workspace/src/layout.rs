//! Where a worktree goes, and what its branch and jj workspace are called.
//!
//! Before issue #65 the destination was `<project>/.clowder/worktrees/<name>`, spelled out inline in
//! both drivers, in the daemon's pre-flight collision check, and in the "don't add a worktree as a
//! project" guard — four copies that agreed only by coincidence. This module is the one place that
//! knows the answer; the daemon holds a single `WorktreeLayout` and hands the same one to all of them.
//!
//! The split matters: only the *destination* depends on the configured base, so `branch_name`,
//! `task_from_branch` and `jj_workspace_name` are free functions. That is what lets `land`/`discard`
//! stay base-free — they work from the stored `Workspace` and never recompute a path.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The directory tree agent worktrees live in: `<base>/<slug>-<hash>/<name>`.
///
/// Cheap to clone (one `PathBuf`) — the daemon and its `ProjectStore` deliberately share one value
/// so the spawner and the "is this a worktree?" guard cannot disagree about where worktrees live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeLayout {
    base: PathBuf,
}

impl WorktreeLayout {
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// The root all worktrees live under. Typically `clowder_config::Config::worktree_base`.
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// The directory holding every worktree of `project`: `<base>/<slug>-<hash>`.
    ///
    /// **`project` MUST already be canonical.** `/tmp/api` and `/private/tmp/api` hash differently,
    /// so a caller that skips canonicalization gets a second, parallel directory. Canonicalizing
    /// here instead would be worse: the path would then depend on the directory *currently existing*,
    /// so a project on a temporarily-unmounted volume would silently relocate its own worktrees.
    /// Both real callers (`Daemon::spawn_agent`, `ProjectStore::add`) canonicalize first.
    pub fn project_dir(&self, project: &Path) -> PathBuf {
        self.base.join(format!("{}-{}", slug(project), short_hash(project)))
    }

    /// Where the worktree named `name` of `project` belongs. See `project_dir` on canonicality.
    pub fn worktree_path(&self, project: &Path, name: &str) -> PathBuf {
        self.project_dir(project).join(name)
    }

    /// `worktree_path`, with the parent directory created and the base marked git-ignored.
    ///
    /// Creates the PARENT only, never the leaf: `git worktree add` and `jj workspace add` both
    /// insist on creating the destination themselves.
    pub fn prepare(&self, project: &Path, name: &str) -> Result<PathBuf> {
        let path = self.worktree_path(project, name);
        let parent = self.project_dir(project);
        std::fs::create_dir_all(&parent)
            .with_context(|| format!("create worktree parent dir {}", parent.display()))?;
        self.mark_ignored();
        Ok(path)
    }

    /// Drop a `*`-matching `.gitignore` at the base. Best effort, and only if absent.
    ///
    /// The default base is under `$XDG_DATA_HOME`, which for anyone who version-controls their
    /// dotfiles (chezmoi, yadm) can sit INSIDE another repo. Without this, every agent worktree
    /// shows up untracked in that repo — and a colocated `jj`, which auto-snapshots on every
    /// command, would suck the entire agent fleet (node_modules, build artifacts, the lot) into its
    /// operation log. Both git and jj honour this file, and it sits ABOVE the linked worktrees, so
    /// neither their contents nor their own indexes are affected.
    fn mark_ignored(&self) {
        let gitignore = self.base.join(".gitignore");
        if !gitignore.exists() {
            let _ = std::fs::write(
                &gitignore,
                "# clowder-managed worktrees; never part of an enclosing repo\n*\n",
            );
        }
    }
}

/// The branch (git) / bookmark (jj) a worktree's work lands on.
pub fn branch_name(name: &str) -> String {
    format!("clowder/{name}")
}

/// The worktree name back out of a branch. A branch clowder did not create passes through
/// unchanged — callers use this for commit messages and workspace names, never for identity.
pub fn task_from_branch(branch: &str) -> &str {
    branch.strip_prefix("clowder/").unwrap_or(branch)
}

/// The `jj workspace` name for a worktree. Deliberately `-` rather than the branch's `/`: jj
/// workspace names are a flat namespace, not refs.
pub fn jj_workspace_name(name: &str) -> String {
    format!("clowder-{name}")
}

/// A human-readable, path-safe stand-in for the project's directory name.
///
/// Purely cosmetic — `short_hash` supplies the uniqueness. This exists so a user can find their
/// project's worktrees by eye instead of by hash.
fn slug(project: &Path) -> String {
    let raw = project.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut s: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '-' })
        .collect();
    s.truncate(32); // every char is ASCII by now, so this is always on a char boundary
    let s = s.trim_start_matches(['.', '-']).to_string();
    // Empty when the project is `/`, or a name made entirely of leading dots/dashes.
    if s.is_empty() { "project".to_string() } else { s }
}

/// The project-path digest that keeps same-named projects apart.
///
/// FNV-1a 64 is spelled out here rather than reaching for `std`'s `DefaultHasher` because this value
/// lands in a PATH that must stay byte-identical across daemon restarts, Rust releases and machines:
/// `DefaultHasher`'s algorithm is documented as NOT stable between Rust versions, so a toolchain
/// bump would silently relocate every project directory and orphan every worktree on disk while the
/// persisted `AgentRecord::worktree_path` still pointed at the old ones.
///
/// Keeps the LEADING 12 hex digits: FNV propagates low→high, so the high bits are the better-mixed
/// ones, and 48 bits puts a collision far beyond practical concern (32 would not — that is roughly
/// one-in-a-million at only ~100 projects).
fn short_hash(project: &Path) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a 64 offset basis
    for b in project.to_string_lossy().as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3); // FNV-1a 64 prime
    }
    format!("{h:016x}")[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_path_is_base_slug_hash_name() {
        let l = WorktreeLayout::new("/base");
        let p = l.worktree_path(Path::new("/Users/rc/code/clowder"), "fix-login");
        let dir = l.project_dir(Path::new("/Users/rc/code/clowder"));
        assert_eq!(p, dir.join("fix-login"));
        assert_eq!(p.parent().unwrap(), dir);
        assert!(dir.starts_with("/base"));
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("clowder-"), "{name}");
        assert_eq!(name.len(), "clowder-".len() + 12, "{name}");
    }

    #[test]
    fn same_basename_different_projects_do_not_collide() {
        let l = WorktreeLayout::new("/base");
        let a = l.project_dir(Path::new("/a/api"));
        let b = l.project_dir(Path::new("/b/api"));
        assert_ne!(a, b);
        // Both stay recognisable as `api` — only the hash differs.
        for d in [&a, &b] {
            assert!(d.file_name().unwrap().to_string_lossy().starts_with("api-"), "{d:?}");
        }
    }

    #[test]
    fn project_dir_is_stable_across_calls_and_layouts() {
        let a = WorktreeLayout::new("/base").project_dir(Path::new("/x/y"));
        let b = WorktreeLayout::new("/base").project_dir(Path::new("/x/y"));
        assert_eq!(a, b);
    }

    /// GOLDEN. This literal is a wire format: it is baked into every worktree path on every user's
    /// disk, while `AgentRecord::worktree_path` holds the absolute path it produced. Changing the
    /// hash or the slug rules orphans all of them — an existing agent's directory would no longer be
    /// found by the collision check or the `ProjectStore` guard. If this test fails, you have made a
    /// BREAKING change; do not "fix" it by updating the literal without a migration story.
    ///
    /// These digests are the standard FNV-1a 64 of the path bytes, independently reproducible:
    ///   python3 -c "
    ///   h=0xcbf29ce484222325
    ///   for b in b'/tmp/api': h=((h^b)*0x100000001b3)&0xFFFFFFFFFFFFFFFF
    ///   print('%016x'%h)"      # -> e3cdda71ea1df834
    #[test]
    fn project_dir_name_is_pinned() {
        let l = WorktreeLayout::new("/base");
        assert_eq!(
            l.project_dir(Path::new("/tmp/api")),
            PathBuf::from("/base/api-e3cdda71ea1d")
        );
        assert_eq!(
            l.project_dir(Path::new("/Users/rc/code/clowder")),
            PathBuf::from("/base/clowder-329dae757627")
        );
    }

    #[test]
    fn slug_sanitizes_odd_basenames() {
        assert_eq!(slug(Path::new("/a/my project")), "my-project"); // space
        assert_eq!(slug(Path::new("/a/caf\u{e9}")), "caf-"); // non-ASCII → '-' (multi-byte, one char)
        assert_eq!(slug(Path::new("/a/.dotfiles")), "dotfiles"); // leading '.' trimmed
        assert_eq!(slug(Path::new("/a/-dashed")), "dashed"); // leading '-' trimmed
        assert_eq!(slug(Path::new("/a/a_b.c-d")), "a_b.c-d"); // legal charset preserved
        assert_eq!(slug(Path::new("/")), "project"); // no file_name at all
        assert_eq!(slug(Path::new("/a/...")), "project"); // trims away to nothing
        assert_eq!(slug(Path::new(&format!("/a/{}", "x".repeat(80)))), "x".repeat(32)); // truncated
    }

    #[test]
    fn slug_collisions_are_disambiguated_by_the_hash() {
        // Two different projects whose slugs are identical must still get distinct directories.
        let l = WorktreeLayout::new("/base");
        assert_ne!(l.project_dir(Path::new("/a/my project")), l.project_dir(Path::new("/a/my-project")));
    }

    #[test]
    fn branch_name_and_task_from_branch_round_trip() {
        assert_eq!(branch_name("fix-login"), "clowder/fix-login");
        assert_eq!(task_from_branch(&branch_name("fix-login")), "fix-login");
        // A branch clowder did not create passes through unchanged.
        assert_eq!(task_from_branch("main"), "main");
        assert_eq!(task_from_branch("feature/clowder/x"), "feature/clowder/x");
    }

    #[test]
    fn jj_workspace_name_is_dash_separated() {
        assert_eq!(jj_workspace_name("fix-login"), "clowder-fix-login");
    }

    #[test]
    fn prepare_creates_the_parent_but_not_the_leaf() {
        let tmp = tempfile::tempdir().unwrap();
        // A base that does not exist yet — `prepare` must create the whole chain.
        let base = tmp.path().join("does").join("not").join("exist");
        let l = WorktreeLayout::new(&base);
        let project = Path::new("/some/project");

        let p = l.prepare(project, "feat").unwrap();
        assert_eq!(p, l.worktree_path(project, "feat"));
        assert!(l.project_dir(project).is_dir(), "parent must exist");
        assert!(!p.exists(), "leaf must NOT exist — git/jj insist on creating it");

        // Idempotent.
        assert_eq!(l.prepare(project, "feat").unwrap(), p);
    }

    #[test]
    fn prepare_marks_the_base_git_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("wt");
        let l = WorktreeLayout::new(&base);
        l.prepare(Path::new("/some/project"), "feat").unwrap();

        let gi = base.join(".gitignore");
        assert!(std::fs::read_to_string(&gi).unwrap().contains("\n*\n"));

        // An existing .gitignore is never clobbered — the base may be user-configured.
        std::fs::write(&gi, "mine\n").unwrap();
        l.prepare(Path::new("/some/project"), "other").unwrap();
        assert_eq!(std::fs::read_to_string(&gi).unwrap(), "mine\n");
    }
}
