//! DEFLATE (RFC 1951), raw streams without a zlib or gzip header: the
//! compression of `.jpk` sections.
//!
//! [`inflate`] reads every kind of block (stored, fixed and dynamic
//! Huffman codes) and rejects streams with anything after the end. [`deflate`] finds repeats with a hash chain and writes
//! them with the fixed codes: a little larger than the best encoders, much
//! simpler.

use alloc::vec;
use alloc::vec::Vec;

/// Base length and extra bits of length codes 257..=285.
const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
/// Base distance and extra bits of distance codes 0..=29.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385,
    24577,
];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
/// Order in which a dynamic block sends the code lengths of its code-length code.
const CLEN_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// The stream is damaged, or decompresses to more than the limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InflateError;

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
}

impl Bits<'_> {
    fn get(&mut self, n: u32) -> Result<u32, InflateError> {
        let mut v = 0;
        for i in 0..n {
            let byte = *self.data.get(self.pos).ok_or(InflateError)?;
            v |= u32::from((byte >> self.bit) & 1) << i;
            self.bit += 1;
            if self.bit == 8 {
                self.bit = 0;
                self.pos += 1;
            }
        }
        Ok(v)
    }

    fn align(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.pos += 1;
        }
    }
}

/// A canonical Huffman code: how many codes of each length, and the
/// symbols in code order.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Huffman, InflateError> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        // Over-subscribed codes are invalid; incomplete ones are allowed.
        let mut left = 1i32;
        for &c in &counts[1..] {
            left = (left << 1) - i32::from(c);
            if left < 0 {
                return Err(InflateError);
            }
        }
        let mut offs = [0u16; 16];
        for i in 1..15 {
            offs[i + 1] = offs[i] + counts[i];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huffman { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Result<u16, InflateError> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= bits.get(1)? as i32;
            let count = i32::from(self.counts[len]);
            if code - count < first {
                return self.symbols.get((index + code - first) as usize).copied().ok_or(InflateError);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(InflateError)
    }
}

fn fixed_lengths() -> ([u8; 288], [u8; 30]) {
    let mut lit = [8u8; 288];
    lit[144..256].fill(9);
    lit[256..280].fill(7);
    (lit, [5u8; 30])
}

/// Decompresses a raw DEFLATE stream of at most `limit` bytes.
pub fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>, InflateError> {
    let mut bits = Bits { data, pos: 0, bit: 0 };
    let mut out = Vec::new();
    loop {
        let last = bits.get(1)? == 1;
        match bits.get(2)? {
            0 => {
                bits.align();
                let at = bits.pos;
                let header = data.get(at..at + 4).ok_or(InflateError)?;
                let len = u16::from_le_bytes([header[0], header[1]]);
                if len != !u16::from_le_bytes([header[2], header[3]]) {
                    return Err(InflateError);
                }
                let block = data.get(at + 4..at + 4 + len as usize).ok_or(InflateError)?;
                if out.len() + block.len() > limit {
                    return Err(InflateError);
                }
                out.extend_from_slice(block);
                bits.pos = at + 4 + len as usize;
            }
            1 => {
                let (lit, dist) = fixed_lengths();
                codes(&mut bits, &mut out, &Huffman::new(&lit)?, &Huffman::new(&dist)?, limit)?;
            }
            2 => {
                let (lit, dist) = dynamic_codes(&mut bits)?;
                codes(&mut bits, &mut out, &lit, &dist, limit)?;
            }
            _ => return Err(InflateError),
        }
        if last {
            // Standard encoders pad the last byte with zeros and stop there:
            // anything else is damage.
            let pad = if bits.bit == 0 { 0 } else { 8 - bits.bit };
            if bits.get(pad)? != 0 || bits.pos != data.len() {
                return Err(InflateError);
            }
            return Ok(out);
        }
    }
}

fn dynamic_codes(bits: &mut Bits<'_>) -> Result<(Huffman, Huffman), InflateError> {
    let nlen = bits.get(5)? as usize + 257;
    let ndist = bits.get(5)? as usize + 1;
    let ncode = bits.get(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err(InflateError);
    }
    let mut clens = [0u8; 19];
    for &i in &CLEN_ORDER[..ncode] {
        clens[i] = bits.get(3)? as u8;
    }
    let clen = Huffman::new(&clens)?;
    let mut lengths = vec![0u8; nlen + ndist];
    let mut i = 0;
    while i < lengths.len() {
        let sym = clen.decode(bits)?;
        let (value, repeat) = match sym {
            0..=15 => (sym as u8, 1),
            16 => (*lengths.get(i.wrapping_sub(1)).ok_or(InflateError)?, 3 + bits.get(2)? as usize),
            17 => (0, 3 + bits.get(3)? as usize),
            _ => (0, 11 + bits.get(7)? as usize),
        };
        if i + repeat > lengths.len() {
            return Err(InflateError);
        }
        lengths[i..i + repeat].fill(value);
        i += repeat;
    }
    if lengths[256] == 0 {
        return Err(InflateError); // no end-of-block code
    }
    Ok((Huffman::new(&lengths[..nlen])?, Huffman::new(&lengths[nlen..])?))
}

fn codes(bits: &mut Bits<'_>, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman, limit: usize) -> Result<(), InflateError> {
    loop {
        let sym = lit.decode(bits)? as usize;
        if sym < 256 {
            if out.len() >= limit {
                return Err(InflateError);
            }
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let i = sym - 257;
            if i >= 29 {
                return Err(InflateError);
            }
            let len = LEN_BASE[i] as usize + bits.get(u32::from(LEN_EXTRA[i]))? as usize;
            let d = dist.decode(bits)? as usize;
            if d >= 30 {
                return Err(InflateError);
            }
            let back = DIST_BASE[d] as usize + bits.get(u32::from(DIST_EXTRA[d]))? as usize;
            if back > out.len() || out.len() + len > limit {
                return Err(InflateError);
            }
            let start = out.len() - back;
            for k in 0..len {
                let b = out[start + k];
                out.push(b);
            }
        }
    }
}

struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, bits: u32) {
        self.acc |= value << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Huffman codes go out most significant bit first.
    fn put_code(&mut self, code: u32, len: u32) {
        let mut rev = 0;
        for i in 0..len {
            rev |= ((code >> i) & 1) << (len - 1 - i);
        }
        self.put(rev, len);
    }

    fn literal(&mut self, sym: u32) {
        match sym {
            0..=143 => self.put_code(0x30 + sym, 8),
            144..=255 => self.put_code(0x190 + sym - 144, 9),
            256..=279 => self.put_code(sym - 256, 7),
            _ => self.put_code(0xC0 + sym - 280, 8),
        }
    }

    fn repeat(&mut self, len: usize, back: usize) {
        let i = LEN_BASE.iter().rposition(|&b| b as usize <= len).unwrap_or(0);
        self.literal(257 + i as u32);
        self.put((len - LEN_BASE[i] as usize) as u32, u32::from(LEN_EXTRA[i]));
        let d = DIST_BASE.iter().rposition(|&b| b as usize <= back).unwrap_or(0);
        self.put_code(d as u32, 5);
        self.put((back - DIST_BASE[d] as usize) as u32, u32::from(DIST_EXTRA[d]));
    }
}

const WINDOW: usize = 32 * 1024;
const HASH_BITS: u32 = 15;
const MAX_CHAIN: usize = 128;

fn hash(data: &[u8], i: usize) -> usize {
    let v = u32::from(data[i]) << 16 | u32::from(data[i + 1]) << 8 | u32::from(data[i + 2]);
    (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

/// Compresses `data` into one raw DEFLATE block with the fixed codes.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter { out: Vec::with_capacity(data.len() / 2 + 16), acc: 0, n: 0 };
    w.put(1, 1); // last block
    w.put(1, 2); // fixed codes
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut prev = vec![usize::MAX; data.len()];
    let mut i = 0;
    let insert = |head: &mut Vec<usize>, prev: &mut Vec<usize>, at: usize| {
        if at + 3 <= data.len() {
            let h = hash(data, at);
            prev[at] = head[h];
            head[h] = at;
        }
    };
    while i < data.len() {
        let (mut best_len, mut best_back) = (0, 0);
        if i + 3 <= data.len() {
            let mut cand = head[hash(data, i)];
            let max = (data.len() - i).min(258);
            let mut chain = 0;
            while cand != usize::MAX && i - cand <= WINDOW && chain < MAX_CHAIN {
                let len = data[cand..].iter().zip(&data[i..i + max]).take_while(|(a, b)| a == b).count();
                if len > best_len {
                    best_len = len;
                    best_back = i - cand;
                    if len == max {
                        break;
                    }
                }
                cand = prev[cand];
                chain += 1;
            }
        }
        if best_len >= 3 {
            w.repeat(best_len, best_back);
            for k in i..i + best_len {
                insert(&mut head, &mut prev, k);
            }
            i += best_len;
        } else {
            w.literal(u32::from(data[i]));
            insert(&mut head, &mut prev, i);
            i += 1;
        }
    }
    w.literal(256);
    if w.n > 0 {
        w.out.push(w.acc as u8);
    }
    w.out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..5000u32 {
            v.extend_from_slice(alloc::format!("line {} of the sample, value {}\n", i, i * 7 % 13).as_bytes());
        }
        v.extend((0..3000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8));
        v
    }

    #[test]
    fn round_trip() {
        for data in [Vec::new(), b"a".to_vec(), b"abcabcabcabcabcabc".to_vec(), vec![0u8; 100_000], sample()] {
            let packed = deflate(&data);
            assert_eq!(inflate(&packed, data.len()).unwrap(), data);
            if data.len() > 1000 {
                assert!(packed.len() < data.len() / 2, "{} -> {}", data.len(), packed.len());
            }
        }
    }

    #[test]
    fn limit_and_damage() {
        let data = sample();
        let packed = deflate(&data);
        assert_eq!(inflate(&packed, data.len() - 1), Err(InflateError));
        assert_eq!(inflate(&packed[..packed.len() / 2], data.len()), Err(InflateError));
        assert_eq!(inflate(&[0xFF; 16], 1000), Err(InflateError));
    }

    #[test]
    fn stored_and_dynamic_blocks() {
        // A stored block.
        assert_eq!(inflate(&[1, 5, 0, 0xFA, 0xFF, b'h', b'e', b'l', b'l', b'o'], 10).unwrap(), b"hello");
        // A block with dynamic codes, made by zlib (level 9, raw) from the
        // letters below.
        let mut text = Vec::new();
        let mut x: u32 = 1;
        for _ in 0..400 {
            x = (x.wrapping_mul(1_103_515_245).wrapping_add(12345)) & 0x7FFF_FFFF;
            text.push(b"eeeeettaaonisrhd"[((x >> 16) % 16) as usize]);
        }
        let zlib = [
            0x1d, 0x90, 0x41, 0x0a, 0xc0, 0x40, 0x08, 0x03, 0xdf, 0x2a, 0x18, 0xd0, 0x8b, 0x0b, 0x9a, 0xff, 0xd3, 0xa4, 0xf4, 0x62, 0xdd,
            0x38, 0x89, 0xb2, 0xd0, 0xdd, 0x68, 0x70, 0xf3, 0x0e, 0x81, 0x44, 0x11, 0x19, 0xc1, 0x78, 0x13, 0xf7, 0x86, 0x7e, 0x43, 0x1c,
            0x98, 0x51, 0xec, 0xc1, 0x53, 0x3d, 0xac, 0x7c, 0x2f, 0x33, 0x59, 0x88, 0xdd, 0x29, 0x0e, 0x58, 0xf7, 0xb6, 0x92, 0x00, 0x3a,
            0x11, 0x04, 0xdf, 0x61, 0xa7, 0xe3, 0xa9, 0xb3, 0x4c, 0x91, 0xc4, 0xd1, 0x4f, 0xd9, 0x46, 0x3c, 0x1a, 0xc9, 0xb7, 0x9c, 0x00,
            0xe3, 0x84, 0xed, 0x45, 0xa5, 0x0c, 0x0c, 0x58, 0x0f, 0xed, 0x36, 0xb6, 0x2a, 0x8b, 0x92, 0x8c, 0x38, 0x9a, 0x8c, 0xfe, 0x8b,
            0x7c, 0xaa, 0x61, 0x37, 0x5b, 0x45, 0x56, 0xbd, 0x56, 0xf8, 0x7d, 0x35, 0xad, 0xae, 0xd0, 0x47, 0x91, 0x4f, 0xbb, 0xfd, 0x09,
            0xa4, 0x26, 0xb5, 0x95, 0x6c, 0xb4, 0x91, 0x27, 0x15, 0x41, 0x62, 0xc1, 0x64, 0xa2, 0xe9, 0xb3, 0xec, 0xdf, 0xe4, 0x24, 0x45,
            0xcb, 0x72, 0xb4, 0xa2, 0x58, 0xeb, 0xe0, 0xba, 0x42, 0xe5, 0x2a, 0xa7, 0xe5, 0xb8, 0x56, 0x10, 0x67, 0xf4, 0x1a, 0x62, 0x47,
            0xa4, 0xf3, 0x3a, 0x01, 0x7d, 0x21, 0x8d, 0x8d, 0x82, 0xd9, 0xc8, 0x3e, 0xfa, 0x16, 0x73, 0xab, 0x37, 0x47, 0xef, 0xe1, 0xa9,
            0x1d, 0xfd, 0x03, 0x74, 0xba, 0x61, 0xac, 0x64, 0xba, 0xfc, 0x07,
        ];
        assert_eq!(inflate(&zlib, text.len()).unwrap(), text);
    }
}
