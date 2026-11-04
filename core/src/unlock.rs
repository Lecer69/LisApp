//! Unlocking the PC with the phone's fingerprint. The phone's key lives in its hardware and signs
//! only after a biometric check, so holding the pairing key isn't enough: the proof is a signature
//! over a one-time challenge from the PC.

use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1};

/// What every X.509 P-256 public key starts with, before the 65 bytes of its point.
const P256_PREFIX: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03,
    0x01, 0x07, 0x03, 0x42, 0x00,
];
const POINT_LEN: usize = 65;

fn point(public_key: &str) -> Option<Vec<u8>> {
    let der = hex::decode(public_key).ok()?;
    let point = der.strip_prefix(&P256_PREFIX)?;
    (point.len() == POINT_LEN).then(|| point.to_vec())
}

pub fn is_valid_key(public_key: &str) -> bool {
    point(public_key).is_some()
}

/// Names both devices, so a signature can't be replayed to another PC or from another phone.
pub fn message(nonce: &str, pc_fingerprint: &str, phone_fingerprint: &str) -> Vec<u8> {
    format!("lis-unlock-1\n{nonce}\n{pc_fingerprint}\n{phone_fingerprint}").into_bytes()
}

pub fn verify(public_key: &str, message: &[u8], signature: &str) -> bool {
    let (Some(point), Ok(signature)) = (point(public_key), hex::decode(signature)) else { return false };
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, point).verify(message, &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

    fn key() -> (EcdsaKeyPair, String) {
        let random = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &random).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &random).unwrap();
        let public = hex::encode([P256_PREFIX.as_slice(), pair.public_key().as_ref()].concat());
        (pair, public)
    }

    fn sign(pair: &EcdsaKeyPair, message: &[u8]) -> String {
        hex::encode(pair.sign(&SystemRandom::new(), message).unwrap().as_ref())
    }

    #[test]
    fn a_signature_from_the_key_passes() {
        let (pair, public) = key();
        let message = message("nonce", "pc", "phone");
        assert!(is_valid_key(&public));
        assert!(verify(&public, &message, &sign(&pair, &message)));
    }

    #[test]
    fn another_key_or_message_fails() {
        let (pair, public) = key();
        let (other, _) = key();
        let right = message("nonce", "pc", "phone");
        assert!(!verify(&public, &right, &sign(&other, &right)));
        for wrong in [message("other", "pc", "phone"), message("nonce", "other", "phone"), message("nonce", "pc", "other")] {
            assert!(!verify(&public, &wrong, &sign(&pair, &right)));
        }
    }

    #[test]
    fn nonsense_is_refused_without_panicking() {
        let (pair, public) = key();
        let message = message("nonce", "pc", "phone");
        let signature = sign(&pair, &message);
        for bad_key in ["", "zz", "00ff", &public[2..]] {
            assert!(!is_valid_key(bad_key), "{bad_key}");
            assert!(!verify(bad_key, &message, &signature));
        }
        for bad_signature in ["", "zz", "00ff"] {
            assert!(!verify(&public, &message, bad_signature));
        }
    }
}
