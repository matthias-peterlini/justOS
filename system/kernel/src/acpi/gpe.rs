//! General-purpose events: the lines through which the chipset tells that
//! something happened (the embedded controller has news, a device was
//! plugged in...). Each has a status and an enable bit in the GPE blocks
//! the FADT names; the SCI fires while an enabled one is set (ACPI 6.5,
//! 4.8.4.1 and 5.6.4).

use crate::arch::cpu::{inb, outb};
use crate::sync::IrqLock;
use alloc::vec::Vec;

#[derive(Clone, Copy)]
struct Block {
    /// The status bytes; the enable bytes follow them.
    port: u16,
    bytes: u16,
    /// The number of its first GPE.
    base: u16,
}

struct Gpes {
    blocks: Vec<Block>,
    /// GPEs that fired and wait for the AML task (one bit each).
    pending: Vec<u8>,
}

static GPES: IrqLock<Gpes> = IrqLock::new(Gpes { blocks: Vec::new(), pending: Vec::new() });

impl Gpes {
    /// The status port, enable port and bit of a GPE.
    fn locate(&self, n: u16) -> Option<(u16, u16, u8)> {
        let b = self.blocks.iter().find(|b| n >= b.base && n < b.base + b.bytes * 8)?;
        let i = n - b.base;
        Some((b.port + i / 8, b.port + b.bytes + i / 8, 1 << (i % 8)))
    }
}

/// Takes the GPE blocks from the FADT; every GPE starts disabled.
pub fn init(gpe0: u32, len0: u8, gpe1: u32, len1: u8, base1: u8) {
    let mut g = GPES.lock();
    for (port, len, base) in [(gpe0, len0, 0u16), (gpe1, len1, base1 as u16)] {
        if port == 0 || port > 0xFFFF || len < 2 {
            continue;
        }
        let b = Block { port: port as u16, bytes: (len / 2) as u16, base };
        for i in 0..b.bytes {
            outb(b.port + b.bytes + i, 0);
            outb(b.port + i, 0xFF);
        }
        g.blocks.push(b);
    }
    let top = g.blocks.iter().map(|b| b.base + b.bytes * 8).max().unwrap_or(0);
    g.pending = alloc::vec![0; top.div_ceil(8) as usize];
}

/// How many GPE numbers there are (0 without blocks).
pub fn count() -> u16 {
    GPES.lock().blocks.iter().map(|b| b.base + b.bytes * 8).max().unwrap_or(0)
}

pub fn enable(n: u16) {
    let g = GPES.lock();
    if let Some((_, en, bit)) = g.locate(n) {
        outb(en, inb(en) | bit);
    }
}

/// Disables every GPE and clears its status (before sleeping).
pub fn disable_all() {
    let g = GPES.lock();
    for b in &g.blocks {
        for i in 0..b.bytes {
            outb(b.port + b.bytes + i, 0);
            outb(b.port + i, 0xFF);
        }
    }
}

/// Clears a GPE's status bit (writing 1 clears it).
pub fn clear(n: u16) {
    let g = GPES.lock();
    if let Some((st, _, bit)) = g.locate(n) {
        outb(st, bit);
    }
}

/// In the SCI handler: disables the enabled GPEs that fired, so the SCI
/// stops, and keeps them for the AML task. True if there were any.
pub fn sci() -> bool {
    let mut g = GPES.lock();
    let mut any = false;
    for bi in 0..g.blocks.len() {
        let b = g.blocks[bi];
        for i in 0..b.bytes {
            let en = inb(b.port + b.bytes + i);
            let fired = inb(b.port + i) & en;
            if fired == 0 {
                continue;
            }
            outb(b.port + b.bytes + i, en & !fired);
            for bit in 0..8 {
                if fired & (1 << bit) != 0 {
                    let n = (b.base + i * 8 + bit) as usize;
                    if let Some(p) = g.pending.get_mut(n / 8) {
                        *p |= 1 << (n % 8);
                    }
                }
            }
            any = true;
        }
    }
    any
}

/// The GPEs that fired since the last call.
pub fn take_pending() -> Vec<u16> {
    let mut g = GPES.lock();
    let mut out = Vec::new();
    for (i, p) in g.pending.iter_mut().enumerate() {
        for bit in 0..8 {
            if *p & (1 << bit) != 0 {
                out.push((i * 8 + bit) as u16);
            }
        }
        *p = 0;
    }
    out
}
