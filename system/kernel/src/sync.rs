//! Locks.
//!
//! * [`IrqLock`] masks interrupts while held and spins while another
//!   processor holds it: for state shared with interrupt handlers and for
//!   short critical sections. Never block while holding it.
//! * [`Mutex`] leaves interrupts on and puts a thread that finds it taken
//!   to sleep until it is let go: for long operations such as disk and file
//!   system access. Never take it from an interrupt handler.

use crate::arch::cpu;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub struct IrqLock<T: ?Sized> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: the lock serialises access to the value.
unsafe impl<T: ?Sized + Send> Sync for IrqLock<T> {}
unsafe impl<T: ?Sized + Send> Send for IrqLock<T> {}

pub struct IrqGuard<'a, T: ?Sized> {
    lock: &'a IrqLock<T>,
    irq: bool,
}

impl<T> IrqLock<T> {
    pub const fn new(value: T) -> IrqLock<T> {
        IrqLock { locked: AtomicBool::new(false), value: UnsafeCell::new(value) }
    }
}

impl<T: ?Sized> IrqLock<T> {
    #[track_caller]
    pub fn lock(&self) -> IrqGuard<'_, T> {
        let irq = cpu::irq_save();
        if self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            let start = contention::now();
            loop {
                // The holder may be another processor waiting for this one
                // to flush its TLB.
                crate::arch::smp::service();
                cpu::relax();
                if !self.locked.load(Ordering::Relaxed) && self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                    break;
                }
            }
            contention::record(core::panic::Location::caller(), start);
        }
        IrqGuard { lock: self, irq }
    }

    pub fn try_lock(&self) -> Option<IrqGuard<'_, T>> {
        let irq = cpu::irq_save();
        if self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            Some(IrqGuard { lock: self, irq })
        } else {
            cpu::irq_restore(irq);
            None
        }
    }

    /// Releases a lock whose holder can no longer release it (panic path).
    ///
    /// # Safety
    /// Only when the holder will never touch the value again.
    pub unsafe fn force_unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

impl<T: ?Sized> Deref for IrqGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the lock is held.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T: ?Sized> DerefMut for IrqGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the lock is held.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T: ?Sized> Drop for IrqGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        cpu::irq_restore(self.irq);
    }
}

pub struct Mutex<T: ?Sized> {
    locked: AtomicBool,
    /// Threads asleep waiting for it (counted, so that letting go skips the
    /// queue when nobody waits).
    waiting: AtomicUsize,
    queue: crate::task::WaitQueue,
    value: UnsafeCell<T>,
}

// SAFETY: the lock serialises access to the value.
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}

pub struct MutexGuard<'a, T: ?Sized> {
    lock: &'a Mutex<T>,
    /// Where it was taken, and when (for [`contention::HOLDS`]).
    at: &'static core::panic::Location<'static>,
    since: u64,
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Mutex<T> {
        Mutex { locked: AtomicBool::new(false), waiting: AtomicUsize::new(0), queue: crate::task::WaitQueue::new(), value: UnsafeCell::new(value) }
    }
}

impl<T: ?Sized> Mutex<T> {
    #[track_caller]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        if self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            let start = contention::now();
            // A holder on another processor may be about to let go: looking
            // a little costs less than sleeping and being woken.
            if crate::arch::smp::count() > 1 {
                for _ in 0..200 {
                    cpu::relax();
                    if !self.locked.load(Ordering::Relaxed) && self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                        contention::record(core::panic::Location::caller(), start);
                        crate::task::mutex_taken();
                        return MutexGuard { lock: self, at: core::panic::Location::caller(), since: contention::now() };
                    }
                }
            }
            // Counted before trying (both sequentially consistent, as is
            // letting go): either the holder sees the count and wakes the
            // queue, or this thread sees the lock free.
            self.waiting.fetch_add(1, Ordering::SeqCst);
            self.queue.wait_for(|| self.locked.compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed).is_ok().then_some(()));
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            contention::record(core::panic::Location::caller(), start);
        }
        crate::task::mutex_taken();
        MutexGuard { lock: self, at: core::panic::Location::caller(), since: contention::now() }
    }

    /// The lock if it is free now.
    #[track_caller]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        let at = core::panic::Location::caller();
        self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).ok().map(|_| {
            crate::task::mutex_taken();
            MutexGuard { lock: self, at, since: contention::now() }
        })
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the lock is held.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the lock is held.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        contention::HOLDS.add(self.at, contention::now().saturating_sub(self.since));
        self.lock.locked.store(false, Ordering::SeqCst);
        if self.lock.waiting.load(Ordering::SeqCst) != 0 {
            self.lock.queue.notify_all();
        }
        crate::task::mutex_released();
    }
}

/// Where locks were found taken (for each place in the code that waited,
/// how often and how long, in time stamp counter cycles) and where
/// [`Mutex`]es were held (how often and how long). The shell's `locks`
/// shows both.
pub mod contention {
    use alloc::vec::Vec;
    use core::panic::Location;
    use core::ptr;
    use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

    const SLOTS: usize = 256;

    /// Counts and cycles by place, in an open-addressed table.
    pub struct Table {
        sites: [AtomicPtr<Location<'static>>; SLOTS],
        counts: [AtomicU64; SLOTS],
        cycles: [AtomicU64; SLOTS],
    }

    pub static WAITS: Table = Table::new();
    pub static HOLDS: Table = Table::new();

    pub fn now() -> u64 {
        // SAFETY: reading the time stamp counter has no side effects.
        unsafe { core::arch::x86_64::_rdtsc() }
    }

    /// A wait for a lock at `at` that began at `start`.
    pub fn record(at: &'static Location<'static>, start: u64) {
        WAITS.add(at, now().saturating_sub(start));
    }

    impl Table {
        const fn new() -> Table {
            Table {
                sites: [const { AtomicPtr::new(ptr::null_mut()) }; SLOTS],
                counts: [const { AtomicU64::new(0) }; SLOTS],
                cycles: [const { AtomicU64::new(0) }; SLOTS],
            }
        }

        pub fn add(&self, at: &'static Location<'static>, cycles: u64) {
            let key = at as *const Location<'static> as *mut Location<'static>;
            let mut i = (key as usize >> 4) % SLOTS;
            for _ in 0..SLOTS {
                let cur = self.sites[i].load(Ordering::Acquire);
                let mine = cur == key || (cur.is_null() && self.sites[i].compare_exchange(ptr::null_mut(), key, Ordering::AcqRel, Ordering::Acquire).map_or_else(|k| k == key, |_| true));
                if mine {
                    self.counts[i].fetch_add(1, Ordering::Relaxed);
                    self.cycles[i].fetch_add(cycles, Ordering::Relaxed);
                    return;
                }
                i = (i + 1) % SLOTS;
            }
        }

        /// (place, count, cycles), most cycles first.
        pub fn top(&self, n: usize) -> Vec<(&'static Location<'static>, u64, u64)> {
            let mut v: Vec<_> = (0..SLOTS)
                .filter_map(|i| {
                    let p = self.sites[i].load(Ordering::Acquire);
                    // SAFETY: only `&'static Location`s are stored.
                    (!p.is_null()).then(|| (unsafe { &*p }, self.counts[i].load(Ordering::Relaxed), self.cycles[i].load(Ordering::Relaxed)))
                })
                .filter(|e| e.1 != 0)
                .collect();
            v.sort_by(|a, b| b.2.cmp(&a.2));
            v.truncate(n);
            v
        }

        pub fn reset(&self) {
            for i in 0..SLOTS {
                self.counts[i].store(0, Ordering::Relaxed);
                self.cycles[i].store(0, Ordering::Relaxed);
            }
        }
    }
}
