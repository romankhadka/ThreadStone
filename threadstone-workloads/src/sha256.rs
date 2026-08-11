//! SHA-256 — cryptographic hashing throughput.
//!
//! A tight dependent chain of 32-bit rotates, shifts, xors, and additions, run
//! 64 rounds per 64-byte block. It stresses a different part of the core than
//! anything else in the suite: integer ALU throughput bounded by a long
//! dependency chain, with essentially no memory traffic and no branches.
//!
//! # Implemented rather than imported
//!
//! There are excellent SHA-256 crates. This suite implements the algorithm for
//! two reasons. First, dependency-free means the numbers cannot silently change
//! when a crate updates its assembly backend, and a benchmark whose result
//! depends on which version of a dependency resolved is not reproducible.
//! Second, the implementation is verified against the NIST test vectors, so it
//! is provably computing SHA-256 rather than something that merely takes a
//! similar amount of time.
//!
//! This is deliberately the portable software path — it does not use the ARMv8
//! or x86 SHA extensions. Hardware SHA instructions are roughly an order of
//! magnitude faster, so a machine that has them would score on a completely
//! different scale, and the workload would stop measuring general integer
//! throughput and start measuring the presence of one instruction.
//!
//! # Sizing
//!
//! The buffer is 64 KiB, comfortably inside L1 or L2 on any modern core, so the
//! measurement is compute-bound. [`crate::stream`] covers the memory system.

use threadstone_core::kernel::{
    Footprint, Kernel, KernelInfo, KernelState, Scaling, SetupCtx, Unit,
};

use crate::rng::Rng;

/// Bytes hashed per iteration.
const BUFFER_BYTES: usize = 64 << 10;

/// SHA-256 round constants: the first 32 bits of the fractional parts of the
/// cube roots of the first 64 primes (FIPS 180-4 §4.2.2).
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Initial hash value: the first 32 bits of the fractional parts of the square
/// roots of the first eight primes (FIPS 180-4 §5.3.3).
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Incremental SHA-256 state over whole blocks.
///
/// Padding and length encoding are handled by [`digest`]; the hot loop uses
/// [`Sha256::compress`] directly, since the benchmark hashes a block-aligned
/// buffer.
#[derive(Debug, Clone, Copy)]
pub struct Sha256 {
    h: [u32; 8],
}

impl Default for Sha256 {
    fn default() -> Sha256 {
        Sha256 { h: H0 }
    }
}

impl Sha256 {
    /// Absorb one 64-byte block.
    #[inline]
    pub fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, chunk) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        for (slot, value) in self.h.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    /// Current chaining value.
    pub fn state(&self) -> [u32; 8] {
        self.h
    }
}

/// Full SHA-256 of `message`, with padding and length encoding.
///
/// Present so the compression function can be checked against the published
/// test vectors; the benchmark loop does not call it.
pub fn digest(message: &[u8]) -> [u8; 32] {
    let mut state = Sha256::default();
    let mut chunks = message.chunks_exact(64);
    for chunk in chunks.by_ref() {
        let mut block = [0u8; 64];
        block.copy_from_slice(chunk);
        state.compress(&block);
    }

    // FIPS 180-4 §5.1.1: append 0x80, pad with zeros, then the 64-bit big-endian
    // bit length. If the remainder leaves under 9 bytes, the length spills into
    // one more block.
    let rest = chunks.remainder();
    let mut block = [0u8; 64];
    block[..rest.len()].copy_from_slice(rest);
    block[rest.len()] = 0x80;

    let bit_len = (message.len() as u64).wrapping_mul(8);
    if rest.len() + 1 + 8 > 64 {
        state.compress(&block);
        block = [0u8; 64];
    }
    block[56..].copy_from_slice(&bit_len.to_be_bytes());
    state.compress(&block);

    let mut out = [0u8; 32];
    for (i, word) in state.h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// One thread's hashing buffer.
struct Hasher {
    buffer: Vec<u8>,
    state: Sha256,
}

impl KernelState for Hasher {
    fn run(&mut self, iters: u64) -> u64 {
        for _ in 0..iters {
            for chunk in self.buffer.chunks_exact(64) {
                let mut block = [0u8; 64];
                block.copy_from_slice(chunk);
                self.state.compress(&block);
            }
        }
        // The chaining value depends on every byte hashed, so this is a genuine
        // data dependency on the whole computation.
        u64::from(self.state.h[0]) << 32 | u64::from(self.state.h[7])
    }
}

/// The SHA-256 throughput workload.
pub struct Sha256Kernel;

impl Kernel for Sha256Kernel {
    fn info(&self) -> KernelInfo {
        KernelInfo {
            id: "sha256",
            name: "SHA-256",
            summary: "Software SHA-256 over 64 KiB: dependent integer ALU throughput",
            unit: Unit::MibPerSec,
            footprint: Footprint::PerThread,
            scaling: Scaling::Scales,
            // Portable software SHA-256 runs at roughly 12 cycles per byte, so
            // a 3 GHz core sustains about 250 MB/s.
            reference: 250.0,
        }
    }

    fn setup(&self, ctx: &SetupCtx) -> Box<dyn KernelState> {
        let mut rng = Rng::new(0x5A45_1234 ^ ctx.thread_index as u64);
        Box::new(Hasher {
            buffer: (0..BUFFER_BYTES).map(|_| rng.next_u64() as u8).collect(),
            state: Sha256::default(),
        })
    }

    fn rate(&self, iters_per_thread: u64, threads: usize, secs: f64) -> f64 {
        let bytes = iters_per_thread as f64 * threads as f64 * BUFFER_BYTES as f64;
        bytes / secs / (1u64 << 20) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    /// The published NIST/FIPS 180-4 test vectors.
    ///
    /// These prove the kernel computes SHA-256 and not merely something of
    /// similar cost. They also cover both padding paths: a message whose
    /// remainder leaves room for the length, and one that forces an extra block.
    #[test]
    fn matches_the_nist_test_vectors() {
        assert_eq!(
            hex(&digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&digest(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex(&digest(
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmn\
                  hijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
            )),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    #[test]
    fn one_million_a_vector() {
        // The long NIST vector, which exercises many blocks and the length field.
        let message = vec![b'a'; 1_000_000];
        assert_eq!(
            hex(&digest(&message)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn padding_boundary_lengths_are_correct() {
        // 55 bytes fits the length in the same block; 56 forces another. Both
        // must agree with an independent expectation.
        assert_eq!(
            hex(&digest(&[b'a'; 55])),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            hex(&digest(&[b'a'; 56])),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            hex(&digest(&[b'a'; 64])),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn compress_is_deterministic() {
        let block = [0x42u8; 64];
        let mut a = Sha256::default();
        let mut b = Sha256::default();
        a.compress(&block);
        b.compress(&block);
        assert_eq!(a.state(), b.state());
    }

    #[test]
    fn hashing_changes_the_state() {
        let k = Sha256Kernel;
        let mut state = k.setup(&SetupCtx {
            threads: 1,
            thread_index: 0,
        });
        let first = state.run(1);
        let second = state.run(1);
        assert_ne!(first, 0);
        assert_ne!(
            first, second,
            "chaining means a second pass must differ from the first"
        );
    }

    #[test]
    fn rate_converts_to_mib_per_second() {
        let k = Sha256Kernel;
        // 16 iterations of 64 KiB is exactly 1 MiB.
        assert!((k.rate(16, 1, 1.0) - 1.0).abs() < 1e-12);
        // Independent per-thread buffers, so throughput adds.
        assert!((k.rate(16, 8, 1.0) - 8.0).abs() < 1e-12);
        assert!((k.rate(16, 1, 0.5) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn buffer_is_block_aligned() {
        assert_eq!(BUFFER_BYTES % 64, 0, "buffer must be whole SHA-256 blocks");
    }
}
