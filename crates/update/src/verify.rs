//! Minisign signature checks for downloaded packages.

use std::io::Read;
use std::path::Path;

use minisign_verify::{PublicKey, Signature};

use crate::UpdateError;

/// The release public key compiled into the app (`just release-keygen` writes it).
const BUILT_IN: &str = include_str!("../release-key.pub");

/// The built-in release public key, or `None` if none has been generated yet (then the app
/// can't verify anything, so it doesn't update).
pub fn built_in_public_key() -> Option<&'static str> {
    has_key(BUILT_IN).then_some(BUILT_IN)
}

fn has_key(text: &str) -> bool {
    parse_public_key(text).is_ok()
}

fn parse_public_key(text: &str) -> Result<PublicKey, UpdateError> {
    let bad = |e: minisign_verify::Error| UpdateError::Verification(format!("public key: {e}"));
    if text.contains("untrusted comment:") {
        PublicKey::decode(text.trim()).map_err(bad)
    } else {
        PublicKey::from_base64(text.trim()).map_err(bad)
    }
}

/// The trusted (signed) comment a release signature must carry for `file_name`.
pub fn trusted_comment_for(file_name: &str) -> String {
    format!("file:{file_name}")
}

/// Checks that `signature` (the text of a `.minisig` file) is a valid signature of the file at
/// `path` by `public_key`, made for a file called `expected_name`.
pub fn verify_file(
    path: &Path,
    signature: &str,
    expected_name: &str,
    public_key: &str,
) -> Result<(), UpdateError> {
    let key = parse_public_key(public_key)?;
    let signature = Signature::decode(signature.trim())
        .map_err(|e| UpdateError::Verification(format!("signature file: {e}")))?;
    // Signed together with the data, so it can't be edited: stops an older (genuinely signed)
    // package from being served under a newer name.
    let expected = trusted_comment_for(expected_name);
    if signature.trusted_comment() != expected {
        return Err(UpdateError::Verification(format!(
            "signed for \"{}\", expected \"{expected}\"",
            signature.trusted_comment()
        )));
    }
    let mut verifier = key
        .verify_stream(&signature)
        .map_err(|e| UpdateError::Verification(e.to_string()))?;
    let mut file = std::fs::File::open(path).map_err(|e| UpdateError::Package(e.to_string()))?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| UpdateError::Package(e.to_string()))?;
        if n == 0 {
            break;
        }
        verifier.update(&buf[..n]);
    }
    verifier
        .finalize()
        .map_err(|e| UpdateError::Verification(e.to_string()))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Cursor;

    use minisign::KeyPair;

    use super::*;

    /// A throwaway key pair: (public key file text, secret key).
    pub(crate) fn test_keys() -> (String, minisign::SecretKey) {
        let kp = KeyPair::generate_unencrypted_keypair().unwrap();
        (kp.pk.to_box().unwrap().into_string(), kp.sk)
    }

    pub(crate) fn sign(sk: &minisign::SecretKey, data: &[u8], file_name: &str) -> String {
        minisign::sign(
            None,
            sk,
            Cursor::new(data),
            Some(&trusted_comment_for(file_name)),
            None,
        )
        .unwrap()
        .into_string()
    }

    fn file_with(data: &[u8]) -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), data).unwrap();
        f
    }

    const NAME: &str = "peeroxide-0.5.0-windows-x64.zip";

    #[test]
    fn accepts_a_genuine_signature() {
        let (pk, sk) = test_keys();
        let data = b"the new version";
        let f = file_with(data);
        verify_file(f.path(), &sign(&sk, data, NAME), NAME, &pk).unwrap();
    }

    #[test]
    fn rejects_a_tampered_file() {
        let (pk, sk) = test_keys();
        let sig = sign(&sk, b"the new version", NAME);
        let f = file_with(b"the new versioN");
        assert!(matches!(
            verify_file(f.path(), &sig, NAME, &pk),
            Err(UpdateError::Verification(_))
        ));
    }

    #[test]
    fn rejects_another_key() {
        let (_, sk) = test_keys();
        let (other_pk, _) = test_keys();
        let data = b"the new version";
        let f = file_with(data);
        assert!(verify_file(f.path(), &sign(&sk, data, NAME), NAME, &other_pk).is_err());
    }

    #[test]
    fn rejects_a_genuine_package_under_another_name() {
        let (pk, sk) = test_keys();
        let old = b"the old version";
        let sig = sign(&sk, old, "peeroxide-0.4.0-windows-x64.zip");
        let f = file_with(old);
        let err = verify_file(f.path(), &sig, NAME, &pk).unwrap_err();
        assert!(err.to_string().contains("expected"), "{err}");
    }

    #[test]
    fn rejects_an_edited_trusted_comment() {
        let (pk, sk) = test_keys();
        let data = b"the old version";
        let sig = sign(&sk, data, "peeroxide-0.4.0-windows-x64.zip").replace("0.4.0", "0.5.0");
        let f = file_with(data);
        assert!(verify_file(f.path(), &sig, NAME, &pk).is_err());
    }

    #[test]
    fn rejects_garbage_signatures_and_keys() {
        let (pk, _) = test_keys();
        let f = file_with(b"x");
        assert!(verify_file(f.path(), "not a signature", NAME, &pk).is_err());
        assert!(verify_file(f.path(), "", NAME, &pk).is_err());
        assert!(!has_key("# no key yet"));
        assert!(has_key(&pk));
        let bare = pk.lines().nth(1).unwrap();
        assert!(has_key(bare));
    }
}
