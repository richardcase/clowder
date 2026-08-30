// SPDX-License-Identifier: Apache-2.0

//! Small auth primitives shared by the remote daemon (token check) and the client (cert TOFU).

use sha2::{Digest, Sha256};

/// Length-checked, data-independent byte comparison for secret material.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Lowercase-hex SHA-256 of a certificate's DER bytes — the TOFU fingerprint.
pub fn cert_fingerprint_hex(cert_der: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(cert_der);
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_and_mismatches() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab")); // length mismatch
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn fingerprint_is_lowercase_hex_sha256() {
        // SHA-256("") = e3b0c442... ; 64 hex chars.
        let fp = cert_fingerprint_hex(b"");
        assert_eq!(fp.len(), 64);
        assert_eq!(&fp[..8], "e3b0c442");
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
