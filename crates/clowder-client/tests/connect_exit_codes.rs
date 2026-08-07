//! `clowder connect` exit codes are a contract with the macOS app's DaemonSupervisor:
//! 4 = the first dial never landed (stop and show the user), anything else = relaunchable.

use std::process::Command;

#[test]
fn connect_to_a_dead_address_exits_4() {
    let dir = tempfile::tempdir().unwrap();
    // 127.0.0.1:1 refuses immediately, so this does not wait for the timeout.
    let out = Command::new(env!("CARGO_BIN_EXE_clowder"))
        .args(["connect", "127.0.0.1:1", "--socket-dir"])
        .arg(dir.path())
        .env("CLOWDER_HOSTS_FILE", dir.path().join("hosts.json"))
        .env("XDG_STATE_HOME", dir.path())
        .output()
        .expect("run clowder");
    assert_eq!(out.status.code(), Some(4), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("127.0.0.1:1"),
        "the error must name the address it could not reach"
    );
}

#[test]
fn connect_to_an_unknown_name_exits_1_not_4() {
    // A typo is a user error to be corrected, not an unreachable host to be retried.
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_clowder"))
        .args(["connect", "nosuchhost"])
        .env("CLOWDER_HOSTS_FILE", dir.path().join("hosts.json"))
        .env("XDG_STATE_HOME", dir.path())
        .output()
        .expect("run clowder");
    assert_eq!(out.status.code(), Some(1));
}
