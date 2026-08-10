# Login-environment capture for pane children

*2026-08-10 — fixes [#76](https://github.com/richardcase/clowder/issues/76)*

## The report

> When a terminal starts in Clowder the PATH is not set correctly and so it doesn't pick up claude
> etc. If I start a terminal outside of clowder on the same machine the PATH is set and I can start
> claude.

## Root cause

A GUI-launched `.app` is started by launchd, not by a shell. Its environment has
`PATH=/usr/bin:/bin:/usr/sbin:/sbin` and **no `SHELL` at all**. That environment reached PTY children
untouched:

1. `DaemonLaunch.swift` builds the daemon's environment from `ProcessInfo.processInfo.environment`,
   prepending only the bundle's `Contents/MacOS` to `PATH`.
2. `Pane::spawn` built a `portable_pty::CommandBuilder`, whose `get_base_env()` seeds the child from
   `std::env::vars_os()` verbatim.
3. `ClaudeAdapter`/`CodexAdapter` launch by **bare name** (`claude`, `codex`) → not found.
4. `Config::resolve` read `shell` from `$SHELL`, which is unset → companion and project-terminal
   panes ran `/bin/sh`, not the user's shell.

Terminal.app is started by launchd too. The difference is that it runs the shell as a *login* shell,
which is what executes `/etc/zprofile` (hence `path_helper`) and `~/.zprofile`.

## Three findings that shaped the fix

**portable-pty resolves the program in the parent, not via `execvp`.**
`CommandBuilder::search_path` (portable-pty 0.8.1 `cmdbuilder.rs:405-445`) walks the builder's *own*
`PATH` before forking, and `as_command()` hands the resolved absolute path to `std::process::Command`.
Two consequences: setting `PATH` on the builder is genuinely what fixes `claude` lookup, and a failure
surfaces as a clean `anyhow` error rather than a post-fork exec failure. It also tries
`<cwd>/<program>` *before* `PATH` for a relative name — a worktree containing a file called `claude`
would shadow the real one. Pre-existing; not guarded here.

**A login zsh does not export `SHELL`.** Verified:

```
$ env -i PATH=/usr/bin:/bin:/usr/sbin:/sbin HOME=$HOME USER=$USER \
    /bin/zsh -l -i -c '/usr/bin/env -0' | tr '\0' '\n' | grep -E '^(SHELL|PATH|TERM|SHLVL)='
PATH=/Users/richard/.local/bin:…:/opt/homebrew/bin:…
SHLVL=0
TERM=screen-256color
```

No `SHELL`. So capturing a login environment cannot fix the shell — the passwd database
(`getpwuid(getuid())->pw_shell`) is the only source of truth in a launchd context, and the fix needs
both halves. The same run also shows why `TERM` must come from the daemon (an rc file set it, with no
tty in sight) and why `SHLVL`/`PWD` must be stripped.

**`path_helper` preserves an inherited `PATH` entry, but moves it to the tail.** The bundle's
`Contents/MacOS` survived a capture — at the end. So the Swift-side prepend in `DaemonLaunch.swift`
is not load-bearing (and `clowder_hook_bin()` resolves an absolute exe-sibling path anyway), but the
daemon should not *depend* on a user's rc file preserving it. It re-prepends its own exe dir itself,
which also puts `clowder`/`clowder-hook` on the PATH of dev panes for free.

## The design

The daemon owns the fix. It runs `<login shell> -l -i -c '<script>'` once at startup and uses the
resulting environment as the base for every PTY child.

- **Why the daemon, not the app.** It fixes every client at once, and a remote daemon started by
  launchd/systemd has the identical problem, which the app cannot reach. It is also the half with
  real unit tests (`clowder-app` has no test target).
- **Why both `-l` and `-i`.** `-l` sources `/etc/zprofile` (`path_helper`) and `~/.zprofile`, where
  Homebrew lands. `-i` sources `~/.zshrc`, where nvm, mise and the Claude native installer land. Only
  the pair covers where `claude` actually ends up.
- **Why nonce markers.** rc files print arbitrary junk — motd, version-manager banners, instant-prompt
  warnings — and EXIT traps print after the dump. A fixed marker is guessable and collidable; a
  per-run one is not.
- **Why `env -0`.** NUL is the only byte that cannot occur inside an `envp` entry. Newlines routinely
  do: bash exports functions as `BASH_FUNC_x%%=() {\n…\n}`, and `PS1`/direnv values are often
  multi-line. A line-splitting parser would mangle those *and* let a newline embedded in any value
  inject a counterfeit `PATH=` record.
- **Why absolute `/usr/bin/printf` and `/usr/bin/env`.** `-i` makes the shell interactive, so aliases
  expand; someone's `alias env='env | sort'` would silently corrupt the dump. An absolute path is
  subject to neither alias nor function lookup.
- **Why after the binds, before `reconcile()`.** The capture takes ~0.8 s on a real machine. Running
  it before the binds would give clients a genuine `ECONNREFUSED` window; running it after the binds
  means a client connecting meanwhile simply waits in the accept backlog. It must still precede
  `reconcile()`, which respawns the whole fleet and is precisely the code that needs a good `PATH`.
- **Never fatal.** On failure or timeout (3 s default, clamped to 1–30 s) panes fall back to
  inheriting the daemon's environment — exactly the pre-#76 behaviour — with a warning to
  `daemon.log`, the only place a GUI-launched daemon's startup is visible.

### Merge precedence (`PaneEnv::resolve`, pure and unit-tested)

1. the captured environment, or the daemon's own when there is nothing captured;
2. minus `PWD`, `OLDPWD`, `SHLVL`, `_`, `COLUMNS`, `LINES`, `TERMCAP` — these describe the capture,
   not the pane, and are never re-added from either source;
3. `TERM` from the daemon, never the capture;
4. every `CLOWDER_*` key from the daemon wins, so a `clowder` run inside a pane reaches *this* daemon
   even if the user's rc exports a stale `CLOWDER_SOCK`;
5. `PATH` from the capture (falling back to the daemon's when absent or empty), with the daemon's own
   directory prepended unless already present;
6. `SHELL` forced to the resolved shell.

Per-pane variables (`CLOWDER_AGENT_ID`, `CLOWDER_HOOK_SOCK`) are layered on afterwards by
`Pane::spawn`, so they always win.

`Pane::spawn` `env_clear()`s the builder first. That is the seam: a child's environment is *stated*,
not "whatever the daemon inherited, plus what we remembered to override".

## Rejected alternatives

**Capture inside `Config::load()`.** `Config::load` is not daemon-private — the `clowder` CLI calls it
on every `attach`, `spawn` and `remote list`, and the app runs `clowder attach` once per libghostty
surface. At a measured 0.775 s per capture that is a per-surface stall and a fork storm.

**A `base` field on `PaneCommand`.** `PaneCommand` is constructed at a dozen sites across adapters,
tests and `companion_command`. A field there invites `env: vec![]`-style defaults that quietly
reinstate the bug; a `Pane::spawn` parameter makes the compiler enumerate the callers instead.

**Making companion/project shell panes login shells (`-l`).** More faithful to Terminal.app, but with
the base environment already resolved it would run `path_helper` a second time on top of a correct
`PATH`, reordering it (moving `/opt/homebrew/bin` behind `/usr/bin`) for no gain. The pane's shell is
interactive, so `~/.zshrc` still runs.

**Hardcoding `/opt/homebrew/bin`, `/usr/local/bin`, `~/.local/bin`.** Covers the common cases and
misses nvm, mise, asdf, volta, pnpm and every custom prefix — i.e. it would have to be extended
forever and would still be wrong for someone.

## Escape hatch

`[env] capture_login = false` / `CLOWDER_CAPTURE_LOGIN_ENV=0` skips the capture entirely, for a dev
running the daemon from a terminal (where its own environment is already correct) and for CI.
`[env] timeout_ms` / `CLOWDER_LOGIN_ENV_TIMEOUT_MS` tunes the deadline.

## Known caveats

- Session-scoped variables produced by the capture (`ATUIN_SESSION`, `STARSHIP_SESSION_KEY`, …) are
  shared by every pane, and a value *computed* under capture conditions can be wrong (`GPG_TTY=not a
  tty`). Not enumerable; the escape hatch covers anyone who cares.
- An empty captured value is honoured rather than falling back to the daemon's. Fidelity to "the pane
  looks like the user's terminal" beats second-guessing their rc.
- `TERM` really belongs to the *attaching client*, not the daemon. Keeping the daemon's preserves
  today's behaviour exactly; doing it properly is a separate change.

## Verification

Unit tests cover the parser (rc noise, decoy markers, multi-line values, malformed entries, missing
markers) and every precedence rule. Integration tests drive **fake shells** in a tempdir — never the
developer's real rc files, which would make CI machine-dependent and could hang the suite — covering
the happy path, a hostile dump, a hang (timeout + kill), and a shell that emits no markers.
`an_agent_launched_by_bare_name_resolves_against_the_pane_environment` is the regression guard: it
fails on the old code with "not found in PATH".

End to end, with the daemon run under `env -i` carrying launchd's `PATH` and no `SHELL`, a pane
reports:

```
MARKER_PATH=/Users/richard/.local/bin:…:/opt/homebrew/bin:…   (the full login PATH)
MARKER_SHELL=/bin/zsh MARKER_SHLVL=1 MARKER_PWD=<the worktree>
$ command -v claude
/opt/homebrew/bin/claude
```
