//! A decoder for QOI ("Quite OK Image") files, the format of the icons and
//! wallpapers in `sys:/system/gui` (written by `tools/assets/qoi.py`).

use crate::gfx::Canvas;
use alloc::vec::Vec;

/// Decodes a QOI file into a canvas of `0xAARRGGBB` pixels.
pub fn decode(data: &[u8]) -> Option<Canvas> {
    if data.len() < 22 || &data[..4] != b"qoif" {
        return None;
    }
    let w = u32::from_be_bytes(data[4..8].try_into().ok()?);
    let h = u32::from_be_bytes(data[8..12].try_into().ok()?);
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return None;
    }
    let total = (w * h) as usize;
    let mut px: Vec<u32> = Vec::with_capacity(total);
    let mut index = [0u32; 64];
    let (mut r, mut g, mut b, mut a) = (0u8, 0u8, 0u8, 255u8);
    let mut i = 14;
    let end = data.len() - 8;
    while px.len() < total && i < end {
        let op = data[i];
        i += 1;
        let mut run = 1;
        match op {
            0xFE => {
                r = data[i];
                g = data[i + 1];
                b = data[i + 2];
                i += 3;
            }
            0xFF => {
                r = data[i];
                g = data[i + 1];
                b = data[i + 2];
                a = data[i + 3];
                i += 4;
            }
            _ => match op >> 6 {
                0 => {
                    let c = index[op as usize];
                    a = (c >> 24) as u8;
                    r = (c >> 16) as u8;
                    g = (c >> 8) as u8;
                    b = c as u8;
                }
                1 => {
                    r = r.wrapping_add(((op >> 4) & 3).wrapping_sub(2));
                    g = g.wrapping_add(((op >> 2) & 3).wrapping_sub(2));
                    b = b.wrapping_add((op & 3).wrapping_sub(2));
                }
                2 => {
                    let dg = (op & 0x3F).wrapping_sub(32);
                    let next = data[i];
                    i += 1;
                    r = r.wrapping_add(dg.wrapping_add((next >> 4).wrapping_sub(8)));
                    g = g.wrapping_add(dg);
                    b = b.wrapping_add(dg.wrapping_add((next & 0xF).wrapping_sub(8)));
                }
                _ => run = (op & 0x3F) as usize + 1,
            },
        }
        let c = (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32;
        let hash = (r as usize * 3 + g as usize * 5 + b as usize * 7 + a as usize * 11) % 64;
        index[hash] = c;
        for _ in 0..run.min(total - px.len()) {
            px.push(c);
        }
    }
    px.resize(total, 0);
    Some(Canvas::from_pixels(w as i32, h as i32, px))
}

/// Encodes a canvas as a QOI file (with alpha), pixel for pixel.
pub fn encode(img: &Canvas) -> Vec<u8> {
    let mut out = Vec::with_capacity(img.px.len() * 2 + 22);
    out.extend_from_slice(b"qoif");
    out.extend_from_slice(&(img.pixel_width() as u32).to_be_bytes());
    out.extend_from_slice(&(img.pixel_height() as u32).to_be_bytes());
    out.extend_from_slice(&[4, 0]);
    let mut index = [[0u8; 4]; 64];
    let mut prev = [0u8, 0, 0, 255];
    let mut run = 0u8;
    let n = img.px.len();
    for (i, &p) in img.px.iter().enumerate() {
        let px = [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8];
        if px == prev {
            run += 1;
            if run == 62 || i == n - 1 {
                out.push(0xC0 | (run - 1));
                run = 0;
            }
            continue;
        }
        if run > 0 {
            out.push(0xC0 | (run - 1));
            run = 0;
        }
        let h = (px[0] as usize * 3 + px[1] as usize * 5 + px[2] as usize * 7 + px[3] as usize * 11) % 64;
        if index[h] == px {
            out.push(h as u8);
        } else {
            index[h] = px;
            if px[3] == prev[3] {
                let dr = px[0].wrapping_sub(prev[0]) as i8;
                let dg = px[1].wrapping_sub(prev[1]) as i8;
                let db = px[2].wrapping_sub(prev[2]) as i8;
                let (dr_dg, db_dg) = (dr.wrapping_sub(dg), db.wrapping_sub(dg));
                if (-2..2).contains(&dr) && (-2..2).contains(&dg) && (-2..2).contains(&db) {
                    out.push(0x40 | (((dr + 2) as u8) << 4) | (((dg + 2) as u8) << 2) | (db + 2) as u8);
                } else if (-32..32).contains(&dg) && (-8..8).contains(&dr_dg) && (-8..8).contains(&db_dg) {
                    out.push(0x80 | (dg + 32) as u8);
                    out.push((((dr_dg + 8) as u8) << 4) | (db_dg + 8) as u8);
                } else {
                    out.extend_from_slice(&[0xFE, px[0], px[1], px[2]]);
                }
            } else {
                out.extend_from_slice(&[0xFF, px[0], px[1], px[2], px[3]]);
            }
        }
        prev = px;
    }
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn encode_then_decode() {
        let mut c = Canvas::new(37, 21, 0);
        for (i, p) in c.px.iter_mut().enumerate() {
            // Runs, small and large changes, and transparency.
            *p = match i % 7 {
                0 | 1 => 0xFF10_2030,
                2 => 0xFF11_2131,
                3 => 0x80FF_0000,
                _ => (i as u32).wrapping_mul(0x9E37_79B9) | 0xFF00_0000,
            };
        }
        let data = encode(&c);
        let back = decode(&data).expect("decodes");
        assert_eq!((back.w, back.h), (37, 21));
        assert_eq!(back.px, c.px);
    }

    #[test]
    fn long_runs() {
        let c = Canvas::from_pixels(200, 1, (0..200).map(|i| if i < 150 { 0xFF00_00FF } else { 0xFFFF_FFFF }).collect::<Vec<u32>>());
        assert_eq!(decode(&encode(&c)).expect("decodes").px, c.px);
    }
}
