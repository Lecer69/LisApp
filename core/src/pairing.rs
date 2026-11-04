//! Numeric comparison, as in Bluetooth pairing. The phone commits to its nonce before it sees the
//! PC's, so a man in the middle can't search for certificates that produce a matching code.

use sha2::{Digest, Sha256};

pub fn new_nonce() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

pub fn commitment(nonce: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"lis-pair-commit\0");
    hash.update(nonce.as_bytes());
    hex::encode(hash.finalize())
}

/// The 6 digits both devices show. Each input is bound to it, including both certificates.
pub fn code(client_fp: &str, server_fp: &str, client_nonce: &str, server_nonce: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"lis-pair-code\0");
    for part in [client_fp, server_fp, client_nonce, server_nonce] {
        hash.update(part.as_bytes());
        hash.update(b"\0");
    }
    let digest = hash.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    format!("{:06}", u64::from_be_bytes(first) % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_depends_on_every_input() {
        let base = code("a", "b", "c", "d");
        assert_eq!(base.len(), 6);
        assert_eq!(base, code("a", "b", "c", "d"));
        for other in [code("x", "b", "c", "d"), code("a", "x", "c", "d"), code("a", "b", "x", "d"), code("a", "b", "c", "x")] {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn parts_cannot_slide_between_fields() {
        assert_ne!(code("ab", "c", "d", "e"), code("a", "bc", "d", "e"));
    }

    #[test]
    fn nonces_differ_and_commitments_bind() {
        let (a, b) = (new_nonce(), new_nonce());
        assert_ne!(a, b);
        assert_eq!(commitment(&a), commitment(&a));
        assert_ne!(commitment(&a), commitment(&b));
    }
}
