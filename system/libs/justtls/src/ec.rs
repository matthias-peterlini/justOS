//! The NIST curves P-256 and P-384 (y^2 = x^3 - 3x + b): ECDSA verification
//! for certificates and handshakes, and ECDH for servers that will not use
//! X25519. Points are in Jacobian coordinates, the field in Montgomery
//! form. Signatures are public data, so verification need not be constant
//! time; the ECDH fallback is not either.

use crate::bigint::{self, Limbs, Mont};
use alloc::vec::Vec;
use core::cmp::Ordering;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Curve {
    P256,
    P384,
}

struct Params {
    p: &'static str,
    b: &'static str,
    gx: &'static str,
    gy: &'static str,
    n: &'static str,
}

const P256: Params = Params {
    p: "ffffffff00000001000000000000000000000000ffffffffffffffffffffffff",
    b: "5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b",
    gx: "6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296",
    gy: "4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5",
    n: "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551",
};

const P384: Params = Params {
    p: "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff",
    b: "b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef",
    gx: "aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab7",
    gy: "3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c00a60b1ce1d7e819d7a431d7c90ea0e5f",
    n: "ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973",
};

fn hex(s: &str) -> Limbs {
    let bytes: Vec<u8> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
    bigint::from_be(&bytes)
}

impl Curve {
    fn params(self) -> &'static Params {
        match self {
            Curve::P256 => &P256,
            Curve::P384 => &P384,
        }
    }

    /// Size of a coordinate in bytes.
    pub fn len(self) -> usize {
        match self {
            Curve::P256 => 32,
            Curve::P384 => 48,
        }
    }
}

/// A curve ready to compute on.
struct Ctx {
    f: Mont,
    b: Limbs,
    gx: Limbs,
    gy: Limbs,
    n: Limbs,
    len: usize,
}

/// A point (X, Y, Z) in Jacobian coordinates, Montgomery form; Z = 0 is
/// the point at infinity.
#[derive(Clone)]
struct Pt {
    x: Limbs,
    y: Limbs,
    z: Limbs,
}

impl Ctx {
    fn new(c: Curve) -> Ctx {
        let p = c.params();
        let f = Mont::new(&hex(p.p));
        Ctx { b: f.to(&hex(p.b)), gx: f.to(&hex(p.gx)), gy: f.to(&hex(p.gy)), n: hex(p.n), f, len: c.len() }
    }

    fn inf(&self) -> Pt {
        Pt { x: self.f.one.clone(), y: self.f.one.clone(), z: bigint::resize(&[0], self.f.m.len()) }
    }

    fn is_inf(p: &Pt) -> bool {
        bigint::is_zero(&p.z)
    }

    fn double(&self, p: &Pt) -> Pt {
        if Self::is_inf(p) {
            return p.clone();
        }
        let f = &self.f;
        let delta = f.mul(&p.z, &p.z);
        let gamma = f.mul(&p.y, &p.y);
        let beta = f.mul(&p.x, &gamma);
        let t = f.mul(&f.sub(&p.x, &delta), &f.add(&p.x, &delta));
        let alpha = f.add(&f.add(&t, &t), &t);
        let beta4 = f.add(&f.add(&beta, &beta), &f.add(&beta, &beta));
        let beta8 = f.add(&beta4, &beta4);
        let x3 = f.sub(&f.mul(&alpha, &alpha), &beta8);
        let yz = f.add(&p.y, &p.z);
        let z3 = f.sub(&f.sub(&f.mul(&yz, &yz), &gamma), &delta);
        let g2 = f.mul(&gamma, &gamma);
        let g8 = {
            let g2x2 = f.add(&g2, &g2);
            let g2x4 = f.add(&g2x2, &g2x2);
            f.add(&g2x4, &g2x4)
        };
        let y3 = f.sub(&f.mul(&alpha, &f.sub(&beta4, &x3)), &g8);
        Pt { x: x3, y: y3, z: z3 }
    }

    fn add(&self, p: &Pt, q: &Pt) -> Pt {
        if Self::is_inf(p) {
            return q.clone();
        }
        if Self::is_inf(q) {
            return p.clone();
        }
        let f = &self.f;
        let z1z1 = f.mul(&p.z, &p.z);
        let z2z2 = f.mul(&q.z, &q.z);
        let u1 = f.mul(&p.x, &z2z2);
        let u2 = f.mul(&q.x, &z1z1);
        let s1 = f.mul(&f.mul(&p.y, &q.z), &z2z2);
        let s2 = f.mul(&f.mul(&q.y, &p.z), &z1z1);
        let h = f.sub(&u2, &u1);
        let r = f.sub(&s2, &s1);
        if bigint::is_zero(&h) {
            return if bigint::is_zero(&r) { self.double(p) } else { self.inf() };
        }
        let h2 = f.add(&h, &h);
        let i = f.mul(&h2, &h2);
        let j = f.mul(&h, &i);
        let r = f.add(&r, &r);
        let v = f.mul(&u1, &i);
        let x3 = f.sub(&f.sub(&f.mul(&r, &r), &j), &f.add(&v, &v));
        let s1j = f.mul(&s1, &j);
        let y3 = f.sub(&f.mul(&r, &f.sub(&v, &x3)), &f.add(&s1j, &s1j));
        let zz = f.add(&p.z, &q.z);
        let z3 = f.mul(&f.sub(&f.sub(&f.mul(&zz, &zz), &z1z1), &z2z2), &h);
        Pt { x: x3, y: y3, z: z3 }
    }

    fn mul(&self, k: &[u64], p: &Pt) -> Pt {
        let mut r = self.inf();
        for i in (0..bigint::bits(k)).rev() {
            r = self.double(&r);
            if (k[i / 64] >> (i % 64)) & 1 == 1 {
                r = self.add(&r, p);
            }
        }
        r
    }

    /// The affine x and y, plain numbers.
    fn affine(&self, p: &Pt) -> Option<(Limbs, Limbs)> {
        if Self::is_inf(p) {
            return None;
        }
        let f = &self.f;
        let zi = f.inv(&p.z);
        let zi2 = f.mul(&zi, &zi);
        let x = f.mul(&p.x, &zi2);
        let y = f.mul(&p.y, &f.mul(&zi2, &zi));
        Some((f.from(&x), f.from(&y)))
    }

    /// An uncompressed point (04 || x || y) checked to be on the curve.
    fn decode(&self, b: &[u8]) -> Option<Pt> {
        if b.len() != 1 + 2 * self.len || b[0] != 4 {
            return None;
        }
        let x = bigint::from_be(&b[1..1 + self.len]);
        let y = bigint::from_be(&b[1 + self.len..]);
        if bigint::cmp(&x, &self.f.m) != Ordering::Less || bigint::cmp(&y, &self.f.m) != Ordering::Less {
            return None;
        }
        let f = &self.f;
        let (xm, ym) = (f.to(&x), f.to(&y));
        // y^2 = x^3 - 3x + b
        let lhs = f.mul(&ym, &ym);
        let x3 = f.mul(&f.mul(&xm, &xm), &xm);
        let three_x = f.add(&f.add(&xm, &xm), &xm);
        let rhs = f.add(&f.sub(&x3, &three_x), &self.b);
        if lhs != rhs {
            return None;
        }
        Some(Pt { x: xm, y: ym, z: f.one.clone() })
    }

    fn g(&self) -> Pt {
        Pt { x: self.gx.clone(), y: self.gy.clone(), z: self.f.one.clone() }
    }
}

/// The hash as a number below n (ECDSA: its leftmost bits).
fn hash_scalar(hash: &[u8], n: &[u64]) -> Limbs {
    let nbits = bigint::bits(n);
    let take = hash.len().min(nbits.div_ceil(8));
    let mut e = bigint::from_be(&hash[..take]);
    let extra = take * 8;
    if extra > nbits {
        // Shift right by the bits beyond n's length.
        let s = extra - nbits;
        let mut carry = 0u64;
        for limb in e.iter_mut().rev() {
            let v = *limb;
            *limb = (v >> s) | carry;
            carry = if s == 0 { 0 } else { v << (64 - s) };
        }
    }
    bigint::reduce_once(&bigint::resize(&e, n.len().max(e.len())), n)
}

/// Checks an ECDSA signature (r, s as big-endian numbers) of `hash` by the
/// public key `point` (uncompressed).
pub fn verify(curve: Curve, point: &[u8], hash: &[u8], r: &[u8], s: &[u8]) -> bool {
    let c = Ctx::new(curve);
    let Some(q) = c.decode(point) else { return false };
    let (r, s) = (bigint::from_be(r), bigint::from_be(s));
    let n = &c.n;
    for v in [&r, &s] {
        if bigint::is_zero(v) || bigint::cmp(v, n) != Ordering::Less {
            return false;
        }
    }
    let nm = Mont::new(n);
    let e = hash_scalar(hash, n);
    let w = nm.inv(&nm.to(&s));
    let u1 = nm.from(&nm.mul(&nm.to(&e), &w));
    let u2 = nm.from(&nm.mul(&nm.to(&r), &w));
    let p = c.add(&c.mul(&u1, &c.g()), &c.mul(&u2, &q));
    let Some((x, _)) = c.affine(&p) else { return false };
    let v = bigint::reduce_once(&bigint::resize(&x, n.len().max(x.len())), n);
    bigint::cmp(&v, &bigint::resize(&r, n.len())) == Ordering::Equal
}

/// A key pair for ECDH: (private scalar, public uncompressed point).
pub fn keypair(curve: Curve, random: &[u8]) -> (Limbs, Vec<u8>) {
    let c = Ctx::new(curve);
    let mut k = bigint::from_be(&random[..c.len]);
    // Into 1..n-1.
    while bigint::cmp(&k, &c.n) != Ordering::Less {
        k = bigint::sub(&k, &c.n);
    }
    if bigint::is_zero(&k) {
        k[0] = 1;
    }
    let (x, y) = c.affine(&c.mul(&k, &c.g())).unwrap();
    let mut out = alloc::vec![4u8];
    out.extend(bigint::to_be(&x, c.len));
    out.extend(bigint::to_be(&y, c.len));
    (k, out)
}

/// The shared secret (x coordinate) of `k` and the peer's point.
pub fn shared(curve: Curve, k: &[u64], peer: &[u8]) -> Option<Vec<u8>> {
    let c = Ctx::new(curve);
    let q = c.decode(peer)?;
    let (x, _) = c.affine(&c.mul(k, &q))?;
    Some(bigint::to_be(&x, c.len))
}
