// SPDX-License-Identifier: Apache-2.0

use crate::login_env::PaneEnv;
use anyhow::Result;
use clowder_proto::PaneId;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::Read;
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct PaneCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<std::path::PathBuf>,
    pub env: Vec<(String, String)>,
}

pub struct Pane {
    id: PaneId,
    writer: Mutex<Box<dyn std::io::Write + Send>>,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    output_tx: broadcast::Sender<Vec<u8>>,
    backlog: Arc<Mutex<Vec<u8>>>,
    exit_rx: tokio::sync::watch::Receiver<Option<Option<i32>>>,
    size: Mutex<(u16, u16)>,
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
}

impl Pane {
    /// `base` is the environment every child starts from — see `crate::login_env`. It is a
    /// parameter rather than a field on `PaneCommand` on purpose: `PaneCommand` is built at a dozen
    /// sites, and a field there would invite `env: vec![]`-style defaults that quietly reinstate
    /// issue #76. Here the compiler enumerates the callers instead.
    pub fn spawn(
        id: PaneId,
        cmd: PaneCommand,
        cols: u16,
        rows: u16,
        backlog_cap: usize,
        base: &PaneEnv,
    ) -> Result<Pane> {
        let pty = native_pty_system();
        let pair = pty.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

        let mut builder = CommandBuilder::new(&cmd.program);
        builder.args(&cmd.args);
        if let Some(cwd) = &cmd.cwd {
            builder.cwd(cwd);
        }
        // Discard portable-pty's own snapshot of the daemon's environment and state the child's
        // outright. Without this the child would be "whatever the daemon inherited, plus whatever
        // we remembered to override" — which under launchd is a PATH with no `claude` in it (#76).
        //
        // Note this also decides how the program is resolved: portable-pty searches THIS PATH, in
        // the parent, before forking.
        builder.env_clear();
        for (k, v) in base.iter() {
            builder.env(k, v);
        }
        // Per-pane vars last, so CLOWDER_AGENT_ID/CLOWDER_HOOK_SOCK beat anything else.
        for (k, v) in &cmd.env {
            builder.env(k, v);
        }
        let mut child = pair.slave.spawn_command(builder)?;
        let killer = child.clone_killer();
        drop(pair.slave);

        let writer = pair.master.take_writer()?;
        let mut reader = pair.master.try_clone_reader()?;

        let (output_tx, _) = broadcast::channel::<Vec<u8>>(1024);
        let backlog = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (exit_tx, exit_rx) = tokio::sync::watch::channel(None);

        // Blocking reader thread -> broadcast + backlog.
        let tx = output_tx.clone();
        let bl = backlog.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = buf[..n].to_vec();
                        {
                            let mut b = bl.lock();
                            b.extend_from_slice(&chunk);
                            if b.len() > backlog_cap {
                                let drop = b.len() - backlog_cap;
                                b.drain(0..drop);
                            }
                            let _ = tx.send(chunk);
                        }
                    }
                }
            }
        });

        // Blocking wait thread -> watch channel.
        std::thread::spawn(move || {
            let status = child.wait().ok();
            let code = status.map(|s| s.exit_code() as i32);
            let _ = exit_tx.send(Some(code));
        });

        Ok(Pane {
            id,
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            output_tx,
            backlog,
            exit_rx,
            size: Mutex::new((cols, rows)),
            killer: Mutex::new(killer),
        })
    }

    pub fn id(&self) -> PaneId {
        self.id
    }

    pub fn write_input(&self, bytes: &[u8]) -> Result<()> {
        use std::io::Write;
        let mut w = self.writer.lock();
        w.write_all(bytes)?;
        w.flush()?;
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let m = self.master.lock();
        m.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        *self.size.lock() = (cols, rows);
        Ok(())
    }

    pub fn size(&self) -> (u16, u16) {
        *self.size.lock()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.output_tx.subscribe()
    }

    pub fn backlog(&self) -> Vec<u8> {
        self.backlog.lock().clone()
    }

    /// Atomically snapshot the current backlog and subscribe to live output.
    /// Because the reader thread holds the backlog lock across both the backlog
    /// append and the broadcast send, taking that same lock here guarantees the
    /// returned receiver sees exactly the chunks appended AFTER this snapshot —
    /// none dropped, none duplicated.
    pub fn snapshot_and_subscribe(&self) -> (Vec<u8>, broadcast::Receiver<Vec<u8>>) {
        let b = self.backlog.lock();
        let rx = self.output_tx.subscribe();
        let snap = b.clone();
        drop(b);
        (snap, rx)
    }

    pub fn kill(&self) -> anyhow::Result<()> {
        self.killer.lock().kill()?;
        Ok(())
    }

    pub async fn wait_exit(&self) -> Option<i32> {
        let mut rx = self.exit_rx.clone();
        loop {
            if let Some(code) = *rx.borrow() {
                return code;
            }
            if rx.changed().await.is_err() {
                return None;
            }
        }
    }
}

impl Drop for Pane {
    /// Backstop: a dropped pane must never leak its child process. `kill()` on an
    /// already-exited child is harmless.
    fn drop(&mut self) {
        let _ = self.killer.lock().kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::time::Duration;

    /// The base environment these tests spawn against — the daemon's own, i.e. pre-#76 behaviour.
    fn test_env() -> PaneEnv {
        PaneEnv::inherited("/bin/sh")
    }

    fn sh(script: &str) -> PaneCommand {
        PaneCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            env: vec![("PS1".into(), "".into())],
        }
    }

    #[tokio::test]
    async fn pane_captures_child_output_in_backlog() {
        let pane = Pane::spawn(PaneId(1), sh("printf clowder-hello"), 80, 24, 256 * 1024, &test_env()).unwrap();
        // give the reader thread time to drain
        for _ in 0..50 {
            if pane.backlog().windows(13).any(|w| w == b"clowder-hello") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let out = pane.backlog();
        assert!(
            out.windows(13).any(|w| w == b"clowder-hello"),
            "backlog missing output: {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    #[tokio::test]
    async fn pane_forwards_input_to_child() {
        // `cat` echoes stdin back to stdout
        let pane = Pane::spawn(PaneId(2), sh("cat"), 80, 24, 256 * 1024, &test_env()).unwrap();
        let mut sub = pane.subscribe();
        pane.write_input(b"ping\n").unwrap();
        let mut seen = Vec::new();
        for _ in 0..50 {
            if let Ok(chunk) = tokio::time::timeout(Duration::from_millis(50), sub.recv()).await {
                if let Ok(bytes) = chunk {
                    seen.extend_from_slice(&bytes);
                    if seen.windows(4).any(|w| w == b"ping") {
                        break;
                    }
                }
            }
        }
        assert!(seen.windows(4).any(|w| w == b"ping"), "child did not echo input");
    }

    #[tokio::test]
    async fn snapshot_and_subscribe_is_atomic_with_reader_thread() {
        // `cat` echoes stdin back to stdout
        let pane = Pane::spawn(PaneId(3), sh("cat"), 80, 24, 256 * 1024, &test_env()).unwrap();
        pane.write_input(b"before\n").unwrap();

        // Wait until "before" has landed in the backlog.
        for _ in 0..50 {
            if pane.backlog().windows(6).any(|w| w == b"before") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let (snap, mut sub) = pane.snapshot_and_subscribe();
        assert!(
            snap.windows(6).any(|w| w == b"before"),
            "snapshot missing prior output: {:?}",
            String::from_utf8_lossy(&snap)
        );

        pane.write_input(b"after\n").unwrap();
        let mut seen = Vec::new();
        for _ in 0..50 {
            if let Ok(Ok(bytes)) =
                tokio::time::timeout(Duration::from_millis(50), sub.recv()).await
            {
                seen.extend_from_slice(&bytes);
                if seen.windows(5).any(|w| w == b"after") {
                    break;
                }
            }
        }
        assert!(
            seen.windows(5).any(|w| w == b"after"),
            "subscriber did not receive post-snapshot output"
        );
    }

    fn pid_alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn dropping_a_pane_kills_its_child() {
        // The child records its own PID, then execs `sleep` (keeping that PID). After we drop the
        // Pane, `Drop for Pane` must kill that PID.
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let script = format!("echo $$ > {}; exec sleep 30", pidfile.display());
        let pane = Pane::spawn(PaneId(9), sh(&script), 80, 24, 4096, &test_env()).unwrap();

        // Wait for the child to write its PID.
        let mut pid = String::new();
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(&pidfile) {
                if !s.trim().is_empty() {
                    pid = s.trim().to_string();
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!pid.is_empty(), "child never wrote its PID");
        assert!(pid_alive(&pid), "child should be alive before drop");

        drop(pane);

        let mut dead = false;
        for _ in 0..100 {
            if !pid_alive(&pid) {
                dead = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(dead, "child process should be killed when its Pane is dropped");
    }

    /// Drain a pane's output until `needle` shows up, then return everything seen.
    async fn wait_for(pane: &Pane, needle: &[u8]) -> Vec<u8> {
        for _ in 0..100 {
            let out = pane.backlog();
            if out.windows(needle.len()).any(|w| w == needle) {
                return out;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        pane.backlog()
    }

    #[tokio::test]
    async fn a_child_sees_the_base_environment_and_nothing_else() {
        // The regression guard for #76's other half: the child's environment must be exactly what
        // the PaneEnv says, not "whatever the daemon happened to inherit". Without env_clear() this
        // leaks every variable in the test process.
        std::env::set_var("CLOWDER_PANE_LEAK_PROBE", "leaked");

        let base = PaneEnv::resolve(
            Some(
                [("PATH".to_string(), "/usr/bin:/bin".to_string()), ("FROM_BASE".to_string(), "yes".to_string())]
                    .into_iter()
                    .collect(),
            ),
            Default::default(),
            None,
            "/bin/sh",
        );
        let cmd = PaneCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "printf 'base=%s leak=[%s]\\n' \"$FROM_BASE\" \"$CLOWDER_PANE_LEAK_PROBE\"".into()],
            cwd: None,
            env: vec![],
        };
        let pane = Pane::spawn(PaneId(11), cmd, 80, 24, 4096, &base).unwrap();
        let out = wait_for(&pane, b"base=").await;
        let out = String::from_utf8_lossy(&out);

        std::env::remove_var("CLOWDER_PANE_LEAK_PROBE");
        assert!(out.contains("base=yes"), "base env did not reach the child: {out:?}");
        assert!(out.contains("leak=[]"), "the daemon's own environment leaked into the child: {out:?}");
    }

    #[tokio::test]
    async fn per_pane_vars_win_over_the_base_environment() {
        let base = PaneEnv::resolve(
            Some([("CLOWDER_HOOK_SOCK".to_string(), "/from/base.sock".to_string())].into_iter().collect()),
            Default::default(),
            None,
            "/bin/sh",
        );
        let cmd = PaneCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "printf 'sock=%s\\n' \"$CLOWDER_HOOK_SOCK\"".into()],
            cwd: None,
            env: vec![("CLOWDER_HOOK_SOCK".into(), "/per/pane.sock".into())],
        };
        let pane = Pane::spawn(PaneId(12), cmd, 80, 24, 4096, &base).unwrap();
        let out = wait_for(&pane, b"sock=").await;
        assert!(
            String::from_utf8_lossy(&out).contains("sock=/per/pane.sock"),
            "per-pane env must beat the base: {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    #[tokio::test]
    async fn a_bare_program_name_resolves_against_the_base_path() {
        // This is issue #76 in miniature: `claude` is launched by bare name, and the ONLY thing that
        // decides whether it is found is the PATH in the PaneEnv.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("clowder-fake-agent");
        std::fs::write(&bin, "#!/bin/sh\nprintf fake-agent-ran\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let base = PaneEnv::resolve(
            Some([("PATH".to_string(), dir.path().to_string_lossy().into_owned())].into_iter().collect()),
            Default::default(),
            None,
            "/bin/sh",
        );
        let cmd = PaneCommand {
            program: "clowder-fake-agent".into(), // bare name, exactly like ClaudeAdapter
            args: vec![],
            // A cwd that does NOT contain the program, so PATH is genuinely what resolves it.
            cwd: Some(std::env::temp_dir()),
            env: vec![],
        };
        let pane = Pane::spawn(PaneId(13), cmd, 80, 24, 4096, &base).unwrap();
        let out = wait_for(&pane, b"fake-agent-ran").await;
        assert!(
            out.windows(14).any(|w| w == b"fake-agent-ran"),
            "a bare program name must resolve against the base PATH: {:?}",
            String::from_utf8_lossy(&out)
        );
    }
}
