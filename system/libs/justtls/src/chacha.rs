//! ChaCha20, Poly1305 and their AEAD (RFC 8439).

use alloc::vec::Vec;

fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// One 64-byte block of key stream.
pub fn block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut s = [0u32; 16];
    s[0] = 0x61707865;
    s[1] = 0x3320646e;
    s[2] = 0x79622d32;
    s[3] = 0x6b206574;
    for i in 0..8 {
        s[4 + i] = u32::from_le_bytes(key[4 * i..4 * i + 4].try_into().unwrap());
    }
    s[12] = counter;
    for i in 0..3 {
        s[13 + i] = u32::from_le_bytes(nonce[4 * i..4 * i + 4].try_into().unwrap());
    }
    let init = s;
    for _ in 0..10 {
        quarter(&mut s, 0, 4, 8, 12);
        quarter(&mut s, 1, 5, 9, 13);
        quarter(&mut s, 2, 6, 10, 14);
        quarter(&mut s, 3, 7, 11, 15);
        quarter(&mut s, 0, 5, 10, 15);
        quarter(&mut s, 1, 6, 11, 12);
        quarter(&mut s, 2, 7, 8, 13);
        quarter(&mut s, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[4 * i..4 * i + 4].copy_from_slice(&s[i].wrapping_add(init[i]).to_le_bytes());
    }
    out
}

/// XORs the key stream from block `counter` into `data`.
pub fn xor(key: &[u8; 32], mut counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    for chunk in data.chunks_mut(64) {
        let ks = block(key, counter, nonce);
        for (d, k) in chunk.iter_mut().zip(ks.iter()) {
            *d ^= k;
        }
        counter = counter.wrapping_add(1);
    }
}

/// Poly1305 with 26-bit limbs.
pub struct Poly1305 {
    r: [u32; 5],
    h: [u32; 5],
    pad: [u32; 4],
    buf: [u8; 16],
    len: usize,
}

impl Poly1305 {
    pub fn new(key: &[u8; 32]) -> Poly1305 {
        let le = |i: usize| u32::from_le_bytes(key[i..i + 4].try_into().unwrap());
        let r = [
            le(0) & 0x3ffffff,
            (le(3) >> 2) & 0x3ffff03,
            (le(6) >> 4) & 0x3ffc0ff,
            (le(9) >> 6) & 0x3f03fff,
            (le(12) >> 8) & 0x00fffff,
        ];
        Poly1305 { r, h: [0; 5], pad: [le(16), le(20), le(24), le(28)], buf: [0; 16], len: 0 }
    }

    fn blocks(&mut self, m: &[u8], hibit: u32) {
        let [r0, r1, r2, r3, r4] = self.r.map(|x| x as u64);
        let (s1, s2, s3, s4) = (r1 * 5, r2 * 5, r3 * 5, r4 * 5);
        let mut h = self.h.map(|x| x as u64);
        for b in m.chunks(16) {
            let le = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as u64;
            h[0] += le(0) & 0x3ffffff;
            h[1] += (le(3) >> 2) & 0x3ffffff;
            h[2] += (le(6) >> 4) & 0x3ffffff;
            h[3] += (le(9) >> 6) & 0x3ffffff;
            h[4] += (le(12) >> 8) | ((hibit as u64) << 24);
            let d0 = h[0] * r0 + h[1] * s4 + h[2] * s3 + h[3] * s2 + h[4] * s1;
            let mut d1 = h[0] * r1 + h[1] * r0 + h[2] * s4 + h[3] * s3 + h[4] * s2;
            let mut d2 = h[0] * r2 + h[1] * r1 + h[2] * r0 + h[3] * s4 + h[4] * s3;
            let mut d3 = h[0] * r3 + h[1] * r2 + h[2] * r1 + h[3] * r0 + h[4] * s4;
            let mut d4 = h[0] * r4 + h[1] * r3 + h[2] * r2 + h[3] * r1 + h[4] * r0;
            let mut c = d0 >> 26;
            h[0] = d0 & 0x3ffffff;
            d1 += c;
            c = d1 >> 26;
            h[1] = d1 & 0x3ffffff;
            d2 += c;
            c = d2 >> 26;
            h[2] = d2 & 0x3ffffff;
            d3 += c;
            c = d3 >> 26;
            h[3] = d3 & 0x3ffffff;
            d4 += c;
            c = d4 >> 26;
            h[4] = d4 & 0x3ffffff;
            h[0] += c * 5;
            c = h[0] >> 26;
            h[0] &= 0x3ffffff;
            h[1] += c;
        }
        self.h = h.map(|x| x as u32);
    }

    pub fn update(&mut self, mut data: &[u8]) {
        if self.len > 0 {
            let n = (16 - self.len).min(data.len());
            self.buf[self.len..self.len + n].copy_from_slice(&data[..n]);
            self.len += n;
            data = &data[n..];
            if self.len == 16 {
                let b = self.buf;
                self.blocks(&b, 1);
                self.len = 0;
            }
        }
        let full = data.len() / 16 * 16;
        if full > 0 {
            self.blocks(&data[..full], 1);
        }
        let rest = &data[full..];
        self.buf[..rest.len()].copy_from_slice(rest);
        self.len += rest.len();
    }

    pub fn finish(mut self) -> [u8; 16] {
        if self.len > 0 {
            let mut b = [0u8; 16];
            b[..self.len].copy_from_slice(&self.buf[..self.len]);
            b[self.len] = 1;
            self.blocks(&b, 0);
        }
        let mut h = self.h;
        let mut c = h[1] >> 26;
        h[1] &= 0x3ffffff;
        h[2] += c;
        c = h[2] >> 26;
        h[2] &= 0x3ffffff;
        h[3] += c;
        c = h[3] >> 26;
        h[3] &= 0x3ffffff;
        h[4] += c;
        c = h[4] >> 26;
        h[4] &= 0x3ffffff;
        h[0] += c * 5;
        c = h[0] >> 26;
        h[0] &= 0x3ffffff;
        h[1] += c;
        // h - p, kept if it does not go negative.
        let mut g = [0u32; 5];
        g[0] = h[0].wrapping_add(5);
        c = g[0] >> 26;
        g[0] &= 0x3ffffff;
        for i in 1..5 {
            g[i] = h[i].wrapping_add(c);
            c = g[i] >> 26;
            g[i] &= 0x3ffffff;
        }
        g[4] = g[4].wrapping_sub(1 << 26);
        let mask = (g[4] >> 31).wrapping_sub(1);
        for i in 0..5 {
            h[i] = (h[i] & !mask) | (g[i] & mask);
        }
        let h0 = h[0] | (h[1] << 26);
        let h1 = (h[1] >> 6) | (h[2] << 20);
        let h2 = (h[2] >> 12) | (h[3] << 14);
        let h3 = (h[3] >> 18) | (h[4] << 8);
        let mut f: u64 = h0 as u64 + self.pad[0] as u64;
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&(f as u32).to_le_bytes());
        f = h1 as u64 + self.pad[1] as u64 + (f >> 32);
        out[4..8].copy_from_slice(&(f as u32).to_le_bytes());
        f = h2 as u64 + self.pad[2] as u64 + (f >> 32);
        out[8..12].copy_from_slice(&(f as u32).to_le_bytes());
        f = h3 as u64 + self.pad[3] as u64 + (f >> 32);
        out[12..16].copy_from_slice(&(f as u32).to_le_bytes());
        out
    }
}

fn aead_tag(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> [u8; 16] {
    let otk: [u8; 32] = block(key, 0, nonce)[..32].try_into().unwrap();
    let mut p = Poly1305::new(&otk);
    let pad = [0u8; 16];
    p.update(aad);
    p.update(&pad[..(16 - aad.len() % 16) % 16]);
    p.update(ct);
    p.update(&pad[..(16 - ct.len() % 16) % 16]);
    p.update(&(aad.len() as u64).to_le_bytes());
    p.update(&(ct.len() as u64).to_le_bytes());
    p.finish()
}

/// Encrypts `data` in place and appends the tag.
pub fn seal(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], data: &mut Vec<u8>) {
    xor(key, 1, nonce, data);
    let tag = aead_tag(key, nonce, aad, data);
    data.extend_from_slice(&tag);
}

/// Checks the tag at the end of `data` and decrypts the rest in place.
pub fn open(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], data: &mut Vec<u8>) -> bool {
    if data.len() < 16 {
        return false;
    }
    let n = data.len() - 16;
    let tag = aead_tag(key, nonce, aad, &data[..n]);
    if !crate::ct_eq(&tag, &data[n..]) {
        return false;
    }
    data.truncate(n);
    xor(key, 1, nonce, data);
    true
}
