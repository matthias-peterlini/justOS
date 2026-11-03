//! X25519 key agreement (RFC 7748): a Montgomery ladder over GF(2^255 - 19)
//! with five 51-bit limbs, in constant time.

pub(crate) type Fe = [u64; 5];
const MASK: u64 = (1 << 51) - 1;

pub(crate) fn load(b: &[u8; 32]) -> Fe {
    let le = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    [le(0) & MASK, (le(6) >> 3) & MASK, (le(12) >> 6) & MASK, (le(19) >> 1) & MASK, (le(24) >> 12) & MASK]
}

pub(crate) fn carry(mut f: Fe) -> Fe {
    for _ in 0..2 {
        for i in 0..4 {
            f[i + 1] += f[i] >> 51;
            f[i] &= MASK;
        }
        f[0] += 19 * (f[4] >> 51);
        f[4] &= MASK;
    }
    f
}

pub(crate) fn store(f: Fe) -> [u8; 32] {
    let mut f = carry(f);
    // Subtract p if f >= p.
    let mut q = (f[0] + 19) >> 51;
    for i in 1..5 {
        q = (f[i] + q) >> 51;
    }
    f[0] += 19 * q;
    for i in 0..4 {
        f[i + 1] += f[i] >> 51;
        f[i] &= MASK;
    }
    f[4] &= MASK;
    let mut out = [0u8; 32];
    let mut acc: u128 = 0;
    let mut bits = 0;
    let mut k = 0;
    for limb in f {
        acc |= (limb as u128) << bits;
        bits += 51;
        while bits >= 8 && k < 32 {
            out[k] = acc as u8;
            acc >>= 8;
            bits -= 8;
            k += 1;
        }
    }
    if k < 32 {
        out[k] = acc as u8;
    }
    out
}

pub(crate) fn add(a: Fe, b: Fe) -> Fe {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3], a[4] + b[4]]
}

pub(crate) fn sub(a: Fe, b: Fe) -> Fe {
    // Add 2p first so nothing goes negative.
    carry([
        a[0] + 0xFFFFFFFFFFFDA - b[0],
        a[1] + 0xFFFFFFFFFFFFE - b[1],
        a[2] + 0xFFFFFFFFFFFFE - b[2],
        a[3] + 0xFFFFFFFFFFFFE - b[3],
        a[4] + 0xFFFFFFFFFFFFE - b[4],
    ])
}

pub(crate) fn mul(a: Fe, b: Fe) -> Fe {
    let m = |x: u64, y: u64| x as u128 * y as u128;
    let b19 = [b[0], b[1] * 19, b[2] * 19, b[3] * 19, b[4] * 19];
    let r0 = m(a[0], b[0]) + m(a[1], b19[4]) + m(a[2], b19[3]) + m(a[3], b19[2]) + m(a[4], b19[1]);
    let r1 = m(a[0], b[1]) + m(a[1], b[0]) + m(a[2], b19[4]) + m(a[3], b19[3]) + m(a[4], b19[2]);
    let r2 = m(a[0], b[2]) + m(a[1], b[1]) + m(a[2], b[0]) + m(a[3], b19[4]) + m(a[4], b19[3]);
    let r3 = m(a[0], b[3]) + m(a[1], b[2]) + m(a[2], b[1]) + m(a[3], b[0]) + m(a[4], b19[4]);
    let r4 = m(a[0], b[4]) + m(a[1], b[3]) + m(a[2], b[2]) + m(a[3], b[1]) + m(a[4], b[0]);
    let mut r = [r0, r1, r2, r3, r4];
    let mut out = [0u64; 5];
    for i in 0..4 {
        r[i + 1] += r[i] >> 51;
        out[i] = (r[i] as u64) & MASK;
    }
    out[4] = (r[4] as u64) & MASK;
    let t = out[0] as u128 + (r[4] >> 51) * 19;
    out[0] = (t as u64) & MASK;
    out[1] += (t >> 51) as u64;
    carry(out)
}

pub(crate) fn sq(a: Fe) -> Fe {
    mul(a, a)
}

fn mul_small(a: Fe, k: u64) -> Fe {
    let mut r = [0u128; 5];
    for i in 0..5 {
        r[i] = a[i] as u128 * k as u128;
    }
    let mut out = [0u64; 5];
    for i in 0..4 {
        r[i + 1] += r[i] >> 51;
        out[i] = (r[i] as u64) & MASK;
    }
    out[4] = (r[4] as u64) & MASK;
    let t = out[0] as u128 + (r[4] >> 51) * 19;
    out[0] = (t as u64) & MASK;
    out[1] += (t >> 51) as u64;
    carry(out)
}

pub(crate) fn invert(z: Fe) -> Fe {
    // z^(p-2) with the usual addition chain.
    let z2 = sq(z);
    let z9 = mul(sq(sq(z2)), z);
    let z11 = mul(z9, z2);
    let z2_5_0 = mul(sq(z11), z9);
    let mut t = z2_5_0;
    for _ in 0..5 {
        t = sq(t);
    }
    let z2_10_0 = mul(t, z2_5_0);
    t = z2_10_0;
    for _ in 0..10 {
        t = sq(t);
    }
    let z2_20_0 = mul(t, z2_10_0);
    t = z2_20_0;
    for _ in 0..20 {
        t = sq(t);
    }
    t = mul(t, z2_20_0);
    for _ in 0..10 {
        t = sq(t);
    }
    let z2_50_0 = mul(t, z2_10_0);
    t = z2_50_0;
    for _ in 0..50 {
        t = sq(t);
    }
    let z2_100_0 = mul(t, z2_50_0);
    t = z2_100_0;
    for _ in 0..100 {
        t = sq(t);
    }
    t = mul(t, z2_100_0);
    for _ in 0..50 {
        t = sq(t);
    }
    t = mul(t, z2_50_0);
    for _ in 0..5 {
        t = sq(t);
    }
    mul(t, z11)
}

fn cswap(a: &mut Fe, b: &mut Fe, swap: u64) {
    let mask = 0u64.wrapping_sub(swap);
    for i in 0..5 {
        let t = mask & (a[i] ^ b[i]);
        a[i] ^= t;
        b[i] ^= t;
    }
}

/// The scalar multiplication k * u.
pub fn x25519(k: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    let mut k = *k;
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;
    let mut ub = *u;
    ub[31] &= 127;
    let x1 = load(&ub);
    let (mut x2, mut z2, mut x3, mut z3) = ([1, 0, 0, 0, 0], [0; 5], x1, [1, 0, 0, 0, 0]);
    let mut swap = 0u64;
    for t in (0..255).rev() {
        let bit = ((k[t / 8] >> (t % 8)) & 1) as u64;
        swap ^= bit;
        cswap(&mut x2, &mut x3, swap);
        cswap(&mut z2, &mut z3, swap);
        swap = bit;
        let a = add(x2, z2);
        let aa = sq(a);
        let b = sub(x2, z2);
        let bb = sq(b);
        let e = sub(aa, bb);
        let c = add(x3, z3);
        let d = sub(x3, z3);
        let da = mul(d, a);
        let cb = mul(c, b);
        x3 = sq(add(da, cb));
        z3 = mul(x1, sq(sub(da, cb)));
        x2 = mul(aa, bb);
        z2 = mul(e, add(aa, mul_small(e, 121665)));
    }
    cswap(&mut x2, &mut x3, swap);
    cswap(&mut z2, &mut z3, swap);
    store(mul(x2, invert(z2)))
}

/// The public key of a private one.
pub fn public(k: &[u8; 32]) -> [u8; 32] {
    let mut nine = [0u8; 32];
    nine[0] = 9;
    x25519(k, &nine)
}
