//! Reach a daemon, report what it presented, and persist nothing.

use crate::forward::RemoteTarget;
use crate::tofu::Trust;
use clowder_proto::{write_hello, Channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::net::TcpStream;

/// What one probe observed. Deliberately not a `Result`: "unreachable" and "reachable but the
/// token was refused" are both useful answers the pairing UI needs to show differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub reachable: bool,
    /// The server certificate's SHA-256, lowercase hex. `None` on a plaintext daemon, or when
    /// the TLS handshake failed.
    pub fingerprint: Option<String>,
    /// Whether the daemon accepted our token. NOTE: a plaintext daemon accepts anything, so
    /// `authenticated` alone does not mean "authenticated" — callers must pair it with `tls`.
    pub authenticated: bool,
    pub error: Option<String>,
}

impl ProbeResult {
    fn unreachable(e: impl std::fmt::Display) -> Self {
        Self { reachable: false, fingerprint: None, authenticated: false, error: Some(e.to_string()) }
    }
}

/// Reach `target`, report what it presented, and **persist nothing** — not `remote_known_hosts`,
/// not the registry. Pairing is a two-step flow precisely so that observing and trusting are
/// separate acts, with a human in between.
pub async fn probe(target: &RemoteTarget, timeout: Duration) -> ProbeResult {
    // A plain connect under one timeout — NOT `dial_with_backoff`, which takes ~15s to give up.
    // A probe runs while a user waits, and "is this address right?" must answer in seconds.
    let tcp = match tokio::time::timeout(timeout, TcpStream::connect(&target.address)).await {
        Err(_) => return ProbeResult::unreachable(format!("timed out after {timeout:?}")),
        Ok(Err(e)) => return ProbeResult::unreachable(e),
        Ok(Ok(s)) => s,
    };

    let sink: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let stream: Box<dyn ProbeStream> = if target.tls {
        let connector = tokio_rustls::TlsConnector::from(crate::tofu::connector(Trust::Capture(sink.clone())));
        let name = match tokio_rustls::rustls::pki_types::ServerName::try_from("clowder") {
            Ok(n) => n,
            Err(e) => return ProbeResult::unreachable(format!("server name: {e}")),
        };
        match tokio::time::timeout(timeout, connector.connect(name, tcp)).await {
            Err(_) => {
                return ProbeResult {
                    reachable: true,
                    fingerprint: fp_of(&sink),
                    authenticated: false,
                    error: Some(format!("TLS handshake timed out after {timeout:?}")),
                }
            }
            Ok(Err(e)) => {
                return ProbeResult {
                    reachable: true,
                    fingerprint: fp_of(&sink),
                    authenticated: false,
                    error: Some(format!("TLS handshake failed: {e}")),
                }
            }
            Ok(Ok(s)) => Box::new(s),
        }
    } else {
        Box::new(tcp)
    };

    let fingerprint = fp_of(&sink);
    match authenticate(stream, target, timeout).await {
        Ok(()) => ProbeResult { reachable: true, fingerprint, authenticated: true, error: None },
        Err(e) => ProbeResult {
            reachable: true,
            fingerprint,
            authenticated: false,
            error: Some(e),
        },
    }
}

/// Send a Control hello and wait for the daemon's first line.
///
/// The daemon's control handler emits a `worktreeList` event unprompted as soon as it dispatches,
/// and `handle_remote_conn` drops the connection BEFORE dispatch when the token is wrong. So a
/// line means the token was accepted, and EOF means it was not — no new protocol needed.
async fn authenticate(
    mut stream: Box<dyn ProbeStream>,
    target: &RemoteTarget,
    timeout: Duration,
) -> Result<(), String> {
    let token = if target.tls { target.token.as_deref() } else { None };
    write_hello(&mut stream, Channel::Control, token)
        .await
        .map_err(|e| format!("sending hello: {e}"))?;

    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    match tokio::time::timeout(timeout, reader.read_line(&mut line)).await {
        Err(_) => Err("the daemon accepted the connection but sent nothing (bad or missing token?)".into()),
        Ok(Err(e)) => Err(format!("reading the daemon's greeting: {e}")),
        Ok(Ok(0)) => Err("the daemon closed the connection (bad or missing token)".into()),
        Ok(Ok(_)) => Ok(()),
    }
}

fn fp_of(sink: &Arc<Mutex<Option<String>>>) -> Option<String> {
    sink.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Object-safe alias so the TLS and plaintext streams share one path (mirrors `forward`'s
/// `RemoteStream`).
trait ProbeStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> ProbeStream for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forward::RemoteTarget;

    /// Guards the process-global `XDG_STATE_HOME` these tests set, against other env-mutating
    /// tests in this crate's binary (see the same pattern in `tofu.rs`).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn target(addr: &str, tls: bool, token: Option<&str>) -> RemoteTarget {
        RemoteTarget {
            label: "probe-test".into(),
            address: addr.into(),
            token: token.map(String::from),
            tls,
            fingerprint: None,
        }
    }

    #[tokio::test]
    async fn a_dead_port_is_unreachable_and_fails_fast() {
        let t = target("127.0.0.1:1", false, None);
        let started = std::time::Instant::now();
        let r = probe(&t, Duration::from_secs(3)).await;
        assert!(!r.reachable);
        assert!(!r.authenticated);
        assert!(r.error.is_some());
        // dial_with_backoff would take ~15s; a probe must not.
        assert!(started.elapsed() < Duration::from_secs(5), "probe must fail fast");
    }

    #[tokio::test]
    async fn a_tls_daemon_with_the_right_token_authenticates_and_reports_its_fingerprint() {
        use clowder_daemon::{server::Daemon, FakeNotifier};
        use std::sync::Arc;
        use tokio::net::TcpListener;

        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", state.path());

        // Pre-populate known_hosts with sentinel content so we can prove the probe never touches
        // it, rather than merely asserting the path stays absent (a vacuous check: `Trust::Capture`
        // carries no path field, so nothing could write there regardless of implementation).
        let kh = crate::tofu::known_hosts_path();
        std::fs::create_dir_all(kh.parent().unwrap()).unwrap();
        let sentinel = "sentinel-host aa11\n";
        std::fs::write(&kh, sentinel).unwrap();

        let creds = clowder_daemon::remote_tls::load_or_generate().unwrap();
        let token = creds.token.clone();
        let expected_fp = clowder_daemon::remote_tls::fingerprint(&creds);
        let tls = clowder_daemon::remote::build_remote_tls(&creds).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let daemon = Arc::new(Daemon::new_with(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-m11a-probe.sock"),
        ));
        tokio::spawn(daemon.serve_remote(listener, Some(tls)));

        let good = probe(&target(&addr.to_string(), true, Some(&token)), Duration::from_secs(5)).await;
        assert!(good.reachable);
        assert!(good.authenticated, "a valid token must authenticate: {:?}", good.error);
        assert_eq!(good.fingerprint.as_deref(), Some(expected_fp.as_str()));

        // A wrong token is refused — but the fingerprint was still observed, which is what lets
        // the pairing UI show the user what it saw even when auth fails.
        let bad = probe(&target(&addr.to_string(), true, Some("wrong")), Duration::from_secs(5)).await;
        assert!(bad.reachable);
        assert!(!bad.authenticated);
        assert_eq!(bad.fingerprint.as_deref(), Some(expected_fp.as_str()));

        // Nothing was persisted: a probe must never pin. Assert byte-for-byte, not mere absence.
        assert_eq!(
            std::fs::read_to_string(&kh).unwrap(),
            sentinel,
            "probe must not write known_hosts"
        );

        std::env::remove_var("XDG_STATE_HOME");
    }

    #[tokio::test]
    async fn a_plaintext_daemon_reports_no_fingerprint() {
        use clowder_daemon::{server::Daemon, FakeNotifier};
        use std::sync::Arc;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let daemon = Arc::new(Daemon::new_with(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-m11a-probe2.sock"),
        ));
        tokio::spawn(daemon.serve_remote(listener, None));

        let r = probe(&target(&addr.to_string(), false, None), Duration::from_secs(5)).await;
        assert!(r.reachable);
        assert_eq!(r.fingerprint, None, "no TLS means no certificate to show");
        // A plaintext daemon passes `expected_token: None`, so it accepts anything. The CLI
        // reports this honestly as "no authentication" rather than as a success.
        assert!(r.authenticated);
    }
}
