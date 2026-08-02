# clowder M9 — Agent survival across a daemon restart

## Context

Today every agent is a PTY **child of the daemon**, and all agent state is in-memory, so a daemon
restart (crash or app-quit → relaunch) kills every running agent and forgets they existed. M9 makes
running agents **survive a daemon restart**: after the daemon comes back, the agents are running again in
their worktrees.

### What exists (ground truth, verified 2026-08-02)

- **`Pane` is the sole owner of the PTY master** (`crates/clowder-daemon/src/pane.rs:17-26`, `:86`
  `master: Mutex::new(pair.master)`). On daemon exit the agent is killed **three ways**: the master
  drops → SIGHUP to the slave; `Drop for Pane` (`pane.rs:157-163`) calls `killer.kill()`; and
  `Daemon::shutdown()` (`server.rs:282-297`) explicitly `p.kill()`s every pane. No child can outlive the
  daemon today.
- **All agent state is in-memory** on `Daemon` (`server.rs:29-51`): `panes`, `agents:
  HashMap<PaneId, AgentMeta>`, `workspaces: HashMap<PaneId, Workspace>`, `attention`, `trees` (split
  trees), `owner`. **Zero persistence** — a repo-wide search for state/persistence found nothing.
  `AgentMeta` holds only `{project, task}` (`server.rs:19-22`) — **the adapter id is dropped after spawn**.
- **Spawn flow** (`server.rs:137-219`, `spawn_agent`): `driver.provision(project, task)` →
  `adapter.provision_hooks` → `adapter.launch_command(&ws.path)` → inject `CLOWDER_AGENT_ID`/`CLOWDER_HOOK_SOCK`
  → `Pane::spawn(...)`; then register pane/workspace/`AgentMeta`/attention/tree/`wait_exit`.
- **Worktrees are the only durable artifact** (`crates/clowder-workspace`): `git worktree add` under
  `project/.clowder/worktrees/<name>`; `Workspace {path, branch, project, kind}`. `Daemon::shutdown()`
  deliberately does **not** land/discard on shutdown ("agents keep their worktrees") — so on a clean
  quit children are killed but worktrees are **orphaned on disk**, indexed only by the lost in-memory map.
- **Restart machinery already exists:** the single-instance `flock` (`instance.rs`); the app-side
  `DaemonSupervisor` relaunch-on-crash (macOS `DaemonSupervisor.swift`); and `AppModel`'s M5d reconnect
  which re-issues `listAgents` on reconnect (and today re-hydrates an empty list). The `Adapter` trait
  (`agent.rs`) builds the launch command; adapters: `claude`, `codex`, `shell`.

### User decisions (brainstorm, 2026-08-02)

- **Fidelity: re-spawn + adapter session-resume (build now); true PTY-host survival is a separate,
  deferred milestone.**
- **Resume all live registry agents on every startup** (crash or clean quit) — the registry is the source
  of truth; land/discard removes entries. No crash-detection.
- **Restore the agent pane only** in the first slice (splits reset to one pane); **full layout restore is
  its own M9 slice.**
- Registry = a JSON state file (YAGNI over SQLite); the `Adapter` trait gains a resume-aware launch.

## Goals / Non-goals

**Goals:** (1) a **durable agent registry** on disk; (2) on daemon startup, **reconcile** it —
re-spawn each live agent in its existing worktree using the adapter's **resume**, pruning entries whose
worktree is gone; (3) agents reappear in the app after any restart (via M5d reconnect → `listAgents`);
(4) never crash the daemon on a bad/missing registry, worktree, or adapter.

**Non-goals (M9a):** preserving the **running process / scrollback** (that's M9c); restoring the **split
layout / companion shells** (M9b); the daemon **outliving the app** (it still dies on app-quit; relaunch
re-spawns); any change to the wire protocol or the macOS app (the app already re-lists on reconnect).

## Component design

### M9a — Persist + reconcile + resume (BUILD)

1. **Durable agent registry.** A JSON file at `$XDG_STATE_HOME › ~/.local/state`, then `/clowder/agents.json`
   — a **durable** per-user dir, distinct from the ephemeral `$TMPDIR`-based runtime/socket dir. One
   record per live agent: `{ agent_id, project, task, adapter_id, worktree_path, branch, workspace_kind,
   cols, rows }`. Written **atomically** (write temp + `rename`) whenever an agent is spawned or finished
   (land/discard). A small `registry` module (load/save/upsert/remove) with an injectable path for tests.
2. **Retain `adapter_id`.** Extend `AgentMeta` (or the registry record) to keep the adapter id — required
   to rebuild the adapter for resume; it is dropped today.
3. **Reconcile on startup.** After acquiring the flock + binding sockets, `Daemon` loads the registry and
   for each entry: verify `worktree_path` exists (else prune the entry) → rebuild the `Workspace` record
   and the adapter by `adapter_id` → **re-spawn** via the adapter's resume launch in that worktree →
   re-register pane/workspace/`AgentMeta`/attention(`Working`)/a fresh single-pane tree/`wait_exit`. The
   re-spawned pane keeps its **original `agent_id`** (stable across restarts). Reconcile is best-effort:
   any per-agent failure prunes/logs that entry and continues.
4. **Adapter resume.** Add a resume-aware launch to the `Adapter` trait — e.g.
   `launch_command(worktree, resume: bool)` (or a sibling `resume_command`). `claude` →
   `claude --continue` (resumes the last conversation in the worktree from Claude's own local session
   state); adapters with no resume path fall back to a fresh launch (`shell` re-spawns a shell; `codex`
   uses its resume if available, else fresh). Hooks are re-provisioned on reconcile as at first spawn.
5. **Split reset.** Each resumed agent gets a fresh single-pane tree; companion shells are not restored
   (M9b). The registry stores no tree structure in M9a.

### M9b — Full layout restore (PLANNED)

Persist each agent's **split tree** (structure + ratios + per-leaf kind) and, on reconcile, rebuild the
companion shell panes in the same arrangement (fresh shells in the worktree cwd). Extends the registry
record with the tree; no new process model. Natural follow-up to M9a; pairs with M9c (where the companion
shells' scrollback would also survive).

### M9c — PTY-host true process survival (DEFERRED — design only)

A persistent **PTY-host** process owns the agent PTYs + child processes + recent backlog and **outlives
the daemon**. The daemon becomes a client of it: on restart the daemon **re-attaches** to the still-running
agents (PTY master fds handed over via `SCM_RIGHTS` fd-passing over a unix socket), so agents — and their
scrollback — survive with **zero disruption** (no re-spawn, no lost conversation). This splits today's
daemon into a small, rarely-restarted PTY-server + the churny restartable control-daemon. Large: a new
component, an fd-passing protocol, backlog ownership, child reaping in the host, and suppressing the
current triple-kill on the daemon side. Designed here; not built until true zero-disruption survival is
wanted. (The parked M7 `serve_remote` accept-loop hardening is unrelated and stays with M7d.)

## Data flow

```
spawn agent          → registry.upsert(record)          (atomic write)
land / discard agent  → registry.remove(agent_id)         (atomic write)
daemon startup        → registry.load()
                         → for each entry: worktree exists? ─no→ prune
                                                          ─yes→ rebuild ws+adapter → re-spawn(resume)
                                                                 → re-register pane/meta/attention/tree
app (M5d reconnect)   → listAgents → sees re-spawned agents → UI repopulates
```

## Decomposition (each its own plan → SDD → PR)

- **M9a — persist + reconcile + resume (agent-pane-only).** Registry module; retain `adapter_id`;
  reconcile-on-startup; `Adapter` resume. **BUILD.**
- **M9b — full layout restore** (persist split tree + rebuild companion shells). PLANNED.
- **M9c — PTY-host true survival** (persistent PTY-host + fd-passing re-attach). DEFERRED (design only).

Order: **M9a**, then M9b, then M9c when zero-disruption survival is wanted.

## Testing

- **M9a (`cargo test`):** registry round-trips (serialize/deserialize) and writes atomically; `upsert`/
  `remove` update the file. Reconcile: given a registry + existing worktree dirs, the daemon re-spawns the
  recorded agents with a **fake adapter** (assert the resume launch is used and the pane/meta/attention
  are re-registered under the original `agent_id`). Prune: an entry whose `worktree_path` is missing is
  dropped and not spawned. Corrupt/absent registry → daemon starts empty, no panic. The Claude adapter's
  resume launch contains `--continue`.
- **Manual/integration (maintainer):** `clowder spawn` an agent, do some work; `kill` the daemon PID; the
  app's supervisor relaunches it; the agent **reappears** in the app in the same worktree with a resumed
  session (for Claude, its prior conversation); `clowder land`/`discard` removes it from the registry so a
  subsequent restart does not resurrect it.

## Risks

1. **Auto-resume surprises.** Resuming every live agent on every startup (incl. after a clean quit) is the
   chosen behavior (survival), but a user who wanted a clean slate must `land`/`discard`. Documented; the
   registry is the single source of truth.
2. **Adapter resume correctness.** `claude --continue` resumes the most-recent conversation *in that cwd*;
   because each agent has its own worktree, that's the right conversation. Adapters without a proven resume
   fall back to a fresh launch (no worse than today). Covered by the adapter resume test.
3. **Stale/duplicate processes.** A crash could leave an orphaned agent process while the registry says it
   should be re-spawned → a duplicate. Mitigation: M9a re-spawns (fresh process) and does not adopt, so
   the orphan (if any) is detached from the daemon and harmless in the common case; true adoption is M9c.
   Land/discard kills the daemon-tracked pane only. (Acceptable for M9a; noted for M9c.)
4. **Registry vs worktree drift.** A worktree deleted out-of-band → prune on reconcile (covered).

## Verification gate

Per slice: its tests green + existing suites green. **M9a end state:** a running agent survives a daemon
`kill` + supervised relaunch — it reappears in the app in its worktree with a resumed session, driven by a
durable `agents.json` that the daemon reconciles on startup; land/discard removes it; a missing worktree
or corrupt registry never crashes the daemon. Deferred: **M9b** full layout restore, **M9c** PTY-host
zero-disruption survival.
