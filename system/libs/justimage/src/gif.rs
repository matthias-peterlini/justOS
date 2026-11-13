//! GIF: the first frame, with its palette, transparency and interlacing.

use super::MAX_PIXELS;
use alloc::vec::Vec;
use justgfx::Canvas;

pub fn decode(d: &[u8]) -> Option<Canvas> {
    let w = u16::from_le_bytes([*d.get(6)?, *d.get(7)?]) as usize;
    let h = u16::from_le_bytes([*d.get(8)?, *d.get(9)?]) as usize;
    if w == 0 || h == 0 || w * h > MAX_PIXELS {
        return None;
    }
    let flags = d[10];
    let mut i = 13;
    let mut global: Vec<u32> = Vec::new();
    if flags & 0x80 != 0 {
        let n = 2usize << (flags & 7);
        global = palette(d.get(i..i + n * 3)?);
        i += n * 3;
    }
    let mut transparent: Option<u8> = None;
    let mut out = alloc::vec![0u32; w * h];
    while i < d.len() {
        match d[i] {
            0x21 => {
                let label = *d.get(i + 1)?;
                i += 2;
                if label == 0xF9 && d.get(i) == Some(&4) {
                    if d[i + 1] & 1 != 0 {
                        transparent = Some(d[i + 4]);
                    }
                }
                // Skip the sub-blocks.
                while i < d.len() && d[i] != 0 {
                    i += 1 + d[i] as usize;
                }
                i += 1;
            }
            0x2C => {
                let b = d.get(i + 1..i + 10)?;
                let fx = u16::from_le_bytes([b[0], b[1]]) as usize;
                let fy = u16::from_le_bytes([b[2], b[3]]) as usize;
                let fw = u16::from_le_bytes([b[4], b[5]]) as usize;
                let fh = u16::from_le_bytes([b[6], b[7]]) as usize;
                let f = b[8];
                i += 10;
                let mut pal = global.clone();
                if f & 0x80 != 0 {
                    let n = 2usize << (f & 7);
                    pal = palette(d.get(i..i + n * 3)?);
                    i += n * 3;
                }
                let interlaced = f & 0x40 != 0;
                let min = *d.get(i)? as u32;
                i += 1;
                let mut data = Vec::new();
                while i < d.len() && d[i] != 0 {
                    let n = d[i] as usize;
                    data.extend_from_slice(d.get(i + 1..i + 1 + n).unwrap_or(&d[i + 1..]));
                    i += 1 + n;
                }
                let idx = lzw(&data, min, fw * fh);
                let rows: Vec<usize> = if interlaced {
                    let mut r = Vec::new();
                    for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
                        r.extend((start..fh).step_by(step));
                    }
                    r
                } else {
                    (0..fh).collect()
                };
                for (n, &ry) in rows.iter().enumerate() {
                    for x in 0..fw {
                        let Some(&c) = idx.get(n * fw + x) else { break };
                        let (px, py) = (fx + x, fy + ry);
                        if px < w && py < h && Some(c) != transparent {
                            out[py * w + px] = pal.get(c as usize).copied().unwrap_or(0xFF00_0000);
                        }
                    }
                }
                return Some(Canvas::from_pixels(w as i32, h as i32, out));
            }
            _ => break,
        }
    }
    None
}

fn palette(b: &[u8]) -> Vec<u32> {
    b.chunks(3).map(|c| 0xFF00_0000 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32).collect()
}

fn lzw(data: &[u8], min: u32, max: usize) -> Vec<u8> {
    let min = min.clamp(2, 11);
    let clear = 1u32 << min;
    let end = clear + 1;
    let mut size = min + 1;
    // Each code: (prefix, byte, length).
    let mut prefix: Vec<u16> = alloc::vec![0; 4096];
    let mut suffix: Vec<u8> = alloc::vec![0; 4096];
    let mut first: Vec<u8> = alloc::vec![0; 4096];
    for c in 0..clear as usize {
        suffix[c] = c as u8;
        first[c] = c as u8;
    }
    let mut next = end + 1;
    let mut prev: Option<u32> = None;
    let mut out = Vec::with_capacity(max);
    let (mut acc, mut n, mut pos) = (0u32, 0u32, 0usize);
    let mut stack = Vec::new();
    while out.len() < max {
        while n < size {
            if pos >= data.len() {
                return out;
            }
            acc |= (data[pos] as u32) << n;
            n += 8;
            pos += 1;
        }
        let code = acc & ((1 << size) - 1);
        acc >>= size;
        n -= size;
        if code == clear {
            size = min + 1;
            next = end + 1;
            prev = None;
            continue;
        }
        if code == end {
            break;
        }
        let Some(p) = prev else {
            out.push(suffix[code as usize]);
            prev = Some(code);
            continue;
        };
        let known = code < next;
        let mut c = if known { code } else { p };
        stack.clear();
        while c > clear {
            stack.push(suffix[c as usize]);
            c = prefix[c as usize] as u32;
        }
        stack.push(c as u8);
        let head = *stack.last().unwrap_or(&0);
        out.extend(stack.iter().rev());
        if !known {
            out.push(head);
        }
        if next < 4096 {
            prefix[next as usize] = p as u16;
            suffix[next as usize] = head;
            first[next as usize] = first[p as usize];
            next += 1;
            if next == 1 << size && size < 12 {
                size += 1;
            }
        }
        prev = Some(code);
    }
    out
}
