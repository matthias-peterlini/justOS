//! The program heap: a free list over memory from the MAP system call.

use crate::sys;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::null_mut;

const UNIT: usize = 16;

struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

struct Heap(UnsafeCell<*mut FreeBlock>);

// SAFETY: programs have a single thread.
unsafe impl Sync for Heap {}

static HEAP: Heap = Heap(UnsafeCell::new(null_mut()));

fn align_up(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

unsafe fn insert(head: *mut *mut FreeBlock, addr: usize, size: usize) {
    // SAFETY: the list only links free blocks.
    unsafe {
        let mut prev: *mut FreeBlock = null_mut();
        let mut cur = *head;
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
            *head = block;
        } else if prev as usize + (*prev).size == addr {
            (*prev).size += (*block).size;
            (*prev).next = (*block).next;
        } else {
            (*prev).next = block;
        }
    }
}

unsafe fn take(head: *mut *mut FreeBlock, size: usize, align: usize) -> *mut u8 {
    // SAFETY: the list only links free blocks.
    unsafe {
        let mut prev: *mut FreeBlock = null_mut();
        let mut cur = *head;
        while !cur.is_null() {
            let start = cur as usize;
            let end = start + (*cur).size;
            let at = align_up(start, align);
            if at + size <= end {
                let next = (*cur).next;
                if prev.is_null() {
                    *head = next;
                } else {
                    (*prev).next = next;
                }
                if at > start {
                    insert(head, start, at - start);
                }
                if at + size < end {
                    insert(head, at + size, end - at - size);
                }
                return at as *mut u8;
            }
            prev = cur;
            cur = (*cur).next;
        }
        null_mut()
    }
}

struct Allocator;

// SAFETY: hands out disjoint, aligned blocks of at least the asked size.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = align_up(layout.size().max(1), UNIT);
        let align = layout.align().max(UNIT);
        let head = HEAP.0.get();
        // SAFETY: single-threaded access to the list.
        unsafe {
            let p = take(head, size, align);
            if !p.is_null() {
                return p;
            }
            let grow = align_up((size + align).max(256 * 1024), 4096);
            let Ok(at) = sys::check(sys::call(sys::MAP, grow as u64, 0, 0, 0, 0)) else { return null_mut() };
            insert(head, at as usize, grow);
            take(head, size, align)
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let size = align_up(layout.size().max(1), UNIT);
        // SAFETY: the block came from alloc with this layout.
        unsafe { insert(HEAP.0.get(), ptr as usize, size) };
    }
}

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
