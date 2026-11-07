//! JustBoot for UEFI firmware.
//!
//! The firmware starts this program from the EFI system partition
//! (`\EFI\BOOT\BOOTX64.EFI`). It does what the BIOS stages do, with the
//! firmware's services instead of the BIOS:
//!   1. picks the system slot with `\JUSTOS\BOOT.CTL` and reads the kernel
//!      and the boot pack from `\JUSTOS\SLOTA.BIN` or `SLOTB.BIN` on the same
//!      partition (a live CD has no slots: `\JUSTOS\KERNEL.BIN` and
//!      `\JUSTOS\BOOTPACK.IMG`);
//!   2. picks a 32-bit framebuffer mode through the Graphics Output Protocol;
//!   3. finds the ACPI tables in the firmware's configuration table;
//!   4. builds the initial page tables, takes the memory map and leaves the
//!      firmware's boot services;
//!   5. jumps to the kernel entry with RDI = physical address of the BootInfo.
//!
//! The kernel then sees the same hand-off as from the BIOS loader, except
//! that it was loaded at any physical address ≡ 1 MiB (mod 2 MiB) and that
//! there is no firmware font: the kernel uses its own.

#![no_std]
#![no_main]

#[allow(dead_code)] // the specification's names, not all of them used
mod efi;

use core::arch::asm;
use core::ffi::c_void;
use core::fmt::{self, Write};
use core::mem::{offset_of, size_of};
use core::ptr::{null_mut, write_bytes};
use core::sync::atomic::{AtomicPtr, Ordering};
use efi::*;

const PAGE: u64 = 4096;
const HUGE: u64 = 2 << 20;
const GIB: u64 = 1 << 30;
const HHDM: u64 = 0xFFFF_8000_0000_0000;
const KERNEL_BASE: u64 = 0xFFFF_FFFF_8000_0000;
const KERNEL_LINK_OFFSET: u64 = 0x10_0000;
/// Everything the loader allocates stays below 4 GiB, where the kernel's
/// early code expects the hand-off.
const BELOW_4G: u64 = 0xFFFF_F000;

// The boot magics keep their arcOS names: this loader stays on the disk
// while updates bring new kernels, and older loaders read new slots.
const KERNEL_FILE_MAGIC: &[u8; 8] = b"ARCKERN1";
const BOOT_MAGIC: &[u8; 8] = b"ARCBOOT1";
const PREFERRED_MODE: (u32, u32) = (1024, 768);

// System slots (see tools/justtool/src/slot.rs).
const CTL_MAGIC: &[u8; 8] = b"ARCCTL01";
const SLOT_MAGIC: &[u8; 8] = b"ARCSLOT1";
const SLOT_FILES: [&str; 2] = ["\\JUSTOS\\SLOTA.BIN", "\\JUSTOS\\SLOTB.BIN"];
/// BootInfo slot flags: not yet known to work; the other slot should have
/// started.
const SLOT_TRIAL: u32 = 0x100;
const SLOT_FALLBACK: u32 = 0x200;
const SECTOR: usize = 512;

// ----------------------------------------------------------------------------
// The hand-off (same layout as kernel/src/boot.rs)

#[repr(C)]
#[derive(Default)]
struct BootInfo {
    magic: u64,
    version: u32,
    boot_drive: u32,
    mmap_addr: u64,
    mmap_count: u64,
    fb_addr: u64,
    fb_width: u32,
    fb_height: u32,
    fb_pitch: u32,
    fb_bpp: u32,
    fb_kind: u32,
    channels: [u8; 6],
    _pad0: [u8; 2],
    _pad1: u32,
    font_addr: u64,
    font_height: u32,
    font_width: u32,
    kernel_phys: u64,
    kernel_size: u64,
    pack_phys: u64,
    pack_size: u64,
    rsdp: u64,
    loader: u32,
    /// 1 = slot A, 2 = slot B, with the SLOT_ flags; 0 without slots.
    slot: u32,
    system_phys: u64,
    system_size: u64,
}

const _: () = {
    assert!(offset_of!(BootInfo, fb_kind) == 0x38);
    assert!(offset_of!(BootInfo, channels) == 0x3C);
    assert!(offset_of!(BootInfo, font_addr) == 0x48);
    assert!(offset_of!(BootInfo, kernel_phys) == 0x58);
    assert!(offset_of!(BootInfo, rsdp) == 0x78);
    assert!(offset_of!(BootInfo, loader) == 0x80);
    assert!(offset_of!(BootInfo, slot) == 0x84);
    assert!(offset_of!(BootInfo, system_phys) == 0x88);
};

const FB_RGB: u32 = 1;
const LOADER_UEFI: u32 = 2;

/// One entry of the memory map handed to the kernel (E820 layout).
#[repr(C)]
#[derive(Clone, Copy)]
struct Region {
    base: u64,
    len: u64,
    kind: u32,
    ext: u32,
}

const MEM_USABLE: u32 = 1;
const MEM_RESERVED: u32 = 2;
const MEM_ACPI_RECLAIM: u32 = 3;
const MEM_ACPI_NVS: u32 = 4;
const MEM_BAD: u32 = 5;

// ----------------------------------------------------------------------------
// Firmware access

static SYSTEM: AtomicPtr<SystemTable> = AtomicPtr::new(null_mut());

fn system() -> &'static SystemTable {
    // SAFETY: set once at entry; the table lives as long as boot services.
    unsafe { &*SYSTEM.load(Ordering::Relaxed) }
}

fn boot_services() -> &'static BootServices {
    // SAFETY: valid until ExitBootServices, after which it is not used.
    unsafe { &*system().boot_services }
}

/// Text on the firmware console (screen and, on most machines, serial).
struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let out = system().con_out;
        if out.is_null() {
            return Ok(());
        }
        let mut buf = [0u16; 128];
        let mut n = 0;
        let flush = |buf: &mut [u16; 128], n: &mut usize| {
            buf[*n] = 0;
            // SAFETY: a null-terminated UCS-2 string.
            unsafe { ((*out).output_string)(out, buf.as_ptr()) };
            *n = 0;
        };
        for c in s.chars() {
            if n + 3 >= buf.len() {
                flush(&mut buf, &mut n);
            }
            if c == '\n' {
                buf[n] = '\r' as u16;
                n += 1;
            }
            buf[n] = if (c as u32) < 0x10000 { c as u16 } else { '?' as u16 };
            n += 1;
        }
        flush(&mut buf, &mut n);
        Ok(())
    }
}

macro_rules! print {
    ($($arg:tt)*) => { let _ = write!(Console, $($arg)*); };
}

/// Stops with a message; the firmware's watchdog restarts the machine.
fn fail(what: &str, status: Status) -> ! {
    print!("\njustOS cannot start: {what} (status {:#x})\n", status);
    loop {
        // SAFETY: plain firmware call.
        unsafe { (boot_services().stall)(1_000_000) };
    }
}

fn check(status: Status, what: &str) {
    if status != SUCCESS {
        fail(what, status);
    }
}

fn allocate_pages(pages: u64, what: &str) -> u64 {
    let mut addr = BELOW_4G;
    // SAFETY: plain firmware call.
    let status = unsafe { (boot_services().allocate_pages)(ALLOCATE_MAX_ADDRESS, LOADER_DATA, pages as usize, &mut addr) };
    check(status, what);
    // SAFETY: freshly allocated, identity-mapped by the firmware.
    unsafe { write_bytes(addr as *mut u8, 0, (pages * PAGE) as usize) };
    addr
}

/// A null-terminated UCS-2 copy of an ASCII path.
fn ucs2<const N: usize>(s: &str) -> [u16; N] {
    let mut out = [0u16; N];
    for (i, b) in s.bytes().take(N - 1).enumerate() {
        out[i] = b as u16;
    }
    out
}

// ----------------------------------------------------------------------------
// Files

struct Volume(*mut File);

impl Volume {
    /// The file system the loader itself was read from.
    fn of_image(image: Handle) -> Volume {
        let bs = boot_services();
        let mut loaded: *mut c_void = null_mut();
        // SAFETY: plain firmware calls with valid out pointers.
        unsafe {
            check((bs.handle_protocol)(image, &LOADED_IMAGE_PROTOCOL, &mut loaded), "no loaded-image protocol");
            let device = (*(loaded as *mut LoadedImage)).device_handle;
            let mut sfs: *mut c_void = null_mut();
            check((bs.handle_protocol)(device, &SIMPLE_FILE_SYSTEM_PROTOCOL, &mut sfs), "the boot partition cannot be read");
            let sfs = sfs as *mut SimpleFileSystem;
            let mut root = null_mut();
            check(((*sfs).open_volume)(sfs, &mut root), "the boot partition cannot be opened");
            Volume(root)
        }
    }

    fn open(&self, path: &str) -> *mut File {
        match self.try_open(path) {
            Ok(file) => file,
            Err(status) => {
                print!("{path}: not found\n");
                fail("a system file is missing", status)
            }
        }
    }

    fn try_open(&self, path: &str) -> Result<*mut File, Status> {
        self.try_open_mode(path, FILE_MODE_READ)
    }

    fn try_open_mode(&self, path: &str, mode: u64) -> Result<*mut File, Status> {
        let name = ucs2::<64>(path);
        let mut file = null_mut();
        // SAFETY: valid protocol pointer and null-terminated name.
        let status = unsafe { ((*self.0).open)(self.0, &mut file, name.as_ptr(), mode, 0) };
        if status == SUCCESS {
            Ok(file)
        } else {
            Err(status)
        }
    }
}

fn file_size(file: *mut File) -> u64 {
    let mut size = 0;
    // SAFETY: valid file; setting the position to all ones moves to the end.
    unsafe {
        check(((*file).set_position)(file, u64::MAX), "seek");
        check(((*file).get_position)(file, &mut size), "seek");
        check(((*file).set_position)(file, 0), "seek");
    }
    size
}

fn read_exact(file: *mut File, dest: *mut u8, len: u64, what: &str) {
    let mut done = 0;
    while done < len {
        let mut n = (len - done) as usize;
        // SAFETY: `dest` has room for `len` bytes.
        check(unsafe { ((*file).read)(file, &mut n, dest.add(done as usize)) }, what);
        if n == 0 {
            fail(what, 0);
        }
        done += n as u64;
    }
}

fn close(file: *mut File) {
    // SAFETY: valid file.
    unsafe { ((*file).close)(file) };
}

/// Reads `len` bytes at `at`; false on any error (the caller has another
/// way).
fn try_read(file: *mut File, at: u64, dest: *mut u8, len: u64) -> bool {
    // SAFETY: valid file; `dest` has room for `len` bytes.
    unsafe {
        if ((*file).set_position)(file, at) != SUCCESS {
            return false;
        }
        let mut done = 0;
        while done < len {
            let mut n = (len - done) as usize;
            if ((*file).read)(file, &mut n, dest.add(done as usize)) != SUCCESS || n == 0 {
                return false;
            }
            done += n as u64;
        }
    }
    true
}

/// Room for a kernel of `memsz` bytes: it must sit 1 MiB into a 2 MiB
/// page, so this allocates 2 MiB more and picks the address inside.
fn place_kernel(memsz: u64) -> u64 {
    let pages = (memsz + HUGE).div_ceil(PAGE);
    let area = allocate_pages(pages, "no memory for the kernel");
    let aligned = area.next_multiple_of(HUGE);
    if aligned >= HUGE && aligned - HUGE + KERNEL_LINK_OFFSET >= area {
        aligned - HUGE + KERNEL_LINK_OFFSET
    } else {
        aligned + KERNEL_LINK_OFFSET
    }
}

/// Loads `KERNEL.BIN`: a 32-byte header (magic, entry, memory size, file
/// size) and the flat image. Returns (physical address, memory size, entry).
fn load_kernel(vol: &Volume) -> (u64, u64, u64) {
    let file = vol.open("\\JUSTOS\\KERNEL.BIN");
    let mut header = [0u8; 32];
    read_exact(file, header.as_mut_ptr(), 32, "kernel header");
    let word = |i: usize| u64::from_le_bytes(header[i * 8..i * 8 + 8].try_into().unwrap());
    let (entry, memsz, filesz) = (word(1), word(2), word(3));
    if &header[..8] != KERNEL_FILE_MAGIC || filesz > memsz || filesz + 32 > file_size(file) {
        fail("KERNEL.BIN is not a justOS kernel", 0);
    }
    // SAFETY: back to the image after the size check.
    unsafe { ((*file).set_position)(file, 32) };
    let phys = place_kernel(memsz);
    read_exact(file, phys as *mut u8, filesz, "cannot read the kernel");
    close(file);
    (phys, memsz, entry)
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// The magic matches and the 128 dwords add up to zero.
fn sector_ok(s: &[u8; SECTOR], magic: &[u8; 8]) -> bool {
    &s[..8] == magic && (0..SECTOR / 4).fold(0u32, |a, i| a.wrapping_add(le32(s, i * 4))) == 0
}

/// Which slot starts (0 = A, 1 = B) and the BootInfo flags, from
/// BOOT.CTL; `None` without one (a live CD). A slot not yet known to work
/// uses up one of its starts, written back at once; with none left, the
/// other starts. A damaged BOOT.CTL starts slot A.
fn choose_slot(vol: &Volume) -> Option<(usize, u32)> {
    let file = vol.try_open_mode("\\JUSTOS\\BOOT.CTL", FILE_MODE_READ | FILE_MODE_WRITE).or_else(|_| vol.try_open("\\JUSTOS\\BOOT.CTL")).ok()?;
    let mut ctl = [0u8; SECTOR];
    if !try_read(file, 0, ctl.as_mut_ptr(), SECTOR as u64) || !sector_ok(&ctl, CTL_MAGIC) {
        close(file);
        return Some((0, 0));
    }
    let mut slot = (ctl[0x14] & 1) as usize;
    let mut flags = 0;
    if (ctl[0x16] >> slot) & 1 == 0 {
        if ctl[0x15] > 0 {
            ctl[0x15] -= 1;
            let seq = le32(&ctl, 0x18).wrapping_add(1);
            ctl[0x18..0x1C].copy_from_slice(&seq.to_le_bytes());
            ctl[0x1FC..].fill(0);
            let sum = (0..SECTOR / 4).fold(0u32, |a, i| a.wrapping_add(le32(&ctl, i * 4)));
            ctl[0x1FC..].copy_from_slice(&sum.wrapping_neg().to_le_bytes());
            let mut n = SECTOR;
            // SAFETY: valid file and buffer; a failed write (a read-only
            // disk) just leaves the count as it was.
            unsafe {
                ((*file).set_position)(file, 0);
                ((*file).write)(file, &mut n, ctl.as_ptr());
                ((*file).flush)(file);
            }
            flags = SLOT_TRIAL;
        } else {
            slot ^= 1;
            flags = SLOT_FALLBACK;
        }
    }
    close(file);
    Some((slot, flags))
}

/// The kernel (address, memory size, entry) and the boot pack (address,
/// size) of a slot, in memory.
type Loaded = ((u64, u64, u64), (u64, u64));

/// Loads the kernel and the boot pack of a slot. `None` if its header is
/// damaged or it cannot be read.
fn load_slot(vol: &Volume, slot: usize) -> Option<Loaded> {
    let file = vol.try_open(SLOT_FILES[slot]).ok()?;
    let mut h = [0u8; SECTOR];
    let loaded = (|| {
        if !try_read(file, 0, h.as_mut_ptr(), SECTOR as u64) || !sector_ok(&h, SLOT_MAGIC) {
            return None;
        }
        let memsz = le32(&h, 0x0C) as u64;
        let entry = u64::from_le_bytes(h[0x10..0x18].try_into().unwrap());
        let (pack_at, kernel_bytes, pack_bytes) = (le32(&h, 0x18) as u64 * SECTOR as u64, le32(&h, 0x20) as u64, le32(&h, 0x24) as u64);
        if kernel_bytes == 0 || kernel_bytes > memsz {
            return None;
        }
        let kernel = place_kernel(memsz);
        if !try_read(file, SECTOR as u64, kernel as *mut u8, kernel_bytes) {
            return None;
        }
        let pack = allocate_pages(pack_bytes.div_ceil(PAGE).max(1), "no memory for the boot pack");
        if !try_read(file, pack_at, pack as *mut u8, pack_bytes) {
            return None;
        }
        Some(((kernel, memsz, entry), (pack, pack_bytes)))
    })();
    close(file);
    loaded
}

fn load_boot_pack(vol: &Volume) -> (u64, u64) {
    let file = vol.open("\\JUSTOS\\BOOTPACK.IMG");
    let size = file_size(file);
    let phys = allocate_pages(size.div_ceil(PAGE).max(1), "no memory for the boot pack");
    read_exact(file, phys as *mut u8, size, "cannot read the boot pack");
    close(file);
    (phys, size)
}

/// The system volume of a live CD, if this boot medium has one: the kernel
/// mounts it from memory as `sys:`.
fn load_system(vol: &Volume) -> (u64, u64) {
    let Ok(file) = vol.try_open("\\JUSTOS\\SYSTEM.IMG") else { return (0, 0) };
    let size = file_size(file);
    let phys = allocate_pages(size.div_ceil(PAGE).max(1), "no memory for the system volume");
    read_exact(file, phys as *mut u8, size, "cannot read the system volume");
    close(file);
    (phys, size)
}

// ----------------------------------------------------------------------------
// Screen and ACPI

fn mask_channel(mask: u32) -> (u8, u8) {
    if mask == 0 {
        (0, 0)
    } else {
        (mask.trailing_zeros() as u8, mask.count_ones() as u8)
    }
}

/// Keeps the current mode when it is already large (on a real computer the
/// firmware starts at the panel's native resolution, which justOS cannot change
/// later without a driver for the card); otherwise switches to the preferred
/// mode if the display has it. Fills the framebuffer fields of the BootInfo.
fn setup_screen(info: &mut BootInfo) {
    let mut gop: *mut c_void = null_mut();
    // SAFETY: plain firmware call.
    if unsafe { (boot_services().locate_protocol)(&GRAPHICS_OUTPUT_PROTOCOL, null_mut(), &mut gop) } != SUCCESS {
        print!("no graphics output: the kernel will use the serial console\n");
        return;
    }
    let gop = gop as *mut GraphicsOutput;
    let usable = |m: &ModeInfo| matches!(m.pixel_format, PIXEL_RGB | PIXEL_BGR | PIXEL_BIT_MASK);
    // SAFETY: the protocol and its mode table are valid during boot services.
    unsafe {
        let max = (*(*gop).mode).max_mode;
        let current = &*(*(*gop).mode).info;
        let large = usable(current) && current.width >= PREFERRED_MODE.0 && current.height >= PREFERRED_MODE.1;
        for n in 0..if large { 0 } else { max } {
            let mut size = 0;
            let mut m: *const ModeInfo = core::ptr::null();
            if ((*gop).query_mode)(gop, n, &mut size, &mut m) == SUCCESS
                && usable(&*m)
                && ((*m).width, (*m).height) == PREFERRED_MODE
                && n != (*(*gop).mode).mode
            {
                ((*gop).set_mode)(gop, n);
                break;
            }
        }
        let mode = &*(*gop).mode;
        let m = &*mode.info;
        if !usable(m) {
            print!("the display has no linear framebuffer\n");
            return;
        }
        let (r, g, b) = match m.pixel_format {
            PIXEL_RGB => ((0, 8), (8, 8), (16, 8)),
            PIXEL_BGR => ((16, 8), (8, 8), (0, 8)),
            _ => (mask_channel(m.red_mask), mask_channel(m.green_mask), mask_channel(m.blue_mask)),
        };
        info.fb_addr = mode.frame_buffer_base;
        info.fb_width = m.width;
        info.fb_height = m.height;
        info.fb_pitch = m.pixels_per_scan_line * 4;
        info.fb_bpp = 32;
        info.fb_kind = FB_RGB;
        info.channels = [r.0, r.1, g.0, g.1, b.0, b.1];
    }
}

fn find_rsdp() -> u64 {
    let st = system();
    // SAFETY: the firmware's configuration table has that many entries.
    let tables = unsafe { core::slice::from_raw_parts(st.configuration_table, st.number_of_table_entries) };
    let find = |guid| tables.iter().find(|t| t.vendor_guid == guid).map(|t| t.vendor_table as u64);
    find(ACPI_20_TABLE).or_else(|| find(ACPI_10_TABLE)).unwrap_or(0)
}

// ----------------------------------------------------------------------------
// Memory map and page tables

struct MemoryMap {
    buf: *mut u8,
    capacity: usize,
    size: usize,
    key: usize,
    desc_size: usize,
}

impl MemoryMap {
    /// A buffer big enough for the map plus what a few more allocations add.
    fn new() -> MemoryMap {
        let bs = boot_services();
        let (mut size, mut key, mut desc_size, mut version) = (0, 0, 0, 0);
        // SAFETY: asking for the size only.
        let status = unsafe { (bs.get_memory_map)(&mut size, null_mut(), &mut key, &mut desc_size, &mut version) };
        if status != BUFFER_TOO_SMALL || desc_size == 0 {
            fail("no memory map", status);
        }
        let capacity = size + 16 * desc_size;
        let mut buf = null_mut();
        // SAFETY: plain firmware call.
        check(unsafe { (bs.allocate_pool)(LOADER_DATA, capacity, &mut buf) }, "no memory for the memory map");
        MemoryMap { buf, capacity, size: 0, key: 0, desc_size }
    }

    fn read(&mut self) -> Status {
        let mut version = 0;
        self.size = self.capacity;
        // SAFETY: the buffer holds `capacity` bytes.
        unsafe { (boot_services().get_memory_map)(&mut self.size, self.buf, &mut self.key, &mut self.desc_size, &mut version) }
    }

    fn entries(&self) -> impl Iterator<Item = MemoryDescriptor> + '_ {
        (0..self.size / self.desc_size).map(move |i| {
            // SAFETY: inside the part the firmware filled in.
            unsafe { core::ptr::read_unaligned(self.buf.add(i * self.desc_size) as *const MemoryDescriptor) }
        })
    }

    fn len(&self) -> usize {
        self.size / self.desc_size
    }
}

fn region_kind(efi_kind: u32) -> u32 {
    match efi_kind {
        // The loader's own code and the firmware's boot-time memory are free
        // once the kernel runs. Loader data holds the kernel, the boot pack,
        // the page tables and this map: the kernel keeps those.
        LOADER_CODE | BOOT_SERVICES_CODE | BOOT_SERVICES_DATA | CONVENTIONAL_MEMORY => MEM_USABLE,
        ACPI_RECLAIM_MEMORY => MEM_ACPI_RECLAIM,
        ACPI_MEMORY_NVS => MEM_ACPI_NVS,
        UNUSABLE_MEMORY => MEM_BAD,
        // Loader data, runtime services, device memory, firmware reserved.
        _ => MEM_RESERVED,
    }
}

/// Converts the firmware map, merging neighbours of the same kind.
fn convert_map(map: &MemoryMap, out: &mut [Region]) -> usize {
    let mut n = 0;
    for d in map.entries() {
        let (base, len, kind) = (d.physical_start, d.number_of_pages * PAGE, region_kind(d.kind));
        if len == 0 {
            continue;
        }
        if n > 0 && out[n - 1].kind == kind && out[n - 1].base + out[n - 1].len == base {
            out[n - 1].len += len;
        } else if n < out.len() {
            out[n] = Region { base, len, kind, ext: 1 };
            n += 1;
        }
    }
    n
}

/// Highest physical address of memory (not device space), at least 4 GiB.
fn memory_top(map: &MemoryMap) -> u64 {
    map.entries()
        .filter(|d| !matches!(d.kind, MEMORY_MAPPED_IO | MEMORY_MAPPED_IO_PORT_SPACE))
        .map(|d| d.physical_start + d.number_of_pages * PAGE)
        .max()
        .unwrap_or(0)
        .max(4 * GIB)
        .next_multiple_of(GIB)
        .min(512 * GIB)
}

const PRESENT: u64 = 1;
const WRITABLE: u64 = 2;
const HUGE_PAGE: u64 = 0x80;

/// Identity map and direct map of `[0, top)`, and the kernel image at
/// KERNEL_BASE. The two low maps share their tables.
fn build_page_tables(top: u64, kernel_phys: u64, kernel_size: u64) -> u64 {
    let gigs = top / GIB;
    let pages = 4 + gigs;
    let base = allocate_pages(pages, "no memory for page tables");
    let table = |i: u64| (base + i * PAGE) as *mut u64;
    let (pml4, pdpt, kpdpt, kpd) = (table(0), table(1), table(2), table(3));
    // SAFETY: the pages were just allocated and zeroed.
    unsafe {
        *pml4 = pdpt as u64 | PRESENT | WRITABLE;
        *pml4.add(((HHDM >> 39) & 511) as usize) = pdpt as u64 | PRESENT | WRITABLE;
        *pml4.add(((KERNEL_BASE >> 39) & 511) as usize) = kpdpt as u64 | PRESENT | WRITABLE;
        for g in 0..gigs {
            let pd = table(4 + g);
            *pdpt.add(g as usize) = pd as u64 | PRESENT | WRITABLE;
            for i in 0..512 {
                *pd.add(i) = (g * GIB + i as u64 * HUGE) | PRESENT | WRITABLE | HUGE_PAGE;
            }
        }
        // KERNEL_BASE: entry 510 of the last PDPT.
        *kpdpt.add(((KERNEL_BASE >> 30) & 511) as usize) = kpd as u64 | PRESENT | WRITABLE;
        let start = kernel_phys - KERNEL_LINK_OFFSET;
        let count = (KERNEL_LINK_OFFSET + kernel_size).div_ceil(HUGE);
        for i in 0..count {
            *kpd.add(i as usize) = (start + i * HUGE) | PRESENT | WRITABLE | HUGE_PAGE;
        }
    }
    pml4 as u64
}

// ----------------------------------------------------------------------------
// Entry

#[no_mangle]
extern "efiapi" fn efi_main(image: Handle, st: *mut SystemTable) -> Status {
    SYSTEM.store(st, Ordering::Relaxed);
    // A clean black screen until the kernel draws its splash.
    let out = system().con_out;
    // SAFETY: firmware calls on the console protocol from the system table.
    unsafe {
        ((*out).clear_screen)(out);
        ((*out).enable_cursor)(out, false);
    }
    // SAFETY: plain firmware call; the loader does not want a 5-minute limit.
    unsafe { (boot_services().set_watchdog_timer)(0, 0, 0, core::ptr::null()) };

    let vol = Volume::of_image(image);
    let ((kernel_phys, kernel_size, entry), (pack_phys, pack_size), slot) = match choose_slot(&vol) {
        Some((slot, flags)) => match load_slot(&vol, slot) {
            Some(loaded) => (loaded.0, loaded.1, (slot as u32 + 1) | flags),
            // A damaged slot: the other one, if it was not the other already.
            None if flags & SLOT_FALLBACK == 0 => match load_slot(&vol, slot ^ 1) {
                Some(loaded) => (loaded.0, loaded.1, ((slot as u32 ^ 1) + 1) | SLOT_FALLBACK),
                None => fail("no system slot can be read", 0),
            },
            None => fail("no system slot can be read", 0),
        },
        None => (load_kernel(&vol), load_boot_pack(&vol), 0),
    };
    let (system_phys, system_size) = load_system(&vol);
    close(vol.0);

    let info_phys = allocate_pages(1, "no memory");
    // SAFETY: a zeroed page of our own.
    let info = unsafe { &mut *(info_phys as *mut BootInfo) };
    *info = BootInfo::default();
    info.magic = u64::from_le_bytes(*BOOT_MAGIC);
    info.version = 1;
    info.kernel_phys = kernel_phys;
    info.kernel_size = kernel_size;
    info.pack_phys = pack_phys;
    info.pack_size = pack_size;
    info.rsdp = find_rsdp();
    info.loader = LOADER_UEFI;
    info.slot = slot;
    info.system_phys = system_phys;
    info.system_size = system_size;

    // The screen is set last: after the mode change the firmware console
    // may not show text any more.
    setup_screen(info);

    let mut map = MemoryMap::new();
    if map.read() != SUCCESS {
        fail("no memory map", 0);
    }
    let top = memory_top(&map);
    let pml4 = build_page_tables(top, kernel_phys, kernel_size);
    let regions_max = map.len() + 32;
    let regions_phys = allocate_pages((regions_max * size_of::<Region>()) as u64 / PAGE + 1, "no memory");

    // From the last GetMemoryMap to ExitBootServices nothing may allocate.
    let mut tries = 0;
    loop {
        let status = map.read();
        if status == SUCCESS {
            // SAFETY: plain firmware call with the current map key.
            if unsafe { (boot_services().exit_boot_services)(image, map.key) } == SUCCESS {
                break;
            }
        }
        tries += 1;
        if tries == 4 {
            fail("the firmware does not let go", status);
        }
    }

    // Boot services are gone: no more firmware calls from here.
    // SAFETY: room for `regions_max` entries was allocated above.
    let regions = unsafe { core::slice::from_raw_parts_mut(regions_phys as *mut Region, regions_max) };
    info.mmap_count = convert_map(&map, regions) as u64;
    info.mmap_addr = regions_phys;

    // SAFETY: the new tables identity-map this code and map the kernel at
    // its link address; the kernel takes over the machine from here.
    unsafe {
        asm!(
            "cli",
            "mov cr3, {pml4}",
            "jmp {entry}",
            pml4 = in(reg) pml4,
            entry = in(reg) entry,
            in("rdi") info_phys,
            options(noreturn),
        );
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    print!("\nJustBoot: {info}\n");
    loop {
        // SAFETY: halting with interrupts on just waits.
        unsafe { asm!("hlt") };
    }
}
