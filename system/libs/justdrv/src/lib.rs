//! Run-time support for justOS driver modules.
//!
//! A driver crate is a `#![no_std]`, `#![no_main]` binary that depends on
//! `justdrv` and declares itself with [`driver!`]:
//!
//! ```ignore
//! justdrv::driver! {
//!     name: "ps2kbd",
//!     version: "1.0.0",
//!     probe: probe,
//! }
//!
//! fn probe(dev: &justdrv::DeviceInfo) -> justdrv::Status { ... }
//! ```
//!
//! The module then has a heap (`alloc` works), a panic handler, logging
//! macros ([`info!`](crate::info), [`warn!`](crate::warn), [`error!`](crate::error))
//! and safe-ish helpers for port I/O, MMIO, DMA, PCI and interrupts.

#![no_std]

pub use justdk::*;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

static API: AtomicPtr<KernelApi> = AtomicPtr::new(core::ptr::null_mut());
static NAME: AtomicPtr<&'static str> = AtomicPtr::new(core::ptr::null_mut());

/// The kernel services. Only valid once the entry point ran.
#[inline]
pub fn api() -> &'static KernelApi {
    // SAFETY: set once by the entry point before any driver code runs; the
    // table lives as long as the kernel.
    unsafe { &*API.load(Ordering::Relaxed) }
}

/// Called by the code [`driver!`] generates. Returns false if the kernel
/// speaks another ABI version.
///
/// # Safety
/// `table` must be the kernel's API table.
#[doc(hidden)]
pub unsafe fn init(table: *const KernelApi, name: &'static &'static str) -> bool {
    // SAFETY: the caller passes the kernel's table.
    let (version, size) = unsafe { ((*table).abi_version, (*table).size) };
    if version != ABI_VERSION || (size as usize) < core::mem::size_of::<KernelApi>() {
        return false;
    }
    API.store(table as *mut KernelApi, Ordering::Relaxed);
    NAME.store(name as *const &str as *mut &str, Ordering::Relaxed);
    true
}

// ----------------------------------------------------------------------------
// Declaring a driver

/// Declares the module's entry point and its [`DriverInfo`].
///
/// `probe` is a `fn(&DeviceInfo) -> Status`; the optional `remove` a
/// `fn(u32)` taking the device id.
#[macro_export]
macro_rules! driver {
    (name: $name:expr, version: $version:expr, probe: $probe:path $(, remove: $remove:path)? $(,)?) => {
        unsafe extern "C" fn __just_probe(dev: *const $crate::DeviceInfo) -> $crate::Status {
            // SAFETY: the kernel passes a valid device for the call's duration.
            $probe(unsafe { &*dev })
        }

        $(
            unsafe extern "C" fn __just_remove(dev: u32) {
                $remove(dev)
            }
        )?

        static __JUST_DRIVER_INFO: $crate::DriverInfo = $crate::DriverInfo {
            magic: $crate::DRIVER_MAGIC,
            abi_version: $crate::ABI_VERSION,
            name: $crate::Str::new($name),
            version: $crate::Str::new($version),
            probe: __just_probe,
            remove: $crate::driver!(@remove $($remove)?),
        };

        #[no_mangle]
        pub unsafe extern "C" fn just_driver_entry(api: *const $crate::KernelApi) -> *const $crate::DriverInfo {
            static NAME: &str = $name;
            // SAFETY: the kernel calls the entry with its API table.
            if !unsafe { $crate::init(api, &NAME) } {
                return core::ptr::null();
            }
            &__JUST_DRIVER_INFO
        }
    };
    (@remove) => { None };
    (@remove $remove:path) => { Some(__just_remove) };
}

// ----------------------------------------------------------------------------
// Logging

struct LineBuf {
    buf: [u8; 256],
    len: usize,
}

impl Write for LineBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

#[doc(hidden)]
pub fn log(level: u32, args: fmt::Arguments<'_>) {
    let mut line = LineBuf { buf: [0; 256], len: 0 };
    let name = NAME.load(Ordering::Relaxed);
    if !name.is_null() {
        // SAFETY: points at the static set by init.
        let _ = line.write_str(unsafe { *name });
        let _ = line.write_str(": ");
    }
    let _ = line.write_fmt(args);
    // SAFETY: valid buffer for the call.
    unsafe { (api().log)(level, line.buf.as_ptr(), line.len) }
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log($crate::LOG_INFO, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log($crate::LOG_WARN, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::log($crate::LOG_ERROR, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => { $crate::log($crate::LOG_DEBUG, format_args!($($arg)*)) };
}

// ----------------------------------------------------------------------------
// Heap and panics

struct KernelHeap;

// SAFETY: forwards to the kernel allocator, which upholds GlobalAlloc's rules.
unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { (api().alloc)(layout.size(), layout.align()) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { (api().free)(ptr, layout.size(), layout.align()) }
    }
}

#[global_allocator]
static HEAP: KernelHeap = KernelHeap;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    log(LOG_ERROR, format_args!("driver panic: {}", info));
    // The module cannot unwind: park this thread for good.
    loop {
        // SAFETY: kernel call.
        unsafe { (api().sleep_ms)(1000) }
    }
}

// ----------------------------------------------------------------------------
// Time

pub fn time_ms() -> u64 {
    // SAFETY: kernel call.
    unsafe { (api().time_ms)() }
}

pub fn sleep_ms(ms: u64) {
    // SAFETY: kernel call.
    unsafe { (api().sleep_ms)(ms) }
}

pub fn yield_now() {
    // SAFETY: kernel call.
    unsafe { (api().yield_now)() }
}

/// Polls `cond` until it holds or `timeout_ms` pass. Returns whether it held.
pub fn wait_until(timeout_ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let start = time_ms();
    loop {
        if cond() {
            return true;
        }
        if time_ms().saturating_sub(start) >= timeout_ms {
            return cond();
        }
        core::hint::spin_loop();
    }
}

/// Starts a kernel thread running `entry(ctx)`.
// `ctx` is only handed back to `entry`, which knows what it points to.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn spawn(name: &str, entry: ThreadEntry, ctx: *mut c_void) -> Status {
    // SAFETY: valid name for the call.
    unsafe { (api().spawn)(name.as_ptr(), name.len(), entry, ctx) }
}

// ----------------------------------------------------------------------------
// Port I/O

pub mod port {
    use core::arch::asm;

    #[inline]
    pub fn inb(port: u16) -> u8 {
        let v: u8;
        // SAFETY: drivers run in ring 0; port I/O has no memory effects.
        unsafe { asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags)) };
        v
    }

    #[inline]
    pub fn outb(port: u16, v: u8) {
        // SAFETY: see inb.
        unsafe { asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags)) };
    }

    #[inline]
    pub fn inw(port: u16) -> u16 {
        let v: u16;
        // SAFETY: see inb.
        unsafe { asm!("in ax, dx", out("ax") v, in("dx") port, options(nomem, nostack, preserves_flags)) };
        v
    }

    #[inline]
    pub fn outw(port: u16, v: u16) {
        // SAFETY: see inb.
        unsafe { asm!("out dx, ax", in("dx") port, in("ax") v, options(nomem, nostack, preserves_flags)) };
    }

    #[inline]
    pub fn inl(port: u16) -> u32 {
        let v: u32;
        // SAFETY: see inb.
        unsafe { asm!("in eax, dx", out("eax") v, in("dx") port, options(nomem, nostack, preserves_flags)) };
        v
    }

    #[inline]
    pub fn outl(port: u16, v: u32) {
        // SAFETY: see inb.
        unsafe { asm!("out dx, eax", in("dx") port, in("eax") v, options(nomem, nostack, preserves_flags)) };
    }

    /// Reads `buf.len()` 16-bit words from one port (ATA data transfers).
    #[inline]
    pub fn insw(port: u16, buf: &mut [u16]) {
        // SAFETY: writes exactly buf.len() words into buf.
        unsafe {
            asm!("rep insw", in("dx") port, inout("rdi") buf.as_mut_ptr() => _, inout("rcx") buf.len() => _, options(nostack, preserves_flags))
        };
    }

    /// Writes `buf.len()` 16-bit words to one port.
    #[inline]
    pub fn outsw(port: u16, buf: &[u16]) {
        // SAFETY: reads exactly buf.len() words from buf.
        unsafe {
            asm!("rep outsw", in("dx") port, inout("rsi") buf.as_ptr() => _, inout("rcx") buf.len() => _, options(nostack, preserves_flags))
        };
    }

    /// Short delay (~1 µs) by writing to the POST diagnostic port.
    #[inline]
    pub fn io_wait() {
        outb(0x80, 0);
    }
}

// ----------------------------------------------------------------------------
// MMIO and DMA

/// A mapped device register window.
#[derive(Clone, Copy)]
pub struct Mmio {
    base: *mut u8,
    size: usize,
}

// SAFETY: device registers are shared hardware state; access is volatile.
unsafe impl Send for Mmio {}
unsafe impl Sync for Mmio {}

impl Mmio {
    /// Maps `size` bytes of device memory at physical `phys`.
    pub fn map(phys: u64, size: usize) -> Option<Mmio> {
        // SAFETY: kernel call.
        let base = unsafe { (api().map_mmio)(phys, size) };
        (!base.is_null()).then_some(Mmio { base, size })
    }

    pub fn base(&self) -> *mut u8 {
        self.base
    }

    #[inline]
    pub fn read32(&self, off: usize) -> u32 {
        assert!(off + 4 <= self.size);
        // SAFETY: inside the mapped window.
        unsafe { core::ptr::read_volatile(self.base.add(off) as *const u32) }
    }

    #[inline]
    pub fn write32(&self, off: usize, v: u32) {
        assert!(off + 4 <= self.size);
        // SAFETY: inside the mapped window.
        unsafe { core::ptr::write_volatile(self.base.add(off) as *mut u32, v) }
    }

    #[inline]
    pub fn read64(&self, off: usize) -> u64 {
        self.read32(off) as u64 | (self.read32(off + 4) as u64) << 32
    }

    #[inline]
    pub fn write64(&self, off: usize, v: u64) {
        self.write32(off, v as u32);
        self.write32(off + 4, (v >> 32) as u32);
    }

    #[inline]
    pub fn read16(&self, off: usize) -> u16 {
        assert!(off + 2 <= self.size);
        // SAFETY: inside the mapped window.
        unsafe { core::ptr::read_volatile(self.base.add(off) as *const u16) }
    }

    #[inline]
    pub fn write16(&self, off: usize, v: u16) {
        assert!(off + 2 <= self.size);
        // SAFETY: inside the mapped window.
        unsafe { core::ptr::write_volatile(self.base.add(off) as *mut u16, v) }
    }

    #[inline]
    pub fn read8(&self, off: usize) -> u8 {
        assert!(off < self.size);
        // SAFETY: inside the mapped window.
        unsafe { core::ptr::read_volatile(self.base.add(off)) }
    }

    #[inline]
    pub fn write8(&self, off: usize, v: u8) {
        assert!(off < self.size);
        // SAFETY: inside the mapped window.
        unsafe { core::ptr::write_volatile(self.base.add(off), v) }
    }
}

/// Registers reachable either through I/O ports or through memory, for
/// devices that offer both (many network cards).
#[derive(Clone, Copy)]
pub enum Regs {
    Port(u16),
    Mem(Mmio),
}

impl Regs {
    /// The first usable window of `dev`: memory if `prefer_memory`, else
    /// I/O ports, falling back to the other kind. `size` is the length
    /// mapped for a memory window.
    pub fn find(dev: &DeviceInfo, prefer_memory: bool, size: usize) -> Option<Regs> {
        let pick = |memory: bool| {
            dev.resources.iter().find_map(|r| match r.kind {
                RES_MEMORY if memory && r.base != 0 => Mmio::map(r.base, size).map(Regs::Mem),
                RES_IO if !memory && r.base != 0 && r.base <= 0xFFFF => Some(Regs::Port(r.base as u16)),
                _ => None,
            })
        };
        let regs = pick(prefer_memory).or_else(|| pick(!prefer_memory))?;
        Pci(dev.id).enable(matches!(regs, Regs::Port(_)), matches!(regs, Regs::Mem(_)), true);
        Some(regs)
    }

    #[inline]
    pub fn read8(&self, off: u16) -> u8 {
        match self {
            Regs::Port(p) => port::inb(p + off),
            Regs::Mem(m) => m.read8(off as usize),
        }
    }

    #[inline]
    pub fn read16(&self, off: u16) -> u16 {
        match self {
            Regs::Port(p) => port::inw(p + off),
            Regs::Mem(m) => m.read16(off as usize),
        }
    }

    #[inline]
    pub fn read32(&self, off: u16) -> u32 {
        match self {
            Regs::Port(p) => port::inl(p + off),
            Regs::Mem(m) => m.read32(off as usize),
        }
    }

    #[inline]
    pub fn write8(&self, off: u16, v: u8) {
        match self {
            Regs::Port(p) => port::outb(p + off, v),
            Regs::Mem(m) => m.write8(off as usize, v),
        }
    }

    #[inline]
    pub fn write16(&self, off: u16, v: u16) {
        match self {
            Regs::Port(p) => port::outw(p + off, v),
            Regs::Mem(m) => m.write16(off as usize, v),
        }
    }

    #[inline]
    pub fn write32(&self, off: u16, v: u32) {
        match self {
            Regs::Port(p) => port::outl(p + off, v),
            Regs::Mem(m) => m.write32(off as usize, v),
        }
    }
}

/// A physically contiguous, zeroed buffer a device can read and write.
pub struct Dma {
    virt: *mut u8,
    phys: u64,
    size: usize,
}

// SAFETY: plain memory owned by this value.
unsafe impl Send for Dma {}
unsafe impl Sync for Dma {}

impl Dma {
    pub fn new(size: usize) -> Option<Dma> {
        let mut phys = 0u64;
        // SAFETY: kernel call with a valid out pointer.
        let virt = unsafe { (api().dma_alloc)(size, &mut phys) };
        (!virt.is_null()).then_some(Dma { virt, phys, size })
    }

    pub fn phys(&self) -> u64 {
        self.phys
    }

    pub fn len(&self) -> usize {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.virt
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: owned allocation of `size` bytes.
        unsafe { core::slice::from_raw_parts(self.virt, self.size) }
    }

    #[allow(clippy::mut_from_ref)]
    pub fn as_mut_slice(&self) -> &mut [u8] {
        // SAFETY: owned allocation; the device is the only other writer.
        unsafe { core::slice::from_raw_parts_mut(self.virt, self.size) }
    }

    /// Volatile access to a `T` at byte offset `off` (descriptors shared with
    /// the device).
    pub fn read<T: Copy>(&self, off: usize) -> T {
        assert!(off + core::mem::size_of::<T>() <= self.size);
        // SAFETY: in bounds; T is plain data.
        unsafe { core::ptr::read_volatile(self.virt.add(off) as *const T) }
    }

    pub fn write<T: Copy>(&self, off: usize, v: T) {
        assert!(off + core::mem::size_of::<T>() <= self.size);
        // SAFETY: in bounds; T is plain data.
        unsafe { core::ptr::write_volatile(self.virt.add(off) as *mut T, v) }
    }
}

impl Drop for Dma {
    fn drop(&mut self) {
        // SAFETY: allocated by dma_alloc with this size.
        unsafe { (api().dma_free)(self.virt, self.size) }
    }
}

/// Physical address of kernel-heap memory (for buffers passed to devices).
pub fn virt_to_phys<T>(ptr: *const T) -> u64 {
    // SAFETY: kernel call.
    unsafe { (api().virt_to_phys)(ptr as *const u8) }
}

// ----------------------------------------------------------------------------
// PCI

/// Configuration-space access for a PCI device the driver owns.
#[derive(Clone, Copy)]
pub struct Pci(pub u32);

impl Pci {
    pub fn read8(&self, off: u16) -> u8 {
        // SAFETY: kernel call.
        unsafe { (api().pci_read)(self.0, off, 1) as u8 }
    }
    pub fn read16(&self, off: u16) -> u16 {
        // SAFETY: kernel call.
        unsafe { (api().pci_read)(self.0, off, 2) as u16 }
    }
    pub fn read32(&self, off: u16) -> u32 {
        // SAFETY: kernel call.
        unsafe { (api().pci_read)(self.0, off, 4) }
    }
    pub fn write16(&self, off: u16, v: u16) {
        // SAFETY: kernel call.
        unsafe { (api().pci_write)(self.0, off, 2, v as u32) }
    }
    pub fn write32(&self, off: u16, v: u32) {
        // SAFETY: kernel call.
        unsafe { (api().pci_write)(self.0, off, 4, v) }
    }

    /// The next capability with this id in the device's list, after the one
    /// at `after` (0: from the start).
    pub fn find_capability(&self, id: u8, after: u16) -> Option<u16> {
        if self.read16(0x06) & (1 << 4) == 0 {
            return None;
        }
        let mut passed = after == 0;
        let mut at = (self.read8(0x34) & 0xFC) as u16;
        for _ in 0..48 {
            if at < 0x40 {
                return None;
            }
            if passed && self.read8(at) == id {
                return Some(at);
            }
            passed |= at == after;
            at = (self.read8(at + 1) & 0xFC) as u16;
        }
        None
    }

    /// Enables memory/I-O decoding and bus mastering (needed for DMA).
    pub fn enable(&self, io: bool, memory: bool, bus_master: bool) {
        let mut cmd = self.read16(0x04);
        if io {
            cmd |= 1;
        }
        if memory {
            cmd |= 2;
        }
        if bus_master {
            cmd |= 4;
        }
        self.write16(0x04, cmd);
    }
}

// ----------------------------------------------------------------------------
// Interrupts and locks

/// Attaches `handler(ctx)` to a legacy interrupt line of `device`.
// `ctx` is only handed back to `handler`, which knows what it points to.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn irq_attach(device: u32, irq: u8, handler: IrqHandler, ctx: *mut c_void) -> Status {
    // SAFETY: kernel call.
    unsafe { (api().irq_attach)(device, irq, handler, ctx) }
}

pub fn irq_detach(device: u32, irq: u8) {
    // SAFETY: kernel call.
    unsafe { (api().irq_detach)(device, irq) }
}

/// Routes the device's MSI-X/MSI to `handler(ctx)`. Returns the vector, or
/// `E_UNSUPPORTED` when only the legacy line is available.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn irq_attach_msi(device: u32, handler: IrqHandler, ctx: *mut c_void) -> Status {
    // SAFETY: kernel call.
    unsafe { (api().irq_attach_msi)(device, handler, ctx) }
}

/// Attaches `handler` to the device's message interrupt if it has one,
/// else to its legacy line. Returns whether an interrupt is attached
/// (without one, the driver has to poll).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn irq_attach_any(dev: &DeviceInfo, handler: IrqHandler, ctx: *mut c_void) -> bool {
    if irq_attach_msi(dev.id, handler, ctx) >= 0 {
        return true;
    }
    dev.irq != IRQ_NONE && irq_attach(dev.id, dev.irq, handler, ctx) == OK
}

/// A flag a thread waits on until another thread or an interrupt handler
/// sets it.
pub struct Event(justdk::Event);

// SAFETY: kernel events are made to be shared between threads and IRQs.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    pub fn new() -> Event {
        // SAFETY: kernel call.
        Event(unsafe { (api().event_new)() })
    }

    /// Sets the event (safe in interrupt handlers).
    pub fn signal(&self) {
        // SAFETY: a live event.
        unsafe { (api().event_signal)(self.0) }
    }

    /// Waits until the event is set, then clears it. Returns false after
    /// `timeout_ms` (`WAIT_FOREVER` for no limit).
    pub fn wait(&self, timeout_ms: u64) -> bool {
        // SAFETY: a live event.
        unsafe { (api().event_wait)(self.0, timeout_ms) == OK }
    }
}

impl Default for Event {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: created by event_new, freed once.
        unsafe { (api().event_free)(self.0) }
    }
}

/// A spin lock that also masks interrupts, so state shared with an interrupt
/// handler can be locked from both sides.
pub struct IrqLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: access to the value is serialised by the lock.
unsafe impl<T: Send> Sync for IrqLock<T> {}
unsafe impl<T: Send> Send for IrqLock<T> {}

pub struct IrqGuard<'a, T> {
    lock: &'a IrqLock<T>,
    flags: u64,
}

impl<T> IrqLock<T> {
    pub const fn new(value: T) -> IrqLock<T> {
        IrqLock { locked: AtomicBool::new(false), value: UnsafeCell::new(value) }
    }

    pub fn lock(&self) -> IrqGuard<'_, T> {
        let flags: u64;
        // SAFETY: saves RFLAGS and masks interrupts (ring 0).
        unsafe { core::arch::asm!("pushfq; pop {}; cli", out(reg) flags, options(nomem)) };
        while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        IrqGuard { lock: self, flags }
    }
}

impl<T> core::ops::Deref for IrqGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the lock is held.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> core::ops::DerefMut for IrqGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the lock is held.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for IrqGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        if self.flags & (1 << 9) != 0 {
            // SAFETY: interrupts were enabled when the lock was taken.
            unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
        }
    }
}

/// A lock for long operations (device I/O): interrupts stay enabled and
/// waiting threads yield the CPU. Never take it in an interrupt handler.
pub struct Mutex<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: access to the value is serialised by the lock.
unsafe impl<T: Send> Sync for Mutex<T> {}
unsafe impl<T: Send> Send for Mutex<T> {}

pub struct MutexGuard<'a, T> {
    lock: &'a Mutex<T>,
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Mutex<T> {
        Mutex { locked: AtomicBool::new(false), value: UnsafeCell::new(value) }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            yield_now();
        }
        MutexGuard { lock: self }
    }
}

impl<T> core::ops::Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the lock is held.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> core::ops::DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the lock is held.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

// ----------------------------------------------------------------------------
// Device classes

/// Hands a disk to the kernel. `desc` must stay alive while the disk exists
/// (leak it or keep it in a static).
pub fn block_register(device: u32, desc: &'static BlockDesc) -> Status {
    // SAFETY: kernel call with a valid descriptor.
    unsafe { (api().block_register)(device, desc) }
}

pub fn input_event(device: u32, kind: u32, code: u32, value: i32) {
    // SAFETY: kernel call.
    unsafe { (api().input_event)(device, kind, code, value) }
}

pub fn net_register(device: u32, desc: &'static NetDesc) -> Status {
    // SAFETY: kernel call with a valid descriptor.
    unsafe { (api().net_register)(device, desc) }
}

pub fn net_receive(iface: u32, frame: &[u8]) {
    // SAFETY: kernel call with a valid buffer.
    unsafe { (api().net_receive)(iface, frame.as_ptr(), frame.len()) }
}

/// Adds a device this driver found (a USB device) as a child of `parent`.
/// Returns its id or an error.
pub fn device_add(parent: u32, info: &DeviceInfo, description: &str, flags: u32) -> Status {
    let d = Str { ptr: description.as_ptr(), len: description.len() };
    // SAFETY: kernel call with valid pointers for the call's duration.
    unsafe { (api().device_add)(parent, info, d, flags) }
}

/// A device this driver added went away. `OK`: no driver uses its
/// interface any more.
pub fn device_remove(device: u32) -> Status {
    // SAFETY: kernel call.
    unsafe { (api().device_remove)(device) }
}

pub fn audio_register(device: u32, desc: &'static AudioDesc) -> Status {
    // SAFETY: kernel call with a valid descriptor.
    unsafe { (api().audio_register)(device, desc) }
}

pub fn display_register(device: u32, desc: &'static DisplayDesc) -> Status {
    // SAFETY: kernel call with a valid descriptor.
    unsafe { (api().display_register)(device, desc) }
}

/// Maps a framebuffer (write-combined device memory).
pub fn map_framebuffer(phys: u64, size: usize) -> Option<*mut u8> {
    // SAFETY: kernel call.
    let p = unsafe { (api().map_framebuffer)(phys, size) };
    (!p.is_null()).then_some(p)
}

/// The framebuffer the firmware set up before the kernel started.
pub fn boot_framebuffer() -> Option<Framebuffer> {
    let mut fb = Framebuffer::default();
    // SAFETY: kernel call with a valid out pointer.
    (unsafe { (api().boot_framebuffer)(&mut fb) } == OK).then_some(fb)
}

/// Registers what a device does when the machine goes to sleep and wakes
/// up (see [`PowerHooks`]).
pub fn power_hooks(device: u32, ctx: *mut c_void, suspend: Option<PowerHook>, resume: PowerHook) -> Status {
    let hooks = PowerHooks { ctx, suspend, resume };
    // SAFETY: kernel call; the kernel copies the hooks.
    unsafe { (api().power_hooks)(device, &hooks) }
}

/// Copies an ASCII model name into the fixed field of [`BlockDesc`].
pub fn model_name(name: &str) -> [u8; 40] {
    let mut out = [0u8; 40];
    let n = name.len().min(39);
    out[..n].copy_from_slice(&name.as_bytes()[..n]);
    out
}

// ----------------------------------------------------------------------------
// Sound

/// The ring buffer most sound devices play from: the device reads it in a
/// loop while the driver writes ahead of it. [`PcmRing`] keeps track of both
/// positions; the driver only says where the device is.
pub mod pcm {
    use super::{Dma, Event, Mutex};

    /// What a sound device's driver provides to a [`PcmRing`].
    pub trait Hardware: Sync {
        /// Byte offset in the ring the device is playing.
        fn position(&self) -> usize;
        /// Starts playing the ring from its beginning.
        fn start(&self);
        fn stop(&self);
    }

    struct State {
        running: bool,
        /// The machine sleeps: the device is off and writers wait.
        paused: bool,
        /// Bytes written since the start (not wrapped).
        written: u64,
        /// Bytes of whole laps the device played.
        laps: u64,
        last: usize,
    }

    pub struct PcmRing<H: Hardware> {
        pub hw: H,
        buf: Dma,
        size: usize,
        state: Mutex<State>,
        /// Signalled by the driver's interrupt at every period, so writers
        /// waiting for room wake up early; without it they poll.
        pub progress: Event,
    }

    impl<H: Hardware> PcmRing<H> {
        /// A ring of `buf.len()` bytes (a multiple of the frame size).
        pub fn new(hw: H, buf: Dma) -> PcmRing<H> {
            let size = buf.len();
            PcmRing {
                hw,
                buf,
                size,
                state: Mutex::new(State { running: false, paused: false, written: 0, laps: 0, last: 0 }),
                progress: Event::new(),
            }
        }

        /// Before sleep: the device stops (what was queued is dropped) and
        /// writers wait until [`PcmRing::unpause`].
        pub fn pause(&self) {
            let mut st = self.state.lock();
            if st.running {
                self.hw.stop();
            }
            st.running = false;
            st.paused = true;
        }

        /// After sleep, once the device is set up again: playing goes on
        /// with the next sound written.
        pub fn unpause(&self) {
            self.state.lock().paused = false;
            self.progress.signal();
        }

        pub fn phys(&self) -> u64 {
            self.buf.phys()
        }

        pub fn size(&self) -> usize {
            self.size
        }

        /// Bytes the device has played since the start.
        fn played(&self, st: &mut State) -> u64 {
            let pos = self.hw.position().min(self.size - 1);
            if pos < st.last {
                st.laps += self.size as u64;
            }
            st.last = pos;
            st.laps + pos as u64
        }

        fn copy_in(&self, at: u64, data: &[u8]) {
            let ring = self.buf.as_mut_slice();
            let start = (at % self.size as u64) as usize;
            let first = data.len().min(self.size - start);
            ring[start..start + first].copy_from_slice(&data[..first]);
            ring[..data.len() - first].copy_from_slice(&data[first..]);
        }

        fn zero(&self, at: u64, len: usize) {
            let ring = self.buf.as_mut_slice();
            let start = (at % self.size as u64) as usize;
            let first = len.min(self.size - start);
            ring[start..start + first].fill(0);
            ring[..len - first].fill(0);
        }

        /// Queues sound (bytes in the device's format), waiting for room.
        pub fn write(&self, mut data: &[u8]) {
            let mut st = self.state.lock();
            while !data.is_empty() {
                if st.paused {
                    drop(st);
                    self.progress.wait(20);
                    st = self.state.lock();
                    continue;
                }
                if !st.running {
                    // Fill the ring from the start, then let it play.
                    self.buf.as_mut_slice().fill(0);
                    let n = data.len().min(self.size);
                    self.copy_in(0, &data[..n]);
                    data = &data[n..];
                    *st = State { running: true, paused: false, written: n as u64, laps: 0, last: 0 };
                    self.hw.start();
                    continue;
                }
                let played = self.played(&mut st);
                if st.written < played {
                    // The device caught up (the writer was late): go on a
                    // little ahead of it.
                    st.written = played + (self.size / 8) as u64 / 4 * 4;
                }
                let room = self.size - (st.written - played) as usize;
                if room < 256 {
                    drop(st);
                    self.progress.wait(5);
                    st = self.state.lock();
                    continue;
                }
                let n = data.len().min(room) & !3;
                self.copy_in(st.written, &data[..n]);
                st.written += n as u64;
                data = &data[n..];
            }
        }

        /// Plays what is queued to its end, then stops the device.
        pub fn drain(&self) {
            let mut st = self.state.lock();
            if !st.running || st.paused {
                return;
            }
            let played = self.played(&mut st);
            if st.written > played {
                // Silence after the end, so nothing old plays again.
                let room = self.size - (st.written - played) as usize;
                self.zero(st.written, room);
            }
            let start = super::time_ms();
            loop {
                let played = self.played(&mut st);
                if played >= st.written || super::time_ms() - start > 10_000 {
                    break;
                }
                drop(st);
                self.progress.wait(5);
                st = self.state.lock();
                if st.paused || !st.running {
                    return; // the machine went to sleep meanwhile
                }
            }
            self.hw.stop();
            st.running = false;
        }
    }
}
