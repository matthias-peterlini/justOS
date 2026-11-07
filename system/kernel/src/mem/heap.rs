//! The kernel heap: an address-ordered free list with coalescing, growing on
//! demand into the heap region. Small blocks that are freed wait in a list
//! of their size and are given out again as they are: most allocations are
//! small, and they then take constant time instead of a walk of the list.
//! Each processor keeps a few such blocks of each size for itself and takes
//! the heap's lock only when it runs out or has too many.
//!
//! Every block start and size is a multiple of 16, so splitting a free block
//! never leaves a fragment too small to describe itself.

use super::{align_up, frame, paging, HEAP_BASE, HEAP_MAX, PAGE_SIZE};
use crate::arch::cpu;
use crate::arch::smp::{self, MAX_CPUS};
use crate::sync::IrqLock;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::null_mut;

const UNIT: usize = 16;
const GROW_MIN: u64 = 1 << 20;
/// Blocks up to this size go to the lists by size when freed.
const QUICK_MAX: usize = 1024;
const QUICK_LISTS: usize = QUICK_MAX / UNIT;
/// Blocks of each size a processor keeps for itself, at most; it hands half
/// of them to the heap when it has this many and gets more again.
const CPU_KEEP: u16 = 32;

struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

struct Heap {
    head: *mut FreeBlock,
    top: u64,
    /// Freed blocks of 16, 32, ... QUICK_MAX bytes (linked by `next`).
    quick: [*mut FreeBlock; QUICK_LISTS],
}

// SAFETY: only reached through the lock.
unsafe impl Send for Heap {}

static HEAP: IrqLock<Heap> = IrqLock::new(Heap { head: null_mut(), top: HEAP_BASE, quick: [null_mut(); QUICK_LISTS] });

/// A processor's own small blocks, by size. Only that processor touches
/// it, with interrupts off.
struct CpuCache {
    lists: [*mut FreeBlock; QUICK_LISTS],
    counts: [u16; QUICK_LISTS],
    /// Bytes allocated on this processor minus bytes freed on it.
    used: isize,
}

struct PerCpu(UnsafeCell<CpuCache>);

// SAFETY: each processor only reaches its own entry (see `this_cache`).
unsafe impl Sync for PerCpu {}

static CPU_CACHES: [PerCpu; MAX_CPUS] =
    [const { PerCpu(UnsafeCell::new(CpuCache { lists: [null_mut(); QUICK_LISTS], counts: [0; QUICK_LISTS], used: 0 })) }; MAX_CPUS];

/// This processor's cache.
///
/// # Safety
/// Interrupts must be off until the reference is dropped (so nothing else
/// runs on this processor meanwhile), and no other reference to it may live.
unsafe fn this_cache() -> &'static mut CpuCache {
    // SAFETY: as the caller promises.
    unsafe { &mut *CPU_CACHES[smp::this_cpu()].0.get() }
}

/// Pushes a free block of list `class` onto `list`.
///
/// # Safety
/// `b` must be a free block of `(class + 1) * UNIT` bytes.
unsafe fn push(list: &mut *mut FreeBlock, b: *mut u8, class: usize) {
    let b = b as *mut FreeBlock;
    // SAFETY: as the caller promises (blocks are at least 16 bytes).
    unsafe { b.write(FreeBlock { size: (class + 1) * UNIT, next: *list }) };
    *list = b;
}

/// Pops a block off `list`, or null.
fn pop(list: &mut *mut FreeBlock) -> *mut u8 {
    let b = *list;
    if !b.is_null() {
        // SAFETY: the lists only link freed blocks.
        *list = unsafe { (*b).next };
    }
    b as *mut u8
}

impl Heap {
    /// Inserts a free range, merging it with its neighbours.
    ///
    /// # Safety
    /// The range must be unused heap memory.
    unsafe fn insert(&mut self, addr: usize, size: usize) {
        let mut prev: *mut FreeBlock = null_mut();
        let mut cur = self.head;
        // SAFETY: the list only links valid free blocks.
        unsafe {
            while !cur.is_null() && (cur as usize) < addr {
                prev = cur;
                cur = (*cur).next;
            }
            let block = addr as *mut FreeBlock;
            block.write(FreeBlock { size, next: cur });
            if !cur.is_null() && addr + size == cur as usize {
                (*block).size += (*cur).size;
                (*block).next = (*cur).next;
            }
            if prev.is_null() {
                self.head = block;
            } else if prev as usize + (*prev).size == addr {
                (*prev).size += (*block).size;
                (*prev).next = (*block).next;
            } else {
                (*prev).next = block;
            }
        }
    }

    fn take(&mut self, size: usize, align: usize) -> *mut u8 {
        let mut prev: *mut FreeBlock = null_mut();
        let mut cur = self.head;
        // SAFETY: the list only links valid free blocks.
        unsafe {
            while !cur.is_null() {
                let start = cur as usize;
                let end = start + (*cur).size;
                let aligned = align_up(start as u64, align as u64) as usize;
                if aligned + size <= end {
                    let next = (*cur).next;
                    // Unlink, then give back the front and back remainders.
                    if prev.is_null() {
                        self.head = next;
                    } else {
                        (*prev).next = next;
                    }
                    if aligned > start {
                        self.insert(start, aligned - start);
                    }
                    if aligned + size < end {
                        self.insert(aligned + size, end - aligned - size);
                    }
                    return aligned as *mut u8;
                }
                prev = cur;
                cur = (*cur).next;
            }
        }
        null_mut()
    }

    /// Gives every block in the lists by size (the heap's and this
    /// processor's) back to the address-ordered list, merging what can be
    /// merged: when memory runs short.
    fn drain_quick(&mut self, mine: &mut CpuCache) {
        for i in 0..QUICK_LISTS {
            let lists = [core::mem::replace(&mut self.quick[i], null_mut()), core::mem::replace(&mut mine.lists[i], null_mut())];
            for mut b in lists {
                while !b.is_null() {
                    // SAFETY: the lists only link freed blocks of this size.
                    unsafe {
                        let next = (*b).next;
                        self.insert(b as usize, (i + 1) * UNIT);
                        b = next;
                    }
                }
            }
            mine.counts[i] = 0;
        }
    }

    fn grow(&mut self, at_least: usize) -> bool {
        let bytes = align_up((at_least as u64).max(GROW_MIN), PAGE_SIZE);
        if self.top + bytes > HEAP_BASE + HEAP_MAX {
            return false;
        }
        let start = self.top;
        for i in 0..bytes / PAGE_SIZE {
            let Some(phys) = frame::alloc() else { return false };
            if paging::map(start + i * PAGE_SIZE, phys, paging::WRITE).is_none() {
                return false;
            }
            self.top += PAGE_SIZE;
        }
        // SAFETY: freshly mapped heap memory.
        unsafe { self.insert(start as usize, (self.top - start) as usize) };
        true
    }

    /// An allocation the processor's cache could not serve. A small one
    /// first takes up to half a cache's worth from the heap's list of its
    /// size.
    fn alloc(&mut self, mine: &mut CpuCache, size: usize, align: usize) -> *mut u8 {
        if align == UNIT && size <= QUICK_MAX {
            let class = size / UNIT - 1;
            let p = pop(&mut self.quick[class]);
            if !p.is_null() {
                while mine.counts[class] < CPU_KEEP / 2 {
                    let b = pop(&mut self.quick[class]);
                    if b.is_null() {
                        break;
                    }
                    // SAFETY: a free block of this size.
                    unsafe { push(&mut mine.lists[class], b, class) };
                    mine.counts[class] += 1;
                }
                return p;
            }
        }
        let p = self.take(size, align);
        if !p.is_null() {
            return p;
        }
        if !self.grow(size + align) {
            self.drain_quick(mine);
        }
        self.take(size, align)
    }
}

pub struct KernelAllocator;

// SAFETY: blocks are disjoint, aligned and at least the requested size.
unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = align_up(layout.size().max(1) as u64, UNIT as u64) as usize;
        let align = layout.align().max(UNIT);
        let irq = cpu::irq_save();
        // SAFETY: interrupts are off until the end; no other reference.
        let mine = unsafe { this_cache() };
        let mut p = null_mut();
        if align == UNIT && size <= QUICK_MAX {
            let class = size / UNIT - 1;
            p = pop(&mut mine.lists[class]);
            if !p.is_null() {
                mine.counts[class] -= 1;
            }
        }
        if p.is_null() {
            p = HEAP.lock().alloc(mine, size, align);
        }
        if !p.is_null() {
            mine.used += size as isize;
        }
        cpu::irq_restore(irq);
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let size = align_up(layout.size().max(1) as u64, UNIT as u64) as usize;
        let irq = cpu::irq_save();
        // SAFETY: interrupts are off until the end; no other reference.
        let mine = unsafe { this_cache() };
        mine.used -= size as isize;
        if size <= QUICK_MAX {
            let class = size / UNIT - 1;
            if mine.counts[class] >= CPU_KEEP {
                // Too many: half go to the heap's list.
                let mut heap = HEAP.lock();
                while mine.counts[class] > CPU_KEEP / 2 {
                    let b = pop(&mut mine.lists[class]);
                    // SAFETY: a free block of this size.
                    unsafe { push(&mut heap.quick[class], b, class) };
                    mine.counts[class] -= 1;
                }
            }
            // SAFETY: the block came from alloc with this size.
            unsafe { push(&mut mine.lists[class], ptr, class) };
            mine.counts[class] += 1;
        } else {
            // SAFETY: the block came from alloc with this layout.
            unsafe { HEAP.lock().insert(ptr as usize, size) };
        }
        cpu::irq_restore(irq);
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator;

pub fn init() {
    HEAP.lock().grow(4 << 20);
}

/// (bytes in use, bytes mapped)
pub fn stats() -> (usize, usize) {
    let used: isize = CPU_CACHES
        .iter()
        // SAFETY: a plain read of another processor's counter (it may be a
        // little out of date).
        .map(|c| unsafe { core::ptr::read_volatile(&(*c.0.get()).used) })
        .sum();
    (used.max(0) as usize, (HEAP.lock().top - HEAP_BASE) as usize)
}
