//! USB mass storage driver: flash drives, card readers, external disks and
//! CD drives that use the bulk-only transport with SCSI commands.
//!
//! Every logical unit with a medium becomes a removable disk. Errors are
//! handled the way the specification asks: a stalled endpoint is cleared,
//! a failed command is followed by REQUEST SENSE (and retried once after a
//! "unit attention"), and a confused device gets a bulk-only reset.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use justdrv::usb::*;
use justdrv::{BlockDesc, DeviceInfo, IrqLock, Mutex, Status, BLOCK_READONLY, BLOCK_REMOVABLE, E_GONE, E_INVAL, E_IO, E_NODEV, E_STALL, OK};
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

justdrv::driver! {
    name: "usbstorage",
    version: "1.0.0",
    probe: probe,
    remove: remove,
}

const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;
const REQ_RESET: u8 = 0xFF;
const REQ_GET_MAX_LUN: u8 = 0xFE;

const TEST_UNIT_READY: u8 = 0x00;
const REQUEST_SENSE: u8 = 0x03;
const INQUIRY: u8 = 0x12;
const MODE_SENSE_6: u8 = 0x1A;
const READ_CAPACITY_10: u8 = 0x25;
const READ_10: u8 = 0x28;
const WRITE_10: u8 = 0x2A;
const SYNC_CACHE_10: u8 = 0x35;
const READ_16: u8 = 0x88;
const WRITE_16: u8 = 0x8A;
const SERVICE_ACTION_16: u8 = 0x9E;

const IO_TIMEOUT: u32 = 20_000;

/// One mass storage interface.
struct Msd {
    iface: &'static UsbInterface,
    bulk_in: u8,
    bulk_out: u8,
    /// One command at a time; holds the next tag.
    lock: Mutex<u32>,
    gone: AtomicBool,
    /// Calls from the kernel in progress.
    busy: AtomicU32,
}

/// A logical unit registered as a disk.
struct Lun {
    msd: &'static Msd,
    lun: u8,
    blocks: u64,
    block_size: u32,
}

#[derive(Debug)]
enum Fail {
    /// The command failed (CHECK CONDITION): ask REQUEST SENSE.
    Check,
    Error(Status),
}

impl Msd {
    fn transfer(&self, ep: u8, buf: &mut [u8], timeout: u32) -> Result<usize, Status> {
        // SAFETY: the interface of the device; the host returns E_GONE once
        // it is unplugged.
        let r = unsafe { (self.iface.transfer)(self.iface.ctx, ep, buf.as_mut_ptr(), buf.len() as u32, timeout) };
        if r < 0 {
            Err(r)
        } else {
            Ok(r as usize)
        }
    }

    fn clear_halt(&self, ep: u8) {
        // SAFETY: as in transfer.
        unsafe { (self.iface.clear_halt)(self.iface.ctx, ep) };
    }

    /// Bulk-only mass storage reset, then both endpoints cleared.
    fn reset_recovery(&self) {
        let setup = Setup::new(TYPE_CLASS | RECIP_INTERFACE, REQ_RESET, 0, self.iface.number as u16, 0);
        // SAFETY: as in transfer.
        unsafe { (self.iface.control)(self.iface.ctx, &setup, core::ptr::null_mut(), 2000) };
        self.clear_halt(self.bulk_in);
        self.clear_halt(self.bulk_out);
    }

    /// Runs one SCSI command; returns the bytes of data moved.
    fn command(&self, lun: u8, cb: &[u8], data: &mut [u8], input: bool, timeout: u32) -> Result<usize, Fail> {
        if self.gone.load(Ordering::Acquire) {
            return Err(Fail::Error(E_GONE));
        }
        let mut tag = self.lock.lock();
        *tag = tag.wrapping_add(1);
        let t = *tag;
        let mut cbw = [0u8; 31];
        cbw[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        cbw[4..8].copy_from_slice(&t.to_le_bytes());
        cbw[8..12].copy_from_slice(&(data.len() as u32).to_le_bytes());
        cbw[12] = if input { 0x80 } else { 0 };
        cbw[13] = lun;
        cbw[14] = cb.len() as u8;
        cbw[15..15 + cb.len()].copy_from_slice(cb);
        match self.transfer(self.bulk_out, &mut cbw, 5000) {
            Ok(31) => {}
            Ok(_) => return Err(Fail::Error(E_IO)),
            Err(E_GONE) => return Err(Fail::Error(E_GONE)),
            Err(e) => {
                self.reset_recovery();
                return Err(Fail::Error(e));
            }
        }
        let mut moved = 0;
        if !data.is_empty() {
            let ep = if input { self.bulk_in } else { self.bulk_out };
            match self.transfer(ep, data, timeout) {
                Ok(n) => moved = n,
                // A stalled data stage still ends with a status.
                Err(E_STALL) => self.clear_halt(ep),
                Err(e) => {
                    if e != E_GONE {
                        self.reset_recovery();
                    }
                    return Err(Fail::Error(e));
                }
            }
        }
        let mut csw = [0u8; 13];
        let mut got = self.transfer(self.bulk_in, &mut csw, 5000);
        if got == Err(E_STALL) {
            self.clear_halt(self.bulk_in);
            got = self.transfer(self.bulk_in, &mut csw, 5000);
        }
        let ok = matches!(got, Ok(13))
            && u32::from_le_bytes(csw[0..4].try_into().unwrap()) == CSW_SIGNATURE
            && u32::from_le_bytes(csw[4..8].try_into().unwrap()) == t;
        if !ok {
            if got != Err(E_GONE) {
                self.reset_recovery();
            }
            return Err(Fail::Error(if got == Err(E_GONE) { E_GONE } else { E_IO }));
        }
        match csw[12] {
            0 => Ok(moved),
            1 => Err(Fail::Check),
            _ => {
                // Phase error: the device lost track.
                self.reset_recovery();
                Err(Fail::Error(E_IO))
            }
        }
    }

    /// (sense key, additional sense code) of the last failure.
    fn sense(&self, lun: u8) -> (u8, u8) {
        let mut s = [0u8; 18];
        match self.command(lun, &[REQUEST_SENSE, 0, 0, 0, 18, 0], &mut s, true, 2000) {
            Ok(n) if n >= 13 => (s[2] & 0xF, s[12]),
            _ => (0xFF, 0),
        }
    }

    /// A command, retried once after a "unit attention" (medium changed,
    /// power-on reset), with the failure turned into a status.
    fn run(&self, lun: u8, cb: &[u8], data: &mut [u8], input: bool) -> Status {
        for attempt in 0..2 {
            match self.command(lun, cb, data, input, IO_TIMEOUT) {
                Ok(_) => return OK,
                Err(Fail::Check) => {
                    let (key, asc) = self.sense(lun);
                    if key == 6 && attempt == 0 {
                        continue;
                    }
                    justdrv::warn!("command {:#04x} failed: sense key {key:#x}, code {asc:#04x}", cb[0]);
                    return E_IO;
                }
                Err(Fail::Error(e)) => return e,
            }
        }
        E_IO
    }
}

impl Lun {
    fn io(&self, lba: u64, count: u32, buf: *mut u8, write: bool) -> Status {
        let m = self.msd;
        m.busy.fetch_add(1, Ordering::AcqRel);
        let st = if m.gone.load(Ordering::Acquire) {
            E_GONE
        } else if count == 0 || lba + count as u64 > self.blocks {
            E_INVAL
        } else {
            // SAFETY: the kernel passes a buffer of `count` blocks.
            let data = unsafe { core::slice::from_raw_parts_mut(buf, count as usize * self.block_size as usize) };
            let cb: Vec<u8> = if lba + count as u64 > u32::MAX as u64 {
                let mut c = alloc::vec![if write { WRITE_16 } else { READ_16 }, 0];
                c.extend_from_slice(&lba.to_be_bytes());
                c.extend_from_slice(&count.to_be_bytes());
                c.extend_from_slice(&[0, 0]);
                c
            } else {
                let mut c = alloc::vec![if write { WRITE_10 } else { READ_10 }, 0];
                c.extend_from_slice(&(lba as u32).to_be_bytes());
                c.push(0);
                c.extend_from_slice(&(count as u16).to_be_bytes());
                c.push(0);
                c
            };
            m.run(self.lun, &cb, data, !write)
        };
        m.busy.fetch_sub(1, Ordering::AcqRel);
        st
    }
}

unsafe extern "C" fn read(ctx: *mut c_void, lba: u64, count: u32, buf: *mut u8) -> Status {
    // SAFETY: ctx is the Lun registered with this descriptor (never freed).
    unsafe { &*(ctx as *const Lun) }.io(lba, count, buf, false)
}

unsafe extern "C" fn write(ctx: *mut c_void, lba: u64, count: u32, buf: *mut u8) -> Status {
    // SAFETY: as in read.
    unsafe { &*(ctx as *const Lun) }.io(lba, count, buf, true)
}

unsafe extern "C" fn flush(ctx: *mut c_void) -> Status {
    // SAFETY: as in read.
    let l = unsafe { &*(ctx as *const Lun) };
    let m = l.msd;
    if m.gone.load(Ordering::Acquire) {
        return E_GONE;
    }
    m.busy.fetch_add(1, Ordering::AcqRel);
    // Many flash drives do not know the command: that is fine.
    let _ = m.command(l.lun, &[SYNC_CACHE_10, 0, 0, 0, 0, 0, 0, 0, 0, 0], &mut [], false, IO_TIMEOUT);
    m.busy.fetch_sub(1, Ordering::AcqRel);
    OK
}

fn text(b: &[u8]) -> String {
    let s: String = b.iter().map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { ' ' }).collect();
    String::from(s.trim())
}

/// Brings up one logical unit; returns whether it became a disk.
fn start_lun(dev: u32, m: &'static Msd, lun: u8) -> bool {
    let mut inq = [0u8; 36];
    if m.command(lun, &[INQUIRY, 0, 0, 0, 36, 0], &mut inq, true, 5000).is_err() {
        return false;
    }
    let kind = inq[0] & 0x1F;
    if inq[0] >> 5 != 0 || !matches!(kind, 0x00 | 0x05 | 0x07 | 0x0E) {
        return false; // not a disk, CD or optical drive
    }
    let model = {
        let v = text(&inq[8..16]);
        let p = text(&inq[16..32]);
        if v.is_empty() || p.starts_with(v.as_str()) {
            p
        } else {
            alloc::format!("{v} {p}")
        }
    };
    // Media can take a few seconds to spin up or be recognised.
    let mut ready = false;
    for _ in 0..25 {
        match m.command(lun, &[TEST_UNIT_READY, 0, 0, 0, 0, 0], &mut [], false, 5000) {
            Ok(_) => {
                ready = true;
                break;
            }
            Err(Fail::Check) => {
                let (key, asc) = m.sense(lun);
                if key == 2 && asc == 0x3A {
                    justdrv::info!("{model}: no medium");
                    return false;
                }
            }
            Err(Fail::Error(_)) => return false,
        }
        justdrv::sleep_ms(200);
    }
    if !ready {
        justdrv::warn!("{model}: not ready");
        return false;
    }
    let mut cap = [0u8; 32];
    let (blocks, block_size) = match m.command(lun, &[READ_CAPACITY_10, 0, 0, 0, 0, 0, 0, 0, 0, 0], &mut cap[..8], true, 5000) {
        Ok(8) => {
            let last = u32::from_be_bytes(cap[0..4].try_into().unwrap());
            let size = u32::from_be_bytes(cap[4..8].try_into().unwrap());
            if last == u32::MAX {
                // More than 2 TiB: READ CAPACITY (16).
                let mut cb = [0u8; 16];
                cb[0] = SERVICE_ACTION_16;
                cb[1] = 0x10;
                cb[13] = 32;
                if m.command(lun, &cb, &mut cap, true, 5000).is_err() {
                    return false;
                }
                (u64::from_be_bytes(cap[0..8].try_into().unwrap()) + 1, u32::from_be_bytes(cap[8..12].try_into().unwrap()))
            } else {
                (last as u64 + 1, size)
            }
        }
        _ => {
            justdrv::warn!("{model}: capacity unknown");
            return false;
        }
    };
    if !(512..=4096).contains(&block_size) || !block_size.is_power_of_two() || blocks == 0 {
        justdrv::warn!("{model}: unsupported block size {block_size}");
        return false;
    }
    // Write protection: MODE SENSE header, byte 2 bit 7. Optional.
    let mut mode = [0u8; 192];
    let protected = match m.command(lun, &[MODE_SENSE_6, 0, 0x3F, 0, 192, 0], &mut mode, true, 5000) {
        Ok(n) if n >= 4 => mode[2] & 0x80 != 0,
        _ => false,
    };
    let readonly = protected || kind == 0x05 || kind == 0x07;
    let l: &'static Lun = Box::leak(Box::new(Lun { msd: m, lun, blocks, block_size }));
    let desc: &'static BlockDesc = Box::leak(Box::new(BlockDesc {
        ctx: l as *const Lun as *mut c_void,
        sector_size: block_size,
        flags: BLOCK_REMOVABLE | if readonly { BLOCK_READONLY } else { 0 },
        sector_count: blocks,
        read,
        write,
        flush: Some(flush),
        model: justdrv::model_name(&model),
    }));
    justdrv::block_register(dev, desc) >= 0
}

static DEVICES: IrqLock<Vec<(u32, usize)>> = IrqLock::new(Vec::new());

fn probe(dev: &DeviceInfo) -> Status {
    if dev.interface == 0 || dev.class != CLASS_STORAGE || dev.prog_if != 0x50 {
        return E_NODEV;
    }
    // SAFETY: the host controller driver keeps the interface alive while
    // the device exists.
    let iface: &'static UsbInterface = unsafe { &*(dev.interface as *const UsbInterface) };
    // SAFETY: as above.
    let config = unsafe { iface.config() };
    let bulk = |input: bool| endpoints(config, iface.number, 0).find(|e| e.kind() == EP_BULK && e.is_in() == input).map(|e| e.address);
    let (Some(bulk_in), Some(bulk_out)) = (bulk(true), bulk(false)) else { return E_NODEV };
    let m: &'static Msd =
        Box::leak(Box::new(Msd { iface, bulk_in, bulk_out, lock: Mutex::new(0), gone: AtomicBool::new(false), busy: AtomicU32::new(0) }));
    // Devices with one unit may stall this request.
    let mut max = [0u8; 1];
    let setup = Setup::new(DIR_IN | TYPE_CLASS | RECIP_INTERFACE, REQ_GET_MAX_LUN, 0, iface.number as u16, 1);
    // SAFETY: the interface's own control function with a 1-byte buffer.
    let luns = match unsafe { (iface.control)(iface.ctx, &setup, max.as_mut_ptr(), 1000) } {
        1 => (max[0] & 0x0F) + 1,
        _ => 1,
    };
    let mut disks = 0;
    for lun in 0..luns.min(8) {
        if start_lun(dev.id, m, lun) {
            disks += 1;
        }
    }
    DEVICES.lock().push((dev.id, m as *const Msd as usize));
    if disks == 0 {
        justdrv::info!("no usable medium ({} unit{})", luns, if luns == 1 { "" } else { "s" });
    }
    OK
}

fn remove(device: u32) {
    let ptr = {
        let mut d = DEVICES.lock();
        let Some(i) = d.iter().position(|(id, _)| *id == device) else { return };
        d.remove(i).1
    };
    // SAFETY: the Msd leaked in probe; it stays allocated (the kernel may
    // still hold the disk descriptors that point at it).
    let m = unsafe { &*(ptr as *const Msd) };
    m.gone.store(true, Ordering::Release);
    // Calls in progress fail quickly now; wait until they have left.
    while m.busy.load(Ordering::Acquire) != 0 {
        justdrv::sleep_ms(5);
    }
}
