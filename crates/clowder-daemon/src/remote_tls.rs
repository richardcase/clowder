// SPDX-License-Identifier: Apache-2.0

//! Remote TLS credential lifecycle: load-or-generate a self-signed cert + a bearer token in the
//! daemon state dir. Generation is idempotent; files are 0600.

use anyhow::{Context, Result};
use base64::Engine;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub struct RemoteCreds {
    pub cert_pem: String,
    pub key_pem: String,
    pub token: String,
    pub cert_der: Vec<u8>,
}

/// Load the three state-dir cred files if all exist; otherwise generate + persist them.
pub fn load_or_generate() -> Result<RemoteCreds> {
    let cert_p = clowder_config::remote_cert_path();
    let key_p = clowder_config::remote_key_path();
    let tok_p = clowder_config::remote_token_path();

    if cert_p.exists() && key_p.exists() && tok_p.exists() {
        let cert_pem = std::fs::read_to_string(&cert_p)?;
        let key_pem = std::fs::read_to_string(&key_p)?;
        let token = std::fs::read_to_string(&tok_p)?.trim().to_string();
        let cert_der = pem_cert_to_der(&cert_pem)?;
        return Ok(RemoteCreds { cert_pem, key_pem, token, cert_der });
    }

    // Generate a self-signed cert (SAN "clowder") + a 32-byte base64url token.
    let cert = rcgen::generate_simple_self_signed(vec!["clowder".to_string()])
        .context("generate self-signed cert")?;
    let cert_pem = cert.cert.pem();
    let key_pem = cert.key_pair.serialize_pem();
    let cert_der = cert.cert.der().to_vec();

    let mut raw = [0u8; 32];
    getrandom::getrandom(&mut raw).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);

    if let Some(dir) = cert_p.parent() {
        std::fs::create_dir_all(dir)?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    write_0600(&cert_p, cert_pem.as_bytes())?;
    write_0600(&key_p, key_pem.as_bytes())?;
    write_0600(&tok_p, token.as_bytes())?;

    Ok(RemoteCreds { cert_pem, key_pem, token, cert_der })
}

pub fn fingerprint(creds: &RemoteCreds) -> String {
    clowder_proto::cert_fingerprint_hex(&creds.cert_der)
}

fn write_0600(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true).create(true).truncate(true).mode(0o600)
        .open(path)
        .with_context(|| format!("write {}", path.display()))?;
    f.write_all(bytes)?;
    // Enforce mode even if the file pre-existed with other perms.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Extract the first certificate's DER from a PEM string (for fingerprinting on load).
fn pem_cert_to_der(pem: &str) -> Result<Vec<u8>> {
    let mut rd = std::io::BufReader::new(pem.as_bytes());
    let first = rustls_pemfile::certs(&mut rd).next()
        .ok_or_else(|| anyhow::anyhow!("no certificate in PEM"))??;
    Ok(first.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_is_idempotent_and_0600() {
        let _g = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", dir.path());  // redirect remote_state_dir()

        let a = load_or_generate().unwrap();
        assert_eq!(a.token.len() >= 32, true, "token is non-trivial");
        // second call loads the SAME creds (no regeneration)
        let b = load_or_generate().unwrap();
        assert_eq!(a.token, b.token);
        assert_eq!(a.cert_pem, b.cert_pem);
        // fingerprint is a 64-char lowercase hex string, stable across loads
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert_eq!(fingerprint(&a).len(), 64);
        // files are 0600
        for p in [clowder_config::remote_cert_path(), clowder_config::remote_key_path(), clowder_config::remote_token_path()] {
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{} perms", p.display());
        }
        std::env::remove_var("XDG_STATE_HOME");
    }
}
