// cli/src/bin/gen_key.rs
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::fs;

fn main() {
    // 1. system RNG that implements `SecureRandom`
    let rng = SystemRandom::new();

    // 2. generate a PKCS#8‑encoded private key
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("key generation");

    // 3. load it back into a key pair so we can grab the public key
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("decode key");

    // 4. make sure the directory exists
    fs::create_dir_all("cli/keys").unwrap();

    // 5. write keys (private key should **never** be committed!)
    fs::write("cli/keys/threadstone.key", pkcs8.as_ref()).unwrap();
    fs::write("cli/keys/threadstone.pub", pair.public_key().as_ref()).unwrap();

    println!("✔  keys written to cli/keys/");
}
