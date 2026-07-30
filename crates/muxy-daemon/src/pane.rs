use anyhow::{anyhow, Result};
use muxy_proto::PaneId;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::Read;
use std::sync::{Arc, Mutex};
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
    pub fn spawn(id: PaneId, cmd: PaneCommand, cols: u16, rows: u16, backlog_cap: usize) -> Result<Pane> {
        let pty = native_pty_system();
        let pair = pty.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

        let mut builder = CommandBuilder::new(&cmd.program);
        builder.args(&cmd.args);
        if let Some(cwd) = &cmd.cwd {
            builder.cwd(cwd);
        }
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
                            let mut b = bl.lock().unwrap();
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
        let mut w = self.writer.lock().map_err(|_| anyhow!("writer poisoned"))?;
        w.write_all(bytes)?;
        w.flush()?;
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let m = self.master.lock().map_err(|_| anyhow!("master poisoned"))?;
        m.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        *self.size.lock().unwrap() = (cols, rows);
        Ok(())
    }

    pub fn size(&self) -> (u16, u16) {
        *self.size.lock().unwrap()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.output_tx.subscribe()
    }

    pub fn backlog(&self) -> Vec<u8> {
        self.backlog.lock().unwrap().clone()
    }

    /// Atomically snapshot the current backlog and subscribe to live output.
    /// Because the reader thread holds the backlog lock across both the backlog
    /// append and the broadcast send, taking that same lock here guarantees the
    /// returned receiver sees exactly the chunks appended AFTER this snapshot —
    /// none dropped, none duplicated.
    pub fn snapshot_and_subscribe(&self) -> (Vec<u8>, broadcast::Receiver<Vec<u8>>) {
        let b = self.backlog.lock().unwrap();
        let rx = self.output_tx.subscribe();
        let snap = b.clone();
        drop(b);
        (snap, rx)
    }

    pub fn kill(&self) -> anyhow::Result<()> {
        self.killer.lock().map_err(|_| anyhow!("killer poisoned"))?.kill()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::time::Duration;

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
        let pane = Pane::spawn(PaneId(1), sh("printf muxy-hello"), 80, 24, 256 * 1024).unwrap();
        // give the reader thread time to drain
        for _ in 0..50 {
            if pane.backlog().windows(10).any(|w| w == b"muxy-hello") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let out = pane.backlog();
        assert!(
            out.windows(10).any(|w| w == b"muxy-hello"),
            "backlog missing output: {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    #[tokio::test]
    async fn pane_forwards_input_to_child() {
        // `cat` echoes stdin back to stdout
        let pane = Pane::spawn(PaneId(2), sh("cat"), 80, 24, 256 * 1024).unwrap();
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
        let pane = Pane::spawn(PaneId(3), sh("cat"), 80, 24, 256 * 1024).unwrap();
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
}
