//! JPEG: baseline and progressive Huffman coding, any sampling factors,
//! restart intervals, greyscale, YCbCr, RGB and Adobe CMYK/YCCK. The IDCT
//! is in integers (the kernel has no floating point unit enabled).

use super::MAX_PIXELS;
use alloc::vec::Vec;
use justgfx::Canvas;

const ZIGZAG: [usize; 80] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37,
    44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
    // Padding for corrupt runs past the end.
    63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63,
];

#[derive(Clone, Default)]
struct Huff {
    fast: Vec<u16>,
    maxcode: [i32; 18],
    valptr: [i32; 17],
    mincode: [i32; 17],
    vals: Vec<u8>,
}

impl Huff {
    fn new(counts: &[u8; 16], vals: &[u8]) -> Huff {
        let mut h = Huff { fast: alloc::vec![0; 256], vals: vals.to_vec(), ..Default::default() };
        let mut code = 0i32;
        let mut k = 0i32;
        for len in 1..=16usize {
            let n = counts[len - 1] as i32;
            h.valptr[len] = k;
            h.mincode[len] = code;
            if n > 0 {
                // Codes of up to 8 bits go in the fast table.
                if len <= 8 {
                    for i in 0..n {
                        let c = (code + i) as usize;
                        let first = c << (8 - len);
                        for j in 0..(1 << (8 - len)) {
                            if let Some(slot) = h.fast.get_mut(first + j) {
                                *slot = ((len as u16) << 8) | vals.get((k + i) as usize).copied().unwrap_or(0) as u16;
                            }
                        }
                    }
                }
                code += n;
                k += n;
                h.maxcode[len] = code - 1;
            } else {
                h.maxcode[len] = -1;
            }
            code <<= 1;
        }
        h.maxcode[17] = i32::MAX;
        h
    }
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    acc: u32,
    n: u32,
    marker: bool,
}

impl Bits<'_> {
    fn fill(&mut self) {
        while self.n <= 24 {
            let mut b = 0u32;
            if !self.marker && self.pos < self.d.len() {
                let c = self.d[self.pos];
                if c == 0xFF {
                    let next = self.d.get(self.pos + 1).copied().unwrap_or(0);
                    if next == 0 {
                        self.pos += 2;
                        b = 0xFF;
                    } else {
                        self.marker = true;
                    }
                } else {
                    self.pos += 1;
                    b = c as u32;
                }
            }
            self.acc |= b << (24 - self.n);
            self.n += 8;
        }
    }

    fn bits(&mut self, k: u32) -> u32 {
        if k == 0 {
            return 0;
        }
        self.fill();
        let v = self.acc >> (32 - k);
        self.acc <<= k;
        self.n -= k;
        v
    }

    fn bit(&mut self) -> u32 {
        self.bits(1)
    }

    fn receive_extend(&mut self, s: u32) -> i32 {
        if s == 0 {
            return 0;
        }
        let v = self.bits(s.min(16)) as i32;
        if v < (1 << (s - 1)) {
            v - (1 << s) + 1
        } else {
            v
        }
    }

    fn decode(&mut self, h: &Huff) -> Option<u8> {
        self.fill();
        let look = (self.acc >> 24) as usize;
        let f = h.fast[look];
        if f != 0 {
            let len = (f >> 8) as u32;
            self.acc <<= len;
            self.n -= len;
            return Some(f as u8);
        }
        let mut code = 0i32;
        for len in 1..=16usize {
            code = (code << 1) | self.bit() as i32;
            if h.maxcode[len] >= 0 && code <= h.maxcode[len] {
                return h.vals.get((h.valptr[len] + code - h.mincode[len]) as usize).copied();
            }
        }
        None
    }

    /// At a restart marker: drops the bits left and skips the marker.
    fn restart(&mut self) {
        self.acc = 0;
        self.n = 0;
        self.marker = false;
        while self.pos + 1 < self.d.len() {
            if self.d[self.pos] == 0xFF && (0xD0..=0xD7).contains(&self.d[self.pos + 1]) {
                self.pos += 2;
                return;
            }
            if self.d[self.pos] == 0xFF && self.d[self.pos + 1] != 0 && self.d[self.pos + 1] != 0xFF {
                return;
            }
            self.pos += 1;
        }
    }
}

#[derive(Clone, Default)]
struct Comp {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    /// Blocks across and down (padded to whole MCUs).
    bw: usize,
    bh: usize,
    coefs: Vec<i16>,
    pred: i32,
    td: usize,
    ta: usize,
}

/// The size in a JPEG's frame header.
pub fn size(d: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 4 <= d.len() {
        if d[i] != 0xFF {
            i += 1;
            continue;
        }
        let m = d[i + 1];
        if m == 0xFF {
            i += 1;
            continue;
        }
        let len = u16::from_be_bytes([d[i + 2], d[i + 3]]) as usize;
        if matches!(m, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let b = d.get(i + 4..i + 9)?;
            return Some((u16::from_be_bytes([b[3], b[4]]) as u32, u16::from_be_bytes([b[1], b[2]]) as u32));
        }
        i += 2 + len;
    }
    None
}

pub fn decode(d: &[u8]) -> Option<Canvas> {
    let mut q = [[1u16; 64]; 4];
    let mut dc: [Huff; 4] = Default::default();
    let mut ac: [Huff; 4] = Default::default();
    let mut comps: Vec<Comp> = Vec::new();
    let (mut w, mut h) = (0usize, 0usize);
    let mut progressive = false;
    let mut restart = 0usize;
    let mut adobe: Option<u8> = None;
    let (mut hmax, mut vmax, mut mcux, mut mcuy) = (1, 1, 0, 0);
    let mut i = 2;
    let mut eobrun;
    while i + 4 <= d.len() {
        if d[i] != 0xFF {
            i += 1;
            continue;
        }
        let m = d[i + 1];
        if m == 0xFF || m == 0x00 || (0xD0..=0xD7).contains(&m) {
            i += if m == 0xFF { 1 } else { 2 };
            continue;
        }
        if m == 0xD9 {
            break;
        }
        let len = u16::from_be_bytes([d[i + 2], d[i + 3]]) as usize;
        let seg = d.get(i + 4..i + 2 + len)?;
        match m {
            0xDB => {
                let mut p = 0;
                while p < seg.len() {
                    let pq = seg[p] >> 4;
                    let tq = (seg[p] & 3) as usize;
                    p += 1;
                    for k in 0..64 {
                        q[tq][k] = if pq == 0 {
                            *seg.get(p + k)? as u16
                        } else {
                            u16::from_be_bytes([*seg.get(p + 2 * k)?, *seg.get(p + 2 * k + 1)?])
                        };
                    }
                    p += if pq == 0 { 64 } else { 128 };
                }
            }
            0xC4 => {
                let mut p = 0;
                while p + 17 <= seg.len() {
                    let class = seg[p] >> 4;
                    let id = (seg[p] & 3) as usize;
                    let mut counts = [0u8; 16];
                    counts.copy_from_slice(&seg[p + 1..p + 17]);
                    let total: usize = counts.iter().map(|&c| c as usize).sum();
                    let vals = seg.get(p + 17..p + 17 + total)?;
                    let t = Huff::new(&counts, vals);
                    if class == 0 {
                        dc[id] = t;
                    } else {
                        ac[id] = t;
                    }
                    p += 17 + total;
                }
            }
            0xC0 | 0xC1 | 0xC2 => {
                progressive = m == 0xC2;
                h = u16::from_be_bytes([seg[1], seg[2]]) as usize;
                w = u16::from_be_bytes([seg[3], seg[4]]) as usize;
                let n = seg[5] as usize;
                if w == 0 || h == 0 || w * h > MAX_PIXELS || n == 0 || n > 4 {
                    return None;
                }
                for c in 0..n {
                    let b = seg.get(6 + c * 3..9 + c * 3)?;
                    comps.push(Comp { id: b[0], h: (b[1] >> 4).max(1) as usize, v: (b[1] & 15).max(1) as usize, tq: (b[2] & 3) as usize, ..Default::default() });
                }
                hmax = comps.iter().map(|c| c.h).max().unwrap_or(1);
                vmax = comps.iter().map(|c| c.v).max().unwrap_or(1);
                mcux = w.div_ceil(8 * hmax);
                mcuy = h.div_ceil(8 * vmax);
                for c in comps.iter_mut() {
                    c.bw = mcux * c.h;
                    c.bh = mcuy * c.v;
                    c.coefs = alloc::vec![0; c.bw * c.bh * 64];
                }
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return None,
            0xDD => restart = u16::from_be_bytes([seg[0], seg[1]]) as usize,
            0xEE => {
                if seg.starts_with(b"Adobe") && seg.len() >= 12 {
                    adobe = Some(seg[11]);
                }
            }
            0xDA => {
                let ns = seg[0] as usize;
                let mut sc: Vec<usize> = Vec::new();
                for k in 0..ns {
                    let id = seg[1 + k * 2];
                    let t = seg[2 + k * 2];
                    let ci = comps.iter().position(|c| c.id == id)?;
                    comps[ci].td = (t >> 4) as usize & 3;
                    comps[ci].ta = (t & 15) as usize & 3;
                    sc.push(ci);
                }
                let p = 1 + ns * 2;
                let (ss, se, ah, al) = (seg[p] as usize, seg[p + 1] as usize, seg[p + 2] >> 4, seg[p + 2] & 15);
                let mut bits = Bits { d, pos: i + 2 + len, acc: 0, n: 0, marker: false };
                for c in comps.iter_mut() {
                    c.pred = 0;
                }
                eobrun = 0;
                scan(&mut comps, &sc, &dc, &ac, &mut bits, progressive, (ss, se, ah as u32, al as u32), restart, (mcux, mcuy, hmax, vmax, w, h), &mut eobrun);
                // Continue after the entropy-coded data.
                let mut j = bits.pos;
                while j + 1 < d.len() && !(d[j] == 0xFF && d[j + 1] != 0 && !(0xD0..=0xD7).contains(&d[j + 1])) {
                    j += 1;
                }
                i = j;
                continue;
            }
            _ => {}
        }
        i += 2 + len;
    }
    if comps.is_empty() {
        return None;
    }
    // Dequantize, transform, then build the pixels.
    let mut planes: Vec<Vec<u8>> = Vec::new();
    for c in &comps {
        let pw = c.bw * 8;
        let mut plane = alloc::vec![0u8; pw * c.bh * 8];
        let qt = &q[c.tq];
        let mut blk = [0i32; 64];
        for by in 0..c.bh {
            for bx in 0..c.bw {
                let co = &c.coefs[(by * c.bw + bx) * 64..][..64];
                for k in 0..64 {
                    blk[ZIGZAG[k]] = co[ZIGZAG[k]] as i32 * qt[k] as i32;
                }
                idct(&blk, &mut plane[by * 8 * pw + bx * 8..], pw);
            }
        }
        planes.push(plane);
    }
    let mut out = alloc::vec![0u32; w * h];
    let n = comps.len();
    let sample = |ci: usize, x: usize, y: usize| -> i32 {
        let c = &comps[ci];
        let sx = x * c.h / hmax;
        let sy = y * c.v / vmax;
        planes[ci][sy * c.bw * 8 + sx] as i32
    };
    let rgb_direct = n == 3 && (adobe == Some(0) || (comps[0].id == b'R' && comps[1].id == b'G'));
    for y in 0..h {
        for x in 0..w {
            let px = match n {
                1 | 2 => {
                    let g = sample(0, x, y) as u32;
                    0xFF00_0000 | g << 16 | g << 8 | g
                }
                3 if rgb_direct => 0xFF00_0000 | (sample(0, x, y) as u32) << 16 | (sample(1, x, y) as u32) << 8 | sample(2, x, y) as u32,
                3 => ycc(sample(0, x, y), sample(1, x, y), sample(2, x, y)),
                _ => {
                    let k = sample(3, x, y);
                    let (r, g, b) = if adobe == Some(2) {
                        let p = ycc(sample(0, x, y), sample(1, x, y), sample(2, x, y));
                        (255 - ((p >> 16) & 255) as i32, 255 - ((p >> 8) & 255) as i32, 255 - (p & 255) as i32)
                    } else {
                        (sample(0, x, y), sample(1, x, y), sample(2, x, y))
                    };
                    // Adobe stores CMYK inverted.
                    let m = |c: i32| ((c * k + 127) / 255).clamp(0, 255) as u32;
                    0xFF00_0000 | m(r) << 16 | m(g) << 8 | m(b)
                }
            };
            out[y * w + x] = px;
        }
    }
    Some(Canvas::from_pixels(w as i32, h as i32, out))
}

fn ycc(y: i32, cb: i32, cr: i32) -> u32 {
    let (cb, cr) = (cb - 128, cr - 128);
    let y = y << 16;
    let r = (y + 91881 * cr + 32768) >> 16;
    let g = (y - 22554 * cb - 46802 * cr + 32768) >> 16;
    let b = (y + 116130 * cb + 32768) >> 16;
    0xFF00_0000 | (r.clamp(0, 255) as u32) << 16 | (g.clamp(0, 255) as u32) << 8 | b.clamp(0, 255) as u32
}

#[allow(clippy::too_many_arguments)]
fn scan(
    comps: &mut [Comp],
    sc: &[usize],
    dc: &[Huff; 4],
    ac: &[Huff; 4],
    bits: &mut Bits<'_>,
    progressive: bool,
    (ss, se, ah, al): (usize, usize, u32, u32),
    restart: usize,
    (mcux, mcuy, hmax, vmax, w, h): (usize, usize, usize, usize, usize, usize),
    eobrun: &mut u32,
) {
    let decode_block = |c: &mut Comp, bx: usize, by: usize, bits: &mut Bits<'_>, eobrun: &mut u32| -> Option<()> {
        let off = (by * c.bw + bx) * 64;
        let data = c.coefs.get_mut(off..off + 64)?;
        if !progressive {
            let t = bits.decode(&dc[c.td])?;
            let diff = bits.receive_extend(t as u32);
            c.pred += diff;
            data[0] = c.pred as i16;
            let mut k = 1;
            while k < 64 {
                let rs = bits.decode(&ac[c.ta])?;
                let s = (rs & 15) as u32;
                let r = (rs >> 4) as usize;
                if s == 0 {
                    if rs != 0xF0 {
                        break;
                    }
                    k += 16;
                } else {
                    k += r;
                    data[ZIGZAG[k.min(79)]] = bits.receive_extend(s) as i16;
                    k += 1;
                }
            }
            return Some(());
        }
        if ss == 0 {
            // DC scans.
            if ah == 0 {
                let t = bits.decode(&dc[c.td])?;
                let diff = bits.receive_extend(t as u32);
                c.pred += diff;
                data[0] = (c.pred << al) as i16;
            } else if bits.bit() != 0 {
                data[0] |= (1 << al) as i16;
            }
            return Some(());
        }
        if ah == 0 {
            if *eobrun > 0 {
                *eobrun -= 1;
                return Some(());
            }
            let mut k = ss;
            while k <= se {
                let rs = bits.decode(&ac[c.ta])?;
                let s = (rs & 15) as u32;
                let r = (rs >> 4) as u32;
                if s == 0 {
                    if r < 15 {
                        *eobrun = (1 << r) - 1;
                        if r > 0 {
                            *eobrun += bits.bits(r);
                        }
                        break;
                    }
                    k += 16;
                } else {
                    k += r as usize;
                    data[ZIGZAG[k.min(79)]] = (bits.receive_extend(s) * (1 << al)) as i16;
                    k += 1;
                }
            }
            return Some(());
        }
        // AC refinement.
        let bit = (1i32 << al) as i16;
        let refine = |p: &mut i16, bits: &mut Bits<'_>| {
            if bits.bit() != 0 && (*p & bit) == 0 {
                if *p > 0 {
                    *p += bit;
                } else {
                    *p -= bit;
                }
            }
        };
        if *eobrun > 0 {
            *eobrun -= 1;
            for k in ss..=se {
                let p = &mut data[ZIGZAG[k]];
                if *p != 0 {
                    refine(p, bits);
                }
            }
            return Some(());
        }
        let mut k = ss;
        while k <= se {
            let rs = bits.decode(&ac[c.ta])?;
            let mut s = (rs & 15) as i32;
            let mut r = (rs >> 4) as i32;
            if s == 0 {
                if r < 15 {
                    *eobrun = (1 << r) - 1;
                    if r > 0 {
                        *eobrun += bits.bits(r as u32);
                    }
                    r = 64;
                }
            } else {
                s = if bits.bit() != 0 { bit as i32 } else { -(bit as i32) };
            }
            while k <= se {
                let p = &mut data[ZIGZAG[k]];
                k += 1;
                if *p != 0 {
                    refine(p, bits);
                } else {
                    if r == 0 {
                        *p = s as i16;
                        break;
                    }
                    r -= 1;
                }
            }
        }
        Some(())
    };
    let mut count = 0usize;
    let check_restart = |count: &mut usize, bits: &mut Bits<'_>, comps: &mut [Comp], eobrun: &mut u32, last: bool| {
        *count += 1;
        if restart > 0 && *count % restart == 0 && !last {
            bits.restart();
            for c in comps.iter_mut() {
                c.pred = 0;
            }
            *eobrun = 0;
        }
    };
    if sc.len() == 1 {
        let ci = sc[0];
        let (cw, ch) = {
            let c = &comps[ci];
            ((w * c.h).div_ceil(hmax).div_ceil(8), (h * c.v).div_ceil(vmax).div_ceil(8))
        };
        for by in 0..ch {
            for bx in 0..cw {
                if decode_block(&mut comps[ci], bx, by, bits, eobrun).is_none() {
                    return;
                }
                check_restart(&mut count, bits, comps, eobrun, by == ch - 1 && bx == cw - 1);
            }
        }
        return;
    }
    for my in 0..mcuy {
        for mx in 0..mcux {
            for &ci in sc {
                let (ch, cv) = (comps[ci].h, comps[ci].v);
                for v in 0..cv {
                    for hh in 0..ch {
                        if decode_block(&mut comps[ci], mx * ch + hh, my * cv + v, bits, eobrun).is_none() {
                            return;
                        }
                    }
                }
            }
            check_restart(&mut count, bits, comps, eobrun, my == mcuy - 1 && mx == mcux - 1);
        }
    }
}

const fn f2f(x: f64) -> i32 {
    (x * 4096.0 + 0.5) as i32
}

#[inline(always)]
fn idct_1d(s: [i32; 8]) -> ([i32; 4], [i32; 4]) {
    let p2 = s[2];
    let p3 = s[6];
    let p1 = (p2 + p3) * f2f(0.5411961);
    let t2 = p1 + p3 * f2f(-1.847759065);
    let t3 = p1 + p2 * f2f(0.765366865);
    let t0 = (s[0] + s[4]) * 4096;
    let t1 = (s[0] - s[4]) * 4096;
    let x0 = t0 + t3;
    let x3 = t0 - t3;
    let x1 = t1 + t2;
    let x2 = t1 - t2;
    let (mut t0, mut t1, mut t2, mut t3) = (s[7], s[5], s[3], s[1]);
    let p3 = t0 + t2;
    let p4 = t1 + t3;
    let p1 = t0 + t3;
    let p2 = t1 + t2;
    let p5 = (p3 + p4) * f2f(1.175875602);
    t0 *= f2f(0.298631336);
    t1 *= f2f(2.053119869);
    t2 *= f2f(3.072711026);
    t3 *= f2f(1.501321110);
    let p1 = p5 + p1 * f2f(-0.899976223);
    let p2 = p5 + p2 * f2f(-2.562915447);
    let p3 = p3 * f2f(-1.961570560);
    let p4 = p4 * f2f(-0.390180644);
    t3 += p1 + p4;
    t2 += p2 + p3;
    t1 += p2 + p4;
    t0 += p1 + p3;
    ([x0, x1, x2, x3], [t0, t1, t2, t3])
}

/// The 8x8 inverse DCT (as in the IJG integer "islow" method).
fn idct(d: &[i32; 64], out: &mut [u8], stride: usize) {
    let mut v = [0i32; 64];
    for i in 0..8 {
        if (1..8).all(|r| d[r * 8 + i] == 0) {
            let dc = d[i] * 4;
            for r in 0..8 {
                v[r * 8 + i] = dc;
            }
            continue;
        }
        let ([x0, x1, x2, x3], [t0, t1, t2, t3]) = idct_1d([d[i], d[8 + i], d[16 + i], d[24 + i], d[32 + i], d[40 + i], d[48 + i], d[56 + i]]);
        let (x0, x1, x2, x3) = (x0 + 512, x1 + 512, x2 + 512, x3 + 512);
        v[i] = (x0 + t3) >> 10;
        v[56 + i] = (x0 - t3) >> 10;
        v[8 + i] = (x1 + t2) >> 10;
        v[48 + i] = (x1 - t2) >> 10;
        v[16 + i] = (x2 + t1) >> 10;
        v[40 + i] = (x2 - t1) >> 10;
        v[24 + i] = (x3 + t0) >> 10;
        v[32 + i] = (x3 - t0) >> 10;
    }
    for r in 0..8 {
        let row = [v[r * 8], v[r * 8 + 1], v[r * 8 + 2], v[r * 8 + 3], v[r * 8 + 4], v[r * 8 + 5], v[r * 8 + 6], v[r * 8 + 7]];
        let ([x0, x1, x2, x3], [t0, t1, t2, t3]) = idct_1d(row);
        let bias = 65536 + (128 << 17);
        let (x0, x1, x2, x3) = (x0 + bias, x1 + bias, x2 + bias, x3 + bias);
        let o = &mut out[r * stride..r * stride + 8];
        let c = |v: i32| (v >> 17).clamp(0, 255) as u8;
        o[0] = c(x0 + t3);
        o[7] = c(x0 - t3);
        o[1] = c(x1 + t2);
        o[6] = c(x1 - t2);
        o[2] = c(x2 + t1);
        o[5] = c(x2 - t1);
        o[3] = c(x3 + t0);
        o[4] = c(x3 - t0);
    }
}
