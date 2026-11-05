//! SD host controller driver: SD, SDHC/SDXC and MMC/eMMC cards in the card
//! readers that follow the SD Host Controller standard.
//!
//! Every slot of a reader gets a thread that watches for cards. A card
//! that goes in is identified, switched to a 4-bit bus at 25 MHz and handed
//! to the kernel as a disk of its own device (bus `BUS_SD`); when it comes
//! out, the device goes away with its volumes. Data moves by DMA through a
//! 64 KiB bounce buffer (ADMA2, or SDMA on older controllers), or through
//! the controller's data port on the few without DMA.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use justdrv::{
    BlockDesc, DeviceInfo, Mmio, Mutex, Pci, Status, BLOCK_READONLY, BLOCK_REMOVABLE, BUS_SD, DEVICE_OWNED, E_GONE, E_IO, E_NODEV,
    E_TIMEOUT, OK, RES_MEMORY,
};
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

justdrv::driver! {
    name: "sdhci",
    version: "1.1.0",
    probe: probe,
}

const BLOCK_SIZE: usize = 0x04;
const BLOCK_COUNT: usize = 0x06;
const ARGUMENT: usize = 0x08;
const TRANSFER_MODE: usize = 0x0C;
const COMMAND: usize = 0x0E;
const RESPONSE: usize = 0x10;
const BUFFER: usize = 0x20;
const PRESENT: usize = 0x24;
const HOST_CONTROL: usize = 0x28;
const POWER: usize = 0x29;
const CLOCK: usize = 0x2C;
const TIMEOUT: usize = 0x2E;
const RESET: usize = 0x2F;
const INT_STATUS: usize = 0x30;
const ERR_STATUS: usize = 0x32;
const INT_ENABLE: usize = 0x34;
const ERR_ENABLE: usize = 0x36;
const INT_SIGNAL: usize = 0x38;
const ERR_SIGNAL: usize = 0x3A;
const CAPABILITIES: usize = 0x40;
const ADMA_ADDRESS: usize = 0x58;
const VERSION: usize = 0xFE;

// Present state
const CMD_INHIBIT: u32 = 1 << 0;
const DAT_INHIBIT: u32 = 1 << 1;
const CARD_INSERTED: u32 = 1 << 16;
const WRITE_ENABLED: u32 = 1 << 19;

// Interrupt status
const INT_COMMAND: u16 = 1 << 0;
const INT_TRANSFER: u16 = 1 << 1;
const INT_DMA: u16 = 1 << 3;
const INT_WRITE_READY: u16 = 1 << 4;
const INT_READ_READY: u16 = 1 << 5;
const INT_ERROR: u16 = 1 << 15;
/// Error status: the command got no response.
const ERR_COMMAND_TIMEOUT: u16 = 1 << 0;

const RESET_ALL: u8 = 1 << 0;
const RESET_CMD: u8 = 1 << 1;
const RESET_DAT: u8 = 1 << 2;

// Transfer mode
const TM_DMA: u16 = 1 << 0;
const TM_BLOCK_COUNT: u16 = 1 << 1;
const TM_AUTO_CMD12: u16 = 1 << 2;
const TM_READ: u16 = 1 << 4;
const TM_MULTI: u16 = 1 << 5;

const SECTOR: usize = 512;
/// Sectors per DMA transfer: the bounce buffer's size.
const DMA_SECTORS: u32 = 128;
const DMA_SIZE: usize = DMA_SECTORS as usize * SECTOR;
/// SDMA stops at every 4 KiB boundary (the default) to be given the next
/// address.
const SDMA_BOUNDARY: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Pio,
    Sdma,
    Adma2,
}

/// The bounce buffer and, for ADMA2, its descriptor table.
struct DmaBuffers {
    data: justdrv::Dma,
    table: justdrv::Dma,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Resp {
    None,
    /// 136 bits: CID, CSD.
    R2,
    /// 48 bits, checked: R1, R6, R7.
    R1,
    /// 48 bits followed by busy signalling on the data line.
    R1b,
    /// 48 bits, not checked: OCR (R3).
    R3,
}

impl Resp {
    fn flags(self) -> u16 {
        const CRC: u16 = 1 << 3;
        const INDEX: u16 = 1 << 4;
        match self {
            Resp::None => 0,
            Resp::R2 => 1 | CRC,
            Resp::R1 => 2 | CRC | INDEX,
            Resp::R1b => 3 | CRC | INDEX,
            Resp::R3 => 2,
        }
    }
}

/// A data transfer that goes with a command.
#[derive(Clone, Copy)]
struct Data {
    blocks: u16,
    read: bool,
    dma: bool,
}

struct Slot {
    regs: Mmio,
    /// Controller specification: 0 = 1.00, 1 = 2.00, 2 = 3.00...
    version: u8,
    /// Base clock in kHz.
    base_khz: u32,
    mode: Mode,
    /// Serialises commands; holds the DMA buffers.
    io: Mutex<Option<DmaBuffers>>,
    /// "00:07.0-0"
    name: String,
    parent: u32,
    /// Asleep: the slot is left alone.
    paused: AtomicBool,
    /// The card in use (null if none).
    card: AtomicPtr<Card>,
}

struct Card {
    slot: &'static Slot,
    /// Sector addresses (SDHC, SDXC, eMMC over 2 GB) instead of bytes.
    block_addressing: bool,
    /// The card's identity, to recognise it after sleep.
    cid: u128,
    gone: AtomicBool,
}

/// Status bits are latched only when enabled; nothing interrupts.
fn enable_status(regs: &Mmio) {
    regs.write16(INT_ENABLE, 0x00FF & !(1 << 8));
    regs.write16(ERR_ENABLE, 0x03FF);
    regs.write16(INT_SIGNAL, 0);
    regs.write16(ERR_SIGNAL, 0);
}

/// The slots of one controller, for its power hooks.
struct Controller {
    slots: Vec<&'static Slot>,
}

/// Before sleep: the transfers have ended (the kernel holds new ones back);
/// the cards lose power.
unsafe extern "C" fn suspend(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Controller.
    let c = unsafe { &*(ctx as *const Controller) };
    for s in &c.slots {
        s.paused.store(true, Ordering::Release);
        let _io = s.io.lock();
        s.power_off();
    }
    OK
}

/// After sleep: each controller reset and its card started again, if it is
/// still the same card; a card swapped meanwhile counts as taken out.
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Controller.
    let c = unsafe { &*(ctx as *const Controller) };
    for s in &c.slots {
        s.regs.write8(RESET, RESET_ALL);
        if !justdrv::wait_until(100, || s.regs.read8(RESET) & RESET_ALL == 0) {
            justdrv::warn!("{}: the controller does not come out of reset", s.name);
        }
        enable_status(&s.regs);
        let card = s.card.load(Ordering::Acquire);
        if !card.is_null() {
            // SAFETY: cards are leaked when found.
            let card = unsafe { &*card };
            let same = s.card_present() && {
                let _io = s.io.lock();
                matches!(identify(s), Ok(id) if id.cid == card.cid && id.block_addressing == card.block_addressing)
            };
            if !same {
                justdrv::warn!("{}: the card is not the one there before sleep", s.name);
                card.gone.store(true, Ordering::Release);
                s.power_off();
            }
        }
        s.paused.store(false, Ordering::Release);
    }
    OK
}

impl Slot {
    fn int_status(&self) -> u16 {
        self.regs.read16(INT_STATUS)
    }

    fn reset(&self, what: u8) {
        self.regs.write8(RESET, what);
        justdrv::wait_until(100, || self.regs.read8(RESET) & what == 0);
    }

    /// Clears the errors and resets the lines after a failed command.
    fn recover(&self) -> u16 {
        let err = self.regs.read16(ERR_STATUS);
        self.regs.write16(ERR_STATUS, 0xFFFF);
        self.regs.write16(INT_STATUS, 0xFFFF);
        self.reset(RESET_CMD | RESET_DAT);
        err
    }

    /// Waits for one of `bits` in the interrupt status. An error ends the
    /// wait: the lines are reset and the error returned.
    fn wait_int(&self, bits: u16, timeout_ms: u64) -> Result<(), Status> {
        let mut status = 0;
        let done = justdrv::wait_until(timeout_ms, || {
            status = self.int_status();
            status & (bits | INT_ERROR) != 0
        });
        if !done {
            self.recover();
            return Err(E_TIMEOUT);
        }
        if status & INT_ERROR != 0 {
            let err = self.recover();
            return Err(if err & ERR_COMMAND_TIMEOUT != 0 { E_TIMEOUT } else { E_IO });
        }
        self.regs.write16(INT_STATUS, status & bits);
        Ok(())
    }

    /// Sends a command and returns its response registers. With `data`, the
    /// caller moves the blocks and waits for the end of the transfer.
    fn command(&self, index: u8, arg: u32, resp: Resp, data: Option<Data>) -> Result<[u32; 4], Status> {
        let mut inhibit = CMD_INHIBIT;
        if data.is_some() || resp == Resp::R1b {
            inhibit |= DAT_INHIBIT;
        }
        if !justdrv::wait_until(1000, || self.regs.read32(PRESENT) & inhibit == 0) {
            self.recover();
            return Err(E_TIMEOUT);
        }
        self.regs.write16(INT_STATUS, 0xFFFF);
        self.regs.write16(ERR_STATUS, 0xFFFF);
        let mut cmd = (index as u16) << 8 | resp.flags();
        if let Some(d) = data {
            cmd |= 1 << 5;
            self.regs.write16(BLOCK_SIZE, SECTOR as u16);
            self.regs.write16(BLOCK_COUNT, d.blocks);
            let mut mode = TM_BLOCK_COUNT;
            if d.dma {
                mode |= TM_DMA;
            }
            if d.read {
                mode |= TM_READ;
            }
            if d.blocks > 1 {
                mode |= TM_MULTI | TM_AUTO_CMD12;
            }
            self.regs.write16(TRANSFER_MODE, mode);
        }
        self.regs.write32(ARGUMENT, arg);
        self.regs.write16(COMMAND, cmd);
        self.wait_int(INT_COMMAND, 1000)?;
        let mut r = [0u32; 4];
        for (i, v) in r.iter_mut().enumerate() {
            *v = self.regs.read32(RESPONSE + i * 4);
        }
        if resp == Resp::R1b && data.is_none() {
            self.wait_int(INT_TRANSFER, 3000)?;
        }
        Ok(r)
    }

    /// An application command (ACMD): CMD55 first.
    fn app_command(&self, rca: u32, index: u8, arg: u32, resp: Resp) -> Result<[u32; 4], Status> {
        self.command(55, rca << 16, Resp::R1, None)?;
        self.command(index, arg, resp, None)
    }

    /// Reads `blocks` blocks of the data phase into `buf`, then waits for
    /// the end of the transfer.
    fn read_data(&self, buf: &mut [u8]) -> Result<(), Status> {
        for block in buf.chunks_mut(SECTOR) {
            self.wait_int(INT_READ_READY, 1000)?;
            for word in block.chunks_mut(4) {
                word.copy_from_slice(&self.regs.read32(BUFFER).to_le_bytes());
            }
        }
        self.wait_int(INT_TRANSFER, 1000)
    }

    fn write_data(&self, buf: &[u8]) -> Result<(), Status> {
        for block in buf.chunks(SECTOR) {
            self.wait_int(INT_WRITE_READY, 1000)?;
            for word in block.chunks(4) {
                self.regs.write32(BUFFER, u32::from_le_bytes(word.try_into().unwrap()));
            }
        }
        // Writing to flash can take a while: the card holds the line busy.
        self.wait_int(INT_TRANSFER, 5000)
    }

    /// Points the controller at the bounce buffer for a transfer of `len`
    /// bytes.
    fn setup_dma(&self, bufs: &DmaBuffers, len: usize) {
        match self.mode {
            Mode::Adma2 => {
                // Descriptors of up to 32 KiB: valid, transfer; the last ends.
                let mut at = 0;
                let mut i = 0;
                while at < len {
                    let n = (len - at).min(32 * 1024);
                    let end = if at + n == len { 2 } else { 0 };
                    let attr = 1 | end | 0x20;
                    bufs.table.write::<u64>(i * 8, attr | (n as u64) << 16 | (bufs.data.phys() + at as u64) << 32);
                    at += n;
                    i += 1;
                }
                self.regs.write32(ADMA_ADDRESS, bufs.table.phys() as u32);
                self.regs.write32(ADMA_ADDRESS + 4, (bufs.table.phys() >> 32) as u32);
            }
            Mode::Sdma => self.regs.write32(0x00, bufs.data.phys() as u32),
            Mode::Pio => {}
        }
    }

    /// Waits for the end of a DMA transfer, giving SDMA its next address at
    /// every boundary.
    fn wait_dma(&self, bufs: &DmaBuffers, timeout_ms: u64) -> Result<(), Status> {
        let mut next = bufs.data.phys() + SDMA_BOUNDARY as u64;
        loop {
            let mut status = 0;
            let done = justdrv::wait_until(timeout_ms, || {
                status = self.int_status();
                status & (INT_TRANSFER | INT_DMA | INT_ERROR) != 0
            });
            if !done {
                self.recover();
                return Err(E_TIMEOUT);
            }
            if status & INT_ERROR != 0 {
                self.recover();
                return Err(E_IO);
            }
            if status & INT_TRANSFER != 0 {
                self.regs.write16(INT_STATUS, INT_TRANSFER | INT_DMA);
                return Ok(());
            }
            self.regs.write16(INT_STATUS, INT_DMA);
            self.regs.write32(0x00, next as u32);
            next += SDMA_BOUNDARY as u64;
        }
    }

    fn set_clock(&self, khz: u32) {
        self.regs.write16(CLOCK, 0);
        let div = if self.base_khz <= khz {
            0
        } else if self.version >= 2 {
            // 10-bit divider: base / (2 * n).
            self.base_khz.div_ceil(2 * khz).min(1023)
        } else {
            // 8-bit divider, powers of two only.
            let mut d = 1;
            while self.base_khz / d > khz && d < 256 {
                d *= 2;
            }
            d / 2
        };
        let mut v = ((div & 0xFF) << 8 | ((div >> 8) & 3) << 6) as u16 | 1;
        self.regs.write16(CLOCK, v);
        if !justdrv::wait_until(150, || self.regs.read16(CLOCK) & 2 != 0) {
            justdrv::warn!("{}: the clock does not settle", self.name);
        }
        v |= 4;
        self.regs.write16(CLOCK, v);
        justdrv::sleep_ms(2);
    }

    fn power_on(&self) {
        let caps = self.regs.read32(CAPABILITIES);
        let volts = if caps & (1 << 24) != 0 {
            0b111 // 3.3 V
        } else if caps & (1 << 25) != 0 {
            0b110 // 3.0 V
        } else {
            0b101 // 1.8 V
        };
        self.regs.write8(POWER, volts << 1);
        self.regs.write8(POWER, volts << 1 | 1);
        justdrv::sleep_ms(10);
    }

    fn power_off(&self) {
        self.regs.write16(CLOCK, 0);
        self.regs.write8(POWER, 0);
    }

    fn card_present(&self) -> bool {
        self.regs.read32(PRESENT) & CARD_INSERTED != 0
    }
}

/// What identification found out about a card.
struct Identity {
    block_addressing: bool,
    sectors: u64,
    description: String,
    cid: u128,
}

/// Bits `hi..=lo` of a 128-bit register (CID, CSD).
fn bits(v: u128, hi: u32, lo: u32) -> u64 {
    ((v >> lo) & ((1u128 << (hi - lo + 1)) - 1)) as u64
}

/// A 136-bit response as the card sent it, without the CRC byte: the
/// controller keeps bits 127..8 in its four response registers.
fn long_response(r: [u32; 4]) -> u128 {
    (r[0] as u128 | (r[1] as u128) << 32 | (r[2] as u128) << 64 | (r[3] as u128) << 96) << 8
}

/// Capacity in sectors from a CSD.
fn csd_sectors(csd: u128) -> u64 {
    if bits(csd, 127, 126) == 1 {
        // CSD 2.0 (SDHC/SDXC): 512 KiB units.
        (bits(csd, 69, 48) + 1) * 1024
    } else {
        let c_size = bits(csd, 73, 62);
        let mult = bits(csd, 49, 47);
        let block_len = bits(csd, 83, 80);
        ((c_size + 1) << (mult + 2 + block_len)) / SECTOR as u64
    }
}

fn product_name(cid: u128, mmc: bool) -> String {
    let (hi, chars) = if mmc { (103, 6) } else { (103, 5) };
    let mut s = String::new();
    for i in 0..chars {
        let c = bits(cid, hi - i * 8, hi - i * 8 - 7) as u8;
        if c.is_ascii_graphic() || c == b' ' {
            s.push(c as char);
        }
    }
    String::from(s.trim())
}

fn size_text(sectors: u64) -> String {
    let mb = sectors / 2048;
    if mb >= 1024 {
        format!("{}.{} GB", mb / 1024, mb % 1024 * 10 / 1024)
    } else {
        format!("{mb} MB")
    }
}

/// Identifies the card in the slot and brings it to the transfer state.
fn identify(s: &Slot) -> Result<Identity, Status> {
    s.reset(RESET_CMD | RESET_DAT);
    // 1-bit bus, normal speed, the DMA kind.
    s.regs.write8(HOST_CONTROL, if s.mode == Mode::Adma2 { 2 << 3 } else { 0 });
    s.power_on();
    s.set_clock(400);
    s.regs.write8(TIMEOUT, 0x0E);
    justdrv::sleep_ms(2);

    s.command(0, 0, Resp::None, None)?;
    // SD 2.0 cards answer CMD8 with the check pattern.
    let v2 = matches!(s.command(8, 0x1AA, Resp::R1, None), Ok(r) if r[0] & 0xFFF == 0x1AA);

    let mut mmc = false;
    let mut ocr = 0;
    let start = justdrv::time_ms();
    // SD: ACMD41 until the card is ready.
    loop {
        match s.app_command(0, 41, if v2 { 1 << 30 } else { 0 } | 0x00FF_8000, Resp::R3) {
            Ok(r) => {
                ocr = r[0];
                if ocr & (1 << 31) != 0 {
                    break;
                }
            }
            Err(_) => {
                // No answer to CMD55: an MMC card.
                mmc = true;
                break;
            }
        }
        if justdrv::time_ms() - start > 1500 {
            return Err(E_TIMEOUT);
        }
        justdrv::sleep_ms(10);
    }
    if mmc {
        s.command(0, 0, Resp::None, None)?;
        let start = justdrv::time_ms();
        loop {
            ocr = s.command(1, 0x40FF_8000, Resp::R3, None)?[0];
            if ocr & (1 << 31) != 0 {
                break;
            }
            if justdrv::time_ms() - start > 1500 {
                return Err(E_TIMEOUT);
            }
            justdrv::sleep_ms(10);
        }
    }
    let block_addressing = ocr & (1 << 30) != 0;

    let cid = long_response(s.command(2, 0, Resp::R2, None)?);
    let rca = if mmc {
        s.command(3, 1 << 16, Resp::R1, None)?;
        1
    } else {
        s.command(3, 0, Resp::R1, None)?[0] >> 16
    };
    let csd = long_response(s.command(9, rca << 16, Resp::R2, None)?);
    s.command(7, rca << 16, Resp::R1b, None)?;

    let mut sectors = csd_sectors(csd);
    if mmc && block_addressing {
        // Over 2 GB the size is in the extended CSD (bytes 212-215).
        let mut ext = [0u8; SECTOR];
        s.command(8, 0, Resp::R1, Some(Data { blocks: 1, read: true, dma: false }))?;
        s.read_data(&mut ext)?;
        sectors = u32::from_le_bytes(ext[212..216].try_into().unwrap()) as u64;
    }

    // 4-bit bus.
    let wide = if mmc {
        // SWITCH: write 1 (4 bits) to BUS_WIDTH (byte 183).
        s.command(6, 3 << 24 | 183 << 16 | 1 << 8, Resp::R1b, None).is_ok()
    } else {
        s.app_command(rca, 6, 2, Resp::R1).is_ok()
    };
    if wide {
        s.regs.write8(HOST_CONTROL, s.regs.read8(HOST_CONTROL) | 2);
    }
    if !block_addressing {
        s.command(16, SECTOR as u32, Resp::R1, None)?;
    }
    s.set_clock(if mmc { 20_000 } else { 25_000 });

    let kind = match (mmc, block_addressing, sectors > 64 * 1024 * 1024 * 2) {
        (true, _, _) => "MMC card",
        (false, false, _) => "SD card",
        (false, true, false) => "SDHC card",
        (false, true, true) => "SDXC card",
    };
    let name = product_name(cid, mmc);
    let description =
        if name.is_empty() { format!("{kind}, {}", size_text(sectors)) } else { format!("{kind} {name}, {}", size_text(sectors)) };
    Ok(Identity { block_addressing, sectors, description, cid })
}

fn transfer(card: &Card, lba: u64, count: u32, buf: *mut u8, write: bool) -> Status {
    let s = card.slot;
    let io = s.io.lock();
    let bufs = io.as_ref().filter(|_| s.mode != Mode::Pio);
    let per_command = if bufs.is_some() { DMA_SECTORS } else { 0xFFFF };
    let mut done = 0u32;
    while done < count {
        if card.gone.load(Ordering::Acquire) || !s.card_present() {
            return E_GONE;
        }
        let n = (count - done).min(per_command);
        let at = lba + done as u64;
        let arg = if card.block_addressing { at as u32 } else { (at * SECTOR as u64) as u32 };
        let len = n as usize * SECTOR;
        // SAFETY: the kernel passes a buffer of count sectors.
        let chunk = unsafe { core::slice::from_raw_parts_mut(buf.add(done as usize * SECTOR), len) };
        let data = Data { blocks: n as u16, read: !write, dma: bufs.is_some() };
        let index = match (write, n > 1) {
            (false, false) => 17,
            (false, true) => 18,
            (true, false) => 24,
            (true, true) => 25,
        };
        let result = match bufs {
            Some(b) => {
                if write {
                    b.data.as_mut_slice()[..len].copy_from_slice(chunk);
                }
                s.setup_dma(b, len);
                let r = s.command(index, arg, Resp::R1, Some(data)).and_then(|_| s.wait_dma(b, 5000));
                if r.is_ok() && !write {
                    chunk.copy_from_slice(&b.data.as_slice()[..len]);
                }
                r
            }
            None if write => s.command(index, arg, Resp::R1, Some(data)).and_then(|_| s.write_data(chunk)),
            None => s.command(index, arg, Resp::R1, Some(data)).and_then(|_| s.read_data(chunk)),
        };
        if let Err(e) = result {
            justdrv::warn!("{}: {} of sector {at} failed: {}", s.name, if write { "write" } else { "read" }, justdrv::status_name(e));
            return e;
        }
        done += n;
    }
    OK
}

unsafe extern "C" fn read(ctx: *mut c_void, lba: u64, count: u32, buf: *mut u8) -> Status {
    // SAFETY: ctx is the card leaked when it was found.
    transfer(unsafe { &*(ctx as *const Card) }, lba, count, buf, false)
}

unsafe extern "C" fn write(ctx: *mut c_void, lba: u64, count: u32, buf: *mut u8) -> Status {
    // SAFETY: as in read.
    transfer(unsafe { &*(ctx as *const Card) }, lba, count, buf, true)
}

/// A card went in: identify it and give it to the kernel. Returns the
/// card and its device.
fn insert(s: &'static Slot) -> Option<(&'static Card, u32)> {
    let id = {
        let _io = s.io.lock();
        match identify(s) {
            Ok(id) => id,
            Err(e) => {
                justdrv::warn!("{}: the card does not answer ({})", s.name, justdrv::status_name(e));
                s.power_off();
                return None;
            }
        }
    };
    let readonly = s.regs.read32(PRESENT) & WRITE_ENABLED == 0;
    let mut info = DeviceInfo::empty();
    info.bus = BUS_SD;
    info.set_name(&s.name);
    let device = justdrv::device_add(s.parent, &info, &id.description, DEVICE_OWNED);
    if device < 0 {
        return None;
    }
    let card: &'static Card =
        Box::leak(Box::new(Card { slot: s, block_addressing: id.block_addressing, cid: id.cid, gone: AtomicBool::new(false) }));
    let desc: &'static BlockDesc = Box::leak(Box::new(BlockDesc {
        ctx: card as *const Card as *mut c_void,
        sector_size: SECTOR as u32,
        flags: BLOCK_REMOVABLE | if readonly { BLOCK_READONLY } else { 0 },
        sector_count: id.sectors,
        read,
        write,
        flush: None,
        model: justdrv::model_name(&id.description),
    }));
    justdrv::info!("{}: {}{}", s.name, id.description, if readonly { ", write-protected" } else { "" });
    justdrv::block_register(device as u32, desc);
    s.card.store(card as *const Card as *mut Card, Ordering::Release);
    Some((card, device as u32))
}

/// Watches a slot for cards going in and out.
unsafe extern "C" fn watch(ctx: *mut c_void) {
    // SAFETY: the slot leaked in probe.
    let s: &'static Slot = unsafe { &*(ctx as *const Slot) };
    let mut card: Option<(&'static Card, u32)> = None;
    // A card that failed to start is not retried until it is taken out.
    let mut failed = false;
    loop {
        if s.paused.load(Ordering::Acquire) {
            justdrv::sleep_ms(300);
            continue;
        }
        let present = s.card_present();
        match card {
            None if present && !failed => {
                justdrv::sleep_ms(100); // let the contacts settle
                card = insert(s);
                failed = card.is_none();
            }
            Some((c, device)) if !present || c.gone.load(Ordering::Acquire) => {
                c.gone.store(true, Ordering::Release);
                s.card.store(core::ptr::null_mut(), Ordering::Release);
                drop(s.io.lock()); // the transfer in progress has ended
                justdrv::device_remove(device);
                s.power_off();
                justdrv::info!("{}: card removed", s.name);
                card = None;
                // A card that stopped answering is not retried until it is
                // taken out.
                failed = present;
            }
            None if !present => failed = false,
            _ => {}
        }
        justdrv::sleep_ms(300);
    }
}

fn probe(dev: &DeviceInfo) -> Status {
    let pci = Pci(dev.id);
    // Slot information: number of slots and the BAR of the first one.
    let info = pci.read8(0x40);
    let (count, first) = (((info >> 4) & 7) as usize + 1, (info & 7) as usize);
    pci.enable(false, true, true);
    let loc = dev.location;
    let mut slots = 0;
    let mut spec = 0;
    let mut dma = Mode::Pio;
    let mut list = Vec::new();
    for i in 0..count {
        let Some(bar) = dev.resources.get(first + i).filter(|r| r.kind == RES_MEMORY && r.base != 0) else { continue };
        let Some(regs) = Mmio::map(bar.base, 0x100) else { continue };
        let version = (regs.read16(VERSION) & 0xFF) as u8;
        if version > 5 {
            continue; // nothing sensible there
        }
        regs.write8(RESET, RESET_ALL);
        if !justdrv::wait_until(100, || regs.read8(RESET) & RESET_ALL == 0) {
            justdrv::warn!("slot {i}: the controller does not come out of reset");
            continue;
        }
        let caps = regs.read32(CAPABILITIES);
        let mut base_mhz = if version >= 2 { (caps >> 8) & 0xFF } else { (caps >> 8) & 0x3F };
        if base_mhz == 0 {
            base_mhz = 50; // not reported: the usual value
        }
        let mut mode = if version >= 1 && caps & (1 << 19) != 0 {
            Mode::Adma2
        } else if caps & (1 << 22) != 0 {
            Mode::Sdma
        } else {
            Mode::Pio
        };
        let bufs = match (justdrv::Dma::new(DMA_SIZE), justdrv::Dma::new(4096)) {
            (Some(data), Some(table)) if mode != Mode::Pio => Some(DmaBuffers { data, table }),
            _ => {
                mode = Mode::Pio;
                None
            }
        };
        enable_status(&regs);
        let slot: &'static Slot = Box::leak(Box::new(Slot {
            regs,
            version,
            base_khz: base_mhz * 1000,
            mode,
            io: Mutex::new(bufs),
            name: format!("{:02x}:{:02x}.{}-{i}", loc >> 8, (loc >> 3) & 0x1F, loc & 7),
            parent: dev.id,
            paused: AtomicBool::new(false),
            card: AtomicPtr::new(core::ptr::null_mut()),
        }));
        justdrv::spawn("sdhci", watch, slot as *const Slot as *mut c_void);
        list.push(slot);
        slots += 1;
        spec = version;
        dma = mode;
    }
    if slots == 0 {
        return E_NODEV;
    }
    let c: &'static Controller = Box::leak(Box::new(Controller { slots: list }));
    justdrv::power_hooks(dev.id, c as *const Controller as *mut c_void, Some(suspend), resume);
    let how = match dma {
        Mode::Adma2 => "ADMA2",
        Mode::Sdma => "SDMA",
        Mode::Pio => "no DMA",
    };
    justdrv::info!("{slots} slot{}, specification {}.00, {how}", if slots == 1 { "" } else { "s" }, spec + 1);
    OK
}
