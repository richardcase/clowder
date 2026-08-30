// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests for the login-environment capture (issue #76).
//!
//! Every test drives a **fake shell** written into a tempdir, never the developer's real login
//! shell. Sourcing a real `~/.zshrc` would make these tests depend on the machine they run on — and
//! a rc file that blocks on input would hang the whole suite rather than fail one test.
//!
//! The fake shells are invoked exactly as the daemon invokes a real one: `-l -i -c <script>`, so
//! `$4` is the script.

use clowder_daemon::login_env::{capture, CaptureSpec};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Write an executable `sh` script into `dir` and return its path.
fn fake_shell(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn spec(shell: &Path, timeout_ms: u64) -> CaptureSpec {
    CaptureSpec {
        shell: shell.to_string_lossy().into_owned(),
        timeout: Duration::from_millis(timeout_ms),
        cwd: None,
    }
}

#[tokio::test]
async fn captures_the_environment_a_login_shell_produces() {
    let dir = tempfile::tempdir().unwrap();
    // A shell that exports something the way a real ~/.zprofile would, then runs the script.
    let shell = fake_shell(
        &dir.path().to_path_buf(),
        "loginish",
        r#"PATH="/opt/homebrew/bin:$PATH"; export PATH
CLOWDER_TEST_MARKER=from-rc; export CLOWDER_TEST_MARKER
exec /bin/sh -c "$4"
"#,
    );

    let env = capture(&spec(&shell, 5000)).await.unwrap();
    assert!(
        env["PATH"].starts_with("/opt/homebrew/bin:"),
        "the rc file's PATH edit must survive the capture, got {:?}",
        env["PATH"]
    );
    assert_eq!(env["CLOWDER_TEST_MARKER"], "from-rc");
    // The daemon tells the capture child what it is, so an rc file can skip heavy work.
    assert_eq!(env["CLOWDER_LOGIN_ENV_CAPTURE"], "1");
}

#[tokio::test]
async fn survives_rc_noise_a_decoy_marker_and_multiline_values() {
    let dir = tempfile::tempdir().unwrap();
    // Everything a real rc file throws at stdout: a motd, a version-manager banner, a marker-shaped
    // string from some *other* run, and an EXIT trap firing after the dump.
    let shell = fake_shell(
        &dir.path().to_path_buf(),
        "noisy",
        r#"echo "Welcome! You have mail."
echo "__CLOWDER_ENV_BEGIN_deadbeef__ PATH=/decoy __CLOWDER_ENV_END_deadbeef__"
MULTI="line one
PATH=/injected
line three"; export MULTI
trap 'echo "goodbye"' EXIT
/bin/sh -c "$4"
"#,
    );

    let env = capture(&spec(&shell, 5000)).await.unwrap();
    assert_eq!(env["MULTI"], "line one\nPATH=/injected\nline three");
    assert_ne!(env["PATH"], "/decoy", "a decoy marker must not be mistaken for ours");
    assert_ne!(env["PATH"], "/injected", "a newline inside a value must not inject a record");
}

#[tokio::test]
async fn a_hanging_rc_file_times_out_instead_of_wedging_startup() {
    let dir = tempfile::tempdir().unwrap();
    let shell = fake_shell(&dir.path().to_path_buf(), "hangs", "exec sleep 60\n");

    let started = Instant::now();
    let err = capture(&spec(&shell, 300)).await.unwrap_err();
    let elapsed = started.elapsed();

    assert!(err.to_string().contains("timed out"), "unexpected error: {err}");
    assert!(
        elapsed < Duration::from_secs(5),
        "capture should give up at ~300ms, took {elapsed:?}"
    );
}

#[tokio::test]
async fn a_timed_out_capture_kills_the_shell() {
    // One leaked shell per daemon start would be a real leak, so prove the process is gone rather
    // than merely abandoned.
    //
    // The timeout is deliberately generous: the shell has to be scheduled and write its PID before
    // the deadline, and with a short one that races under parallel test load — the capture kills it
    // first and the pidfile never appears. The test still finishes in ~1.5s.
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let shell = fake_shell(
        &dir.path().to_path_buf(),
        "hangs-slowly",
        &format!("echo $$ > '{}'\nexec sleep 60\n", pidfile.display()),
    );

    let err = capture(&spec(&shell, 1500)).await.unwrap_err();
    assert!(err.to_string().contains("timed out"), "unexpected error: {err}");

    let pid = std::fs::read_to_string(&pidfile)
        .expect("fake shell should have recorded its PID well inside a 1.5s deadline");
    let pid = pid.trim();
    let mut dead = false;
    for _ in 0..100 {
        if !pid_alive(pid) {
            dead = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(dead, "the timed-out capture shell (pid {pid}) was left running");
}

fn pid_alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn a_shell_that_prints_no_markers_is_an_error_not_an_empty_environment() {
    let dir = tempfile::tempdir().unwrap();
    // Silently returning an empty map would wipe every pane's environment — it must fail loudly so
    // the caller falls back to inheriting the daemon's.
    let quiet = fake_shell(&dir.path().to_path_buf(), "quiet", "exit 0\n");
    assert!(capture(&spec(&quiet, 5000)).await.is_err());

    let chatty = fake_shell(&dir.path().to_path_buf(), "chatty", "echo hello; exit 0\n");
    assert!(capture(&spec(&chatty, 5000)).await.is_err());
}

#[tokio::test]
async fn a_shell_that_fails_to_start_or_exits_nonzero_is_an_error() {
    let dir = tempfile::tempdir().unwrap();

    let broken = fake_shell(&dir.path().to_path_buf(), "broken", "echo 'rc error' >&2; exit 1\n");
    assert!(capture(&spec(&broken, 5000)).await.is_err());

    // No such shell at all — e.g. a passwd entry pointing at an uninstalled shell.
    let missing = dir.path().join("does-not-exist");
    assert!(capture(&spec(&missing, 5000)).await.is_err());
}

#[tokio::test]
async fn the_capture_runs_in_the_requested_directory() {
    let dir = tempfile::tempdir().unwrap();
    let shell = fake_shell(&dir.path().to_path_buf(), "pwd-reporting", "WHERE=$(pwd); export WHERE\nexec /bin/sh -c \"$4\"\n");

    let home = tempfile::tempdir().unwrap();
    let mut s = spec(&shell, 5000);
    s.cwd = Some(home.path().to_path_buf());
    let env = capture(&s).await.unwrap();

    // Canonicalised because macOS tempdirs live under a /var → /private/var symlink.
    assert_eq!(
        std::fs::canonicalize(&env["WHERE"]).unwrap(),
        std::fs::canonicalize(home.path()).unwrap()
    );
    // ...but PWD itself never reaches a pane; that is asserted in PaneEnv::resolve's unit tests.
}
