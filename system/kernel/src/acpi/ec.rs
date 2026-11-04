//! The embedded controller: the small processor of laptops that runs the
//! battery, the lid, the fans and the hot keys. The firmware's AML reads
//! and writes its registers (address space 3) through two I/O ports, and
//! the controller signals events with a GPE, answered by `_Qxx` methods
//! (ACPI 6.5, chapter 12).

use crate::arch::cpu::{inb, io_wait, outb};
use core::sync::atomic::{AtomicU16, AtomicU32, Ordering};

/// Status register bits.
const OBF: u8 = 1 << 0;
const IBF: u8 = 1 << 1;
/// An event is waiting to be queried.
pub const SCI_EVT: u8 = 1 << 5;

const RD_EC: u8 = 0x80;
const WR_EC: u8 = 0x81;
const QR_EC: u8 = 0x84;

static DATA: AtomicU16 = AtomicU16::new(0);
static COMMAND: AtomicU16 = AtomicU16::new(0);
/// Transfers that timed out (a stuck controller is not waited on forever).
static TIMEOUTS: AtomicU32 = AtomicU32::new(0);

pub fn set_ports(data: u16, command: u16) {
    DATA.store(data, Ordering::Relaxed);
    COMMAND.store(command, Ordering::Relaxed);
}

pub fn present() -> bool {
    COMMAND.load(Ordering::Relaxed) != 0
}

pub fn ports() -> (u16, u16) {
    (DATA.load(Ordering::Relaxed), COMMAND.load(Ordering::Relaxed))
}

pub fn status() -> u8 {
    inb(COMMAND.load(Ordering::Relaxed))
}

/// Waits until the status has (or has not) a bit: busy for a moment, then
/// giving the processor away, for at most 100 ms.
fn wait(bit: u8, set: bool) -> bool {
    for _ in 0..2000 {
        if (status() & bit != 0) == set {
            return true;
        }
        io_wait();
    }
    let until = crate::time::uptime_ms() + 100;
    while crate::time::uptime_ms() < until {
        if (status() & bit != 0) == set {
            return true;
        }
        crate::task::yield_now();
    }
    TIMEOUTS.fetch_add(1, Ordering::Relaxed);
    false
}

fn command(c: u8) -> bool {
    if !wait(IBF, false) {
        return false;
    }
    outb(COMMAND.load(Ordering::Relaxed), c);
    true
}

fn write_data(v: u8) -> bool {
    if !wait(IBF, false) {
        return false;
    }
    outb(DATA.load(Ordering::Relaxed), v);
    true
}

fn read_data() -> Option<u8> {
    wait(OBF, true).then(|| inb(DATA.load(Ordering::Relaxed)))
}

/// Reads a register of the controller.
pub fn read(addr: u8) -> Option<u8> {
    if !present() {
        return None;
    }
    // A stale byte in the output buffer would be taken for the answer.
    if status() & OBF != 0 {
        inb(DATA.load(Ordering::Relaxed));
    }
    if !command(RD_EC) || !write_data(addr) {
        return None;
    }
    read_data()
}

pub fn write(addr: u8, v: u8) -> bool {
    present() && command(WR_EC) && write_data(addr) && write_data(v) && wait(IBF, false)
}

/// Asks which event is waiting: the number of the `_Qxx` method to run,
/// or None when there is none.
pub fn query() -> Option<u8> {
    if !present() || !command(QR_EC) {
        return None;
    }
    read_data().filter(|&q| q != 0)
}

pub fn timeouts() -> u32 {
    TIMEOUTS.load(Ordering::Relaxed)
}
