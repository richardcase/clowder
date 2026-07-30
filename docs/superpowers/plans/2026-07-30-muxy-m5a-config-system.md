# muxy M5a — Config System

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `muxy-config` crate that loads `~/.config/muxy/config.toml` and resolves each setting
with precedence **env var › config file › hardcoded default**, wired into the daemon (socket
paths, backlog cap, shell, default pane size — retiring the hardcoded `BACKLOG_CAP` const) and the
`muxy` CLI (socket resolution). Existing env/dev flows are unchanged (env always wins).

**Architecture:** New `crates/muxy-config` owns all resolution; consumers call `Config::load()` and
read fields. The daemon gains config-derived fields (defaulted so existing `Daemon::new`/`new_with`
callers/tests are untouched) + a `new_from_config` constructor used by `main.rs`; `Pane::spawn`
takes a `backlog_cap`. `muxy-client` reads socket paths via the same `Config`.

**Tech Stack:** Rust, `serde` + `toml`; `muxy-config`, `muxy-daemon`, `muxy-client`. Spec:
`docs/superpowers/specs/2026-07-30-muxy-m5-robustness-design.md` (§1 Config system).

## Global Constraints

- **Precedence per field: env › file › default.** A missing or invalid config file is **non-fatal**
  (log to stderr, fall back). Env always overrides the file (keeps dev/packaging flows working).
- Config location: `$XDG_CONFIG_HOME/muxy/config.toml`, else `$HOME/.config/muxy/config.toml`
  (manual XDG-style resolution — no `directories` dep).
- **No behavior change** for existing daemon/tests: `Daemon::new`/`new_with` keep their signatures
  and use the current hardcoded defaults (256 KiB backlog, `$SHELL` else `/bin/sh`, 80×24). Only
  `main.rs` opts into config via a new constructor.
- No proto/client(Swift) change. `anyhow::Result`. Test: `source "$HOME/.cargo/env" && cargo test`.
- Env vars/keys: `MUXY_SOCK`/`sockets.client`, `MUXY_CONTROL_SOCK`/`sockets.control`,
  `MUXY_HOOK_SOCK`/`sockets.hook`, `MUXY_BACKLOG_CAP`/`pane.backlog_cap`, `SHELL`/`pane.shell`,
  (no env)`pane.cols`/`pane.rows`. Defaults: `/tmp/muxy.sock`, `/tmp/muxy-control.sock`,
  `/tmp/muxy-hook.sock`, `262144`, `/bin/sh`, `80`, `24`.

---

## Task 1: `muxy-config` crate

**Files:**
- Create: `crates/muxy-config/Cargo.toml`, `crates/muxy-config/src/lib.rs`
- (workspace `members = ["crates/*"]` already includes it)

**Interfaces:**
- Produces: `muxy_config::Config { client_sock, control_sock, hook_sock: PathBuf, backlog_cap: usize,
  shell: String, default_cols: u16, default_rows: u16 }`, `Config::load() -> Config`.

- [ ] **Step 1: `Cargo.toml`**

```toml
[package]
name = "muxy-config"
version = "0.0.0"
edition = "2021"

[dependencies]
serde = { workspace = true }
toml = "0.8"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write the failing tests** — add to `crates/muxy-config/src/lib.rs` a `#[cfg(test)]`
module exercising the **pure resolver** (`resolve`) with an injected env-getter (deterministic, no
real env/files) + one file-parse test:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn no_env(_: &str) -> Option<String> { None }

    #[test]
    fn defaults_when_empty() {
        let c = Config::resolve(FileConfig::default(), &no_env);
        assert_eq!(c.client_sock, PathBuf::from("/tmp/muxy.sock"));
        assert_eq!(c.backlog_cap, 262144);
        assert_eq!(c.shell, "/bin/sh");
        assert_eq!((c.default_cols, c.default_rows), (80, 24));
    }

    #[test]
    fn file_overrides_default() {
        let f: FileConfig = toml::from_str(
            "[sockets]\nclient = \"/run/c.sock\"\n[pane]\nbacklog_cap = 1024\ncols = 120\n",
        ).unwrap();
        let c = Config::resolve(f, &no_env);
        assert_eq!(c.client_sock, PathBuf::from("/run/c.sock"));
        assert_eq!(c.backlog_cap, 1024);
        assert_eq!(c.default_cols, 120);
        assert_eq!(c.default_rows, 24); // unspecified → default
    }

    #[test]
    fn env_overrides_file() {
        let f: FileConfig = toml::from_str("[sockets]\nclient = \"/run/c.sock\"\n[pane]\nbacklog_cap = 1024\n").unwrap();
        let env = |k: &str| match k { "MUXY_SOCK" => Some("/env/c.sock".into()), "MUXY_BACKLOG_CAP" => Some("4096".into()), _ => None };
        let c = Config::resolve(f, &env);
        assert_eq!(c.client_sock, PathBuf::from("/env/c.sock")); // env wins over file
        assert_eq!(c.backlog_cap, 4096);
    }

    #[test]
    fn invalid_backlog_env_falls_through_to_file() {
        let f: FileConfig = toml::from_str("[pane]\nbacklog_cap = 1024\n").unwrap();
        let env = |k: &str| if k == "MUXY_BACKLOG_CAP" { Some("notanumber".into()) } else { None };
        assert_eq!(Config::resolve(f, &env).backlog_cap, 1024);
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-config`
Expected: FAIL (compile) — `Config`/`FileConfig`/`resolve` don't exist.

- [ ] **Step 4: Implement `lib.rs`** (above the test module):

```rust
use serde::Deserialize;
use std::path::PathBuf;

const DEFAULT_CLIENT_SOCK: &str = "/tmp/muxy.sock";
const DEFAULT_CONTROL_SOCK: &str = "/tmp/muxy-control.sock";
const DEFAULT_HOOK_SOCK: &str = "/tmp/muxy-hook.sock";
const DEFAULT_BACKLOG_CAP: usize = 256 * 1024;
const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;

/// Fully-resolved configuration (env > file > default, applied per field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub client_sock: PathBuf,
    pub control_sock: PathBuf,
    pub hook_sock: PathBuf,
    pub backlog_cap: usize,
    pub shell: String,
    pub default_cols: u16,
    pub default_rows: u16,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    sockets: Option<Sockets>,
    pane: Option<PaneCfg>,
}
#[derive(Debug, Default, Deserialize)]
struct Sockets { client: Option<PathBuf>, control: Option<PathBuf>, hook: Option<PathBuf> }
#[derive(Debug, Default, Deserialize)]
struct PaneCfg { backlog_cap: Option<usize>, shell: Option<String>, cols: Option<u16>, rows: Option<u16> }

impl Config {
    /// Load `$XDG_CONFIG_HOME/muxy/config.toml` (else `$HOME/.config/muxy/config.toml`), then apply
    /// env overrides. A missing/invalid file is non-fatal.
    pub fn load() -> Config {
        let file = config_path().and_then(read_file).unwrap_or_default();
        Config::resolve(file, &|k| std::env::var(k).ok())
    }

    /// Pure resolver (testable): env > file > default. `get_env(key)` yields the env value.
    fn resolve(f: FileConfig, get_env: &dyn Fn(&str) -> Option<String>) -> Config {
        let s = f.sockets.unwrap_or_default();
        let p = f.pane.unwrap_or_default();
        let path = |env: &str, file: Option<PathBuf>, def: &str| {
            get_env(env).map(PathBuf::from).or(file).unwrap_or_else(|| PathBuf::from(def))
        };
        Config {
            client_sock: path("MUXY_SOCK", s.client, DEFAULT_CLIENT_SOCK),
            control_sock: path("MUXY_CONTROL_SOCK", s.control, DEFAULT_CONTROL_SOCK),
            hook_sock: path("MUXY_HOOK_SOCK", s.hook, DEFAULT_HOOK_SOCK),
            backlog_cap: get_env("MUXY_BACKLOG_CAP").and_then(|v| v.parse().ok())
                .or(p.backlog_cap).unwrap_or(DEFAULT_BACKLOG_CAP),
            shell: get_env("SHELL").or(p.shell).unwrap_or_else(|| "/bin/sh".into()),
            default_cols: p.cols.unwrap_or(DEFAULT_COLS),
            default_rows: p.rows.unwrap_or(DEFAULT_ROWS),
        }
    }
}

impl Default for Config {
    fn default() -> Self { Config::resolve(FileConfig::default(), &|_| None) }
}

fn config_path() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() { return Some(PathBuf::from(x).join("muxy").join("config.toml")); }
    }
    std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config").join("muxy").join("config.toml"))
}

fn read_file(path: PathBuf) -> Option<FileConfig> {
    let text = std::fs::read_to_string(&path).ok()?;
    match toml::from_str(&text) {
        Ok(cfg) => Some(cfg),
        Err(e) => { eprintln!("muxy-config: ignoring invalid {}: {e}", path.display()); None }
    }
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-config`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-config
git commit -m "feat(config): muxy-config crate — env>file>default resolver"
```

---

## Task 2: Wire config into the daemon

**Files:**
- Modify: `crates/muxy-daemon/Cargo.toml` (add `muxy-config` dep)
- Modify: `crates/muxy-daemon/src/pane.rs` (`Pane::spawn` takes `backlog_cap`; use it not the const)
- Modify: `crates/muxy-daemon/src/server.rs` (Daemon config fields + `new_from_config`; use them in spawn)
- Modify: `crates/muxy-daemon/src/main.rs` (load config; sockets + `new_from_config`)

**Interfaces:**
- Consumes: `muxy_config::Config` (Task 1).
- Produces: `Daemon::new_from_config(notifier, Config)`; `Pane::spawn(id, cmd, cols, rows, backlog_cap)`.

- [ ] **Step 1: Add the dep** to `crates/muxy-daemon/Cargo.toml` `[dependencies]`:
```toml
muxy-config = { path = "../muxy-config" }
```

- [ ] **Step 2: `Pane::spawn` takes `backlog_cap`** (`pane.rs`). Change the signature
`pub fn spawn(id: PaneId, cmd: PaneCommand, cols: u16, rows: u16) -> Result<Pane>` →
`pub fn spawn(id: PaneId, cmd: PaneCommand, cols: u16, rows: u16, backlog_cap: usize) -> Result<Pane>`.
Inside the reader thread, replace the two `BACKLOG_CAP` uses (the `if b.len() > BACKLOG_CAP { let drop = b.len() - BACKLOG_CAP; … }` block, ~`pane.rs:66-67`) with the passed `backlog_cap` (move it into the thread with a `let cap = backlog_cap;` capture). Delete the `const BACKLOG_CAP` (or keep it as the module default only if still referenced — prefer deleting). Update `pane.rs`'s own tests that call `Pane::spawn(...)` to pass a cap (use `256 * 1024`).

- [ ] **Step 3: Daemon config fields + constructor** (`server.rs`). Add fields to the `Daemon`
struct, marked **`pub(crate)`** so in-crate tests can set them: `pub(crate) backlog_cap: usize`,
`pub(crate) default_cols: u16`, `pub(crate) default_rows: u16`, `pub(crate) shell: String`. In
`new_with` (unchanged signature), initialize them to the current defaults so existing callers/tests
are unaffected:
```rust
            backlog_cap: 256 * 1024,
            default_cols: 80,
            default_rows: 24,
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
```
Add a config constructor that uses `OsNotifier` internally (mirroring `Daemon::new`, so `main.rs`
needn't import `OsNotifier`):
```rust
    pub fn new_from_config(config: muxy_config::Config) -> Daemon {
        let mut d = Daemon::new_with(Arc::new(OsNotifier), config.hook_sock);
        d.backlog_cap = config.backlog_cap;
        d.default_cols = config.default_cols;
        d.default_rows = config.default_rows;
        d.shell = config.shell;
        d
    }
```

- [ ] **Step 4: Use the fields at spawn** (`server.rs`):
  - `spawn_pane` (line ~109): `Pane::spawn(id, cmd, cols, rows)` → `Pane::spawn(id, cmd, cols, rows, self.backlog_cap)`.
  - `spawn_agent` (line ~131): `Pane::spawn(id, cmd, 80, 24)` → `Pane::spawn(id, cmd, self.default_cols, self.default_rows, self.backlog_cap)`.
  - Companion spawn (lines ~306-307): replace `let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());` + `self.spawn_pane(companion_command(shell, path), 80, 24)` with
    `self.spawn_pane(companion_command(self.shell.clone(), path), self.default_cols, self.default_rows)`.

- [ ] **Step 5: `main.rs` uses config** — replace the env reads + `Daemon::new()` with:
```rust
    let config = muxy_config::Config::load();
    let sock_path = config.client_sock.clone();
    let control_path = config.control_sock.clone();
    let daemon = Arc::new(Daemon::new_from_config(config));
    let hook_path = daemon.hook_sock().to_path_buf();
```
(Keep the existing `remove_file` + `UnixListener::bind` + `serve*` flow unchanged. The stale-socket
`remove_file` stays here for M5a — the single-instance guard is M5b.)

- [ ] **Step 6: Add a backlog test** (`server.rs` tests) proving the configured cap is honored —
build a daemon with a tiny `backlog_cap` (set the `pub(crate)` field before Arc-wrapping), spawn a
companion that emits more than the cap, and assert the backlog stays bounded near the cap (reuse the
existing backlog/`spawn_pane` test harness):
```rust
#[tokio::test]
async fn small_backlog_cap_bounds_the_buffer() {
    let mut d = Daemon::new_with(StdArc::new(FakeNotifier::new()), "/tmp/unused-cap.sock".into());
    d.backlog_cap = 4096;                       // pub(crate) — set before wrapping in Arc
    let daemon = StdArc::new(d);
    // spawn a pane printing well over 4096 bytes, wait for it to land, then:
    //   assert!(daemon.pane_backlog(pane).len() <= 4096 + ONE_CHUNK);
    // (use whatever backlog accessor the existing backlog test uses).
}
```

- [ ] **Step 7: Run**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon` then whole-workspace `cargo test`.
Expected: PASS — new backlog test + all existing (unchanged `new_with` defaults keep them green).

- [ ] **Step 8: Commit**

```bash
git add crates/muxy-daemon
git commit -m "feat(daemon): drive backlog cap / shell / pane size from muxy-config"
```

---

## Task 3: Wire config into the `muxy` client

**Files:**
- Modify: `crates/muxy-client/Cargo.toml` (add `muxy-config` dep)
- Modify: `crates/muxy-client/src/lib.rs` (line ~33 socket) and `src/main.rs` (line ~13 control socket)

**Interfaces:**
- Consumes: `muxy_config::Config`.

- [ ] **Step 1: Add the dep** to `crates/muxy-client/Cargo.toml`:
```toml
muxy-config = { path = "../muxy-config" }
```

- [ ] **Step 2: Resolve sockets via config.** In `lib.rs` (~line 33) replace
`let sock = std::env::var("MUXY_SOCK").unwrap_or_else(|_| "/tmp/muxy.sock".into());`
with `let sock = muxy_config::Config::load().client_sock;` (a `PathBuf`; adjust the downstream
`UnixStream::connect(&sock)` if it expected a `String` — `connect` takes `AsRef<Path>`, so a
`PathBuf` works). In `main.rs` (~line 13) replace the `MUXY_CONTROL_SOCK` env read with
`muxy_config::Config::load().control_sock`. (Env precedence is unchanged — `Config::load` reads
`MUXY_SOCK`/`MUXY_CONTROL_SOCK` first.)

- [ ] **Step 3: Test** — add a `muxy-client` unit test that a `config.toml` (no env) resolves the
client socket, and env overrides it. Since `Config::load()` reads process env/HOME, test the
**resolver** indirectly by asserting `muxy_config::Config::default().client_sock == "/tmp/muxy.sock"`
and rely on Task 1's `env_overrides_file` for precedence (the client just calls `Config::load()`).
If a client-local integration test is impractical, a doc/comment noting the delegation suffices —
the resolution logic is fully covered in `muxy-config`.

- [ ] **Step 4: Run**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-client` then whole-workspace `cargo test`.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/muxy-client
git commit -m "feat(client): resolve socket paths via muxy-config"
```

---

## Final verification

- `source "$HOME/.cargo/env" && cargo test` (whole workspace) → green (new config + backlog tests +
  all existing; `new_with` defaults keep daemon tests unchanged).
- Precedence holds: env › `~/.config/muxy/config.toml` › default (unit-tested in `muxy-config`); a
  missing/invalid file is non-fatal.
- The hardcoded 256 KiB `BACKLOG_CAP` const is gone — the cap comes from config; sockets/shell/pane
  size are config-driven in the daemon, and the `muxy` CLI resolves its socket via config.
- No Swift/proto change (M5d handles the app; M5b/M5c are the other slices).
- **Manual (optional):** write `~/.config/muxy/config.toml` with `[pane] backlog_cap = 8192`, run the
  daemon, confirm it starts and honors it; set `MUXY_SOCK=/tmp/x.sock` and confirm env wins.
