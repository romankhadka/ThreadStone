//! Ed25519 signing over canonical JSON.
//!
//! # What the previous implementation did
//!
//! ```text
//! pub fn sign(_data: &[u8], _private_key: &[u8]) -> String {
//!     base64::encode(&[0, 1, 2, 3, 4])   // Dummy signature
//! }
//!
//! pub fn verify(_data: &[u8], signature: &str, _public_key: &[u8]) -> bool {
//!     !signature.is_empty()              // Always returns true
//! }
//! ```
//!
//! Every signed result carried the same five bytes, `"AAECAwQ="`, and
//! verification passed for any non-empty string. A file could be edited
//! arbitrarily and still verify. The `ring` dependency was present but unused.
//!
//! # What it does now
//!
//! Real Ed25519 over the report's canonical JSON — sorted keys, no whitespace,
//! `signature` field removed before hashing. Canonicalisation is what makes the
//! signature portable: signing pretty-printed output would tie validity to
//! indentation and key ordering, so a verifier that re-serialised the same
//! document differently would reject it.
//!
//! # What a signature proves
//!
//! Integrity, not authority. It shows a result has not been modified since it
//! was signed by the holder of a particular key. It does not show the number is
//! honest, that the machine is what the file claims, or that the key belongs to
//! anyone in particular — a signer can always run the benchmark under favourable
//! conditions and sign the result truthfully. The public key travels inside the
//! file so verification needs nothing else; deciding whether to trust that key
//! is left to whoever is reading.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};

use threadstone_core::report::Signature;

/// Algorithm identifier written into every signature.
pub const ALGORITHM: &str = "ed25519";

/// Why a signing or verification step failed.
#[derive(Debug)]
pub enum SigningError {
    /// The private key file was not a valid PKCS#8 Ed25519 key.
    BadPrivateKey,
    /// The public key in the file was not valid base64, or was the wrong size.
    BadPublicKey,
    /// The signature field was not valid base64.
    BadSignatureEncoding,
    /// The signature did not match the document under the given public key.
    ///
    /// Either the file was modified after signing, or it was signed by a
    /// different key.
    Mismatch,
    /// The signature named an algorithm this build does not implement.
    UnsupportedAlgorithm(String),
    /// The system random number generator was unavailable.
    RandomUnavailable,
}

impl std::fmt::Display for SigningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SigningError::BadPrivateKey => {
                write!(f, "not a valid PKCS#8 Ed25519 private key")
            }
            SigningError::BadPublicKey => write!(f, "public key is malformed"),
            SigningError::BadSignatureEncoding => {
                write!(f, "signature is not valid base64")
            }
            SigningError::Mismatch => write!(
                f,
                "signature does not match: the file was modified after signing, \
                 or was signed with a different key"
            ),
            SigningError::UnsupportedAlgorithm(a) => {
                write!(
                    f,
                    "unsupported signature algorithm '{a}' (expected ed25519)"
                )
            }
            SigningError::RandomUnavailable => {
                write!(f, "system random number generator unavailable")
            }
        }
    }
}

impl std::error::Error for SigningError {}

/// A freshly generated Ed25519 key pair.
pub struct GeneratedKey {
    /// PKCS#8 v2 encoding of the private key. Never commit this.
    pub pkcs8: Vec<u8>,
    /// Raw 32-byte public key.
    pub public: Vec<u8>,
}

/// Generate a new Ed25519 key pair from the system CSPRNG.
pub fn generate() -> Result<GeneratedKey, SigningError> {
    let rng = SystemRandom::new();
    let pkcs8 =
        Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| SigningError::RandomUnavailable)?;
    let pair =
        Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| SigningError::BadPrivateKey)?;
    Ok(GeneratedKey {
        pkcs8: pkcs8.as_ref().to_vec(),
        public: pair.public_key().as_ref().to_vec(),
    })
}

/// Sign `message` with a PKCS#8-encoded Ed25519 private key.
///
/// The returned [`Signature`] embeds the matching public key, so the signed
/// document is self-contained.
pub fn sign(message: &[u8], pkcs8: &[u8]) -> Result<Signature, SigningError> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| SigningError::BadPrivateKey)?;
    let signature = pair.sign(message);
    Ok(Signature {
        algorithm: ALGORITHM.to_string(),
        public_key: BASE64.encode(pair.public_key().as_ref()),
        value: BASE64.encode(signature.as_ref()),
    })
}

/// Verify `message` against a [`Signature`].
///
/// Returns `Ok(())` only when the algorithm is recognised, both fields decode,
/// and the signature validates. Every other outcome is an error naming the
/// specific failure — this function has no path that returns success without
/// having performed the check.
pub fn verify(message: &[u8], signature: &Signature) -> Result<(), SigningError> {
    if signature.algorithm != ALGORITHM {
        return Err(SigningError::UnsupportedAlgorithm(
            signature.algorithm.clone(),
        ));
    }
    let public_key = BASE64
        .decode(&signature.public_key)
        .map_err(|_| SigningError::BadPublicKey)?;
    let value = BASE64
        .decode(&signature.value)
        .map_err(|_| SigningError::BadSignatureEncoding)?;

    UnparsedPublicKey::new(&ED25519, &public_key)
        .verify(message, &value)
        .map_err(|_| SigningError::Mismatch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_verifies_against_its_own_message() {
        let key = generate().unwrap();
        let message = b"threadstone canonical bytes";
        let sig = sign(message, &key.pkcs8).unwrap();
        assert_eq!(sig.algorithm, "ed25519");
        assert!(verify(message, &sig).is_ok());
    }

    #[test]
    fn the_embedded_public_key_matches_the_signing_key() {
        let key = generate().unwrap();
        let sig = sign(b"x", &key.pkcs8).unwrap();
        assert_eq!(BASE64.decode(&sig.public_key).unwrap(), key.public);
    }

    #[test]
    fn a_modified_message_fails() {
        // The defect the old stub had: any edit must now be detected.
        let key = generate().unwrap();
        let sig = sign(b"original message", &key.pkcs8).unwrap();
        let err = verify(b"modified message", &sig).unwrap_err();
        assert!(matches!(err, SigningError::Mismatch));
    }

    #[test]
    fn a_single_flipped_bit_fails() {
        let key = generate().unwrap();
        let mut message = b"threadstone".to_vec();
        let sig = sign(&message, &key.pkcs8).unwrap();
        message[0] ^= 0x01;
        assert!(matches!(
            verify(&message, &sig).unwrap_err(),
            SigningError::Mismatch
        ));
    }

    #[test]
    fn a_signature_from_another_key_fails() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        let message = b"same message";
        let mut sig = sign(message, &a.pkcs8).unwrap();
        // Claim someone else's public key while keeping A's signature.
        sig.public_key = BASE64.encode(&b.public);
        assert!(matches!(
            verify(message, &sig).unwrap_err(),
            SigningError::Mismatch
        ));
    }

    #[test]
    fn a_tampered_signature_value_fails() {
        let key = generate().unwrap();
        let message = b"payload";
        let mut sig = sign(message, &key.pkcs8).unwrap();
        let mut raw = BASE64.decode(&sig.value).unwrap();
        raw[10] ^= 0xFF;
        sig.value = BASE64.encode(&raw);
        assert!(matches!(
            verify(message, &sig).unwrap_err(),
            SigningError::Mismatch
        ));
    }

    #[test]
    fn the_old_dummy_signature_is_rejected() {
        // "AAECAwQ=" is base64 of [0,1,2,3,4] — exactly what the previous
        // implementation emitted for every result, and what shipped in
        // examples/stream.json. It must not verify against anything.
        let key = generate().unwrap();
        let sig = Signature {
            algorithm: "ed25519".to_string(),
            public_key: BASE64.encode(&key.public),
            value: "AAECAwQ=".to_string(),
        };
        assert!(matches!(
            verify(b"anything", &sig).unwrap_err(),
            SigningError::Mismatch
        ));
    }

    #[test]
    fn an_empty_signature_is_rejected() {
        // The old `verify` returned true for any non-empty string and false
        // only for an empty one; nothing about emptiness should be special.
        let key = generate().unwrap();
        let sig = Signature {
            algorithm: "ed25519".to_string(),
            public_key: BASE64.encode(&key.public),
            value: String::new(),
        };
        assert!(matches!(
            verify(b"m", &sig).unwrap_err(),
            SigningError::Mismatch
        ));
    }

    #[test]
    fn malformed_encodings_are_reported_precisely() {
        let key = generate().unwrap();
        let good = sign(b"m", &key.pkcs8).unwrap();

        let bad_sig = Signature {
            value: "not base64!!!".to_string(),
            ..good.clone()
        };
        assert!(matches!(
            verify(b"m", &bad_sig).unwrap_err(),
            SigningError::BadSignatureEncoding
        ));

        let bad_key = Signature {
            public_key: "not base64!!!".to_string(),
            ..good.clone()
        };
        assert!(matches!(
            verify(b"m", &bad_key).unwrap_err(),
            SigningError::BadPublicKey
        ));

        let short_key = Signature {
            public_key: BASE64.encode([1u8, 2, 3]),
            ..good.clone()
        };
        // A wrong-length key cannot validate anything.
        assert!(verify(b"m", &short_key).is_err());
    }

    #[test]
    fn an_unknown_algorithm_is_refused_rather_than_ignored() {
        let key = generate().unwrap();
        let sig = Signature {
            algorithm: "rot13".to_string(),
            ..sign(b"m", &key.pkcs8).unwrap()
        };
        match verify(b"m", &sig).unwrap_err() {
            SigningError::UnsupportedAlgorithm(a) => assert_eq!(a, "rot13"),
            other => panic!("expected UnsupportedAlgorithm, got {other:?}"),
        }
    }

    #[test]
    fn a_bad_private_key_is_rejected() {
        assert!(matches!(
            sign(b"m", b"definitely not pkcs8").unwrap_err(),
            SigningError::BadPrivateKey
        ));
    }

    #[test]
    fn generated_keys_are_distinct() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert_ne!(a.public, b.public);
        assert_eq!(a.public.len(), 32, "Ed25519 public keys are 32 bytes");
    }
}
