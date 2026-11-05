//! HTTPS for JustOS, written from scratch: SHA-2, HMAC and HKDF, AES-GCM,
//! ChaCha20-Poly1305, X25519, Ed25519, P-256 and P-384, RSA, X.509 and a
//! TLS 1.3 / 1.2 client. No unsafe code and no allocation-free promises: it runs in
//! the kernel and in programs.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod aes;
pub mod bigint;
pub mod chacha;
pub mod ec;
pub mod ed25519;
pub mod hash;
pub mod rsa;
pub mod sha2;
pub mod tls;
pub mod x25519;
pub mod x509;

#[cfg(test)]
mod tests;

/// Compares in time that does not depend on where the inputs differ.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A random generator from a 32-byte seed: ChaCha20 as a stream, re-keyed
/// from its own output after every request (forward secrecy).
pub struct Drbg {
    key: [u8; 32],
    counter: u64,
}

impl Drbg {
    pub fn new(seed: &[u8; 32]) -> Drbg {
        Drbg { key: *seed, counter: 0 }
    }

    /// Mixes more entropy in.
    pub fn reseed(&mut self, data: &[u8]) {
        let mut h = sha2::Sha256::new();
        h.update(&self.key);
        h.update(data);
        self.key = h.finish();
    }

    pub fn fill(&mut self, out: &mut [u8]) {
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&self.counter.to_le_bytes());
        self.counter += 1;
        for b in out.iter_mut() {
            *b = 0;
        }
        chacha::xor(&self.key, 1, &nonce, out);
        let next = chacha::block(&self.key, 0, &nonce);
        self.key.copy_from_slice(&next[..32]);
    }
}
