//! AHCI (Serial ATA) disk driver.
//!
//! One command slot per port, polled completion, DMA through a bounce buffer
//! (the kernel's buffers are not physically contiguous). Before sleep the
//! disks are flushed and put in standby; after it the controller and every
//! port are set up again.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use arcdrv::{BlockDesc, DeviceInfo, Dma, Mmio, Mutex, Status, E_INVAL, E_IO, E_NODEV, E_NOMEM, E_TIMEOUT, OK, RES_MEMORY};
use core::ffi::c_void;

arcdrv::driver! {
    name: "ahci",
    version: "1.1.0",
    probe: probe,
}

// HBA registers
const CAP: usize = 0x00;
const GHC: usize = 0x04;
const PI: usize = 0x0C;
const CAP2: usize = 0x24;
const BOHC: usize = 0x28;

// Port registers (offset from 0x100 + port * 0x80)
const P_CLB: usize = 0x00;
const P_FB: usize = 0x08;
const P_IS: usize = 0x10;
const P_IE: usize = 0x14;
const P_CMD: usize = 0x18;
const P_TFD: usize = 0x20;
const P_SIG: usize = 0x24;
const P_SSTS: usize = 0x28;
const P_SCTL: usize = 0x2C;
const P_SERR: usize = 0x30;
const P_CI: usize = 0x38;

const CMD_ST: u32 = 1;
const CMD_SUD: u32 = 1 << 1;
const CMD_POD: u32 = 1 << 2;
const CMD_FRE: u32 = 1 << 4;
const CMD_FR: u32 = 1 << 14;
const CMD_CR: u32 = 1 << 15;

const SIG_ATA: u32 = 0x0000_0101;

const ATA_READ_DMA_EXT: u8 = 0x25;
const ATA_WRITE_DMA_EXT: u8 = 0x35;
const ATA_FLUSH_EXT: u8 = 0xEA;
const ATA_IDENTIFY: u8 = 0xEC;
const ATA_STANDBY_IMMEDIATE: u8 = 0xE0;

/// Sectors per transfer through the bounce buffer (the kernel never asks
/// for more than 128 at a time).
const MAX_SECTORS: u32 = 128;

/// Memory layout of the per-port DMA page.
const CMD_LIST: usize = 0; // 32 headers x 32 bytes
const FIS_AREA: usize = 1024; // 256 bytes
const CMD_TABLE: usize = 2048; // 128-byte header + PRDT

struct Port {
    hba: Mmio,
    base: usize,
    mem: Dma,
    bounce: Dma,
}

struct Disk {
    port: Mutex<Port>,
    sectors: u64,
}

impl Port {
    fn read(&self, reg: usize) -> u32 {
        self.hba.read32(self.base + reg)
    }

    fn write(&self, reg: usize, v: u32) {
        self.hba.write32(self.base + reg, v)
    }

    fn stop(&self) -> bool {
        self.write(P_CMD, self.read(P_CMD) & !CMD_ST);
        let ok = arcdrv::wait_until(500, || self.read(P_CMD) & CMD_CR == 0);
        self.write(P_CMD, self.read(P_CMD) & !CMD_FRE);
        ok && arcdrv::wait_until(500, || self.read(P_CMD) & CMD_FR == 0)
    }

    fn start(&self) {
        arcdrv::wait_until(500, || self.read(P_CMD) & CMD_CR == 0);
        self.write(P_CMD, self.read(P_CMD) | CMD_FRE);
        self.write(P_CMD, self.read(P_CMD) | CMD_ST);
    }

    /// Stops the port and gives it its command list and FIS area.
    fn setup(&self) -> bool {
        if !self.stop() {
            return false;
        }
        let phys = self.mem.phys();
        self.write(P_CLB, phys as u32);
        self.write(P_CLB + 4, (phys >> 32) as u32);
        self.write(P_FB, (phys + FIS_AREA as u64) as u32);
        self.write(P_FB + 4, ((phys + FIS_AREA as u64) >> 32) as u32);
        self.write(P_SERR, u32::MAX);
        self.write(P_IS, u32::MAX);
        self.write(P_IE, 0);
        self.start();
        true
    }

    /// After sleep: the device powered and spun up, the link up (with a
    /// link reset if it does not come by itself), the port set up again,
    /// and the disk ready.
    fn wake(&self) -> Status {
        self.write(P_CMD, self.read(P_CMD) | CMD_POD | CMD_SUD);
        let link = || self.read(P_SSTS) & 0xF == 3;
        if !arcdrv::wait_until(1000, link) {
            self.write(P_SCTL, (self.read(P_SCTL) & !0xF) | 1);
            arcdrv::sleep_ms(2);
            self.write(P_SCTL, self.read(P_SCTL) & !0xF);
            if !arcdrv::wait_until(1000, link) {
                return E_IO;
            }
        }
        if !self.setup() {
            return E_IO;
        }
        // A hard disk takes seconds to spin up.
        if arcdrv::wait_until(15_000, || self.read(P_TFD) & 0x88 == 0) {
            OK
        } else {
            E_TIMEOUT
        }
    }

    /// Runs one ATA command on slot 0. `bytes` of data move through the
    /// bounce buffer; `write` is the direction.
    fn command(&self, cmd: u8, lba: u64, count: u32, bytes: usize, write: bool) -> Status {
        if !arcdrv::wait_until(1000, || self.read(P_TFD) & 0x88 == 0) {
            return E_TIMEOUT;
        }
        let m = &self.mem;
        // Command header 0: FIS length 5 dwords, direction, one PRD.
        let flags: u32 = 5 | if write { 1 << 6 } else { 0 } | (if bytes > 0 { 1u32 } else { 0 }) << 16;
        m.write::<u32>(CMD_LIST, flags);
        m.write::<u32>(CMD_LIST + 4, 0);
        m.write::<u64>(CMD_LIST + 8, m.phys() + CMD_TABLE as u64);
        // Command FIS: register, host to device.
        let mut fis = [0u8; 20];
        fis[0] = 0x27;
        fis[1] = 0x80;
        fis[2] = cmd;
        fis[4] = lba as u8;
        fis[5] = (lba >> 8) as u8;
        fis[6] = (lba >> 16) as u8;
        fis[7] = 1 << 6; // LBA mode
        fis[8] = (lba >> 24) as u8;
        fis[9] = (lba >> 32) as u8;
        fis[10] = (lba >> 40) as u8;
        fis[12] = count as u8;
        fis[13] = (count >> 8) as u8;
        for (i, b) in fis.iter().enumerate() {
            m.write::<u8>(CMD_TABLE + i, *b);
        }
        if bytes > 0 {
            m.write::<u64>(CMD_TABLE + 0x80, self.bounce.phys());
            m.write::<u32>(CMD_TABLE + 0x88, 0);
            m.write::<u32>(CMD_TABLE + 0x8C, (bytes - 1) as u32);
        }
        self.write(P_IS, u32::MAX);
        self.write(P_CI, 1);
        let mut failed = false;
        let done = arcdrv::wait_until(10_000, || {
            if self.read(P_IS) & (1 << 30) != 0 {
                failed = true; // task file error
                return true;
            }
            self.read(P_CI) & 1 == 0
        });
        if failed || self.read(P_TFD) & 1 != 0 {
            arcdrv::warn!("command {cmd:#x} failed (TFD {:#x})", self.read(P_TFD));
            // Recover the port for the next command.
            self.stop();
            self.write(P_SERR, u32::MAX);
            self.write(P_IS, u32::MAX);
            self.start();
            return E_IO;
        }
        if !done {
            return E_TIMEOUT;
        }
        OK
    }
}

impl Disk {
    fn transfer(&self, lba: u64, count: u32, buf: *mut u8, write: bool) -> Status {
        if count == 0 || lba + count as u64 > self.sectors {
            return E_INVAL;
        }
        let port = self.port.lock();
        let mut done = 0;
        while done < count {
            let n = (count - done).min(MAX_SECTORS);
            let bytes = n as usize * 512;
            // SAFETY: the kernel passes a buffer of `count` sectors.
            let chunk = unsafe { core::slice::from_raw_parts_mut(buf.add(done as usize * 512), bytes) };
            if write {
                port.bounce.as_mut_slice()[..bytes].copy_from_slice(chunk);
            }
            let st = port.command(if write { ATA_WRITE_DMA_EXT } else { ATA_READ_DMA_EXT }, lba + done as u64, n, bytes, write);
            if st != OK {
                return st;
            }
            if !write {
                chunk.copy_from_slice(&port.bounce.as_slice()[..bytes]);
            }
            done += n;
        }
        OK
    }
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
    let disk = unsafe { &*(ctx as *const Disk) };
    disk.port.lock().command(ATA_FLUSH_EXT, 0, 0, 0, false)
}

fn model(id: &[u8]) -> String {
    let mut s = String::new();
    for pair in id[54..94].chunks(2) {
        for &b in [pair[1], pair[0]].iter() {
            if b.is_ascii_graphic() || b == b' ' {
                s.push(b as char);
            }
        }
    }
    String::from(s.trim())
}

/// The controller and its disks, for sleep.
struct Hba {
    regs: Mmio,
    disks: Vec<&'static Disk>,
}

/// Takes the controller from the firmware and switches it to AHCI mode,
/// with its interrupt off (completion is polled).
fn take_hba(hba: &Mmio) {
    if hba.read32(CAP2) & 1 != 0 {
        hba.write32(BOHC, hba.read32(BOHC) | 2);
        arcdrv::wait_until(100, || hba.read32(BOHC) & 1 == 0);
    }
    hba.write32(GHC, hba.read32(GHC) | (1 << 31));
    hba.write32(GHC, hba.read32(GHC) & !2);
}

/// Before sleep: everything written reaches the platters, the heads park,
/// the ports stop.
unsafe extern "C" fn suspend(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Hba.
    let hba = unsafe { &*(ctx as *const Hba) };
    // First every flush, so a disk that refuses leaves all of them running.
    for d in &hba.disks {
        let st = d.port.lock().command(ATA_FLUSH_EXT, 0, 0, 0, false);
        if st != OK {
            return st;
        }
    }
    for d in &hba.disks {
        let port = d.port.lock();
        port.command(ATA_STANDBY_IMMEDIATE, 0, 0, 0, false);
        port.stop();
    }
    OK
}

/// After sleep the controller is back in its power-on state.
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Hba.
    let hba = unsafe { &*(ctx as *const Hba) };
    take_hba(&hba.regs);
    let mut result = OK;
    for d in &hba.disks {
        let st = d.port.lock().wake();
        if st != OK {
            result = st;
        }
    }
    result
}

fn setup_port(dev: u32, hba: Mmio, index: usize) -> Result<&'static Disk, Status> {
    let base = 0x100 + index * 0x80;
    let (Some(mem), Some(bounce)) = (Dma::new(4096), Dma::new(MAX_SECTORS as usize * 512)) else { return Err(E_NOMEM) };
    let port = Port { hba, base, mem, bounce };
    if !port.setup() {
        arcdrv::warn!("port {index} does not stop");
        return Err(E_IO);
    }

    let st = port.command(ATA_IDENTIFY, 0, 0, 512, false);
    if st != OK {
        return Err(st);
    }
    let id = &port.bounce.as_slice()[..512];
    let word = |i: usize| u16::from_le_bytes([id[i * 2], id[i * 2 + 1]]) as u64;
    let sectors =
        if word(83) & (1 << 10) != 0 { word(100) | word(101) << 16 | word(102) << 32 | word(103) << 48 } else { word(60) | word(61) << 16 };
    let name = model(id);
    if sectors == 0 {
        return Err(E_IO);
    }
    let disk: &'static Disk = Box::leak(Box::new(Disk { port: Mutex::new(port), sectors }));
    let desc: &'static BlockDesc = Box::leak(Box::new(BlockDesc {
        ctx: disk as *const Disk as *mut c_void,
        sector_size: 512,
        flags: 0,
        sector_count: sectors,
        read,
        write,
        flush: Some(flush),
        model: arcdrv::model_name(&name),
    }));
    let n = arcdrv::block_register(dev, desc);
    if n < 0 {
        Err(n)
    } else {
        Ok(disk)
    }
}

fn probe(dev: &DeviceInfo) -> Status {
    if dev.class != 0x01 || dev.subclass != 0x06 || dev.prog_if != 0x01 {
        return E_NODEV;
    }
    let bar = dev.resources[5];
    if bar.kind != RES_MEMORY || bar.base == 0 {
        arcdrv::warn!("no AHCI register window (BAR5)");
        return E_IO;
    }
    let pci = arcdrv::Pci(dev.id);
    pci.enable(false, true, true);
    let Some(hba) = Mmio::map(bar.base, (bar.size as usize).max(0x1100)) else { return E_NOMEM };

    take_hba(&hba);

    let ports = hba.read32(PI);
    let slots = (hba.read32(CAP) >> 8 & 0x1F) + 1;
    arcdrv::debug!("{} ports implemented, {slots} command slots", ports.count_ones());
    let mut disks = Vec::new();
    for i in 0..32 {
        if ports & (1 << i) == 0 {
            continue;
        }
        let base = 0x100 + i * 0x80;
        let ssts = hba.read32(base + P_SSTS);
        if ssts & 0xF != 3 || (ssts >> 8) & 0xF != 1 {
            continue; // no device, or not active
        }
        if hba.read32(base + P_SIG) != SIG_ATA {
            continue; // ATAPI, port multiplier, enclosure...
        }
        match setup_port(dev.id, hba, i) {
            Ok(d) => disks.push(d),
            Err(e) => arcdrv::warn!("port {i}: {}", arcdrv::status_name(e)),
        }
    }
    if disks.is_empty() {
        arcdrv::info!("no SATA disks attached");
    }
    let ctl: &'static Hba = Box::leak(Box::new(Hba { regs: hba, disks }));
    arcdrv::power_hooks(dev.id, ctl as *const Hba as *mut c_void, Some(suspend), resume);
    OK
}
