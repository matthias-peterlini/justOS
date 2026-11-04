//! Resource templates: the buffers `_CRS` and `_PRS` give, which say which
//! I/O ports, memory ranges and interrupts a device uses (ACPI 6.5, 6.4).

use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    /// Interrupts the device may use (IRQ and Extended Interrupt).
    Irq { irqs: Vec<u32>, edge: bool, active_low: bool, shared: bool },
    Dma { channels: Vec<u8> },
    /// I/O ports: `len` ports somewhere between `min` and `max` (a fixed
    /// range has `min == max`).
    Io { min: u16, max: u16, len: u16 },
    Memory { base: u64, len: u64, writable: bool },
    /// A Word, DWord or QWord address space descriptor: `kind` 0 is
    /// memory, 1 I/O, 2 a range of bus numbers.
    Address { kind: u8, min: u64, max: u64, translation: u64, len: u64 },
    /// A Generic Register descriptor.
    Register { space: u8, bit_width: u8, bit_offset: u8, access: u8, address: u64 },
    /// Something else, by its descriptor type.
    Other(u8),
}

fn le(b: &[u8], at: usize, n: usize) -> u64 {
    b.get(at..at + n).map(|s| s.iter().rev().fold(0, |a, &x| a << 8 | x as u64)).unwrap_or(0)
}

fn bits(mask: u64, width: u32) -> impl Iterator<Item = u32> {
    (0..width).filter(move |i| mask >> i & 1 != 0)
}

/// Reads a resource template. Stops at the end tag, or at a descriptor
/// that runs past the end.
pub fn parse(b: &[u8]) -> Vec<Resource> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < b.len() {
        let tag = b[at];
        if tag & 0x80 == 0 {
            let kind = (tag >> 3) & 0x0F;
            let len = (tag & 7) as usize;
            let d = match b.get(at + 1..at + 1 + len) {
                Some(d) => d,
                None => break,
            };
            match kind {
                0x0F => break,
                0x04 => {
                    let info = d.get(2).copied().unwrap_or(1);
                    out.push(Resource::Irq {
                        irqs: bits(le(d, 0, 2), 16).collect(),
                        edge: info & 1 != 0,
                        active_low: info & 8 != 0,
                        shared: info & 0x10 != 0,
                    });
                }
                0x05 => out.push(Resource::Dma { channels: bits(le(d, 0, 1), 8).map(|x| x as u8).collect() }),
                0x08 => out.push(Resource::Io { min: le(d, 1, 2) as u16, max: le(d, 3, 2) as u16, len: le(d, 6, 1) as u16 }),
                0x09 => {
                    let base = le(d, 0, 2) as u16 & 0x3FF;
                    out.push(Resource::Io { min: base, max: base, len: le(d, 2, 1) as u16 });
                }
                0x0A => {
                    // Fixed DMA
                    out.push(Resource::Dma { channels: alloc::vec![le(d, 0, 2) as u8] });
                }
                k => out.push(Resource::Other(k)),
            }
            at += 1 + len;
        } else {
            let len = le(b, at + 1, 2) as usize;
            let d = match b.get(at + 3..at + 3 + len) {
                Some(d) => d,
                None => break,
            };
            let kind = tag & 0x7F;
            let g = |i: usize| d.get(i).copied().unwrap_or(0);
            match kind {
                0x01 => out.push(Resource::Memory { base: le(d, 1, 2) << 8, len: le(d, 7, 2) << 8, writable: g(0) & 1 != 0 }),
                0x02 => out.push(Resource::Register {
                    space: g(0),
                    bit_width: g(1),
                    bit_offset: g(2),
                    access: g(3),
                    address: le(d, 4, 8),
                }),
                0x05 => out.push(Resource::Memory { base: le(d, 1, 4), len: le(d, 13, 4), writable: g(0) & 1 != 0 }),
                0x06 => out.push(Resource::Memory { base: le(d, 1, 4), len: le(d, 5, 4), writable: g(0) & 1 != 0 }),
                0x07 | 0x08 | 0x0A => {
                    let n = match kind {
                        0x08 => 2,
                        0x07 => 4,
                        _ => 8,
                    };
                    // type, general flags, type flags, then granularity,
                    // min, max, translation and length.
                    let f = |i: usize| le(d, 3 + i * n, n);
                    out.push(Resource::Address { kind: g(0), min: f(1), max: f(2), translation: f(3), len: f(4) });
                }
                0x09 => {
                    let flags = g(0);
                    let count = d.get(1).copied().unwrap_or(0) as usize;
                    let irqs = (0..count).map(|i| le(d, 2 + i * 4, 4) as u32).collect();
                    out.push(Resource::Irq {
                        irqs,
                        edge: flags & 2 != 0,
                        active_low: flags & 4 != 0,
                        shared: flags & 8 != 0,
                    });
                }
                k => out.push(Resource::Other(0x80 | k)),
            }
            at += 3 + len;
        }
    }
    out
}

/// The I/O ports of a template, fixed ranges first in their order.
pub fn io_ports(r: &[Resource]) -> Vec<u16> {
    r.iter()
        .filter_map(|x| match x {
            Resource::Io { min, .. } => Some(*min),
            _ => None,
        })
        .collect()
}
