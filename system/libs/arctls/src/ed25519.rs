//! Ed25519 signatures (RFC 8032) over the field of [`crate::x25519`]:
//! verifying (signed update indexes) and signing (the host tool that makes
//! them). Points are in extended coordinates; the scalar multiplication is
//! a plain double-and-add, so signing is not constant time: it is meant for
//! keys on the developer's own computer, not for secrets on a server.

use crate::sha2::Sha512;
use crate::x25519::{add, carry, invert, load, mul, sq, store, sub, Fe};

const ZERO: Fe = [0; 5];
const ONE: Fe = [1, 0, 0, 0, 0];

/// The order of the base point, little endian.
const L: [u64; 4] = [0x5812_631a_5cf5_d3ed, 0x14de_f9de_a2f7_9cd6, 0, 0x1000_0000_0000_0000];

/// The base point, encoded (y = 4/5, x even).
const BASE: [u8; 32] = [
    0x58, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
    0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
];

fn small(k: u64) -> Fe {
    [k, 0, 0, 0, 0]
}

fn neg(a: Fe) -> Fe {
    sub(ZERO, a)
}

fn plus(a: Fe, b: Fe) -> Fe {
    carry(add(a, b))
}

fn eq(a: Fe, b: Fe) -> bool {
    store(a) == store(b)
}

/// d = -121665 / 121666.
fn d() -> Fe {
    mul(neg(small(121665)), invert(small(121666)))
}

/// z^(2^252 - 3), for square roots.
fn pow2523(z: Fe) -> Fe {
    let sqn = |mut t: Fe, n: usize| {
        for _ in 0..n {
            t = sq(t);
        }
        t
    };
    let z2 = sq(z);
    let z9 = mul(sqn(z2, 2), z);
    let z11 = mul(z9, z2);
    let z5 = mul(sq(z11), z9); // 2^5 - 1
    let z10 = mul(sqn(z5, 5), z5);
    let z20 = mul(sqn(z10, 10), z10);
    let z40 = mul(sqn(z20, 20), z20);
    let z50 = mul(sqn(z40, 10), z10);
    let z100 = mul(sqn(z50, 50), z50);
    let z200 = mul(sqn(z100, 100), z100);
    let z250 = mul(sqn(z200, 50), z50);
    mul(sqn(z250, 2), z)
}

/// A point (X : Y : Z : T) with x = X/Z, y = Y/Z, x·y = T/Z.
#[derive(Clone, Copy)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

const IDENTITY: Point = Point { x: ZERO, y: ONE, z: ONE, t: ZERO };

impl Point {
    fn add(&self, q: &Point, d2: Fe) -> Point {
        let a = mul(sub(self.y, self.x), sub(q.y, q.x));
        let b = mul(plus(self.y, self.x), plus(q.y, q.x));
        let c = mul(mul(self.t, d2), q.t);
        let dd = mul(plus(self.z, self.z), q.z);
        let (e, f, g, h) = (sub(b, a), sub(dd, c), plus(dd, c), plus(b, a));
        Point { x: mul(e, f), y: mul(g, h), z: mul(f, g), t: mul(e, h) }
    }

    fn neg(&self) -> Point {
        Point { x: neg(self.x), t: neg(self.t), ..*self }
    }

    /// k·P for a little-endian scalar.
    fn times(&self, k: &[u8; 32], d2: Fe) -> Point {
        let mut r = IDENTITY;
        for i in (0..256).rev() {
            r = r.add(&r, d2);
            if (k[i / 8] >> (i % 8)) & 1 == 1 {
                r = r.add(self, d2);
            }
        }
        r
    }

    fn encode(&self) -> [u8; 32] {
        let zi = invert(self.z);
        let mut out = store(mul(self.y, zi));
        out[31] |= (store(mul(self.x, zi))[0] & 1) << 7;
        out
    }

    fn decode(b: &[u8; 32], d: Fe) -> Option<Point> {
        let sign = b[31] >> 7;
        let mut yb = *b;
        yb[31] &= 0x7f;
        let y = load(&yb);
        if store(y) != yb {
            return None; // y >= p
        }
        let yy = sq(y);
        let u = sub(yy, ONE);
        let v = plus(mul(d, yy), ONE);
        let v3 = mul(sq(v), v);
        let mut x = mul(mul(u, v3), pow2523(mul(u, mul(sq(v3), v))));
        let vxx = mul(v, sq(x));
        if !eq(vxx, u) {
            if !eq(vxx, neg(u)) {
                return None;
            }
            // sqrt(-1) = 2^((p-1)/4)
            let sqrt_m1 = mul(pow2523(small(4)), small(2));
            x = mul(x, sqrt_m1);
        }
        let xb = store(x);
        if xb == [0; 32] && sign == 1 {
            return None;
        }
        if xb[0] & 1 != sign {
            x = neg(x);
        }
        Some(Point { x, y, z: ONE, t: mul(x, y) })
    }
}

/// A little-endian number of any length, modulo L.
fn reduce(n: &[u8]) -> [u8; 32] {
    let mut r = [0u64; 4];
    for i in (0..n.len() * 8).rev() {
        let mut bit = ((n[i / 8] >> (i % 8)) & 1) as u64;
        for limb in r.iter_mut() {
            let top = *limb >> 63;
            *limb = (*limb << 1) | bit;
            bit = top;
        }
        if !less(&r, &L) {
            let mut borrow = 0u64;
            for (a, b) in r.iter_mut().zip(L) {
                let (s, o1) = a.overflowing_sub(b);
                let (s, o2) = s.overflowing_sub(borrow);
                *a = s;
                borrow = (o1 | o2) as u64;
            }
        }
    }
    let mut out = [0u8; 32];
    for (i, l) in r.iter().enumerate() {
        out[8 * i..8 * i + 8].copy_from_slice(&l.to_le_bytes());
    }
    out
}

fn less(a: &[u64; 4], b: &[u64; 4]) -> bool {
    for i in (0..4).rev() {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
    }
    false
}

fn limbs(b: &[u8; 32]) -> [u64; 4] {
    core::array::from_fn(|i| u64::from_le_bytes(b[8 * i..8 * i + 8].try_into().unwrap()))
}

/// (a·b + c) mod L.
fn mul_add(a: &[u8; 32], b: &[u8; 32], c: &[u8; 32]) -> [u8; 32] {
    let (a, b) = (limbs(a), limbs(b));
    let mut p = [0u64; 9];
    for (i, ai) in a.iter().enumerate() {
        let mut carry = 0u128;
        for (j, bj) in b.iter().enumerate() {
            let t = p[i + j] as u128 + *ai as u128 * *bj as u128 + carry;
            p[i + j] = t as u64;
            carry = t >> 64;
        }
        p[i + 4] = carry as u64;
    }
    let mut carry = 0u128;
    for (i, ci) in limbs(c).iter().enumerate().chain([(4, &0u64)]) {
        let t = p[i] as u128 + *ci as u128 + carry;
        p[i] = t as u64;
        carry = t >> 64;
    }
    for limb in p.iter_mut().skip(5) {
        let t = *limb as u128 + carry;
        *limb = t as u64;
        carry = t >> 64;
    }
    let mut bytes = [0u8; 72];
    for (i, l) in p.iter().enumerate() {
        bytes[8 * i..8 * i + 8].copy_from_slice(&l.to_le_bytes());
    }
    reduce(&bytes)
}

fn hash(parts: &[&[u8]]) -> [u8; 64] {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    h.finish().0
}

fn base(d: Fe) -> Point {
    Point::decode(&BASE, d).expect("the base point")
}

/// The secret scalar and the nonce prefix of a 32-byte private key (seed).
fn expand(seed: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let h = hash(&[seed]);
    let mut a: [u8; 32] = h[..32].try_into().unwrap();
    a[0] &= 248;
    a[31] &= 127;
    a[31] |= 64;
    (a, h[32..].try_into().unwrap())
}

/// The public key of a private key (its 32-byte seed).
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    let d = d();
    base(d).times(&expand(seed).0, plus(d, d)).encode()
}

/// Signs `msg` with the private key `seed`.
pub fn sign(seed: &[u8; 32], msg: &[u8]) -> [u8; 64] {
    let d = d();
    let d2 = plus(d, d);
    let b = base(d);
    let (a, prefix) = expand(seed);
    let pk = b.times(&a, d2).encode();
    let r = reduce(&hash(&[&prefix, msg]));
    let big_r = b.times(&r, d2).encode();
    let k = reduce(&hash(&[&big_r, &pk, msg]));
    let s = mul_add(&k, &a, &r);
    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&big_r);
    sig[32..].copy_from_slice(&s);
    sig
}

/// Whether `sig` is `public`'s signature of `msg`.
pub fn verify(public: &[u8; 32], msg: &[u8], sig: &[u8]) -> bool {
    let Ok(sig) = <&[u8; 64]>::try_from(sig) else { return false };
    let r: [u8; 32] = sig[..32].try_into().unwrap();
    let s: [u8; 32] = sig[32..].try_into().unwrap();
    if !less(&limbs(&s), &L) {
        return false;
    }
    let d = d();
    let d2 = plus(d, d);
    let Some(a) = Point::decode(public, d) else { return false };
    let k = reduce(&hash(&[&r, public, msg]));
    let check = base(d).times(&s, d2).add(&a.neg().times(&k, d2), d2);
    crate::ct_eq(&check.encode(), &r)
}
