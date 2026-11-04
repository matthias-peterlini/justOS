//! The two cascaded 8259 interrupt controllers, remapped to vectors 32..47.

use super::cpu::{inb, io_wait, outb};
use crate::sync::IrqLock;

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

static MASK: IrqLock<u16> = IrqLock::new(0xFFFF);

pub fn init() {
    program();
    let mut mask = MASK.lock();
    *mask = !(1 << 2); // everything masked but the cascade
    apply(*mask);
}

/// Which lines are level-triggered (the chipset's ELCR), kept across sleep.
static ELCR: IrqLock<Option<u16>> = IrqLock::new(None);
/// Lines that are always edge-triggered (timer, keyboard, cascade, clock,
/// FPU): never set in the ELCR.
const ELCR_EDGE_ONLY: u16 = 1 << 0 | 1 << 1 | 1 << 2 | 1 << 8 | 1 << 13;

/// Before sleep: remembers the trigger mode of each line.
pub fn suspend() {
    let v = inb(0x4D0) as u16 | (inb(0x4D1) as u16) << 8;
    // All ones: no ELCR (nothing answers at these ports).
    *ELCR.lock() = (v != 0xFFFF).then_some(v & !ELCR_EDGE_ONLY);
}

/// After sleep: the controllers start from scratch; the lines in use are
/// unmasked again, with their trigger modes.
pub fn resume() {
    if let Some(v) = *ELCR.lock() {
        outb(0x4D0, v as u8);
        outb(0x4D1, (v >> 8) as u8);
    }
    program();
    apply(*MASK.lock());
}

fn program() {
    outb(PIC1_CMD, 0x11); // ICW1: init, ICW4 follows
    io_wait();
    outb(PIC2_CMD, 0x11);
    io_wait();
    outb(PIC1_DATA, 32); // ICW2: vector offsets
    io_wait();
    outb(PIC2_DATA, 40);
    io_wait();
    outb(PIC1_DATA, 4); // ICW3: slave on IRQ 2
    io_wait();
    outb(PIC2_DATA, 2);
    io_wait();
    outb(PIC1_DATA, 1); // ICW4: 8086 mode
    io_wait();
    outb(PIC2_DATA, 1);
    io_wait();
}

fn apply(mask: u16) {
    outb(PIC1_DATA, mask as u8);
    outb(PIC2_DATA, (mask >> 8) as u8);
}

pub fn unmask(irq: u8) {
    let mut mask = MASK.lock();
    *mask &= !(1 << irq);
    apply(*mask);
}

pub fn mask(irq: u8) {
    if irq == 2 {
        return;
    }
    let mut mask = MASK.lock();
    *mask |= 1 << irq;
    apply(*mask);
}

pub fn eoi(irq: u8) {
    if irq >= 8 {
        outb(PIC2_CMD, 0x20);
    }
    outb(PIC1_CMD, 0x20);
}

/// IRQ 7 and 15 can fire spuriously; the in-service register tells.
pub fn is_spurious(irq: u8) -> bool {
    match irq {
        7 => {
            outb(PIC1_CMD, 0x0B);
            inb(PIC1_CMD) & 0x80 == 0
        }
        15 => {
            outb(PIC2_CMD, 0x0B);
            if inb(PIC2_CMD) & 0x80 == 0 {
                outb(PIC1_CMD, 0x20); // the master still saw the cascade
                true
            } else {
                false
            }
        }
        _ => false,
    }
}
