//! The environment the daemon hands to every PTY child.
//!
//! A GUI-launched `Clowder.app` is started by launchd, whose environment is `PATH=/usr/bin:/bin:
//! /usr/sbin:/sbin` and no `SHELL`. That environment reached agent panes verbatim, so `claude` and
//! `codex` — launched by bare name — were simply not found (issue #76).
//!
//! The daemon therefore runs the user's login shell once at startup, captures the environment it
//! produces, and uses it as the base for every pane. This is the same thing Terminal.app gives the
//! user, which is exactly the comparison the bug report makes.
//!
//! Two halves, deliberately separated: [`capture`] forks a shell (untestable without one),
//! [`parse_marked_env`] and [`PaneEnv::resolve`] are pure and hold all the policy.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// Keys that describe the *capture*, not the pane, and must never survive into a child.
///
/// The capture ran in `$HOME` with no tty, so each of these would be an outright lie: an inherited
/// `PWD` makes `pwd` in a worktree pane report `$HOME` (zsh and bash trust a stat-matching inherited
/// `PWD` over `getcwd`), `SHLVL` starts every pane one level deep, and `COLUMNS`/`LINES` captured
/// without a tty override the real window size in anything that prefers them to `TIOCGWINSZ`.
///
/// These are stripped unconditionally — they are not re-added from the daemon's own environment
/// either, because the PTY and the child's own `chdir` are the only honest source for them.
const CAPTURE_ARTEFACTS: &[&str] =
    &["PWD", "OLDPWD", "SHLVL", "_", "COLUMNS", "LINES", "TERMCAP", CAPTURE_MARKER_VAR];

/// Set on the capture child so an rc file can cheaply skip heavy work (`[[ -n $CLOWDER_LOGIN_ENV_CAPTURE ]] && return`).
/// Also a re-entrancy marker. Stripped from the result — a pane is not a capture.
pub const CAPTURE_MARKER_VAR: &str = "CLOWDER_LOGIN_ENV_CAPTURE";

/// Used when the daemon's own environment has no `TERM` of its own (the normal GUI-launched case,
/// since launchd sets none). Every pane is a real PTY rendered by libghostty, which is at least this
/// capable — `xterm-256color` is universally present in terminfo databases, unlike `xterm-ghostty`.
const DEFAULT_TERM: &str = "xterm-256color";

/// Used when the daemon's own environment has no `COLORTERM` of its own, for the same reason as
/// [`DEFAULT_TERM`] — libghostty renders true color, so a pane should be able to say so.
const DEFAULT_COLORTERM: &str = "truecolor";

/// What [`capture`] needs to run. Separate from `Config` so tests can point it at a fake shell.
#[derive(Debug, Clone)]
pub struct CaptureSpec {
    /// The login shell to run, already resolved (see `clowder_config::login_shell`).
    pub shell: String,
    /// How long to wait before giving up and killing the shell.
    pub timeout: Duration,
    /// Where to run it. `$HOME` in production; an rc file that `cd`s is normal, and the resulting
    /// `PWD` is stripped anyway.
    pub cwd: Option<std::path::PathBuf>,
}

/// The base environment for every PTY child: a captured login environment with the daemon's own
/// non-negotiables layered back on top. Built once at startup, then read-only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneEnv {
    vars: BTreeMap<String, String>,
}

impl PaneEnv {
    /// The pre-#76 behaviour: the daemon's own environment. Used when capture is disabled, when it
    /// fails, and by every `Daemon` constructed without an explicit environment (i.e. tests).
    pub fn inherited(shell: &str) -> PaneEnv {
        PaneEnv::resolve(None, env_snapshot(), exe_dir().as_deref(), shell)
    }

    /// Merge a captured login environment with the daemon's own. Pure — no I/O, no `std::env` — so
    /// the whole precedence policy is one testable function.
    ///
    /// Lowest layer first:
    /// 1. the captured environment, or the daemon's own when there is nothing captured;
    /// 2. minus [`CAPTURE_ARTEFACTS`];
    /// 3. `TERM`/`COLORTERM` from the daemon, never from the capture — rc files set them, and the
    ///    capture child had no tty, so captured values describe nothing real. When the daemon has
    ///    neither (the normal GUI-launched case), fall back to [`DEFAULT_TERM`]/[`DEFAULT_COLORTERM`]
    ///    rather than leaving the pane with none — every pane is a real, color-capable PTY rendered
    ///    by libghostty, regardless of what launchd handed the daemon;
    /// 4. every `CLOWDER_*` key from the daemon wins, so a `clowder` run *inside* a pane reaches
    ///    *this* daemon even if the user's rc exports a stale `CLOWDER_SOCK`;
    /// 5. `PATH` from the capture (falling back to the daemon's if it is absent or empty), with the
    ///    daemon's own directory prepended so `clowder`/`clowder-hook` are always reachable;
    /// 6. `SHELL` forced to the resolved shell — a login zsh exports none, so the capture never
    ///    carries one, and a pane's `$SHELL` must match the program its shell panes actually run.
    ///
    /// Per-pane variables are *not* part of this merge; `Pane::spawn` layers them on afterwards, so
    /// `CLOWDER_AGENT_ID`/`CLOWDER_HOOK_SOCK` always win over both of these sources.
    pub fn resolve(
        captured: Option<BTreeMap<String, String>>,
        daemon: BTreeMap<String, String>,
        exe_dir: Option<&Path>,
        shell: &str,
    ) -> PaneEnv {
        let mut vars = captured.unwrap_or_else(|| daemon.clone());

        for key in CAPTURE_ARTEFACTS {
            vars.remove(*key);
        }

        // 3. TERM/COLORTERM: the daemon's, or a color-capable default. Never the captured one.
        let term = daemon.get("TERM").cloned().unwrap_or_else(|| DEFAULT_TERM.to_string());
        vars.insert("TERM".into(), term);
        let colorterm = daemon.get("COLORTERM").cloned().unwrap_or_else(|| DEFAULT_COLORTERM.to_string());
        vars.insert("COLORTERM".into(), colorterm);

        // 4. The daemon owns its own namespace.
        for (k, v) in &daemon {
            if k.starts_with("CLOWDER_") && k != CAPTURE_MARKER_VAR {
                vars.insert(k.clone(), v.clone());
            }
        }

        // 5. PATH. An empty captured PATH is useless, so fall back rather than honour it.
        let path = vars
            .get("PATH")
            .filter(|p| !p.is_empty())
            .cloned()
            .or_else(|| daemon.get("PATH").filter(|p| !p.is_empty()).cloned())
            .unwrap_or_default();
        let path = match exe_dir.map(|d| d.to_string_lossy().into_owned()) {
            Some(dir) if !dir.is_empty() && !std::env::split_paths(&path).any(|p| p == Path::new(&dir)) => {
                if path.is_empty() { dir } else { format!("{dir}:{path}") }
            }
            _ => path,
        };
        if !path.is_empty() {
            vars.insert("PATH".into(), path);
        }

        // 6. SHELL.
        vars.insert("SHELL".into(), shell.to_string());

        PaneEnv { vars }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.vars.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.vars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }
}

/// The daemon's own environment as a map. Kept next to `PaneEnv` so the impure half is one call.
pub fn env_snapshot() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

/// The directory holding the running daemon binary — where `clowder` and `clowder-hook` are its
/// siblings, in both the packaged bundle and `target/debug`.
pub fn exe_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

/// Run the user's login shell and read back the environment it produces.
///
/// `<shell> -l -i -c '<script>'`, with stdin on `/dev/null`. Both flags are load-bearing: `-l`
/// sources `/etc/zprofile` (hence `path_helper`) and `~/.zprofile`, which is where Homebrew lands;
/// `-i` sources `~/.zshrc`, which is where nvm, mise and the Claude native installer land. Only the
/// pair covers where `claude` actually ends up.
///
/// Never propagates: the caller warns and falls back to the daemon's own environment. A hostile or
/// merely slow rc file must not stop the daemon from starting.
pub async fn capture(spec: &CaptureSpec) -> Result<BTreeMap<String, String>> {
    let nonce = nonce();
    let (begin, end) = (format!("__CLOWDER_ENV_BEGIN_{nonce}__"), format!("__CLOWDER_ENV_END_{nonce}__"));

    let mut cmd = tokio::process::Command::new(&spec.shell);
    cmd.arg("-l")
        .arg("-i")
        .arg("-c")
        .arg(capture_script(&begin, &end))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // The shell is a grandchild we do not want to outlive the timeout; dropping the future on
        // elapse must actually kill it.
        .kill_on_drop(true)
        .env(CAPTURE_MARKER_VAR, "1");
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }

    let child = cmd.spawn().with_context(|| format!("could not run {} for the login-env capture", spec.shell))?;
    let out = match tokio::time::timeout(spec.timeout, child.wait_with_output()).await {
        Ok(r) => r.context("login-env capture failed while reading the shell's output")?,
        Err(_) => bail!("login-env capture timed out after {:?}", spec.timeout),
    };

    // Diagnostics before the verdict: `set -x` traces and rc-file errors land on stderr, and for a
    // GUI-launched app daemon.log is the only place any of this is ever visible.
    if !out.stderr.is_empty() {
        let tail = String::from_utf8_lossy(&out.stderr[..out.stderr.len().min(2048)]).into_owned();
        if out.status.success() {
            tracing::debug!("login-env capture stderr: {tail}");
        } else {
            tracing::warn!("login-env capture stderr: {tail}");
        }
    }
    if !out.status.success() {
        bail!("login-env capture shell exited with {}", out.status);
    }
    parse_marked_env(&out.stdout, &begin, &end)
}

/// A per-run marker suffix. rc files print arbitrary junk, so a *fixed* marker could plausibly be
/// echoed back by one (or left over in a scrollback the shell replays); a random one cannot.
fn nonce() -> String {
    let mut raw = [0u8; 8];
    if getrandom::getrandom(&mut raw).is_err() {
        // Only reachable if the OS RNG is unavailable, at which point the daemon has bigger
        // problems. A constant marker still parses; it is just guessable.
        return "fallback".into();
    }
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

/// The script the capture shell runs: markers around a NUL-delimited environment dump.
///
/// `/usr/bin/printf` and `/usr/bin/env` are absolute on purpose. The shell runs interactive (`-i`)
/// so that `~/.zshrc` is sourced, which means **aliases are expanded** — someone's
/// `alias env='env | sort'` would silently corrupt the dump. An absolute path is subject to neither
/// alias nor function lookup.
pub(crate) fn capture_script(begin: &str, end: &str) -> String {
    format!("/usr/bin/printf %s '{begin}'; /usr/bin/env -0; /usr/bin/printf %s '{end}'")
}

/// Extract the environment from a capture shell's stdout.
///
/// rc files print arbitrary noise — motd, version-manager chatter, instant-prompt warnings — before
/// the dump, and `zshexit`/EXIT traps print after it, so the dump is framed by nonce markers and
/// everything outside them is discarded.
///
/// Entries are NUL-delimited (`env -0`), never newline-delimited. NUL is the only byte that cannot
/// occur inside an `envp` entry, whereas newlines routinely do: bash exports functions as
/// `BASH_FUNC_x%%=() {\n …\n}`, and `PS1`/direnv values are often multi-line. A line-splitting
/// parser would both mangle those and let a newline embedded in any value *inject* a counterfeit
/// `PATH=` record.
pub(crate) fn parse_marked_env(stdout: &[u8], begin: &str, end: &str) -> Result<BTreeMap<String, String>> {
    let start = find(stdout, begin.as_bytes()).context("capture output has no start marker")?
        + begin.len();
    let rest = &stdout[start..];
    let len = find(rest, end.as_bytes()).context("capture output has no end marker")?;
    let region = &rest[..len];

    let mut out = BTreeMap::new();
    for entry in region.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        let Some(eq) = entry.iter().position(|b| *b == b'=') else {
            tracing::debug!("login-env: skipping capture entry with no '='");
            continue;
        };
        let (key, value) = (&entry[..eq], &entry[eq + 1..]);
        // Non-UTF-8 environment entries are vanishingly rare on macOS and unrepresentable in the
        // String-keyed map the rest of the daemon uses; drop them rather than fail the capture.
        let (Ok(key), Ok(value)) = (std::str::from_utf8(key), std::str::from_utf8(value)) else {
            tracing::debug!("login-env: skipping non-UTF-8 capture entry");
            continue;
        };
        if key.is_empty() {
            continue;
        }
        out.insert(key.to_string(), value.to_string());
    }

    if out.is_empty() {
        bail!("capture produced no environment entries");
    }
    Ok(out)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn framed(begin: &str, end: &str, entries: &[&str]) -> Vec<u8> {
        let mut out = begin.as_bytes().to_vec();
        for e in entries {
            out.extend_from_slice(e.as_bytes());
            out.push(0);
        }
        out.extend_from_slice(end.as_bytes());
        out
    }

    // ---- parse_marked_env -------------------------------------------------

    #[test]
    fn parses_a_clean_dump() {
        let out = framed("BEGIN1", "END1", &["PATH=/a:/b", "HOME=/home/rc"]);
        let env = parse_marked_env(&out, "BEGIN1", "END1").unwrap();
        assert_eq!(env.get("PATH").unwrap(), "/a:/b");
        assert_eq!(env.get("HOME").unwrap(), "/home/rc");
    }

    #[test]
    fn discards_rc_noise_on_both_sides_including_a_decoy_marker() {
        // A previous run's marker, a motd, and an EXIT trap's parting shot.
        let mut out = b"Welcome to zsh!\nnvm: using v20\nBEGIN_OTHERNONCE PATH=/junk\n".to_vec();
        out.extend_from_slice(&framed("BEGIN_abc123", "END_abc123", &["PATH=/real"]));
        out.extend_from_slice(b"\nGoodbye\n");
        let env = parse_marked_env(&out, "BEGIN_abc123", "END_abc123").unwrap();
        assert_eq!(env.get("PATH").unwrap(), "/real");
        assert_eq!(env.len(), 1);
    }

    #[test]
    fn keeps_values_containing_equals_and_newlines() {
        // A bash exported function is multi-line; a line-splitting parser would mangle it AND let
        // the embedded `PATH=` line through as a counterfeit entry.
        let out = framed(
            "B",
            "E",
            &["BASH_FUNC_x%%=() {\nPATH=/injected\n}", "OPTS=a=b=c"],
        );
        let env = parse_marked_env(&out, "B", "E").unwrap();
        assert_eq!(env.get("OPTS").unwrap(), "a=b=c");
        assert_eq!(env.get("BASH_FUNC_x%%").unwrap(), "() {\nPATH=/injected\n}");
        assert_eq!(env.get("PATH"), None, "an embedded newline must not inject a record");
    }

    #[test]
    fn skips_malformed_entries_but_keeps_the_rest() {
        let mut out = b"B".to_vec();
        out.extend_from_slice(b"no-equals-sign\0"); // no '='
        out.extend_from_slice(b"=novalue\0"); // empty key
        out.extend_from_slice(b"BAD=\xff\xfe\0"); // non-UTF-8 value
        out.extend_from_slice(b"GOOD=yes\0");
        out.extend_from_slice(b"E");
        let env = parse_marked_env(&out, "B", "E").unwrap();
        assert_eq!(env.get("GOOD").unwrap(), "yes");
        assert_eq!(env.len(), 1);
    }

    #[test]
    fn rejects_missing_markers_and_an_empty_region() {
        let out = framed("B", "E", &["PATH=/a"]);
        assert!(parse_marked_env(&out, "NOPE", "E").is_err());
        assert!(parse_marked_env(&out, "B", "NOPE").is_err());
        assert!(parse_marked_env(b"BE", "B", "E").is_err());
        assert!(parse_marked_env(b"", "B", "E").is_err());
    }

    #[test]
    fn capture_script_dumps_between_the_markers() {
        let s = capture_script("BEGIN_x", "END_x");
        let begin = s.find("BEGIN_x").unwrap();
        let dump = s.find("/usr/bin/env -0").unwrap();
        let end = s.find("END_x").unwrap();
        assert!(begin < dump && dump < end, "markers must bracket the dump: {s}");
        assert!(s.contains("/usr/bin/printf"), "printf must be absolute (aliases expand under -i)");
    }

    // ---- PaneEnv::resolve -------------------------------------------------

    #[test]
    fn captured_path_beats_the_daemons() {
        let env = PaneEnv::resolve(
            Some(map(&[("PATH", "/opt/homebrew/bin:/usr/bin")])),
            map(&[("PATH", "/usr/bin:/bin")]),
            None,
            "/bin/zsh",
        );
        assert_eq!(env.get("PATH").unwrap(), "/opt/homebrew/bin:/usr/bin");
    }

    #[test]
    fn an_absent_or_empty_captured_path_falls_back_to_the_daemons() {
        let daemon = map(&[("PATH", "/usr/bin:/bin")]);
        for captured in [map(&[]), map(&[("PATH", "")])] {
            let env = PaneEnv::resolve(Some(captured), daemon.clone(), None, "/bin/sh");
            assert_eq!(env.get("PATH").unwrap(), "/usr/bin:/bin");
        }
    }

    #[test]
    fn the_exe_dir_is_prepended_exactly_once() {
        let exe = Path::new("/Apps/Clowder.app/Contents/MacOS");
        let env = PaneEnv::resolve(Some(map(&[("PATH", "/usr/bin")])), map(&[]), Some(exe), "/bin/sh");
        assert_eq!(env.get("PATH").unwrap(), "/Apps/Clowder.app/Contents/MacOS:/usr/bin");

        // Already present (the app prepends it to the daemon's PATH, and path_helper keeps it) —
        // don't duplicate it.
        let already = map(&[("PATH", "/usr/bin:/Apps/Clowder.app/Contents/MacOS")]);
        let env = PaneEnv::resolve(Some(already), map(&[]), Some(exe), "/bin/sh");
        assert_eq!(env.get("PATH").unwrap(), "/usr/bin:/Apps/Clowder.app/Contents/MacOS");
    }

    #[test]
    fn the_daemon_owns_the_clowder_namespace() {
        let env = PaneEnv::resolve(
            // A stale export in someone's .zshrc pointing at another daemon's runtime dir.
            Some(map(&[("CLOWDER_SOCK", "/stale/clowder.sock"), ("EDITOR", "hx")])),
            map(&[("CLOWDER_SOCK", "/real/clowder.sock"), ("EDITOR", "vi")]),
            None,
            "/bin/sh",
        );
        assert_eq!(env.get("CLOWDER_SOCK").unwrap(), "/real/clowder.sock");
        // ...but only that namespace: the user's own settings still win.
        assert_eq!(env.get("EDITOR").unwrap(), "hx");
    }

    #[test]
    fn term_comes_from_the_daemon_never_the_capture() {
        // An rc file setting TERM describes the user's *other* terminal, not this pane.
        let env = PaneEnv::resolve(
            Some(map(&[("TERM", "screen-256color")])),
            map(&[("TERM", "xterm-ghostty")]),
            None,
            "/bin/sh",
        );
        assert_eq!(env.get("TERM").unwrap(), "xterm-ghostty");

        // A GUI daemon has no TERM at all — the captured one must not fill the gap, but the pane
        // still needs a color-capable TERM, so it gets the default rather than none.
        let env = PaneEnv::resolve(Some(map(&[("TERM", "screen-256color")])), map(&[]), None, "/bin/sh");
        assert_eq!(env.get("TERM").unwrap(), DEFAULT_TERM);
    }

    #[test]
    fn colorterm_comes_from_the_daemon_never_the_capture() {
        // Same shape as TERM: the daemon's own COLORTERM wins over a captured one.
        let env = PaneEnv::resolve(
            Some(map(&[("COLORTERM", "24bit")])),
            map(&[("COLORTERM", "truecolor")]),
            None,
            "/bin/sh",
        );
        assert_eq!(env.get("COLORTERM").unwrap(), "truecolor");

        // A GUI daemon has no COLORTERM at all — fall back to the default rather than leaving the
        // pane with none, since libghostty renders true color regardless.
        let env = PaneEnv::resolve(Some(map(&[("COLORTERM", "24bit")])), map(&[]), None, "/bin/sh");
        assert_eq!(env.get("COLORTERM").unwrap(), DEFAULT_COLORTERM);
    }

    #[test]
    fn capture_artefacts_never_reach_a_pane() {
        let artefacts = map(&[
            ("PWD", "/Users/rc"),
            ("OLDPWD", "/"),
            ("SHLVL", "2"),
            ("_", "/usr/bin/env"),
            ("COLUMNS", "80"),
            ("LINES", "24"),
            ("TERMCAP", "junk"),
            (CAPTURE_MARKER_VAR, "1"),
        ]);
        // Present in BOTH sources, so this also pins that they aren't re-added from the daemon's.
        let env = PaneEnv::resolve(Some(artefacts.clone()), artefacts, None, "/bin/sh");
        for key in CAPTURE_ARTEFACTS {
            assert_eq!(env.get(key), None, "{key} must not survive into a pane");
        }
    }

    #[test]
    fn shell_is_forced_to_the_resolved_shell() {
        let env = PaneEnv::resolve(Some(map(&[("SHELL", "/bin/bash")])), map(&[]), None, "/bin/zsh");
        assert_eq!(env.get("SHELL").unwrap(), "/bin/zsh");

        // The real case: a login zsh exports no SHELL at all, so there is nothing to override.
        let env = PaneEnv::resolve(Some(map(&[("PATH", "/usr/bin")])), map(&[]), None, "/bin/zsh");
        assert_eq!(env.get("SHELL").unwrap(), "/bin/zsh");
    }

    #[test]
    fn no_capture_falls_back_to_the_daemons_environment() {
        let daemon = map(&[("PATH", "/usr/bin:/bin"), ("HOME", "/Users/rc"), ("PWD", "/tmp")]);
        let env = PaneEnv::resolve(None, daemon, None, "/bin/zsh");
        assert_eq!(env.get("PATH").unwrap(), "/usr/bin:/bin");
        assert_eq!(env.get("HOME").unwrap(), "/Users/rc");
        assert_eq!(env.get("PWD"), None); // same normalisation applies
        assert_eq!(env.get("SHELL").unwrap(), "/bin/zsh");
    }
}
