//! Tiny helper for Ed25519  ✨
//
// Uses the minimal “age”/“rage” key format (PEM‑like but *not* OpenSSH).
// If you generate with `age-keygen`, the private file starts with
//   # created: 2025-…
//   # public key: age1…
//   AGE-SECRET-KEY-1...

use base64::{
    engine::general_purpose::URL_SAFE_NO_PAD,
    Engine as _,
};
use ring::signature::{self as ring_sig, KeyPair};

/// Sign UTF‑8 `msg` with a raw 64‑byte seed+pubkey private key
pub fn sign(msg: &[u8], priv_key: &[u8]) -> String {
    // priv_key layout = 32‑byte seed || 32‑byte pub
    assert_eq!(priv_key.len(), 64, "expecting 64‑byte raw key");
    let kp = ring_sig::Ed25519KeyPair::from_seed_and_public_key(
        &priv_key[..32],
        &priv_key[32..],
    )
    .expect("bad key material");
    let sig = kp.sign(msg);
    URL_SAFE_NO_PAD.encode(sig.as_ref())
}

/// Verify UTF‑8 `msg` against base64url signature
pub fn verify(msg: &[u8], sig_b64: &str, pub_key: &[u8]) -> bool {
    let sig = match URL_SAFE_NO_PAD.decode(sig_b64) {
        Ok(s) => s,
        Err(_) => return false,
    };
    ring_sig::UnparsedPublicKey::new(&ring_sig::ED25519, pub_key)
        .verify(msg, &sig)
        .is_ok()
}
