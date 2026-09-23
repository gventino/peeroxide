use std::fmt;
use std::path::Path;

use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

use crate::NetError;

const CERT_FILE: &str = "identity.cert.der";
const KEY_FILE: &str = "identity.key.der";

/// SHA-256 of a peer's certificate (DER). Doubles as the stable peer id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    pub fn of(cert_der: &[u8]) -> Self {
        Self(Sha256::digest(cert_der).into())
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 64 || !s.is_ascii() {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(Self(out))
    }

    /// Human-comparable short form, e.g. `3F9A-07C2`.
    pub fn short(&self) -> String {
        let h = self.to_hex().to_uppercase();
        format!("{}-{}", &h[0..4], &h[4..8])
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.short())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.to_hex())
    }
}

/// This peer's certificate and private key.
pub struct Identity {
    pub(crate) cert: CertificateDer<'static>,
    pub(crate) key: PrivatePkcs8KeyDer<'static>,
    fingerprint: Fingerprint,
}

impl Clone for Identity {
    fn clone(&self) -> Self {
        Self {
            cert: self.cert.clone(),
            key: self.key.clone_key(),
            fingerprint: self.fingerprint,
        }
    }
}

impl Identity {
    pub fn generate() -> Result<Self, NetError> {
        let certified = rcgen::generate_simple_self_signed(vec!["p2pss.local".to_string()])
            .map_err(|e| NetError::Config(format!("certificate generation failed: {e}")))?;
        let cert = certified.cert.der().clone();
        let key = PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
        Ok(Self::from_parts(cert, key))
    }

    /// Loads the identity stored in `dir`, creating and saving a new one on first run.
    pub fn load_or_create(dir: &Path) -> Result<Self, NetError> {
        let (cert_path, key_path) = (dir.join(CERT_FILE), dir.join(KEY_FILE));
        if cert_path.exists() && key_path.exists() {
            let cert = CertificateDer::from(std::fs::read(&cert_path)?);
            let key = PrivatePkcs8KeyDer::from(std::fs::read(&key_path)?);
            return Ok(Self::from_parts(cert, key));
        }
        let id = Self::generate()?;
        std::fs::create_dir_all(dir)?;
        std::fs::write(&key_path, id.key.secret_pkcs8_der())?;
        std::fs::write(&cert_path, id.cert.as_ref())?;
        Ok(id)
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    fn from_parts(cert: CertificateDer<'static>, key: PrivatePkcs8KeyDer<'static>) -> Self {
        let fingerprint = Fingerprint::of(&cert);
        Self {
            cert,
            key,
            fingerprint,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip_and_short_form() {
        let fp = Fingerprint::of(b"hello");
        assert_eq!(Fingerprint::from_hex(&fp.to_hex()), Some(fp));
        assert_eq!(fp.to_hex().len(), 64);
        let short = fp.short();
        assert_eq!(short.len(), 9);
        assert_eq!(&short[4..5], "-");
        assert_eq!(short, short.to_uppercase());
    }

    #[test]
    fn rejects_malformed_hex() {
        assert_eq!(Fingerprint::from_hex("abc"), None);
        assert_eq!(Fingerprint::from_hex(&"zz".repeat(32)), None);
        assert_eq!(Fingerprint::from_hex(&"é".repeat(32)), None);
    }

    #[test]
    fn identity_persists_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(dir.path()).unwrap();
        let b = Identity::load_or_create(dir.path()).unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.fingerprint(), Identity::generate().unwrap().fingerprint());
    }
}
