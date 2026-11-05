//! RSA signature verification (RFC 8017): PKCS #1 v1.5 and PSS.

use crate::bigint::{self, Mont};
use crate::hash::{self, Alg};
use alloc::vec::Vec;
use core::cmp::Ordering;

/// s^e mod n as `n`-sized bytes, if s < n.
fn public_op(n: &[u8], e: &[u8], sig: &[u8]) -> Option<Vec<u8>> {
    let nl = bigint::from_be(n);
    if bigint::bits(&nl) < 1024 || bigint::bits(&nl) > 8192 || nl[0] & 1 == 0 {
        return None;
    }
    let s = bigint::from_be(sig);
    if bigint::cmp(&s, &nl) != Ordering::Less {
        return None;
    }
    let m = Mont::new(&nl);
    let k = bigint::bits(&nl).div_ceil(8);
    Some(bigint::to_be(&m.pow(&s, &bigint::from_be(e)), k))
}

fn digest_info(alg: Alg) -> &'static [u8] {
    match alg {
        Alg::Sha256 => &[0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00, 0x04, 0x20],
        Alg::Sha384 => &[0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02, 0x05, 0x00, 0x04, 0x30],
        Alg::Sha512 => &[0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03, 0x05, 0x00, 0x04, 0x40],
    }
}

/// PKCS #1 v1.5: EM = 00 01 FF..FF 00 DigestInfo(hash).
pub fn verify_pkcs1(n: &[u8], e: &[u8], alg: Alg, msg: &[u8], sig: &[u8]) -> bool {
    let Some(em) = public_op(n, e, sig) else { return false };
    let h = hash::digest(alg, msg);
    let di = digest_info(alg);
    let t_len = di.len() + h.len();
    if em.len() < t_len + 11 {
        return false;
    }
    let ps = em.len() - t_len - 3;
    em[0] == 0
        && em[1] == 1
        && em[2..2 + ps].iter().all(|&b| b == 0xff)
        && em[2 + ps] == 0
        && &em[3 + ps..3 + ps + di.len()] == di
        && em[3 + ps + di.len()..] == h[..]
}

fn mgf1(alg: Alg, seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut c = 0u32;
    while out.len() < len {
        let mut hh = hash::Hasher::new(alg);
        hh.update(seed);
        hh.update(&c.to_be_bytes());
        out.extend(hh.finish());
        c += 1;
    }
    out.truncate(len);
    out
}

/// RSASSA-PSS with MGF1 of the same hash; the salt may be of any length.
pub fn verify_pss(n: &[u8], e: &[u8], alg: Alg, msg: &[u8], sig: &[u8]) -> bool {
    let Some(em_full) = public_op(n, e, sig) else { return false };
    let mod_bits = bigint::bits(&bigint::from_be(n));
    let em_bits = mod_bits - 1;
    let em_len = em_bits.div_ceil(8);
    let em = &em_full[em_full.len() - em_len..];
    if em_full.len() > em_len && em_full[0] != 0 {
        return false;
    }
    let h_len = alg.len();
    if em_len < h_len + 2 || em[em_len - 1] != 0xbc {
        return false;
    }
    let (masked, rest) = em.split_at(em_len - h_len - 1);
    let hh = &rest[..h_len];
    let top = 8 * em_len - em_bits;
    if top > 0 && masked[0] >> (8 - top) != 0 {
        return false;
    }
    let mut db: Vec<u8> = masked.iter().zip(mgf1(alg, hh, masked.len())).map(|(a, b)| a ^ b).collect();
    if top > 0 {
        db[0] &= 0xff >> top;
    }
    let Some(one) = db.iter().position(|&b| b != 0) else { return false };
    if db[one] != 1 {
        return false;
    }
    let salt = &db[one + 1..];
    let mut m = hash::Hasher::new(alg);
    m.update(&[0u8; 8]);
    m.update(&hash::digest(alg, msg));
    m.update(salt);
    m.finish() == hh
}
