//! The `f64` functions that Rust keeps in `std` (`sqrt`, `sin`, `exp`, ...),
//! written in plain Rust for `no_std`.
//!
//! [`F64Ext`] gives them the usual method names, so `x.sqrt()` works once
//! the trait is in scope. `sqrt`, the rounding functions and exact powers
//! are correctly rounded; the rest are within a few units in the last
//! place, which is what a scripting language needs.

/// ln 2 in two parts (as in fdlibm): 32 significant bits, then the rest.
const LN2_HI: f64 = f64::from_bits(0x3FE6_2E42_FEE0_0000);
const LN2_LO: f64 = f64::from_bits(0x3DEA_39EF_3579_3C76);
const LN2: f64 = core::f64::consts::LN_2;
const PI: f64 = core::f64::consts::PI;
const FRAC_PI_2: f64 = core::f64::consts::FRAC_PI_2;
/// π/2 in three parts, for the reduction of large arguments.
const PIO2_1: f64 = f64::from_bits(0x3FF9_21FB_5440_0000);
const PIO2_2: f64 = f64::from_bits(0x3DD0_B461_1A60_0000);
const PIO2_3: f64 = f64::from_bits(0x3BA3_198A_2E00_0000);

fn bits(x: f64) -> u64 {
    x.to_bits()
}

/// Exponent without bias (normal numbers).
fn exponent(x: f64) -> i32 {
    ((bits(x) >> 52) & 0x7FF) as i32 - 1023
}

pub fn trunc(x: f64) -> f64 {
    let e = exponent(x);
    if e >= 52 {
        return x; // already whole, or inf / NaN
    }
    if e < 0 {
        return f64::from_bits(bits(x) & (1 << 63)); // ±0
    }
    f64::from_bits(bits(x) & !((1u64 << (52 - e)) - 1))
}

pub fn floor(x: f64) -> f64 {
    let t = trunc(x);
    if t > x {
        t - 1.0
    } else {
        t
    }
}

pub fn ceil(x: f64) -> f64 {
    let t = trunc(x);
    if t < x {
        t + 1.0
    } else {
        t
    }
}

/// Halfway cases away from zero.
pub fn round(x: f64) -> f64 {
    let t = trunc(x);
    let diff = x - t; // exact
    if diff >= 0.5 {
        t + 1.0
    } else if diff <= -0.5 {
        t - 1.0
    } else {
        t
    }
}

/// `x * 2^n`.
pub fn ldexp(mut x: f64, mut n: i32) -> f64 {
    // In steps that keep each factor a normal number.
    while n > 1023 {
        x *= f64::from_bits(0x7FE0_0000_0000_0000); // 2^1023
        n -= 1023;
        if x.is_infinite() {
            return x;
        }
    }
    while n < -1022 {
        x *= f64::from_bits(0x0010_0000_0000_0000) * f64::from_bits(0x4340_0000_0000_0000); // 2^-1022 * 2^53
        n += 1022 - 53;
        if x == 0.0 {
            return x;
        }
    }
    x * f64::from_bits(((n + 1023) as u64) << 52)
}

/// `(m, e)` with `x = m * 2^e`, `m` a 53-bit integer (x finite, positive).
fn decompose(x: f64) -> (u64, i32) {
    let b = bits(x);
    let raw = ((b >> 52) & 0x7FF) as i32;
    let frac = b & ((1 << 52) - 1);
    if raw == 0 {
        // Subnormal: normalise.
        let shift = frac.leading_zeros() as i32 - 11;
        (frac << shift, -1074 - shift)
    } else {
        (frac | (1 << 52), raw - 1075)
    }
}

fn isqrt(n: u128) -> u128 {
    if n < 2 {
        return n;
    }
    let mut x = 1u128 << (128 - n.leading_zeros()).div_ceil(2);
    loop {
        let y = (x + n / x) / 2;
        if y >= x {
            return x;
        }
        x = y;
    }
}

/// Correctly rounded.
pub fn sqrt(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 || x.is_infinite() {
        return x;
    }
    let (mut m, mut e) = decompose(x);
    if e & 1 != 0 {
        m <<= 1;
        e -= 1;
    }
    // m < 2^54: shifting by 2*36 leaves room in 128 bits and gives a root
    // of at least 62 bits, more than the 53 kept.
    let big = (m as u128) << 72;
    let mut r = isqrt(big);
    if r * r != big {
        r |= 1; // sticky bit: decides ties in the conversion below
    }
    // r has more than 53 bits; drop the excess keeping a sticky bit, so the
    // conversion to f64 rounds exactly once.
    let extra = 128 - r.leading_zeros() as i32 - 60;
    let mut shift = 0;
    if extra > 0 {
        let lost = r & ((1 << extra) - 1);
        r >>= extra;
        if lost != 0 {
            r |= 1;
        }
        shift = extra;
    }
    ldexp(r as u64 as f64, e / 2 - 36 + shift)
}

/// Natural logarithm.
pub fn ln(x: f64) -> f64 {
    let (hi, lo) = ln2(x);
    hi + lo
}

/// `a + b` exactly, as a sum and its rounding error.
fn two_sum(a: f64, b: f64) -> (f64, f64) {
    let s = a + b;
    let v = s - a;
    (s, (a - (s - v)) + (b - v))
}

/// `a * b` exactly, as a product and its rounding error (Dekker).
fn two_prod(a: f64, b: f64) -> (f64, f64) {
    fn split(x: f64) -> (f64, f64) {
        let c = 134_217_729.0 * x; // 2^27 + 1
        let hi = c - (c - x);
        (hi, x - hi)
    }
    let p = a * b;
    let ((ah, al), (bh, bl)) = (split(a), split(b));
    (p, ((ah * bh - p) + ah * bl + al * bh) + al * bl)
}

/// ln(x) as a sum `hi + lo` with about 70 bits, for `powf`.
fn ln2(x: f64) -> (f64, f64) {
    if x.is_nan() || x < 0.0 {
        return (f64::NAN, 0.0);
    }
    if x == 0.0 {
        return (f64::NEG_INFINITY, 0.0);
    }
    if x.is_infinite() {
        return (x, 0.0);
    }
    let (m, e) = decompose(x);
    // x = f * 2^k with f in [sqrt(1/2), sqrt(2)).
    let mut k = e + 52;
    let mut f = f64::from_bits((m & ((1 << 52) - 1)) | (1023u64 << 52));
    if f > core::f64::consts::SQRT_2 {
        f *= 0.5;
        k += 1;
    }
    // ln(f) = g - g²/2 + s (g²/2 + R) with g = f - 1 (exact), s = g / (2 + g)
    // and R = 2s²/3 + 2s⁴/5 + ... (the atanh series without its first term).
    let g = f - 1.0;
    let s = g / (2.0 + g);
    let s2 = s * s;
    let mut r = 0.0;
    for n in (1..14).rev() {
        r = s2 * (2.0 / (2 * n + 1) as f64 + r);
    }
    let hfsq = 0.5 * g * g;
    let k = k as f64;
    let tail = s * (hfsq + r) - hfsq + k * LN2_LO;
    // k * LN2_HI is exact: LN2_HI has 32 significant bits.
    let (hi, lo) = two_sum(k * LN2_HI, g);
    two_sum(hi, lo + tail)
}

pub fn exp(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x > 709.782_712_893_384 {
        return f64::INFINITY;
    }
    if x < -745.133_219_101_941_1 {
        return 0.0;
    }
    let k = round(x / LN2);
    let r = (x - k * LN2_HI) - k * LN2_LO;
    // Taylor series, |r| <= 0.35.
    let mut term = 1.0;
    let mut sum = 1.0;
    for n in 1..20 {
        term *= r / n as f64;
        sum += term;
    }
    ldexp(sum, k as i32)
}

/// Exponentiation by squaring.
pub fn powi(x: f64, n: i32) -> f64 {
    let negative = n < 0;
    let mut base = x;
    let mut n = n.unsigned_abs();
    let mut acc = 1.0;
    while n > 0 {
        if n & 1 == 1 {
            acc *= base;
        }
        base *= base;
        n >>= 1;
    }
    // One division at the end rounds less than inverting first.
    if negative {
        1.0 / acc
    } else {
        acc
    }
}

fn is_integer(y: f64) -> bool {
    y.is_finite() && trunc(y) == y
}

fn is_odd_integer(y: f64) -> bool {
    is_integer(y) && y.abs() < 9.007_199_254_740_992e15 && (y as i64) & 1 == 1
}

pub fn powf(x: f64, y: f64) -> f64 {
    if y == 0.0 || x == 1.0 {
        return 1.0;
    }
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    if x == 0.0 {
        let odd = is_odd_integer(y);
        return match (y < 0.0, odd) {
            (true, true) => f64::INFINITY.copysign(x),
            (true, false) => f64::INFINITY,
            (false, true) => x,
            (false, false) => 0.0,
        };
    }
    if y.is_infinite() {
        let ax = x.abs();
        return if ax == 1.0 {
            1.0
        } else if (ax > 1.0) == (y > 0.0) {
            f64::INFINITY
        } else {
            0.0
        };
    }
    if x.is_infinite() {
        let positive = if y > 0.0 { f64::INFINITY } else { 0.0 };
        return if x < 0.0 && is_odd_integer(y) { -positive } else { positive };
    }
    // Small whole exponents: exact where the result is representable.
    if is_integer(y) && y.abs() <= 64.0 {
        return powi(x, y as i32);
    }
    if x < 0.0 {
        if !is_integer(y) {
            return f64::NAN;
        }
        let r = exp_of_product(y, -x);
        return if is_odd_integer(y) { -r } else { r };
    }
    exp_of_product(y, x)
}

/// exp(y ln x), keeping the bits a plain product would lose.
fn exp_of_product(y: f64, x: f64) -> f64 {
    let (hi, lo) = ln2(x);
    let (p, err) = two_prod(y, hi);
    let rest = err + y * lo;
    if p.is_infinite() || p.is_nan() {
        return exp(p);
    }
    // exp(p + rest) = exp(p) (1 + rest), rest being tiny.
    let e = exp(p);
    e + e * rest
}

/// Exact for the powers of 10 a double holds exactly.
pub fn log10(x: f64) -> f64 {
    if x > 0.0 && x.is_finite() {
        let mut p = 1.0;
        for n in 0..23 {
            if p == x {
                return n as f64;
            }
            p *= 10.0;
        }
    }
    ln(x) / core::f64::consts::LN_10
}

/// Exact for powers of two.
pub fn log2(x: f64) -> f64 {
    if x > 0.0 && x.is_finite() {
        let (m, e) = decompose(x);
        if m.is_power_of_two() {
            return (e + m.trailing_zeros() as i32) as f64;
        }
    }
    ln(x) / LN2
}

/// `x - k π/2` and `k mod 4`.
fn reduce(x: f64) -> (f64, i64) {
    if x.abs() < FRAC_PI_2 / 2.0 {
        return (x, 0);
    }
    let k = round(x / FRAC_PI_2);
    let r = ((x - k * PIO2_1) - k * PIO2_2) - k * PIO2_3;
    (r, (k as i64).rem_euclid(4))
}

/// Taylor series on |x| <= π/4.
fn sin_kernel(x: f64) -> f64 {
    let x2 = x * x;
    let mut term = x;
    let mut sum = x;
    let mut n = 1.0;
    for _ in 0..11 {
        term *= -x2 / ((n + 1.0) * (n + 2.0));
        sum += term;
        n += 2.0;
    }
    sum
}

fn cos_kernel(x: f64) -> f64 {
    let x2 = x * x;
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut n = 0.0;
    for _ in 0..11 {
        term *= -x2 / ((n + 1.0) * (n + 2.0));
        sum += term;
        n += 2.0;
    }
    sum
}

pub fn sin(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let (r, k) = reduce(x);
    match k {
        0 => sin_kernel(r),
        1 => cos_kernel(r),
        2 => -sin_kernel(r),
        _ => -cos_kernel(r),
    }
}

pub fn cos(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let (r, k) = reduce(x);
    match k {
        0 => cos_kernel(r),
        1 => -sin_kernel(r),
        2 => -cos_kernel(r),
        _ => sin_kernel(r),
    }
}

pub fn tan(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let (r, k) = reduce(x);
    let (s, c) = (sin_kernel(r), cos_kernel(r));
    if k & 1 == 0 {
        s / c
    } else {
        -c / s
    }
}

pub fn atan(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x.abs() > 1.0 {
        // Also covers ±inf.
        return FRAC_PI_2.copysign(x) - atan(1.0 / x);
    }
    // atan(x) = 2 atan(x / (1 + sqrt(1 + x²))), twice: |x| <= tan(π/16).
    let mut y = x;
    for _ in 0..2 {
        y /= 1.0 + sqrt(1.0 + y * y);
    }
    let y2 = y * y;
    let mut term = y;
    let mut sum = 0.0;
    let mut n = 1.0;
    for _ in 0..16 {
        sum += term / n;
        term *= -y2;
        n += 2.0;
    }
    4.0 * sum
}

pub fn atan2(y: f64, x: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    if x == 0.0 && y == 0.0 {
        let base = if x.is_sign_negative() { PI } else { 0.0 };
        return base.copysign(y);
    }
    if x.is_infinite() && y.is_infinite() {
        let a = if x > 0.0 { PI / 4.0 } else { 3.0 * PI / 4.0 };
        return a.copysign(y);
    }
    if x == 0.0 || y.is_infinite() {
        return FRAC_PI_2.copysign(y);
    }
    if x.is_infinite() {
        return if x > 0.0 { 0.0f64.copysign(y) } else { PI.copysign(y) };
    }
    let a = atan((y / x).abs());
    match (x > 0.0, y.is_sign_negative()) {
        (true, false) => a,
        (true, true) => -a,
        (false, false) => PI - a,
        (false, true) => a - PI,
    }
}

pub fn asin(x: f64) -> f64 {
    if !(-1.0..=1.0).contains(&x) {
        return f64::NAN;
    }
    atan2(x, sqrt((1.0 - x) * (1.0 + x)))
}

pub fn acos(x: f64) -> f64 {
    if !(-1.0..=1.0).contains(&x) {
        return f64::NAN;
    }
    atan2(sqrt((1.0 - x) * (1.0 + x)), x)
}

pub fn hypot(x: f64, y: f64) -> f64 {
    if x.is_infinite() || y.is_infinite() {
        return f64::INFINITY;
    }
    let (a, b) = (x.abs(), y.abs());
    let big = a.max(b);
    if big == 0.0 || big.is_nan() {
        return big + a.min(b);
    }
    let small = a.min(b) / big;
    big * sqrt(1.0 + small * small)
}

/// The `std` method names for the functions above.
pub trait F64Ext: Sized {
    fn trunc(self) -> f64;
    fn floor(self) -> f64;
    fn ceil(self) -> f64;
    fn round(self) -> f64;
    fn fract(self) -> f64;
    fn sqrt(self) -> f64;
    fn exp(self) -> f64;
    fn ln(self) -> f64;
    fn log(self, base: f64) -> f64;
    fn log2(self) -> f64;
    fn log10(self) -> f64;
    fn powi(self, n: i32) -> f64;
    fn powf(self, y: f64) -> f64;
    fn sin(self) -> f64;
    fn cos(self) -> f64;
    fn tan(self) -> f64;
    fn asin(self) -> f64;
    fn acos(self) -> f64;
    fn atan(self) -> f64;
    fn atan2(self, x: f64) -> f64;
    fn hypot(self, y: f64) -> f64;
    fn rem_euclid(self, rhs: f64) -> f64;
    fn div_euclid(self, rhs: f64) -> f64;
}

impl F64Ext for f64 {
    fn trunc(self) -> f64 {
        trunc(self)
    }
    fn floor(self) -> f64 {
        floor(self)
    }
    fn ceil(self) -> f64 {
        ceil(self)
    }
    fn round(self) -> f64 {
        round(self)
    }
    fn fract(self) -> f64 {
        self - trunc(self)
    }
    fn sqrt(self) -> f64 {
        sqrt(self)
    }
    fn exp(self) -> f64 {
        exp(self)
    }
    fn ln(self) -> f64 {
        ln(self)
    }
    fn log(self, base: f64) -> f64 {
        ln(self) / ln(base)
    }
    fn log2(self) -> f64 {
        log2(self)
    }
    fn log10(self) -> f64 {
        log10(self)
    }
    fn powi(self, n: i32) -> f64 {
        powi(self, n)
    }
    fn powf(self, y: f64) -> f64 {
        powf(self, y)
    }
    fn sin(self) -> f64 {
        sin(self)
    }
    fn cos(self) -> f64 {
        cos(self)
    }
    fn tan(self) -> f64 {
        tan(self)
    }
    fn asin(self) -> f64 {
        asin(self)
    }
    fn acos(self) -> f64 {
        acos(self)
    }
    fn atan(self) -> f64 {
        atan(self)
    }
    fn atan2(self, x: f64) -> f64 {
        atan2(self, x)
    }
    fn hypot(self, y: f64) -> f64 {
        hypot(self, y)
    }
    fn rem_euclid(self, rhs: f64) -> f64 {
        let r = self % rhs;
        if r < 0.0 {
            r + rhs.abs()
        } else {
            r
        }
    }
    fn div_euclid(self, rhs: f64) -> f64 {
        let q = trunc(self / rhs);
        if self % rhs < 0.0 {
            return if rhs > 0.0 { q - 1.0 } else { q + 1.0 };
        }
        q
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        a == b || (a - b).abs() <= 4.0 * f64::EPSILON * b.abs().max(f64::MIN_POSITIVE)
    }

    /// Compared with the `std` functions, which these replace.
    #[test]
    fn match_std() {
        let samples = [
            0.0, -0.0, 1e-310, 1e-300, 1e-8, 0.1, 0.25, 0.5, 0.7, 1.0, 1.5, 2.0, 2.5, 3.0, 3.75, 10.0, 12.3456, 100.0, 1e5, 1e15, 4.5e15,
            1e300, -0.1, -0.5, -1.0, -2.5, -3.5, -1e15,
        ];
        for &x in &samples {
            assert_eq!(trunc(x).to_bits(), x.trunc().to_bits(), "trunc {x}");
            assert_eq!(floor(x), x.floor(), "floor {x}");
            assert_eq!(ceil(x), x.ceil(), "ceil {x}");
            assert_eq!(round(x), x.round(), "round {x}");
            assert_eq!(sqrt(x.abs()), x.abs().sqrt(), "sqrt {x}");
            if x > 0.0 {
                assert!(close(ln(x), x.ln()), "ln {x}: {} {}", ln(x), x.ln());
                assert!(close(log2(x), x.log2()), "log2 {x}");
                assert!(close(log10(x), x.log10()), "log10 {x}");
            }
            if x.abs() < 700.0 {
                assert!(close(exp(x), x.exp()), "exp {x}: {} {}", exp(x), x.exp());
            }
            if x.abs() < 1e6 {
                assert!((sin(x) - x.sin()).abs() < 1e-15, "sin {x}");
                assert!((cos(x) - x.cos()).abs() < 1e-15, "cos {x}");
                assert!(close(tan(x), x.tan()) || (tan(x) - x.tan()).abs() < 1e-14, "tan {x}");
            }
            assert!(close(atan(x), x.atan()), "atan {x}: {} {}", atan(x), x.atan());
            if x.abs() <= 1.0 {
                assert!(close(asin(x), x.asin()), "asin {x}");
                assert!(close(acos(x), x.acos()), "acos {x}");
            }
            for &y in &[1.0, -1.0, 0.5, -2.0, 0.0] {
                assert!(close(atan2(x, y), x.atan2(y)), "atan2 {x} {y}");
                assert!(close(hypot(x, y), x.hypot(y)), "hypot {x} {y}");
            }
        }
        for &(x, y) in
            &[(2.0, 10.0), (10.0, -3.0), (2.0, 0.5), (9.0, 0.5), (1.5, 2.7), (-2.0, 3.0), (0.0, -1.0), (-8.0, 1.0 / 3.0), (1e10, 1.7)]
        {
            let (a, b) = (powf(x, y), f64::powf(x, y));
            assert!(close(a, b) || (a.is_nan() && b.is_nan()), "pow {x} {y}: {a} {b}");
        }
        assert_eq!(log10(1000.0), 3.0);
        assert_eq!(powi(10.0, -2), 10f64.powi(-2));
        assert_eq!(sqrt(2.0), core::f64::consts::SQRT_2);
        assert!(sqrt(-1.0).is_nan() && ln(-1.0).is_nan());
        assert_eq!(exp(1000.0), f64::INFINITY);
        assert_eq!(ldexp(1.0, -1074), f64::from_bits(1));
    }
}
