//! Trust-on-first-use verification of the remote daemon's self-signed cert (SSH host-key style).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_rustls::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{DigitallySignedStruct, Error, SignatureScheme};

/// `<client state dir>/clowder/remote_known_hosts` (lines: `<host> <sha256-hex>`).
pub fn known_hosts_path() -> PathBuf {
    clowder_config::remote_state_dir().join("remote_known_hosts")
}

/// Record-or-verify `fp` for `host`. Ok = trusted (recorded on first sight); Err(msg) = refuse.
pub fn check(path: &Path, host: &str, fp: &str) -> Result<(), String> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read known_hosts {}: {e}", path.display())),
    };
    for line in existing.lines() {
        let mut it = line.split_whitespace();
        if let (Some(h), Some(f)) = (it.next(), it.next()) {
            if h == host {
                return if f == fp {
                    Ok(())
                } else {
                    Err(format!(
                        "REMOTE DAEMON IDENTIFICATION HAS CHANGED for {host}: known {f}, got {fp}. \
                         If you rotated the daemon cert, remove the line from {}; otherwise this may be a MITM.",
                        path.display()
                    ))
                };
            }
        }
    }
    // First sight: record and accept.
    if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') { content.push('\n'); }
    content.push_str(&format!("{host} {fp}\n"));
    std::fs::write(path, content).map_err(|e| format!("write known_hosts: {e}"))?;
    Ok(())
}

/// How to decide whether a presented server certificate is the daemon we meant to reach.
#[derive(Debug, Clone)]
pub enum Trust {
    /// The host entry carries a pinned fingerprint: strict compare, no recording, and
    /// `remote_known_hosts` is never consulted. This is what pairing produces.
    Pinned(String),
    /// No pin yet (or a legacy config-only host): today's record-on-first-sight behavior.
    /// `host` is the key written into `known_hosts` — it must be the DIAL ADDRESS, so entries
    /// recorded by earlier versions keep matching.
    Tofu { host: String, known_hosts: PathBuf },
    /// Probe only: accept whatever is presented, publish its fingerprint, persist nothing.
    Capture(Arc<std::sync::Mutex<Option<String>>>),
}

#[derive(Debug)]
pub struct RemoteVerifier {
    pub trust: Trust,
    pub provider: Arc<tokio_rustls::rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for RemoteVerifier {
    fn verify_server_cert(&self, end_entity: &CertificateDer, _i: &[CertificateDer], _n: &ServerName, _o: &[u8], _t: UnixTime) -> Result<ServerCertVerified, Error> {
        let fp = clowder_proto::cert_fingerprint_hex(end_entity);
        let result = match &self.trust {
            Trust::Pinned(expected) => {
                if fp == *expected {
                    Ok(())
                } else {
                    Err(format!(
                        "REMOTE DAEMON IDENTIFICATION HAS CHANGED: pinned {expected}, got {fp}. \
                         If you rotated the daemon cert, re-pair this host; otherwise this may be a MITM."
                    ))
                }
            }
            Trust::Tofu { host, known_hosts } => check(known_hosts, host, &fp),
            Trust::Capture(sink) => {
                *sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(fp);
                Ok(())
            }
        };
        result.map(|_| ServerCertVerified::assertion()).map_err(Error::General)
    }
    // Fingerprint pinning above proves identity (the peer presented the expected cert), but
    // that alone isn't enough — an active MITM can also hold a copy of that (public) cert. These
    // two checks prove key POSSESSION: the peer signed the handshake transcript with the
    // private key matching the pinned cert, which a MITM without that key cannot forge.
    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        tokio_rustls::rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        tokio_rustls::rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Build a TLS connector that verifies the daemon under `trust`.
pub fn connector(trust: Trust) -> Arc<tokio_rustls::rustls::ClientConfig> {
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let verifier = RemoteVerifier { trust, provider: provider.clone() };
    Arc::new(
        tokio_rustls::rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions().unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tofu_records_then_verifies_then_refuses_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        // first sight: record + accept
        assert!(check(&kh, "host:7777", "aa11").is_ok());
        // same fingerprint next time: accept
        assert!(check(&kh, "host:7777", "aa11").is_ok());
        // different fingerprint for the same host: refuse
        let err = check(&kh, "host:7777", "bb22").unwrap_err();
        assert!(err.to_lowercase().contains("changed"), "loud refuse: {err}");
        // a different host records independently
        assert!(check(&kh, "other:7777", "cc33").is_ok());
    }

    /// This crate is the only place in the workspace that both exercises the real client TOFU
    /// verifier (private to this crate) *and* dev-depends on `clowder-daemon` for a real TLS
    /// accept loop, so the full-stack e2e round-trip lives here rather than in clowder-daemon
    /// (which would need a new dev-dependency on this crate purely for its private `tofu`
    /// module — see task-5 report for the cross-crate direction rationale). Guards the process-
    /// global `XDG_STATE_HOME` env var against races with any other env-mutating test that might
    /// later land in this crate's test binary.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[tokio::test]
    async fn e2e_tls_tofu_records_then_refuses_on_cert_change() {
        use clowder_daemon::{server::Daemon, FakeNotifier};
        use std::sync::Arc;
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", state.path()); // daemon creds + client known_hosts both land here

        let test_daemon = || Arc::new(Daemon::new_with(Arc::new(FakeNotifier::new()), std::path::PathBuf::from("/tmp/unused-m7d-e2e.sock")));
        // The TOFU known_hosts file keys on this host STRING, not the dial address. Both
        // connectors below use this constant label while dialing two different ephemeral
        // `127.0.0.1:0` addresses — that's what makes the rotated-cert refuse trigger reliably,
        // without fighting SO_REUSEADDR to rebind the exact same port for daemon #2.
        const TOFU_HOST_LABEL: &str = "e2e-host";

        // Daemon #1
        let creds = clowder_daemon::remote_tls::load_or_generate().unwrap();
        let token = creds.token.clone();
        let tls = clowder_daemon::remote::build_remote_tls(&creds).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(test_daemon().serve_remote(listener, Some(tls)));

        // Client connects with the REAL TOFU connector → first sight records the fingerprint + succeeds.
        let tls_connector = tokio_rustls::TlsConnector::from(connector(Trust::Tofu {
            host: TOFU_HOST_LABEL.to_string(),
            known_hosts: known_hosts_path(),
        }));
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let name = tokio_rustls::rustls::pki_types::ServerName::try_from("clowder").unwrap();
        let mut s = tls_connector.connect(name, tcp).await.expect("first connect (TOFU record) ok");
        clowder_proto::write_hello(&mut s, clowder_proto::Channel::Control, Some(&token)).await.unwrap();
        s.write_all(b"{\"type\":\"listWorktrees\"}\n").await.unwrap();

        // Rotate the daemon cert (delete + regenerate) → a new fingerprint, served on a fresh addr.
        std::fs::remove_file(clowder_config::remote_cert_path()).unwrap();
        std::fs::remove_file(clowder_config::remote_key_path()).unwrap();
        let creds2 = clowder_daemon::remote_tls::load_or_generate().unwrap();
        let tls2 = clowder_daemon::remote::build_remote_tls(&creds2).unwrap();
        let listener2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr2 = listener2.local_addr().unwrap();
        tokio::spawn(test_daemon().serve_remote(listener2, Some(tls2)));

        // Client re-connects under the SAME TOFU host label → the recorded fingerprint no longer
        // matches the (rotated) cert presented by daemon #2 → handshake refused.
        let tls_connector2 = tokio_rustls::TlsConnector::from(connector(Trust::Tofu {
            host: TOFU_HOST_LABEL.to_string(),
            known_hosts: known_hosts_path(),
        }));
        let tcp2 = tokio::net::TcpStream::connect(addr2).await.unwrap();
        let name2 = tokio_rustls::rustls::pki_types::ServerName::try_from("clowder").unwrap();
        let err = tls_connector2.connect(name2, tcp2).await.expect_err("changed cert must be refused");
        let msg = format!("{err}").to_lowercase();
        assert!(
            msg.contains("changed") || msg.contains("mismatch") || msg.contains("general"),
            "refuse surfaced: {err}"
        );

        std::env::remove_var("XDG_STATE_HOME");
    }

    use std::sync::{Arc, Mutex};

    /// Build a real self-signed cert so the verifier sees a genuine DER, not a fabricated one.
    fn a_cert() -> (Vec<u8>, String) {
        let c = rcgen::generate_simple_self_signed(vec!["clowder".to_string()]).unwrap();
        let der = c.cert.der().to_vec();
        let fp = clowder_proto::cert_fingerprint_hex(&der);
        (der, fp)
    }

    fn verify(trust: Trust, der: &[u8]) -> Result<(), String> {
        use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
        let v = RemoteVerifier {
            trust,
            provider: Arc::new(tokio_rustls::rustls::crypto::ring::default_provider()),
        };
        v.verify_server_cert(
            &CertificateDer::from(der.to_vec()),
            &[],
            &ServerName::try_from("clowder").unwrap(),
            &[],
            UnixTime::now(),
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    #[test]
    fn pinned_accepts_the_matching_fingerprint_and_refuses_any_other() {
        let (der, fp) = a_cert();
        assert!(verify(Trust::Pinned(fp.clone()), &der).is_ok());
        let err = verify(Trust::Pinned("deadbeef".into()), &der).unwrap_err();
        assert!(err.to_lowercase().contains("changed"), "loud refuse: {err}");
    }

    #[test]
    fn pinned_never_touches_known_hosts() {
        // This is the whole point of pairing: a pinned entry must not consult, and must not
        // silently record into, the TOFU file.
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        let (der, fp) = a_cert();
        assert!(verify(Trust::Pinned(fp), &der).is_ok());
        assert!(!kh.exists(), "Pinned must not write known_hosts");
    }

    #[test]
    fn capture_accepts_anything_and_publishes_the_fingerprint_without_persisting() {
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        let (der, fp) = a_cert();
        let sink: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        assert!(verify(Trust::Capture(sink.clone()), &der).is_ok());
        assert_eq!(sink.lock().unwrap().as_deref(), Some(fp.as_str()));
        assert!(!kh.exists(), "a probe must persist nothing");
    }

    #[test]
    fn tofu_arm_still_records_then_refuses_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        let (der, _) = a_cert();
        let t = || Trust::Tofu { host: "h:7777".into(), known_hosts: kh.clone() };
        assert!(verify(t(), &der).is_ok(), "first sight records");
        assert!(verify(t(), &der).is_ok(), "same cert accepts");
        let (other_der, _) = a_cert();
        assert!(verify(t(), &other_der).is_err(), "changed cert refuses");
    }
}
