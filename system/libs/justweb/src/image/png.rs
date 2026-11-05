//! PNG: every colour type and bit depth, palettes with transparency, and
//! Adam7 interlacing.

use super::MAX_PIXELS;
use alloc::vec::Vec;
use justgfx::Canvas;

pub fn decode(data: &[u8]) -> Option<Canvas> {
    let mut i = 8;
    let (mut w, mut h, mut depth, mut ctype, mut interlace) = (0usize, 0usize, 0u8, 0u8, 0u8);
    let mut palette: Vec<u32> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat = Vec::new();
    while i + 8 <= data.len() {
        let len = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        let kind = &data[i + 4..i + 8];
        let body = data.get(i + 8..i + 8 + len)?;
        match kind {
            b"IHDR" => {
                w = u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize;
                h = u32::from_be_bytes([body[4], body[5], body[6], body[7]]) as usize;
                depth = body[8];
                ctype = body[9];
                interlace = body[12];
            }
            b"PLTE" => palette = body.chunks(3).map(|c| 0xFF00_0000 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32).collect(),
            b"tRNS" => trns = body.to_vec(),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        i += 12 + len;
    }
    if w == 0 || h == 0 || w * h > MAX_PIXELS {
        return None;
    }
    let raw = justzip::zlib(&idat)?;
    let channels = match ctype {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        _ => return None,
    };
    let bpp = (channels * depth as usize).div_ceil(8).max(1);
    let mut out = alloc::vec![0u32; w * h];
    let passes: &[(usize, usize, usize, usize)] = if interlace == 1 {
        &[(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]
    } else {
        &[(0, 0, 1, 1)]
    };
    let mut at = 0;
    for &(x0, y0, dx, dy) in passes {
        if x0 >= w || y0 >= h {
            continue;
        }
        let pw = (w - x0).div_ceil(dx);
        let ph = (h - y0).div_ceil(dy);
        let stride = (pw * channels * depth as usize).div_ceil(8);
        let mut prev = alloc::vec![0u8; stride];
        let mut line = alloc::vec![0u8; stride];
        for py in 0..ph {
            let filter = *raw.get(at)?;
            let src = raw.get(at + 1..at + 1 + stride)?;
            at += 1 + stride;
            for x in 0..stride {
                let a = if x >= bpp { line[x - bpp] } else { 0 } as i32;
                let b = prev[x] as i32;
                let c = if x >= bpp { prev[x - bpp] } else { 0 } as i32;
                let v = src[x] as i32;
                line[x] = match filter {
                    0 => v,
                    1 => v + a,
                    2 => v + b,
                    3 => v + (a + b) / 2,
                    4 => {
                        let p = a + b - c;
                        let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                        v + if pa <= pb && pa <= pc {
                            a
                        } else if pb <= pc {
                            b
                        } else {
                            c
                        }
                    }
                    _ => return None,
                } as u8;
            }
            let y = y0 + py * dy;
            for px in 0..pw {
                let x = x0 + px * dx;
                out[y * w + x] = pixel(&line, px, ctype, depth, &palette, &trns);
            }
            core::mem::swap(&mut prev, &mut line);
        }
    }
    Some(Canvas::from_pixels(w as i32, h as i32, out))
}

fn sample(line: &[u8], index: usize, depth: u8) -> u32 {
    match depth {
        8 => line[index] as u32,
        16 => line[index * 2] as u32,
        d => {
            let bit = index * d as usize;
            let byte = line[bit / 8];
            let shift = 8 - d as usize - bit % 8;
            let v = (byte >> shift) as u32 & ((1 << d) - 1);
            v
        }
    }
}

fn pixel(line: &[u8], x: usize, ctype: u8, depth: u8, palette: &[u32], trns: &[u8]) -> u32 {
    let scale = |v: u32| match depth {
        1 => v * 255,
        2 => v * 85,
        4 => v * 17,
        _ => v,
    };
    let raw16 = |i: usize| -> u16 { if depth == 16 { u16::from_be_bytes([line[i * 2], line[i * 2 + 1]]) } else { line[i] as u16 } };
    match ctype {
        0 => {
            let v = sample(line, x, depth);
            let g = scale(v);
            let transparent = trns.len() >= 2 && {
                let t = u16::from_be_bytes([trns[0], trns[1]]);
                if depth == 16 { raw16(x) == t } else { v as u16 == t }
            };
            if transparent { 0 } else { 0xFF00_0000 | g << 16 | g << 8 | g }
        }
        2 => {
            let (r, g, b) = (sample(line, x * 3, depth), sample(line, x * 3 + 1, depth), sample(line, x * 3 + 2, depth));
            let transparent = trns.len() >= 6 && {
                let t = |i: usize| u16::from_be_bytes([trns[i * 2], trns[i * 2 + 1]]);
                if depth == 16 { (raw16(x * 3), raw16(x * 3 + 1), raw16(x * 3 + 2)) == (t(0), t(1), t(2)) } else { (r as u16, g as u16, b as u16) == (t(0), t(1), t(2)) }
            };
            if transparent { 0 } else { 0xFF00_0000 | r << 16 | g << 8 | b }
        }
        3 => {
            let i = sample(line, x, depth) as usize;
            let c = palette.get(i).copied().unwrap_or(0xFF00_0000);
            let a = trns.get(i).copied().unwrap_or(255) as u32;
            (c & 0xFF_FFFF) | a << 24
        }
        4 => {
            let g = sample(line, x * 2, depth);
            let a = sample(line, x * 2 + 1, depth);
            a << 24 | g << 16 | g << 8 | g
        }
        _ => {
            let (r, g, b, a) = (sample(line, x * 4, depth), sample(line, x * 4 + 1, depth), sample(line, x * 4 + 2, depth), sample(line, x * 4 + 3, depth));
            a << 24 | r << 16 | g << 8 | b
        }
    }
}
