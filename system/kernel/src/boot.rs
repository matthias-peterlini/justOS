//! The hand-off structure from the boot loader (see boot/bios/stage2.asm).

use core::mem::offset_of;

pub const MAGIC: u64 = u64::from_le_bytes(*b"ARCBOOT1");

// fb_kind: 0 = no screen.
pub const FB_RGB: u32 = 1;
pub const FB_TEXT: u32 = 2;

// loader: 1 = BIOS.
pub const LOADER_UEFI: u32 = 2;

/// The slot is new and not yet known to work: this start used up one of
/// its tries.
pub const SLOT_TRIAL: u32 = 0x100;
/// The active slot had no tries left (or could not be read): the loader
/// started the other one.
pub const SLOT_FALLBACK: u32 = 0x200;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BootInfo {
    pub magic: u64,
    pub version: u32,
    pub boot_drive: u32,
    pub mmap_addr: u64,
    pub mmap_count: u64,
    pub fb_addr: u64,
    pub fb_width: u32,
    pub fb_height: u32,
    pub fb_pitch: u32,
    pub fb_bpp: u32,
    pub fb_kind: u32,
    pub red_pos: u8,
    pub red_size: u8,
    pub green_pos: u8,
    pub green_size: u8,
    pub blue_pos: u8,
    pub blue_size: u8,
    pub _pad0: [u8; 2],
    pub _pad1: u32,
    pub font_addr: u64,
    pub font_height: u32,
    pub font_width: u32,
    pub kernel_phys: u64,
    pub kernel_size: u64,
    pub pack_phys: u64,
    pub pack_size: u64,
    pub rsdp: u64,
    pub loader: u32,
    /// The system slot that started: 1 = A, 2 = B (0: a disk without
    /// slots, a live CD), with [`SLOT_TRIAL`] and [`SLOT_FALLBACK`].
    pub slot: u32,
    /// A live system volume (booted from a CD), or 0.
    pub system_phys: u64,
    pub system_size: u64,
}

const _: () = {
    assert!(offset_of!(BootInfo, mmap_addr) == 0x10);
    assert!(offset_of!(BootInfo, fb_addr) == 0x20);
    assert!(offset_of!(BootInfo, fb_kind) == 0x38);
    assert!(offset_of!(BootInfo, red_pos) == 0x3C);
    assert!(offset_of!(BootInfo, font_addr) == 0x48);
    assert!(offset_of!(BootInfo, kernel_phys) == 0x58);
    assert!(offset_of!(BootInfo, pack_phys) == 0x68);
    assert!(offset_of!(BootInfo, rsdp) == 0x78);
    assert!(offset_of!(BootInfo, loader) == 0x80);
    assert!(offset_of!(BootInfo, slot) == 0x84);
    assert!(offset_of!(BootInfo, system_phys) == 0x88);
};

/// One entry of the firmware memory map (E820 layout).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MemoryRegion {
    pub base: u64,
    pub len: u64,
    pub kind: u32,
    pub ext: u32,
}

/// Free RAM. Other kinds (2 reserved, 3 ACPI tables, 4 ACPI NVS, 5 bad)
/// are never handed out.
pub const MEM_USABLE: u32 = 1;

impl BootInfo {
    /// The memory map. The boot loader's identity map is still active, or
    /// the table has been copied by [`crate::mem`].
    pub fn memory_map(&self) -> &'static [MemoryRegion] {
        // SAFETY: the loader stores `mmap_count` entries at `mmap_addr`, which
        // is reachable through the direct map.
        unsafe { core::slice::from_raw_parts(crate::mem::phys_to_virt(self.mmap_addr) as *const MemoryRegion, self.mmap_count as usize) }
    }
}
