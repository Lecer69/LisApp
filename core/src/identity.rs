//! A device's long-lived TLS certificate. Its fingerprint is what pairing pins.

use sha2::{Digest, Sha256};
use std::{fs, io, path::Path};

pub struct Identity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

impl Identity {
    pub fn generate() -> Result<Self, rcgen::Error> {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)?;
        let params = rcgen::CertificateParams::new(vec!["lis-app".to_string()])?;
        let cert = params.self_signed(&key)?;
        Ok(Self { cert_der: cert.der().to_vec(), key_der: key.serialize_der() })
    }

    /// Reads the identity from `dir`, creating it the first time.
    pub fn load_or_create(dir: &Path) -> io::Result<Self> {
        let cert_path = dir.join("identity.cert");
        let key_path = dir.join("identity.key");
        if let (Ok(cert_der), Ok(key_der)) = (fs::read(&cert_path), fs::read(&key_path)) {
            return Ok(Self { cert_der, key_der });
        }

        fs::create_dir_all(dir)?;
        let identity = Self::generate().map_err(io::Error::other)?;
        write_private(&key_path, &identity.key_der)?;
        write_private(&cert_path, &identity.cert_der)?;
        Ok(identity)
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.cert_der)
    }
}

pub fn fingerprint(cert_der: &[u8]) -> String {
    hex::encode(Sha256::digest(cert_der))
}

/// Readable only by the owner, from the moment it exists.
pub fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let tmp = path.with_extension("tmp");
    let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    file.write_all(data)?;
    file.sync_all()?;
    fs::rename(&tmp, path)
}
