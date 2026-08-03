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
    let existing = std::fs::read_to_string(path).unwrap_or_default();
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

#[derive(Debug)]
pub struct TofuVerifier {
    pub host: String,
    pub known_hosts_path: PathBuf,
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(&self, end_entity: &CertificateDer, _i: &[CertificateDer], _n: &ServerName, _o: &[u8], _t: UnixTime) -> Result<ServerCertVerified, Error> {
        let fp = clowder_proto::cert_fingerprint_hex(end_entity);
        check(&self.known_hosts_path, &self.host, &fp)
            .map(|_| ServerCertVerified::assertion())
            .map_err(|msg| Error::General(msg))
    }
    fn verify_tls12_signature(&self, _m: &[u8], _c: &CertificateDer, _d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> { Ok(HandshakeSignatureValid::assertion()) }
    fn verify_tls13_signature(&self, _m: &[u8], _c: &CertificateDer, _d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> { Ok(HandshakeSignatureValid::assertion()) }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ECDSA_NISTP256_SHA256, SignatureScheme::ED25519, SignatureScheme::RSA_PSS_SHA256, SignatureScheme::RSA_PKCS1_SHA256]
    }
}

/// Build a TLS connector that verifies `host` via TOFU.
pub fn connector(host: &str) -> Arc<tokio_rustls::rustls::ClientConfig> {
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let verifier = TofuVerifier { host: host.to_string(), known_hosts_path: known_hosts_path() };
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
}
