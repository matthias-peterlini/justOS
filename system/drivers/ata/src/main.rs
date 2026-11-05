//! ATA/IDE disk driver (PIO).
//!
//! Drives every hard disk on the two channels of an IDE controller, in
//! compatibility (legacy ports) or native (PCI BARs) mode, with 28- and
//! 48-bit addressing. Transfers use programmed I/O and polling, which is
//! slow but works on every controller and in every emulator.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use justdrv::{port, BlockDesc, DeviceInfo, Mutex, Status, E_INVAL, E_IO, E_NODEV, E_TIMEOUT, OK, RES_IO};
use core::ffi::c_void;

justdrv::driver! {
    name: "ata",
    version: "1.1.0",
    probe: probe,
}

// Register offsets from the command block base.
const DATA: u16 = 0;
const ERROR: u16 = 1;
const COUNT: u16 = 2;
const LBA0: u16 = 3;
const LBA1: u16 = 4;
const LBA2: u16 = 5;
const DRIVE: u16 = 6;
const STATUS: u16 = 7;
const COMMAND: u16 = 7;

const BSY: u8 = 0x80;
const DF: u8 = 0x20;
const DRQ: u8 = 0x08;
const ERR: u8 = 0x01;

const CMD_READ: u8 = 0x20;
const CMD_READ_EXT: u8 = 0x24;
const CMD_WRITE: u8 = 0x30;
const CMD_WRITE_EXT: u8 = 0x34;
const CMD_FLUSH: u8 = 0xE7;
const CMD_FLUSH_EXT: u8 = 0xEA;
const CMD_IDENTIFY: u8 = 0xEC;
const CMD_STANDBY_IMMEDIATE: u8 = 0xE0;

struct Channel {
    base: u16,
    ctrl: u16,
}

struct Disk {
    channel: &'static Mutex<Channel>,
    slave: bool,
    lba48: bool,
    sectors: u64,
}

impl Channel {
    fn status(&self) -> u8 {
        port::inb(self.base + STATUS)
    }

    /// ~400 ns: four reads of the alternate status register.
    fn delay(&self) {
        for _ in 0..4 {
            port::inb(self.ctrl);
        }
    }

    fn wait_not_busy(&self, timeout_ms: u64) -> Result<u8, Status> {
        let mut status = 0;
        if justdrv::wait_until(timeout_ms, || {
            status = self.status();
            status & BSY == 0
        }) {
            Ok(status)
        } else {
            Err(E_TIMEOUT)
        }
    }

    /// Waits until the drive has data for us (or wants data).
    fn wait_drq(&self) -> Result<(), Status> {
        let status = self.wait_not_busy(5000)?;
        if status & (ERR | DF) != 0 {
            return Err(E_IO);
        }
        if status & DRQ == 0 {
            let mut s = 0;
            if !justdrv::wait_until(1000, || {
                s = self.status();
                s & (DRQ | ERR | DF) != 0
            }) {
                return Err(E_TIMEOUT);
            }
            if s & (ERR | DF) != 0 {
                return Err(E_IO);
            }
        }
        Ok(())
    }

    fn select(&self, slave: bool, high_bits: u8) {
        port::outb(self.base + DRIVE, 0xE0 | (slave as u8) << 4 | (high_bits & 0x0F));
        self.delay();
    }

    fn setup(&self, slave: bool, lba: u64, count: u32, lba48: bool) {
        if lba48 {
            self.select(slave, 0);
            port::outb(self.base + COUNT, (count >> 8) as u8);
            port::outb(self.base + LBA0, (lba >> 24) as u8);
            port::outb(self.base + LBA1, (lba >> 32) as u8);
            port::outb(self.base + LBA2, (lba >> 40) as u8);
            port::outb(self.base + COUNT, count as u8);
            port::outb(self.base + LBA0, lba as u8);
            port::outb(self.base + LBA1, (lba >> 8) as u8);
            port::outb(self.base + LBA2, (lba >> 16) as u8);
        } else {
            self.select(slave, (lba >> 24) as u8);
            port::outb(self.base + COUNT, count as u8);
            port::outb(self.base + LBA0, lba as u8);
            port::outb(self.base + LBA1, (lba >> 8) as u8);
            port::outb(self.base + LBA2, (lba >> 16) as u8);
        }
    }

    /// IDENTIFY DEVICE: 256 words, or None for no (or a non-ATA) drive.
    fn identify(&self, slave: bool) -> Option<[u16; 256]> {
        self.select(slave, 0);
        port::outb(self.base + COUNT, 0);
        port::outb(self.base + LBA0, 0);
        port::outb(self.base + LBA1, 0);
        port::outb(self.base + LBA2, 0);
        port::outb(self.base + COMMAND, CMD_IDENTIFY);
        self.delay();
        if self.status() == 0 {
            return None; // no drive
        }
        self.wait_not_busy(2000).ok()?;
        // ATAPI and SATA signatures: not an ATA disk.
        if port::inb(self.base + LBA1) != 0 || port::inb(self.base + LBA2) != 0 {
            return None;
        }
        self.wait_drq().ok()?;
        let mut words = [0u16; 256];
        port::insw(self.base + DATA, &mut words);
        Some(words)
    }
}

impl Disk {
    fn transfer(&self, lba: u64, count: u32, buf: *mut u8, write: bool) -> Status {
        if count == 0 || lba + count as u64 > self.sectors {
            return E_INVAL;
        }
        let ch = self.channel.lock();
        let mut done = 0u32;
        while done < count {
            let lba48 = self.lba48 && (lba + done as u64 + 256 > 1 << 28 || count - done > 256);
            let n = (count - done).min(if lba48 { 65536 } else { 256 });
            if let Err(e) = ch.wait_not_busy(5000) {
                return e;
            }
            ch.setup(self.slave, lba + done as u64, n & 0xFFFF, lba48);
            let cmd = match (write, lba48) {
                (false, false) => CMD_READ,
                (false, true) => CMD_READ_EXT,
                (true, false) => CMD_WRITE,
                (true, true) => CMD_WRITE_EXT,
            };
            port::outb(ch.base + COMMAND, cmd);
            for i in 0..n {
                if let Err(e) = ch.wait_drq() {
                    justdrv::warn!(
                        "{} sector {}: error {:#x}",
                        if write { "write" } else { "read" },
                        lba + (done + i) as u64,
                        port::inb(ch.base + ERROR)
                    );
                    return e;
                }
                // SAFETY: the kernel passes a buffer of `count` sectors.
                let words = unsafe { core::slice::from_raw_parts_mut(buf.add((done + i) as usize * 512) as *mut u16, 256) };
                if write {
                    port::outsw(ch.base + DATA, words);
                } else {
                    port::insw(ch.base + DATA, words);
                }
                ch.delay();
            }
            done += n;
        }
        if write {
            if let Err(e) = ch.wait_not_busy(5000) {
                return e;
            }
        }
        OK
    }

    fn flush(&self) -> Status {
        let ch = self.channel.lock();
        self.flush_locked(&ch)
    }

    fn flush_locked(&self, ch: &Channel) -> Status {
        ch.select(self.slave, 0);
        port::outb(ch.base + COMMAND, if self.lba48 { CMD_FLUSH_EXT } else { CMD_FLUSH });
        match ch.wait_not_busy(30_000) {
            Ok(s) if s & ERR == 0 => OK,
            Ok(_) => E_IO,
            Err(e) => e,
        }
    }
}

/// The channels and disks of one controller, for sleep.
struct Controller {
    channels: Vec<&'static Mutex<Channel>>,
    disks: Vec<&'static Disk>,
}

/// Before sleep: the disks write out their caches and park their heads.
unsafe extern "C" fn suspend(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the Controller registered with the hooks.
    let c = unsafe { &*(ctx as *const Controller) };
    for d in &c.disks {
        let ch = d.channel.lock();
        let st = d.flush_locked(&ch);
        if st != OK {
            return st;
        }
        ch.select(d.slave, 0);
        port::outb(ch.base + COMMAND, CMD_STANDBY_IMMEDIATE);
        let _ = ch.wait_not_busy(10_000);
    }
    OK
}

/// After sleep: each channel is reset and waits for its disks to spin up;
/// interrupts stay off (we poll).
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: as in suspend.
    let c = unsafe { &*(ctx as *const Controller) };
    let mut status = OK;
    for ch in &c.channels {
        let ch = ch.lock();
        port::outb(ch.ctrl, 0x06); // software reset, interrupts off
        justdrv::sleep_ms(1);
        port::outb(ch.ctrl, 0x02);
        justdrv::sleep_ms(2);
        if ch.status() != 0xFF && ch.wait_not_busy(31_000).is_err() {
            status = E_TIMEOUT;
        }
    }
    status
}

unsafe extern "C" fn read(ctx: *mut c_void, lba: u64, count: u32, buf: *mut u8) -> Status {
    // SAFETY: ctx is the Disk registered with this descriptor.
    unsafe { &*(ctx as *const Disk) }.transfer(lba, count, buf, false)
}

unsafe extern "C" fn write(ctx: *mut c_void, lba: u64, count: u32, buf: *mut u8) -> Status {
    // SAFETY: as in read.
    unsafe { &*(ctx as *const Disk) }.transfer(lba, count, buf, true)
}

unsafe extern "C" fn flush(ctx: *mut c_void) -> Status {
    // SAFETY: as in read.
    unsafe { &*(ctx as *const Disk) }.flush()
}

fn model(words: &[u16; 256]) -> String {
    let mut s = String::new();
    for w in &words[27..47] {
        for b in [(w >> 8) as u8, *w as u8] {
            if b.is_ascii_graphic() || b == b' ' {
                s.push(b as char);
            }
        }
    }
    String::from(s.trim())
}

fn probe(dev: &DeviceInfo) -> Status {
    if dev.class != 0x01 || dev.subclass != 0x01 {
        return E_NODEV;
    }
    let io = |i: usize| (dev.resources[i].kind == RES_IO && dev.resources[i].base != 0).then_some(dev.resources[i].base as u16);
    // prog-if bit 0/2: channel in native mode, ports in the BARs.
    let primary = if dev.prog_if & 1 != 0 { (io(0), io(1).map(|p| p + 2)) } else { (Some(0x1F0), Some(0x3F6)) };
    let secondary = if dev.prog_if & 4 != 0 { (io(2), io(3).map(|p| p + 2)) } else { (Some(0x170), Some(0x376)) };
    justdrv::Pci(dev.id).enable(true, false, false);

    let mut found = 0;
    let mut ctl = Controller { channels: Vec::new(), disks: Vec::new() };
    for (base, ctrl) in [primary, secondary] {
        let (Some(base), Some(ctrl)) = (base, ctrl) else { continue };
        let channel = Channel { base, ctrl };
        if channel.status() == 0xFF {
            continue; // floating bus: nothing attached
        }
        port::outb(ctrl, 0x02); // no interrupts: we poll
        let channel: &'static Mutex<Channel> = Box::leak(Box::new(Mutex::new(channel)));
        ctl.channels.push(channel);
        for slave in [false, true] {
            let Some(id) = channel.lock().identify(slave) else { continue };
            let lba48 = id[83] & (1 << 10) != 0;
            let sectors = if lba48 {
                id[100] as u64 | (id[101] as u64) << 16 | (id[102] as u64) << 32 | (id[103] as u64) << 48
            } else {
                id[60] as u64 | (id[61] as u64) << 16
            };
            if sectors == 0 {
                continue;
            }
            let disk: &'static Disk = Box::leak(Box::new(Disk { channel, slave, lba48, sectors }));
            let desc: &'static BlockDesc = Box::leak(Box::new(BlockDesc {
                ctx: disk as *const Disk as *mut c_void,
                sector_size: 512,
                flags: 0,
                sector_count: sectors,
                read,
                write,
                flush: Some(flush),
                model: justdrv::model_name(&model(&id)),
            }));
            if justdrv::block_register(dev.id, desc) >= 0 {
                found += 1;
                ctl.disks.push(disk);
            }
        }
    }
    if found == 0 {
        // A controller with only CD drives or nothing: still ours.
        justdrv::info!("no ATA disks on this controller");
    }
    let ctl: &'static Controller = Box::leak(Box::new(ctl));
    justdrv::power_hooks(dev.id, ctl as *const Controller as *mut c_void, Some(suspend), resume);
    OK
}
