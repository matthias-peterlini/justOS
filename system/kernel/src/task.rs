//! Threads and the scheduler.
//!
//! Preemptive round robin over every processor: one queue of ready threads
//! that each processor takes from, and a 10 ms slice per thread. Threads
//! sleep with [`sleep_ms`], wait for events with [`WaitQueue`], and end with
//! [`exit`] (or by returning). Each processor has an idle thread of its own.
//!
//! A thread switched away from is handed to the next thread on the same
//! processor ([`after_switch`]): only once its registers are saved can it be
//! queued again, run elsewhere or, when it has ended, have its stack freed.
//! Threads of user programs run anywhere too: the processors say which
//! address space they have loaded, so that a page a program unmaps is
//! flushed where the program runs ([`smp::shootdown_user`]).

use crate::arch::smp::{self, MAX_CPUS};
use crate::arch::{cpu, gdt, syscall};
use crate::mem::{self, region};
use crate::sync::IrqLock;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::ffi::c_void;

pub type Tid = u32;

const STACK_PAGES: u64 = 16; // 64 KiB
const SLICE_TICKS: u32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Ready,
    Running,
    Sleeping,
    Blocked,
    Dead,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Ready => "ready",
            State::Running => "running",
            State::Sleeping => "sleeping",
            State::Blocked => "waiting",
            State::Dead => "ended",
        }
    }
}

/// 512-byte FXSAVE area for a thread's x87/SSE state, 16-byte aligned.
#[repr(C, align(16))]
struct FpuArea([u8; 512]);

impl FpuArea {
    fn new() -> Box<FpuArea> {
        // A clean initial state: FCW = 0x037F, MXCSR = 0x1F80.
        let mut a = Box::new(FpuArea([0; 512]));
        a.0[0..2].copy_from_slice(&0x037Fu16.to_le_bytes());
        a.0[24..28].copy_from_slice(&0x1F80u32.to_le_bytes());
        a
    }
}

/// # Safety
/// `area` must point at a 16-byte aligned 512-byte buffer.
unsafe fn fxsave(area: *mut FpuArea) {
    // SAFETY: guaranteed by the caller.
    unsafe { core::arch::asm!("fxsave [{}]", in(reg) area, options(nostack, preserves_flags)) };
}

/// # Safety
/// `area` must point at a buffer previously written by `fxsave` (or a valid
/// initial state).
unsafe fn fxrstor(area: *const FpuArea) {
    // SAFETY: guaranteed by the caller.
    unsafe { core::arch::asm!("fxrstor [{}]", in(reg) area, options(nostack, preserves_flags)) };
}

struct Thread {
    tid: Tid,
    name: String,
    state: State,
    rsp: u64,
    /// Lowest address of the stack (0 for the boot thread's static stack).
    stack: u64,
    stack_pages: u64,
    wake_at: u64,
    cpu_ticks: u64,
    /// Address space (0 = kernel only).
    cr3: u64,
    /// The process this thread belongs to (0 = kernel).
    pid: u32,
    /// User FS and GS bases, while the thread is not running.
    fs_base: u64,
    gs_base: u64,
    /// x87/SSE registers, while the thread is not running.
    fpu: Box<FpuArea>,
    /// Running on a processor, or not yet handed over after it stopped:
    /// its registers are not saved yet.
    on_cpu: bool,
}

impl Thread {
    fn new(tid: Tid, name: &str, state: State) -> Thread {
        Thread {
            tid,
            name: String::from(name),
            state,
            rsp: 0,
            stack: 0,
            stack_pages: 0,
            wake_at: 0,
            cpu_ticks: 0,
            cr3: 0,
            pid: 0,
            fs_base: 0,
            gs_base: 0,
            fpu: FpuArea::new(),
            on_cpu: false,
        }
    }

    fn stack_top(&self) -> u64 {
        self.stack + self.stack_pages * mem::PAGE_SIZE
    }
}

/// Whether a thread may run on `cpu`: the other processors run nothing
/// while they are being stopped (before sleep).
fn allowed_on(_t: &Thread, cpu: usize) -> bool {
    cpu == 0 || !smp::parking()
}

/// What the scheduler keeps for each processor.
#[derive(Clone, Copy)]
struct Cpu {
    current: Tid,
    idle: Tid,
    slice: u32,
    /// The thread just switched away from, and whether it goes back in the
    /// ready queue (see [`after_switch`]).
    prev: Option<(Tid, bool)>,
}

struct Sched {
    // Boxed: a thread's saved stack pointer must stay put while its
    // context is being saved into it.
    threads: BTreeMap<Tid, Box<Thread>>,
    ready: VecDeque<Tid>,
    sleepers: Vec<Tid>,
    cpus: [Cpu; MAX_CPUS],
    next_tid: Tid,
    started: bool,
}

impl Sched {
    /// The thread running on this processor (with the lock held, so the
    /// caller cannot move to another processor).
    fn current(&self) -> Tid {
        self.cpus[smp::this_cpu()].current
    }

    fn runnable_on(&self, tid: Tid, cpu: usize) -> bool {
        self.threads.get(&tid).is_some_and(|t| t.state == State::Ready && !t.on_cpu && allowed_on(t, cpu))
    }

    /// Takes the next thread `cpu` may run from the ready queue. Threads it
    /// may not run (yet) stay queued.
    fn pick(&mut self, cpu: usize) -> Option<Tid> {
        for _ in 0..self.ready.len() {
            let tid = self.ready.pop_front()?;
            if self.runnable_on(tid, cpu) {
                return Some(tid);
            }
            if self.threads.get(&tid).is_some_and(|t| t.state == State::Ready) {
                self.ready.push_back(tid);
            }
        }
        None
    }
}

static SCHED: IrqLock<Sched> = IrqLock::new(Sched {
    threads: BTreeMap::new(),
    ready: VecDeque::new(),
    sleepers: Vec::new(),
    cpus: [Cpu { current: 0, idle: 0, slice: SLICE_TICKS, prev: None }; MAX_CPUS],
    next_tid: 1,
    started: false,
});

global_asm!(
    r#"
    .global context_switch
context_switch:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rdi], rsp
    mov rsp, rsi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

    .global thread_trampoline
thread_trampoline:
    mov rdi, r12
    mov rsi, r13
    call thread_start
    ud2
"#
);

extern "C" {
    fn context_switch(old_rsp: *mut u64, new_rsp: u64);
    fn thread_trampoline();
}

type Entry = unsafe extern "C" fn(*mut c_void);

#[no_mangle]
extern "C" fn thread_start(entry: Entry, arg: *mut c_void) -> ! {
    after_switch();
    cpu::enable_irq();
    // SAFETY: entry/arg were given to spawn_raw together.
    unsafe { entry(arg) };
    exit();
}

/// Turns the boot code into thread 1 and creates the idle thread.
pub fn init() {
    let mut s = SCHED.lock();
    let tid = s.next_tid;
    s.next_tid += 1;
    let mut init = Thread::new(tid, "init", State::Running);
    init.on_cpu = true;
    s.threads.insert(tid, Box::new(init));
    s.cpus[0].current = tid;
    drop(s);
    let idle = spawn_raw("idle", idle_main, core::ptr::null_mut()).expect("cannot create the idle thread");
    let mut s = SCHED.lock();
    s.ready.retain(|&t| t != idle);
    s.cpus[0].idle = idle;
    s.started = true;
}

/// Turns the code running on a processor that just started (on `stack`)
/// into that processor's idle thread.
pub fn start_cpu(cpu: usize, stack: u64, pages: u64) {
    let mut s = SCHED.lock();
    let tid = s.next_tid;
    s.next_tid += 1;
    let mut idle = Thread::new(tid, "idle", State::Running);
    (idle.stack, idle.stack_pages, idle.on_cpu) = (stack, pages, true);
    s.threads.insert(tid, Box::new(idle));
    s.cpus[cpu] = Cpu { current: tid, idle: tid, slice: SLICE_TICKS, prev: None };
}

unsafe extern "C" fn idle_main(_: *mut c_void) {
    loop {
        cpu::enable_and_halt();
    }
}

/// Starts a thread running `entry(arg)`.
pub fn spawn_raw(name: &str, entry: Entry, arg: *mut c_void) -> Option<Tid> {
    spawn_in(name, entry, arg, 0, 0)
}

pub(crate) fn spawn_in(name: &str, entry: Entry, arg: *mut c_void, cr3: u64, pid: u32) -> Option<Tid> {
    spawn_with(name, entry, arg, cr3, pid, false)
}

/// Like [`spawn_in`], but the thread waits for [`wake`] before it runs.
pub(crate) fn spawn_suspended(name: &str, entry: Entry, arg: *mut c_void, cr3: u64, pid: u32) -> Option<Tid> {
    spawn_with(name, entry, arg, cr3, pid, true)
}

fn spawn_with(name: &str, entry: Entry, arg: *mut c_void, cr3: u64, pid: u32, suspended: bool) -> Option<Tid> {
    spawn_sized(name, entry, arg, cr3, pid, suspended, STACK_PAGES)
}

#[allow(clippy::too_many_arguments)]
fn spawn_sized(name: &str, entry: Entry, arg: *mut c_void, cr3: u64, pid: u32, suspended: bool, pages: u64) -> Option<Tid> {
    let stack = region::alloc_pages(&region::STACKS, pages, 1)?;
    let top = stack + pages * mem::PAGE_SIZE;
    let rsp = top - 9 * 8;
    let frame = [0u64, 0, arg as u64, entry as usize as u64, 0, 0, thread_trampoline as *const () as u64, 0, 0];
    // SAFETY: the stack was just mapped and is owned by the new thread.
    unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), rsp as *mut u64, frame.len()) };
    let mut s = SCHED.lock();
    let tid = s.next_tid;
    s.next_tid += 1;
    let mut t = Thread::new(tid, name, if suspended { State::Blocked } else { State::Ready });
    (t.rsp, t.stack, t.stack_pages, t.cr3, t.pid) = (rsp, stack, pages, cr3, pid);
    s.threads.insert(tid, Box::new(t));
    if !suspended {
        s.ready.push_back(tid);
    }
    Some(tid)
}

/// Starts a thread running a closure.
pub fn spawn<F: FnOnce() + Send + 'static>(name: &str, f: F) -> Option<Tid> {
    spawn_big(name, STACK_PAGES * mem::PAGE_SIZE, f)
}

/// Starts a thread running a closure on a stack of at least `bytes` (for
/// deep recursion, such as laying out web pages).
pub fn spawn_big<F: FnOnce() + Send + 'static>(name: &str, bytes: u64, f: F) -> Option<Tid> {
    unsafe extern "C" fn run<F: FnOnce()>(arg: *mut c_void) {
        // SAFETY: arg is the Box leaked below, used exactly once.
        let f = unsafe { Box::from_raw(arg as *mut F) };
        f();
    }
    let arg = Box::into_raw(Box::new(f)) as *mut c_void;
    let pages = bytes.div_ceil(mem::PAGE_SIZE).max(STACK_PAGES);
    let tid = spawn_sized(name, run::<F>, arg, 0, 0, false, pages);
    if tid.is_none() {
        // SAFETY: the thread was not created; reclaim the closure.
        drop(unsafe { Box::from_raw(arg as *mut F) });
    }
    tid
}

/// Runs on the thread switched to: hands over the thread switched away
/// from, whose registers are now saved. It is queued again if it was
/// preempted, or freed if it has ended.
fn after_switch() {
    let dead = {
        let mut s = SCHED.lock();
        let me = smp::this_cpu();
        let Some((prev, requeue)) = s.cpus[me].prev.take() else { return };
        let Some(t) = s.threads.get_mut(&prev) else { return };
        t.on_cpu = false;
        match t.state {
            State::Dead => s.threads.remove(&prev),
            State::Ready if requeue => {
                s.ready.push_back(prev);
                None
            }
            _ => None,
        }
    };
    if let Some(t) = dead {
        if t.stack != 0 {
            region::free_pages(&region::STACKS, t.stack, t.stack_pages, 1);
        }
    }
}

/// Picks the next thread and switches to it.
fn schedule() {
    let irq = cpu::irq_save();
    let switch = {
        let mut s = SCHED.lock();
        let me = smp::this_cpu();
        let (cur, idle) = (s.cpus[me].current, s.cpus[me].idle);
        let state = s.threads.get(&cur).map(|t| t.state);
        if !s.started {
            None
        } else if state == Some(State::Ready) && cur != idle {
            // Woken (by another processor) before it got to stop.
            s.threads.get_mut(&cur).unwrap().state = State::Running;
            None
        } else {
            let requeue = state == Some(State::Running) && cur != idle;
            if state == Some(State::Running) {
                s.threads.get_mut(&cur).unwrap().state = State::Ready;
            }
            let stay = requeue && s.threads.get(&cur).is_some_and(|t| allowed_on(t, me));
            let next = match s.pick(me) {
                Some(t) => t,
                None if stay => cur,
                None => idle,
            };
            s.cpus[me].slice = SLICE_TICKS;
            let n = s.threads.get_mut(&next).unwrap();
            n.state = State::Running;
            n.on_cpu = true;
            if next == cur {
                None
            } else {
                let (new_rsp, top, cr3, fs, gs, has_stack) = (n.rsp, n.stack_top(), n.cr3, n.fs_base, n.gs_base, n.stack != 0);
                let new_fpu: *const FpuArea = &*n.fpu;
                let c = s.threads.get_mut(&cur).unwrap();
                (c.fs_base, c.gs_base) = syscall::user_bases();
                // SAFETY: the area is this thread's own aligned buffer.
                unsafe { fxsave(&mut *c.fpu) };
                // SAFETY: new_fpu is the next thread's aligned buffer; the
                // kernel is soft-float, so nothing touches SSE before the
                // switch completes and the new thread resumes.
                unsafe { fxrstor(new_fpu) };
                syscall::set_user_bases(fs, gs);
                if has_stack {
                    gdt::set_kernel_stack(top);
                }
                let target = if cr3 != 0 { cr3 } else { mem::paging::kernel_pml4() };
                if cpu::read_cr3() != target {
                    // SAFETY: every address space maps the kernel half.
                    unsafe { smp::load_cr3(target) };
                }
                s.cpus[me].current = next;
                s.cpus[me].prev = Some((cur, requeue));
                let old_ptr: *mut u64 = &mut s.threads.get_mut(&cur).unwrap().rsp;
                Some((old_ptr, new_rsp))
            }
        }
    };
    if let Some((old, new)) = switch {
        // SAFETY: interrupts are off; both stacks are valid. The old thread
        // is marked on_cpu, so nothing else runs, moves or frees it until
        // after_switch hands it over.
        unsafe { context_switch(old, new) };
        after_switch();
    }
    cpu::irq_restore(irq);
}

/// Gives the CPU to another ready thread, if any.
pub fn yield_now() {
    if SCHED.lock().ready.is_empty() {
        return;
    }
    schedule();
}

/// Ends the calling thread.
pub fn exit() -> ! {
    cpu::disable_irq();
    {
        let mut s = SCHED.lock();
        let cur = s.current();
        if let Some(t) = s.threads.get_mut(&cur) {
            t.state = State::Dead;
        }
    }
    schedule();
    unreachable!("a dead thread was scheduled");
}

pub fn sleep_ms(ms: u64) {
    if ms == 0 {
        yield_now();
        return;
    }
    let irq = cpu::irq_save();
    {
        let mut s = SCHED.lock();
        if !s.started {
            drop(s);
            cpu::irq_restore(irq);
            let until = crate::time::uptime_ms() + ms;
            while crate::time::uptime_ms() < until {
                cpu::halt();
            }
            return;
        }
        let cur = s.current();
        let t = s.threads.get_mut(&cur).unwrap();
        t.state = State::Sleeping;
        t.wake_at = crate::time::uptime_ms() + ms;
        s.sleepers.push(cur);
    }
    schedule();
    cpu::irq_restore(irq);
}

/// Called by the system timer interrupt (on the first processor).
pub fn timer_tick() {
    crate::time::tick();
    tick(true);
}

/// Called by the timer interrupt of the other processors.
pub fn ap_tick() {
    tick(false);
}

/// Charges the tick to the running thread, wakes sleepers that are due
/// (once, on the first processor) and preempts at the end of the slice.
fn tick(wake_sleepers: bool) {
    let preempt = {
        let mut s = SCHED.lock();
        if !s.started {
            return;
        }
        let now = crate::time::uptime_ms();
        let me = smp::this_cpu();
        let cur = s.cpus[me].current;
        if let Some(t) = s.threads.get_mut(&cur) {
            t.cpu_ticks += 1;
        }
        let mut i = 0;
        while wake_sleepers && i < s.sleepers.len() {
            let tid = s.sleepers[i];
            let due = match s.threads.get(&tid) {
                Some(t) if t.state == State::Sleeping => t.wake_at <= now,
                _ => {
                    s.sleepers.swap_remove(i);
                    continue;
                }
            };
            if due {
                s.sleepers.swap_remove(i);
                s.threads.get_mut(&tid).unwrap().state = State::Ready;
                s.ready.push_back(tid);
            } else {
                i += 1;
            }
        }
        let c = &mut s.cpus[me];
        c.slice = c.slice.saturating_sub(1);
        let leave = me != 0 && cur != c.idle && smp::parking();
        leave || ((c.slice == 0 || cur == c.idle) && s.ready.iter().any(|&t| s.runnable_on(t, me)))
    };
    if preempt {
        schedule();
    }
}

/// Switches the running thread back to the kernel-only address space (its
/// process is going away).
pub fn leave_address_space() {
    let mut s = SCHED.lock();
    let cur = s.current();
    if let Some(t) = s.threads.get_mut(&cur) {
        t.cr3 = 0;
    }
    // SAFETY: the kernel tables map everything the kernel needs.
    unsafe { smp::load_cr3(mem::paging::kernel_pml4()) };
}

/// Moves the running thread to another address space (`execve`).
pub fn enter_address_space(cr3: u64) {
    let mut s = SCHED.lock();
    let cur = s.current();
    if let Some(t) = s.threads.get_mut(&cur) {
        t.cr3 = cr3;
    }
    // SAFETY: a new address space shares the kernel half.
    unsafe { smp::load_cr3(cr3) };
}

/// The running thread.
pub fn current_tid() -> Tid {
    SCHED.lock().current()
}

/// Lets a thread made with [`spawn_suspended`] run.
pub fn wake_new(tid: Tid) {
    wake(tid);
}

pub fn current_pid() -> u32 {
    let s = SCHED.lock();
    s.threads.get(&s.current()).map(|t| t.pid).unwrap_or(0)
}

/// The process a thread belongs to.
pub fn pid_of(tid: Tid) -> Option<u32> {
    SCHED.lock().threads.get(&tid).map(|t| t.pid)
}

/// Name of the running thread, without allocating (panic path).
pub fn current_name_lossy() -> &'static str {
    match SCHED.try_lock() {
        Some(s) => match s.threads.get(&s.current()) {
            // SAFETY: thread names live until the thread is reaped; this is
            // only used to print a message right away.
            Some(t) => unsafe { &*(t.name.as_str() as *const str) },
            None => "?",
        },
        None => "?",
    }
}

/// Marks the running thread as waiting (until `wake_at`, if given);
/// returns its id. The caller must have interrupts disabled and call
/// [`schedule`] or [`unblock_current`] next.
fn block_current(wake_at: Option<u64>) -> Tid {
    let mut s = SCHED.lock();
    let cur = s.current();
    let t = s.threads.get_mut(&cur).unwrap();
    match wake_at {
        Some(at) => {
            t.state = State::Sleeping;
            t.wake_at = at;
            s.sleepers.push(cur);
        }
        None => t.state = State::Blocked,
    }
    cur
}

/// Undoes [`block_current`]: what the thread waited for happened already.
/// A queue entry a waker may have added meanwhile is skipped later.
fn unblock_current() {
    let mut s = SCHED.lock();
    let cur = s.current();
    s.threads.get_mut(&cur).unwrap().state = State::Running;
}

/// Makes a waiting thread ready again.
pub fn wake(tid: Tid) {
    let mut s = SCHED.lock();
    if let Some(t) = s.threads.get_mut(&tid) {
        if t.state == State::Blocked || t.state == State::Sleeping {
            t.state = State::Ready;
            s.ready.push_back(tid);
        }
    }
}

/// A set of threads waiting for something to happen.
pub struct WaitQueue {
    waiters: IrqLock<VecDeque<Tid>>,
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl WaitQueue {
    pub const fn new() -> WaitQueue {
        WaitQueue { waiters: IrqLock::new(VecDeque::new()) }
    }

    /// Blocks until `ready()` returns `Some`. `ready` runs with interrupts
    /// disabled, so an interrupt handler cannot change the condition between
    /// the check and going to sleep.
    pub fn wait_for<T>(&self, mut ready: impl FnMut() -> Option<T>) -> T {
        let irq = cpu::irq_save();
        loop {
            if let Some(v) = ready() {
                cpu::irq_restore(irq);
                return v;
            }
            let started = SCHED.lock().started;
            if !started {
                cpu::enable_and_halt();
                cpu::disable_irq();
                continue;
            }
            if let Some(v) = self.block(None, &mut ready) {
                cpu::irq_restore(irq);
                return v;
            }
        }
    }

    /// Waits on the queue, unless `ready` holds once the thread is in it:
    /// another processor may have changed the condition and notified the
    /// queue just before.
    fn block<T>(&self, wake_at: Option<u64>, ready: &mut impl FnMut() -> Option<T>) -> Option<T> {
        let me = block_current(wake_at);
        self.waiters.lock().push_back(me);
        if let Some(v) = ready() {
            unblock_current();
            self.waiters.lock().retain(|&t| t != me);
            return Some(v);
        }
        schedule();
        None
    }

    /// Like [`wait_for`](Self::wait_for), giving up after `ms` milliseconds.
    pub fn wait_for_timeout<T>(&self, ms: u64, mut ready: impl FnMut() -> Option<T>) -> Option<T> {
        let until = crate::time::uptime_ms().saturating_add(ms);
        let irq = cpu::irq_save();
        loop {
            if let Some(v) = ready() {
                cpu::irq_restore(irq);
                return Some(v);
            }
            if crate::time::uptime_ms() >= until {
                cpu::irq_restore(irq);
                return None;
            }
            let started = SCHED.lock().started;
            if !started {
                cpu::enable_and_halt();
                cpu::disable_irq();
                continue;
            }
            // Asleep until the deadline, unless notified first.
            if let Some(v) = self.block(Some(until), &mut ready) {
                cpu::irq_restore(irq);
                return Some(v);
            }
            // Woken by the timer: do not leave a stale entry behind.
            let me = current_tid();
            self.waiters.lock().retain(|&t| t != me);
        }
    }

    pub fn notify_all(&self) {
        let waiters = core::mem::take(&mut *self.waiters.lock());
        for t in waiters {
            wake(t);
        }
    }
}

/// A flag one thread waits on and others (or interrupt handlers) set.
pub struct Event {
    set: core::sync::atomic::AtomicBool,
    queue: WaitQueue,
}

impl Default for Event {
    fn default() -> Self {
        Self::new()
    }
}

impl Event {
    pub const fn new() -> Event {
        Event { set: core::sync::atomic::AtomicBool::new(false), queue: WaitQueue::new() }
    }

    /// Sets the event and wakes its waiters (safe in interrupt handlers).
    pub fn signal(&self) {
        self.set.store(true, core::sync::atomic::Ordering::Release);
        self.queue.notify_all();
    }

    /// Waits until the event is set, then clears it. `None` waits forever.
    /// Returns false on timeout.
    pub fn wait(&self, timeout_ms: Option<u64>) -> bool {
        let ready = || self.set.swap(false, core::sync::atomic::Ordering::Acquire).then_some(());
        match timeout_ms {
            None => {
                self.queue.wait_for(ready);
                true
            }
            Some(ms) => self.queue.wait_for_timeout(ms, ready).is_some(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ThreadInfo {
    pub tid: Tid,
    pub name: String,
    pub state: State,
    pub cpu_ms: u64,
    pub pid: u32,
}

pub fn list() -> Vec<ThreadInfo> {
    let s = SCHED.lock();
    s.threads
        .values()
        .map(|t| ThreadInfo {
            tid: t.tid,
            name: t.name.clone(),
            state: t.state,
            cpu_ms: t.cpu_ticks * 1000 / crate::arch::pit::HZ,
            pid: t.pid,
        })
        .collect()
}

/// The thread each running processor has on it, with the ready queue's
/// length.
pub fn on_cpus() -> (Vec<Tid>, usize) {
    let s = SCHED.lock();
    let n = smp::count().max(1);
    (s.cpus[..n].iter().map(|c| c.current).collect(), s.ready.len())
}

/// Where a thread of a program that is not on a processor stopped in its
/// program: the process, the instruction pointer, and the Linux system call
/// it is in, if any, from the frame its last entry into the kernel left at
/// the top of its kernel stack (in a system call, where the program made
/// it).
pub fn user_frame(tid: Tid) -> Option<(u32, u64, Option<u64>)> {
    let s = SCHED.lock();
    let t = s.threads.get(&tid)?;
    if t.pid == 0 || t.stack == 0 || t.on_cpu {
        return None;
    }
    let at = t.stack_top() - core::mem::size_of::<crate::arch::idt::TrapFrame>() as u64;
    // SAFETY: the thread's kernel stack stays mapped while it exists (the
    // scheduler's lock keeps it), and entries from user mode put their
    // frame at its top.
    let f = unsafe { &*(at as *const crate::arch::idt::TrapFrame) };
    let call = (f.vector == crate::arch::syscall::VEC_LINUX).then_some(f.rax);
    f.is_user().then_some((t.pid, f.rip, call))
}

// ----------------------------------------------------------------------------
// User mode hooks (see `crate::user`).

pub fn syscall(frame: &mut crate::arch::idt::TrapFrame) {
    crate::user::syscall(frame);
}

pub fn user_fault(frame: &mut crate::arch::idt::TrapFrame, what: &str) {
    crate::user::fault(frame, what);
}
