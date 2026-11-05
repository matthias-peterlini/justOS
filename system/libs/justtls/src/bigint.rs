//! Unsigned big integers for RSA and the elliptic curves: little-endian
//! 64-bit limbs, and Montgomery multiplication modulo an odd number.

use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Ordering;

/// Numbers are plain limb vectors, least significant first.
pub type Limbs = Vec<u64>;

pub fn from_be(b: &[u8]) -> Limbs {
    let b = strip(b);
    let mut out = vec![0u64; b.len().div_ceil(8).max(1)];
    for (i, &byte) in b.iter().rev().enumerate() {
        out[i / 8] |= (byte as u64) << (8 * (i % 8));
    }
    out
}

fn strip(b: &[u8]) -> &[u8] {
    let z = b.iter().take_while(|&&x| x == 0).count();
    &b[z..]
}

/// Big-endian bytes, left-padded to `len`.
pub fn to_be(a: &[u64], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    for i in 0..len {
        let limb = i / 8;
        if limb < a.len() {
            out[len - 1 - i] = (a[limb] >> (8 * (i % 8))) as u8;
        }
    }
    out
}

pub fn bits(a: &[u64]) -> usize {
    for i in (0..a.len()).rev() {
        if a[i] != 0 {
            return i * 64 + 64 - a[i].leading_zeros() as usize;
        }
    }
    0
}

pub fn is_zero(a: &[u64]) -> bool {
    a.iter().all(|&x| x == 0)
}

pub fn cmp(a: &[u64], b: &[u64]) -> Ordering {
    let n = a.len().max(b.len());
    for i in (0..n).rev() {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x.cmp(&y);
        }
    }
    Ordering::Equal
}

/// a - b (a >= b), with a's length.
pub fn sub(a: &[u64], b: &[u64]) -> Limbs {
    let mut out = a.to_vec();
    let mut borrow = 0u64;
    for i in 0..out.len() {
        let (d1, o1) = out[i].overflowing_sub(b.get(i).copied().unwrap_or(0));
        let (d2, o2) = d1.overflowing_sub(borrow);
        out[i] = d2;
        borrow = (o1 | o2) as u64;
    }
    out
}

/// a + b, one limb longer than the longer one.
pub fn add(a: &[u64], b: &[u64]) -> Limbs {
    let n = a.len().max(b.len());
    let mut out = vec![0u64; n + 1];
    let mut carry = 0u64;
    for i in 0..n {
        let (s1, o1) = a.get(i).copied().unwrap_or(0).overflowing_add(b.get(i).copied().unwrap_or(0));
        let (s2, o2) = s1.overflowing_add(carry);
        out[i] = s2;
        carry = (o1 | o2) as u64;
    }
    out[n] = carry;
    out
}

/// a with the length `n` (cut or zero-extended).
pub fn resize(a: &[u64], n: usize) -> Limbs {
    let mut v = a.to_vec();
    v.resize(n, 0);
    v
}

/// a mod m for a < 2m (one subtraction at most).
pub fn reduce_once(a: &[u64], m: &[u64]) -> Limbs {
    let r = if cmp(a, m) != Ordering::Less { sub(a, m) } else { a.to_vec() };
    resize(&r, m.len())
}

/// Arithmetic modulo an odd `m` in Montgomery form.
#[derive(Clone)]
pub struct Mont {
    pub m: Limbs,
    n: usize,
    inv: u64,
    r2: Limbs,
    pub one: Limbs,
}

impl Mont {
    pub fn new(m: &[u64]) -> Mont {
        let mut m = m.to_vec();
        while m.len() > 1 && *m.last().unwrap() == 0 {
            m.pop();
        }
        let n = m.len();
        // -m^-1 mod 2^64 by Newton's iteration.
        let mut inv = 1u64;
        for _ in 0..6 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(m[0].wrapping_mul(inv)));
        }
        let inv = inv.wrapping_neg();
        // R^2 mod m by doubling 1, 2 * 64n times.
        let mut r = vec![0u64; n];
        r[0] = 1;
        let r = {
            let mut x = reduce_once(&r, &m);
            for _ in 0..2 * 64 * n {
                let d = add(&x, &x);
                x = if cmp(&d, &m) != Ordering::Less { resize(&sub(&d, &m), n) } else { resize(&d, n) };
            }
            x
        };
        let mut mt = Mont { m, n, inv, r2: r, one: Vec::new() };
        let mut one = vec![0u64; n];
        one[0] = 1;
        mt.one = mt.to(&one);
        mt
    }

    /// a * b * R^-1 mod m (CIOS), both below m.
    pub fn mul(&self, a: &[u64], b: &[u64]) -> Limbs {
        let n = self.n;
        let mut t = vec![0u64; n + 2];
        for i in 0..n {
            let ai = a.get(i).copied().unwrap_or(0);
            let mut c: u128 = 0;
            for j in 0..n {
                let s = t[j] as u128 + ai as u128 * b.get(j).copied().unwrap_or(0) as u128 + c;
                t[j] = s as u64;
                c = s >> 64;
            }
            let s = t[n] as u128 + c;
            t[n] = s as u64;
            t[n + 1] = (s >> 64) as u64;
            let mm = t[0].wrapping_mul(self.inv);
            let s = t[0] as u128 + mm as u128 * self.m[0] as u128;
            let mut c = s >> 64;
            for j in 1..n {
                let s = t[j] as u128 + mm as u128 * self.m[j] as u128 + c;
                t[j - 1] = s as u64;
                c = s >> 64;
            }
            let s = t[n] as u128 + c;
            t[n - 1] = s as u64;
            t[n] = t[n + 1] + (s >> 64) as u64;
            t[n + 1] = 0;
        }
        let r = &t[..n + 1];
        if cmp(r, &self.m) != Ordering::Less {
            resize(&sub(r, &self.m), n)
        } else {
            resize(r, n)
        }
    }

    /// Into Montgomery form (a < m).
    pub fn to(&self, a: &[u64]) -> Limbs {
        self.mul(&resize(a, self.n), &self.r2)
    }

    /// Out of Montgomery form.
    pub fn from(&self, a: &[u64]) -> Limbs {
        let mut one = vec![0u64; self.n];
        one[0] = 1;
        self.mul(a, &one)
    }

    pub fn add(&self, a: &[u64], b: &[u64]) -> Limbs {
        let s = add(a, b);
        if cmp(&s, &self.m) != Ordering::Less {
            resize(&sub(&s, &self.m), self.n)
        } else {
            resize(&s, self.n)
        }
    }

    pub fn sub(&self, a: &[u64], b: &[u64]) -> Limbs {
        if cmp(a, b) == Ordering::Less {
            resize(&sub(&add(a, &self.m), b), self.n)
        } else {
            resize(&sub(a, b), self.n)
        }
    }

    /// base^exp mod m, plain numbers in and out (base < m).
    pub fn pow(&self, base: &[u64], exp: &[u64]) -> Limbs {
        let b = self.to(base);
        let mut x = self.one.clone();
        for i in (0..bits(exp)).rev() {
            x = self.mul(&x, &x);
            if (exp[i / 64] >> (i % 64)) & 1 == 1 {
                x = self.mul(&x, &b);
            }
        }
        self.from(&x)
    }

    /// a^-1 mod m for a prime m (Fermat), in Montgomery form in and out.
    pub fn inv(&self, a: &[u64]) -> Limbs {
        let mut two = vec![0u64; self.n];
        two[0] = 2;
        let e = sub(&self.m, &two);
        let mut x = self.one.clone();
        for i in (0..bits(&e)).rev() {
            x = self.mul(&x, &x);
            if (e[i / 64] >> (i % 64)) & 1 == 1 {
                x = self.mul(&x, a);
            }
        }
        x
    }
}
