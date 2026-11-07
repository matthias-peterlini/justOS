//! Four-level page tables.
//!
//! The kernel half (PML4 entries 256..511) is created once at boot and shared
//! by every address space, so kernel mappings made later are visible
//! everywhere without touching other tables.

use super::{align_up, frame, phys_to_virt, HHDM, KERNEL_BASE, PAGE_SIZE};
use crate::arch::cpu;
use core::sync::atomic::{AtomicU64, Ordering};

pub const PRESENT: u64 = 1;
pub const WRITE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
/// With the PAT set up by [`init`], PWT alone selects write-combining.
pub const WRITE_COMBINE: u64 = 1 << 3;
pub const NO_CACHE: u64 = (1 << 4) | (1 << 3);
pub const HUGE: u64 = 1 << 7;
/// A bit of our own (the processor ignores it): the page's frame belongs
/// to shared memory, not to this address space, so fork maps the same
/// frame instead of copying it and nothing here ever frees it. Without
/// [`WRITE`] it is a page of a mapped file that the mapping has not
/// written to (see `linux::mm::copy_on_write`).
pub const SHARED: u64 = 1 << 9;
pub const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

static KERNEL_PML4: AtomicU64 = AtomicU64::new(0);

const HUGE_SIZE: u64 = 2 << 20;

fn table(phys: u64) -> &'static mut [u64; 512] {
    // SAFETY: page tables live in RAM reachable through the direct map and
    // are only modified under the callers' serialisation.
    unsafe { &mut *(phys_to_virt(phys) as *mut [u64; 512]) }
}

pub fn kernel_pml4() -> u64 {
    KERNEL_PML4.load(Ordering::Relaxed)
}

fn index(virt: u64, level: u32) -> usize {
    ((virt >> (12 + 9 * level)) & 0x1FF) as usize
}

/// Returns the next-level table of `entry`, creating it if needed.
fn next_table(entry: &mut u64, user: bool) -> Option<u64> {
    if *entry & PRESENT == 0 {
        let phys = frame::alloc()?;
        *entry = phys | PRESENT | WRITE | if user { USER } else { 0 };
    } else if user {
        *entry |= USER;
    }
    assert!(*entry & HUGE == 0, "mapping inside a huge page");
    Some(*entry & ADDR_MASK)
}

static MAP_LOCK: crate::sync::IrqLock<()> = crate::sync::IrqLock::new(());

/// Maps one 4 KiB page in the address space `pml4`.
pub fn map_in(pml4: u64, virt: u64, phys: u64, flags: u64) -> Option<()> {
    let _guard = MAP_LOCK.lock();
    let user = flags & USER != 0;
    let mut t = pml4;
    for level in (1..4).rev() {
        t = next_table(&mut table(t)[index(virt, level)], user)?;
    }
    table(t)[index(virt, 0)] = (phys & ADDR_MASK) | flags | PRESENT;
    cpu::invlpg(virt);
    Some(())
}

/// Maps one 4 KiB page unless something is mapped there already (two
/// threads may fault on the same page at once: the second must not replace
/// what the first one made and may have written). Some(false) when it was
/// taken, None when page tables could not be made.
pub fn map_vacant_in(pml4: u64, virt: u64, phys: u64, flags: u64) -> Option<bool> {
    let _guard = MAP_LOCK.lock();
    let user = flags & USER != 0;
    let mut t = pml4;
    for level in (1..4).rev() {
        t = next_table(&mut table(t)[index(virt, level)], user)?;
    }
    let e = &mut table(t)[index(virt, 0)];
    if *e & PRESENT != 0 {
        return Some(false);
    }
    *e = (phys & ADDR_MASK) | flags | PRESENT;
    cpu::invlpg(virt);
    Some(true)
}

/// Points a 4 KiB page somewhere else if its entry is still `old` (the
/// accessed and dirty bits aside, which the processor sets on its own).
pub fn replace_in(pml4: u64, virt: u64, old: u64, phys: u64, flags: u64) -> bool {
    const SEEN: u64 = (1 << 5) | (1 << 6);
    let _guard = MAP_LOCK.lock();
    let Some((e, size)) = walk(pml4, virt) else { return false };
    if size != PAGE_SIZE || *e & !SEEN != old & !SEEN {
        return false;
    }
    *e = (phys & ADDR_MASK) | flags | PRESENT;
    cpu::invlpg(virt);
    true
}

/// The entry of a present 4 KiB page.
pub fn entry_in(pml4: u64, virt: u64) -> Option<u64> {
    let (e, size) = walk(pml4, virt)?;
    (size == PAGE_SIZE).then_some(*e)
}

/// Maps one 4 KiB page in the kernel half.
pub fn map(virt: u64, phys: u64, flags: u64) -> Option<()> {
    map_in(kernel_pml4(), virt, phys, flags)
}

fn map_huge(pml4: u64, virt: u64, phys: u64, flags: u64) {
    let mut t = pml4;
    for level in (2..4).rev() {
        t = next_table(&mut table(t)[index(virt, level)], false).expect("out of memory for page tables");
    }
    table(t)[index(virt, 1)] = phys | flags | PRESENT | HUGE;
}

/// Leaf entry of `virt` in `pml4`, with the size of the page it maps.
fn walk(pml4: u64, virt: u64) -> Option<(&'static mut u64, u64)> {
    let mut t = pml4;
    for level in (1..4).rev() {
        let e = &mut table(t)[index(virt, level)];
        if *e & PRESENT == 0 {
            return None;
        }
        if *e & HUGE != 0 {
            return Some((e, 1 << (12 + 9 * level)));
        }
        t = *e & ADDR_MASK;
    }
    let e = &mut table(t)[index(virt, 0)];
    (*e & PRESENT != 0).then_some((e, PAGE_SIZE))
}

/// Is the page at `virt` present and reachable from user mode?
pub fn user_accessible(pml4: u64, virt: u64) -> bool {
    let mut t = pml4;
    for level in (0..4).rev() {
        let e = table(t)[index(virt, level)];
        if e & PRESENT == 0 || e & USER == 0 {
            return false;
        }
        if level == 0 || e & HUGE != 0 {
            return true;
        }
        t = e & ADDR_MASK;
    }
    false
}

pub fn translate_in(pml4: u64, virt: u64) -> Option<u64> {
    let (e, size) = walk(pml4, virt)?;
    Some((*e & ADDR_MASK & !(size - 1)) + (virt & (size - 1)))
}

pub fn translate(virt: u64) -> Option<u64> {
    translate_in(kernel_pml4(), virt)
}

/// Unmaps a 4 KiB page and returns the frame it mapped.
pub fn unmap_in(pml4: u64, virt: u64) -> Option<u64> {
    let _guard = MAP_LOCK.lock();
    let (e, size) = walk(pml4, virt)?;
    assert_eq!(size, PAGE_SIZE, "unmapping part of a huge page");
    let phys = *e & ADDR_MASK;
    *e = 0;
    cpu::invlpg(virt);
    Some(phys)
}

/// Unmaps a user page and returns its frame for freeing, unless the page
/// is [`SHARED`] (then the frame belongs to someone else).
pub fn unmap_owned_in(pml4: u64, virt: u64) -> Option<u64> {
    let _guard = MAP_LOCK.lock();
    let (e, size) = walk(pml4, virt)?;
    assert_eq!(size, PAGE_SIZE, "unmapping part of a huge page");
    let old = *e;
    *e = 0;
    cpu::invlpg(virt);
    (old & SHARED == 0).then_some(old & ADDR_MASK)
}

pub fn unmap(virt: u64) -> Option<u64> {
    unmap_in(kernel_pml4(), virt)
}

/// Creates the kernel's page tables and switches to them.
pub fn init() {
    init_pat();
    let pml4 = frame::alloc().expect("out of memory");
    for e in table(pml4)[256..].iter_mut() {
        *e = frame::alloc().expect("out of memory") | PRESENT | WRITE;
    }

    // Direct map: all RAM, and at least the low 4 GiB (device memory).
    let top = align_up(frame::top().max(4 << 30), 1 << 30);
    let mut phys = 0;
    while phys < top {
        map_huge(pml4, HHDM + phys, phys, WRITE);
        phys += HUGE_SIZE;
    }
    // Kernel image: KERNEL_BASE maps the 2 MiB page the image starts in.
    let (start, end) = super::kernel_range();
    let base = start - super::KERNEL_LINK_OFFSET;
    let mut off = 0;
    while base + off < align_up(end, HUGE_SIZE) {
        map_huge(pml4, KERNEL_BASE + off, base + off, WRITE);
        off += HUGE_SIZE;
    }
    KERNEL_PML4.store(pml4, Ordering::Relaxed);
    // SAFETY: the new tables map the kernel image, its stack and the direct map.
    unsafe { cpu::write_cr3(pml4) };
}

/// Sets up the memory types [`WRITE_COMBINE`] relies on, on the processor
/// running this code (every processor needs the same).
pub fn init_pat() {
    // PAT entry 1 = write-combining (default: write-through).
    let pat = cpu::rdmsr(0x277);
    // SAFETY: only changes the memory type of PWT-only mappings, which the
    // kernel does not use yet on this processor.
    unsafe { cpu::wrmsr(0x277, (pat & !(0xFF << 8)) | (0x01 << 8)) };
}

/// A new address space sharing the kernel half.
pub fn new_address_space() -> Option<u64> {
    let pml4 = frame::alloc()?;
    let k = table(kernel_pml4());
    table(pml4)[256..].copy_from_slice(&k[256..]);
    Some(pml4)
}

/// A new address space with a private copy of every user page of `src`
/// (what `fork` gives the child), except shared pages, which both map.
/// None when memory runs out.
pub fn copy_address_space(src: u64) -> Option<u64> {
    fn copy_level(src: u64, dst: u64, level: u32) -> bool {
        let count = if level == 3 { 256 } else { 512 };
        for i in 0..count {
            let e = table(src)[i];
            if e & PRESENT == 0 || (level > 0 && e & HUGE != 0) {
                continue; // user memory is never mapped with huge pages
            }
            if level == 0 && e & SHARED != 0 {
                table(dst)[i] = e;
                continue;
            }
            let Some(phys) = frame::alloc() else { return false };
            table(dst)[i] = phys | (e & !ADDR_MASK);
            if level == 0 {
                // SAFETY: both frames are RAM reachable through the direct map.
                unsafe {
                    core::ptr::copy_nonoverlapping(phys_to_virt(e & ADDR_MASK) as *const u8, phys_to_virt(phys) as *mut u8, PAGE_SIZE as usize);
                }
            } else if !copy_level(e & ADDR_MASK, phys, level - 1) {
                return false;
            }
        }
        true
    }
    let dst = new_address_space()?;
    let ok = {
        let _guard = MAP_LOCK.lock();
        copy_level(src, dst, 3)
    };
    if !ok {
        free_address_space(dst);
        return None;
    }
    Some(dst)
}

/// Frees the user half of an address space: page tables and mapped frames
/// (not the frames of shared pages). Waits until no other processor has
/// it loaded.
pub fn free_address_space(pml4: u64) {
    crate::arch::smp::wait_unloaded(pml4);
    fn free_level(t: u64, level: u32) {
        for &e in table(t).iter() {
            if e & PRESENT == 0 {
                continue;
            }
            let next = e & ADDR_MASK;
            if level > 0 && e & HUGE == 0 {
                free_level(next, level - 1);
            } else if level == 0 && e & SHARED == 0 {
                frame::free(next);
            }
        }
        frame::free(t);
    }
    for i in 0..256 {
        let e = table(pml4)[i];
        if e & PRESENT != 0 {
            free_level(e & ADDR_MASK, 2);
        }
    }
    frame::free(pml4);
}
