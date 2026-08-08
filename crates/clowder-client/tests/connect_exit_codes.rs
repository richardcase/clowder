//! `clowder connect` exit codes are a contract with the macOS app's DaemonSupervisor:
//! 4 = the first dial never landed (stop and show the user), anything else = relaunchable.
//!
//! The default socket directory is a second contract with the same app: ClowderCore's
//! `forwarderSocketDir` derives `<control parent>/remote` independently, so the two must agree.

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

/// With no `--socket-dir`, the forwarder must bind exactly where it always has:
/// `<control-sock parent>/remote/{clowder.sock,clowder-control.sock}` — FLAT, with no per-host
/// segment. The macOS app re-derives this path in Swift and does not pass `--socket-dir`, so a
/// per-host default would leave it watching an empty directory forever. Per-host isolation is
/// opt-in via `--socket-dir`, which is why this default is a compatibility guarantee and not an
/// implementation detail.
#[test]
fn the_default_socket_dir_is_flat_and_has_no_per_host_segment() {
    let tmp = tempfile::tempdir().unwrap();

    // A real listener, so the fail-fast pre-dial lands and we reach the bind. It is never
    // accept()ed — the kernel completes the handshake from the backlog, which is all the
    // pre-dial checks.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let control = tmp.path().join("clowder-control.sock");
    let mut child = Command::new(env!("CARGO_BIN_EXE_clowder"))
        .args(["connect", &addr])
        .env("CLOWDER_CONTROL_SOCK", &control)
        .env("CLOWDER_HOSTS_FILE", tmp.path().join("hosts.json"))
        .env("XDG_STATE_HOME", tmp.path())
        .spawn()
        .expect("run clowder");

    let flat = tmp.path().join("remote");
    let render_sock = flat.join("clowder.sock");
    let control_sock = flat.join("clowder-control.sock");

    // The forwarder binds both sockets before serving, so they appear promptly or not at all.
    for _ in 0..100 {
        if render_sock.exists() && control_sock.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Snapshot before killing, so the assertions below report on a stable filesystem.
    let got_render = render_sock.exists();
    let got_control = control_sock.exists();
    let per_host = flat.join(&addr);
    let got_per_host = per_host.exists();

    let _ = child.kill();
    let _ = child.wait();

    assert!(got_render, "expected the render socket at {}", render_sock.display());
    assert!(got_control, "expected the control socket at {}", control_sock.display());
    assert!(
        !got_per_host,
        "the default must not create a per-host subdirectory, but {} exists",
        per_host.display()
    );
}
