//! Hashes by name, HMAC (RFC 2104) and HKDF (RFC 5869).

use crate::sha2::{Sha256, Sha512};
use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alg {
    Sha256,
    Sha384,
    Sha512,
}

impl Alg {
    pub fn len(self) -> usize {
        match self {
            Alg::Sha256 => 32,
            Alg::Sha384 => 48,
            Alg::Sha512 => 64,
        }
    }

    fn block(self) -> usize {
        match self {
            Alg::Sha256 => 64,
            _ => 128,
        }
    }
}

/// A running hash of any of the algorithms.
#[derive(Clone)]
pub enum Hasher {
    S256(Sha256),
    S512(Sha512),
}

impl Hasher {
    pub fn new(alg: Alg) -> Hasher {
        match alg {
            Alg::Sha256 => Hasher::S256(Sha256::new()),
            Alg::Sha384 => Hasher::S512(Sha512::new384()),
            Alg::Sha512 => Hasher::S512(Sha512::new()),
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::S256(h) => h.update(data),
            Hasher::S512(h) => h.update(data),
        }
    }

    pub fn finish(self) -> Vec<u8> {
        match self {
            Hasher::S256(h) => h.finish().to_vec(),
            Hasher::S512(h) => {
                let (d, n) = h.finish();
                d[..n].to_vec()
            }
        }
    }
}

pub fn digest(alg: Alg, data: &[u8]) -> Vec<u8> {
    let mut h = Hasher::new(alg);
    h.update(data);
    h.finish()
}

/// HMAC over the concatenation of `parts`.
pub fn hmac_parts(alg: Alg, key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let block = alg.block();
    let mut k = if key.len() > block { digest(alg, key) } else { key.to_vec() };
    k.resize(block, 0);
    let mut inner = Hasher::new(alg);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    inner.update(&ipad);
    for p in parts {
        inner.update(p);
    }
    let ih = inner.finish();
    let mut outer = Hasher::new(alg);
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    outer.update(&opad);
    outer.update(&ih);
    outer.finish()
}

pub fn hmac(alg: Alg, key: &[u8], data: &[u8]) -> Vec<u8> {
    hmac_parts(alg, key, &[data])
}

pub fn hkdf_extract(alg: Alg, salt: &[u8], ikm: &[u8]) -> Vec<u8> {
    let zero = vec![0u8; alg.len()];
    hmac(alg, if salt.is_empty() { &zero } else { salt }, ikm)
}

pub fn hkdf_expand(alg: Alg, prk: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut t: Vec<u8> = Vec::new();
    let mut i = 1u8;
    while out.len() < len {
        t = hmac_parts(alg, prk, &[&t, info, &[i]]);
        out.extend_from_slice(&t);
        i += 1;
    }
    out.truncate(len);
    out
}

/// HKDF-Expand-Label of TLS 1.3 (RFC 8446 7.1).
pub fn expand_label(alg: Alg, secret: &[u8], label: &str, context: &[u8], len: usize) -> Vec<u8> {
    let mut info = Vec::with_capacity(10 + label.len() + context.len());
    info.extend_from_slice(&(len as u16).to_be_bytes());
    info.push((6 + label.len()) as u8);
    info.extend_from_slice(b"tls13 ");
    info.extend_from_slice(label.as_bytes());
    info.push(context.len() as u8);
    info.extend_from_slice(context);
    hkdf_expand(alg, secret, &info, len)
}

/// The TLS 1.2 PRF (RFC 5246 5): P_hash with HMAC.
pub fn prf12(alg: Alg, secret: &[u8], label: &[u8], seed: &[u8], len: usize) -> Vec<u8> {
    let mut a = hmac_parts(alg, secret, &[label, seed]);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        out.extend_from_slice(&hmac_parts(alg, secret, &[&a, label, seed]));
        a = hmac(alg, secret, &a);
    }
    out.truncate(len);
    out
}
