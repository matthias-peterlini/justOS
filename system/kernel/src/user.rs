//! User programs: processes, their address spaces and the system calls.
//!
//! A program is a position-independent ELF image (`.app`). Each process has
//! its own address space (the kernel half is shared), one thread, a table of
//! handles and a current folder. System calls arrive through `int 0x80`; see
//! `justdk::syscall` for the interface.
//!
//! Two other kinds of program run here too: Linux programs (any other ELF
//! file), which talk to [`crate::linux`] through `syscall`, and Windows
//! programs (`.exe`), which `winrun.app` loads and runs, and Android apps
//! and Java programs (`.apk`, `.dex`, `.jar`), which `dalvik.app` runs.
//!
//! A process started from an installed app carries that app's identity
//! ([`crate::sandbox::App`]); its file and port calls are checked against
//! the app's sandbox, and files it picked in the system picker are kept in
//! `picked`.

use crate::arch::{cpu, gdt, idt::TrapFrame};
use crate::fs::{self, Path};
use crate::mem::{self, frame, paging, PAGE_SIZE};
use crate::sync::IrqLock;
use crate::task::{self, Tid, WaitQueue};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use justdk::syscall::{self as sys, Error};
use core::ffi::c_void;

const STACK_PAGES: u64 = 64;
pub(crate) const USER_TOP: u64 = 0x0000_8000_0000_0000;
const MAX_HANDLES: usize = 64;

enum Handle {
    File { path: Path, pos: u64, write: bool },
    /// A port of [`crate::ipc`]: one this process owns (and receives
    /// from), or a right to send to another.
    Port { id: u32, own: bool },
}

impl Handle {
    fn file(&mut self) -> Result<(&Path, &mut u64, bool), Error> {
        match self {
            Handle::File { path, pos, write } => Ok((path, pos, *write)),
            Handle::Port { .. } => Err(Error::BadHandle),
        }
    }

    /// Gives back what the handle held.
    fn release(self) {
        match self {
            Handle::Port { id, own: true } => crate::ipc::close(id),
            Handle::Port { id, own: false } => crate::ipc::drop_sender(id),
            Handle::File { .. } => {}
        }
    }
}

/// Which system call interface a process uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Native,
    Linux,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Running,
    Exited(i64),
}

struct Process {
    name: String,
    cr3: u64,
    tid: Tid,
    /// Every thread of the process (the first one included).
    threads: Vec<Tid>,
    /// The exit code once one thread ended the whole process, while the
    /// others are still stopping.
    exit_code: Option<i64>,
    handles: Vec<Option<Handle>>,
    heap_next: u64,
    cwd: Path,
    state: State,
    kill: bool,
    /// The app it runs as, in its sandbox (None: a system program).
    app: Option<crate::sandbox::App>,
    /// Files the user chose for it in the file picker (and may it write).
    picked: Vec<(Path, bool)>,
    /// The program file it was started from.
    exe: Path,
    /// The Linux process that made it with fork or vfork, which may wait
    /// for it (None: started by justOS, waited for with [`wait`]).
    parent: Option<u32>,
    /// A vfork child running in its parent's memory until it execs or
    /// ends (the memory is not its own to free).
    shared_vm: bool,
    /// Its parent ended first: no one will wait for it.
    orphan: bool,
}

static PROCS: IrqLock<BTreeMap<u32, Process>> = IrqLock::new(BTreeMap::new());
static NEXT_PID: IrqLock<u32> = IrqLock::new(1);
static FOREGROUND: IrqLock<Option<u32>> = IrqLock::new(None);
static EXITED: WaitQueue = WaitQueue::new();

pub(crate) fn fs_error(e: fs::Error) -> Error {
    match e {
        fs::Error::NotFound => Error::NotFound,
        fs::Error::Exists => Error::Exists,
        fs::Error::NotDir => Error::NotDir,
        fs::Error::IsDir => Error::IsDir,
        fs::Error::NotEmpty => Error::NotEmpty,
        fs::Error::NoSpace | fs::Error::NoNodes | fs::Error::TooFragmented => Error::NoSpace,
        fs::Error::NameTooLong | fs::Error::InvalidName => Error::InvalidName,
        fs::Error::ReadOnly => Error::ReadOnly,
        fs::Error::Corrupt | fs::Error::Io => Error::Io,
        fs::Error::Loop => Error::NotFound,
    }
}

/// Where a program name points: `edit` -> `sys:/apps/edit.app`, else
/// `edit.jpk` or `edit.jt` there (JustScript programs), `edit.exe` (Windows),
/// `edit.apk` (Android) or `edit` (Linux); paths are taken as they are.
pub fn find_program(name: &str, cwd: &Path) -> Option<Path> {
    let lower = name.to_ascii_lowercase();
    if name.contains(':') || name.contains('/') || lower.ends_with(".app") || lower.ends_with(".exe") || is_script(name) || is_android(name) {
        let p = Path::parse(name, cwd).ok()?;
        return fs::stat(&p).ok().filter(|s| s.kind == justfs::Kind::File).map(|_| p);
    }
    [".app", ".jpk", ".jt", ".exe", ".apk", ""].iter().find_map(|ext| {
        let p = fs::abs(&alloc::format!("sys:/apps/{lower}{ext}")).ok()?;
        fs::stat(&p).ok().filter(|s| s.kind == justfs::Kind::File).map(|_| p)
    })
}

/// JustScript packages and sources, which the `just` runner starts.
fn is_script(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".jpk") || lower.ends_with(".jt")
}

/// Android apps and Java programs in DEX form, which `dalvik` runs.
fn is_android(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".apk") || lower.ends_with(".dex") || lower.ends_with(".jar")
}

const SCRIPT_RUNNER: &str = "sys:/apps/just.app";
const WINDOWS_RUNNER: &str = "sys:/apps/winrun.app";
const ANDROID_RUNNER: &str = "sys:/apps/dalvik.app";

struct Start {
    image: Vec<u8>,
    args: String,
    kind: Kind,
    /// The program file (Linux programs see it as /proc/self/exe).
    exe: String,
}

/// A path as one argument, quoted if it has spaces.
fn quoted(path: &Path) -> String {
    let file = alloc::format!("{path}");
    if file.contains(char::is_whitespace) {
        alloc::format!("\"{file}\"")
    } else {
        file
    }
}

/// The app a new program runs as: its own, if it is an installed app;
/// else the app of the program starting it, so an app cannot leave its
/// sandbox by starting another program. An app starts in its own folder.
fn app_for(path: &Path, cwd: Path) -> (Option<crate::sandbox::App>, Path) {
    if let Some(app) = crate::sandbox::for_program(path) {
        let home = crate::sandbox::prepare(&app).unwrap_or(cwd);
        return (Some(app), home);
    }
    (current_app(), cwd)
}

/// Starts a program. Returns its process id.
pub fn spawn(path: &Path, args: &str, cwd: Path) -> Result<u32, Error> {
    let name = path.name();
    let (app, cwd) = app_for(path, cwd);
    if is_script(name) {
        // `just <file> <args>`, under the script's own name.
        let runner = fs::abs(SCRIPT_RUNNER).map_err(fs_error)?;
        let image = fs::read_file(&runner).map_err(fs_error)?;
        let shown = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        return start(image, String::from(shown), &alloc::format!("{} {args}", quoted(path)), cwd, Kind::Native, path, app);
    }
    let mut magic = [0u8; 4];
    fs::read_at(path, 0, &mut magic).map_err(fs_error)?;
    let runner = if magic[..2] == *b"MZ" {
        Some(WINDOWS_RUNNER)
    } else if magic == *b"dex\n" || is_android(name) {
        Some(ANDROID_RUNNER)
    } else {
        None
    };
    if let Some(runner) = runner {
        // `winrun <file> <args>` or `dalvik <file> <args>`, under the
        // program's own name.
        let runner = fs::abs(runner).map_err(fs_error)?;
        let image = fs::read_file(&runner).map_err(fs_error)?;
        let shown = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        return start(image, String::from(shown), &alloc::format!("{} {args}", quoted(path)), cwd, Kind::Native, path, app);
    }
    let image = fs::read_file(path).map_err(fs_error)?;
    let kind = if name.to_ascii_lowercase().ends_with(".app") { Kind::Native } else { Kind::Linux };
    let shown = if kind == Kind::Native { &name[..name.len() - 4] } else { name };
    start(image, String::from(shown), args, cwd, kind, path, app)
}

#[allow(clippy::too_many_arguments)]
fn start(image: Vec<u8>, name: String, args: &str, cwd: Path, kind: Kind, exe: &Path, app: Option<crate::sandbox::App>) -> Result<u32, Error> {
    if image.len() < 64 || &image[..4] != b"\x7fELF" {
        return Err(Error::BadProgram);
    }
    let cr3 = paging::new_address_space().ok_or(Error::NoMemory)?;
    let pid = next_pid();
    PROCS.lock().insert(
        pid,
        Process {
            name: name.clone(),
            cr3,
            tid: 0,
            threads: Vec::new(),
            exit_code: None,
            handles: Vec::new(),
            heap_next: sys::USER_HEAP_BASE,
            cwd,
            state: State::Running,
            kill: false,
            app,
            picked: Vec::new(),
            exe: exe.clone(),
            parent: None,
            shared_vm: false,
            orphan: false,
        },
    );
    let start = Box::into_raw(Box::new(Start { image, args: String::from(args), kind, exe: alloc::format!("{exe}") }));
    match task::spawn_suspended(&name, user_start, start as *mut c_void, cr3, pid) {
        Some(tid) => {
            if let Some(p) = PROCS.lock().get_mut(&pid) {
                p.tid = tid;
                p.threads.push(tid);
            }
            task::wake_new(tid);
            Ok(pid)
        }
        None => {
            // SAFETY: the thread was not created; take the box back.
            drop(unsafe { Box::from_raw(start) });
            PROCS.lock().remove(&pid);
            paging::free_address_space(cr3);
            Err(Error::NoMemory)
        }
    }
}

fn next_pid() -> u32 {
    let mut n = NEXT_PID.lock();
    let pid = *n;
    *n += 1;
    pid
}

/// fork and vfork: a new process whose first thread resumes user mode with
/// the registers of `child` (the caller's, with the return value and stack
/// set). It gets a copy of the caller's memory, or, with `share_vm`, the
/// caller's memory itself until it execs or ends (the caller then waits in
/// [`wait_vfork`]). Returns the child's pid and thread, created suspended:
/// wake it with [`task::wake_new`] once its Linux state is set up.
pub(crate) fn fork(child: TrapFrame, fs: u64, share_vm: bool) -> Result<(u32, Tid), Error> {
    let me = task::current_pid();
    let (name, cr3, cwd, app, picked, exe, heap_next) = {
        let procs = PROCS.lock();
        let p = procs.get(&me).ok_or(Error::Unknown)?;
        (p.name.clone(), p.cr3, p.cwd.clone(), p.app.clone(), p.picked.clone(), p.exe.clone(), p.heap_next)
    };
    let child_cr3 = if share_vm { cr3 } else { paging::copy_address_space(cr3).ok_or(Error::NoMemory)? };
    let pid = next_pid();
    PROCS.lock().insert(
        pid,
        Process {
            name: name.clone(),
            cr3: child_cr3,
            tid: 0,
            threads: Vec::new(),
            exit_code: None,
            handles: Vec::new(),
            heap_next,
            cwd,
            state: State::Running,
            kill: false,
            app,
            picked,
            exe,
            parent: Some(me),
            shared_vm: share_vm,
            orphan: false,
        },
    );
    let arg = Box::into_raw(Box::new(ThreadStart { frame: child, fs }));
    match task::spawn_suspended(&name, thread_start, arg as *mut c_void, child_cr3, pid) {
        Some(tid) => {
            if let Some(p) = PROCS.lock().get_mut(&pid) {
                p.tid = tid;
                p.threads.push(tid);
            }
            Ok((pid, tid))
        }
        None => {
            // SAFETY: not handed to any thread; take it back.
            drop(unsafe { Box::from_raw(arg) });
            PROCS.lock().remove(&pid);
            if !share_vm {
                paging::free_address_space(child_cr3);
            }
            Err(Error::NoMemory)
        }
    }
}

/// The parent of a vfork child: waits until the child execs or ends.
pub(crate) fn wait_vfork(child: u32) {
    EXITED.wait_for(|| match PROCS.lock().get(&child) {
        Some(p) if p.shared_vm && p.state == State::Running => None,
        _ => Some(()),
    });
}

/// Writes `data` at `addr` in another process's memory (fork's
/// CLONE_CHILD_SETTID). Pages it never touched are left alone.
pub(crate) fn write_in(pid: u32, addr: u64, data: &[u8]) {
    let Some(cr3) = PROCS.lock().get(&pid).map(|p| p.cr3) else { return };
    for (i, b) in data.iter().enumerate() {
        let at = addr + i as u64;
        if at >= USER_TOP {
            return;
        }
        if let Some(phys) = paging::translate_in(cr3, at) {
            // SAFETY: a user page of that process, reached through the
            // direct map.
            unsafe { *(mem::phys_to_virt(phys) as *mut u8) = *b };
        }
    }
}

/// Is the calling thread its process's only one (as execve needs)?
pub(crate) fn only_thread() -> bool {
    let pid = task::current_pid();
    PROCS.lock().get(&pid).is_some_and(|p| p.threads.len() <= 1)
}

/// execve: the calling process gets a fresh, empty address space and the
/// new program's name. The old memory is freed (or, for a vfork child,
/// left to the parent, which stops waiting). The caller loads the program.
pub(crate) fn exec_address_space(name: &str, exe: &Path) -> Result<(), Error> {
    let pid = task::current_pid();
    let new = paging::new_address_space().ok_or(Error::NoMemory)?;
    let (old, shared) = {
        let mut procs = PROCS.lock();
        let Some(p) = procs.get_mut(&pid) else {
            paging::free_address_space(new);
            return Err(Error::Unknown);
        };
        p.name = String::from(name);
        p.exe = exe.clone();
        (core::mem::replace(&mut p.cr3, new), core::mem::replace(&mut p.shared_vm, false))
    };
    task::enter_address_space(new);
    if shared {
        EXITED.notify_all();
    } else {
        paging::free_address_space(old);
    }
    Ok(())
}

/// Starts the program just loaded by exec: user mode at `entry` with the
/// stack at `rsp` and every other register cleared.
pub(crate) fn start_loaded(entry: u64, rsp: u64) -> ! {
    let (_, gs) = crate::arch::syscall::user_bases();
    crate::arch::syscall::set_user_bases(0, gs);
    // SAFETY: the program was just loaded into this address space.
    unsafe { enter_user(entry, rsp, 0, 0) }
}

/// The Linux parent of the calling process (1, as for an orphan, if none).
pub(crate) fn parent_pid() -> u32 {
    let pid = task::current_pid();
    PROCS.lock().get(&pid).and_then(|p| p.parent).unwrap_or(1)
}

/// wait4: a child of the calling process that has ended (`want`: that one,
/// or any), removed from the table with its exit code. Ok(None) with
/// `nohang` when children are still running; Err(NotFound) if there are
/// none; Err(Cancelled) if the caller is being stopped.
pub(crate) fn wait_child(want: Option<u32>, nohang: bool, stop: impl Fn() -> bool) -> Result<Option<(u32, i64)>, Error> {
    let me = task::current_pid();
    EXITED.wait_for(|| {
        // A signal for the caller (it was woken by `wake_waiters`).
        if stop() {
            return Some(Err(Error::Cancelled));
        }
        let mut procs = PROCS.lock();
        if procs.get(&me).is_some_and(|p| p.kill) {
            return Some(Err(Error::Cancelled));
        }
        let mut any = false;
        let mut ended = None;
        for (&id, p) in procs.iter() {
            if p.parent != Some(me) || want.is_some_and(|w| w != id) {
                continue;
            }
            any = true;
            if let State::Exited(code) = p.state {
                ended = Some((id, code));
                break;
            }
        }
        match ended {
            Some((id, code)) => {
                procs.remove(&id);
                Some(Ok(Some((id, code))))
            }
            None if !any => Some(Err(Error::NotFound)),
            None if nohang => Some(Ok(None)),
            None => None,
        }
    })
}

/// A process ended: its ended children go (no one will wait for them now),
/// its running ones become orphans, and it goes too if its own parent is
/// no longer there to wait for it, or will not (`reap`).
fn settle(pid: u32, reap: bool) {
    let mut procs = PROCS.lock();
    let children: Vec<u32> = procs.iter().filter(|(_, p)| p.parent == Some(pid)).map(|(&id, _)| id).collect();
    for id in children {
        if procs.get(&id).is_some_and(|p| matches!(p.state, State::Exited(_))) {
            procs.remove(&id);
        } else if let Some(p) = procs.get_mut(&id) {
            p.parent = None;
            p.orphan = true;
        }
    }
    let gone = reap || procs.get(&pid).is_some_and(|p| {
        // Nobody waits for an orphan: it goes as soon as it ends.
        p.orphan || p.parent.is_some_and(|parent| !procs.get(&parent).is_some_and(|q| q.state == State::Running))
    });
    if gone {
        procs.remove(&pid);
    }
}

/// Maps `pages` fresh user pages at `virt` in the running address space.
pub(crate) fn map_user(virt: u64, pages: u64) -> Option<()> {
    let cr3 = cpu::read_cr3();
    for i in 0..pages {
        let phys = frame::alloc()?;
        paging::map_in(cr3, virt + i * PAGE_SIZE, phys, paging::USER | paging::WRITE)?;
    }
    Some(())
}

/// First code of every process thread (already in the new address space).
unsafe extern "C" fn user_start(arg: *mut c_void) {
    // SAFETY: the Box leaked by spawn, used once.
    let start = unsafe { Box::from_raw(arg as *mut Start) };
    let (entry, rsp, rdi, rsi) = match start.kind {
        Kind::Native => native_image(&start),
        Kind::Linux => match crate::linux::load(&start.image, &current_name(), &start.args, &start.exe) {
            Ok((entry, rsp)) => (entry, rsp, 0, 0),
            Err(e) => {
                crate::println!("\x1b[91m{}: {}\x1b[0m", current_name(), e.as_str());
                terminate(-(e as i64));
            }
        },
    };
    drop(start);
    // SAFETY: the entry point and stack were just set up in this address space.
    unsafe { enter_user(entry, rsp, rdi, rsi) }
}

/// Loads a justOS program; returns its entry, stack and the two arguments
/// of its entry function (the argument text and its length).
fn native_image(start: &Start) -> (u64, u64, u64, u64) {
    let loaded = crate::driver::elf::load_with(&start.image, |pages| map_user(sys::USER_BASE, pages).map(|_| sys::USER_BASE), |_| {});
    let image = match loaded {
        Ok(i) => i,
        Err(e) => {
            crate::println!("\x1b[91m{}: {e}\x1b[0m", current_name());
            terminate(-(Error::BadProgram as i64));
        }
    };
    let stack_bottom = sys::USER_STACK_TOP - STACK_PAGES * PAGE_SIZE;
    if map_user(stack_bottom, STACK_PAGES).is_none() {
        terminate(-(Error::NoMemory as i64));
    }
    // Arguments at the top of the stack.
    let args = start.args.as_bytes();
    let args_at = (sys::USER_STACK_TOP - args.len() as u64 - 1) & !15;
    // SAFETY: the stack pages were just mapped in this address space.
    unsafe { core::ptr::copy_nonoverlapping(args.as_ptr(), args_at as *mut u8, args.len()) };
    let rsp = args_at - 8; // as if called: rsp % 16 == 8 at entry
    (image.entry, rsp, args_at, args.len() as u64)
}

/// Jumps to ring 3, with every other register cleared.
///
/// # Safety
/// `entry` and `rsp` must be mapped user memory of the running address space.
pub(crate) unsafe fn enter_user(entry: u64, rsp: u64, rdi: u64, rsi: u64) -> ! {
    // SAFETY: guaranteed by the caller. `swapgs` makes the thread's user GS
    // base active, as the return path of a trap would.
    unsafe {
        core::arch::asm!(
            "cli",
            "swapgs",
            "push {ss}",
            "push {rsp}",
            "push 0x202",
            "push {cs}",
            "push {rip}",
            "xor eax, eax",
            "xor ebx, ebx",
            "xor ecx, ecx",
            "xor edx, edx",
            "xor ebp, ebp",
            "xor r8d, r8d",
            "xor r9d, r9d",
            "xor r10d, r10d",
            "xor r11d, r11d",
            "xor r12d, r12d",
            "xor r13d, r13d",
            "xor r14d, r14d",
            "xor r15d, r15d",
            "iretq",
            ss = in(reg) gdt::USER_DATA as u64,
            rsp = in(reg) rsp,
            cs = in(reg) gdt::USER_CODE as u64,
            rip = in(reg) entry,
            in("rdi") rdi,
            in("rsi") rsi,
            options(noreturn)
        );
    }
}

/// The name of process `pid` ("" when there is none).
pub(crate) fn name_of(pid: u32) -> String {
    PROCS.lock().get(&pid).map(|p| p.name.clone()).unwrap_or_default()
}

pub(crate) fn current_name() -> String {
    let pid = task::current_pid();
    PROCS.lock().get(&pid).map(|p| p.name.clone()).unwrap_or_default()
}

/// Ends the calling process. With other threads still running, they are
/// told to stop; the last one out frees the address space.
pub(crate) fn terminate(code: i64) -> ! {
    let pid = task::current_pid();
    let me = task::current_tid();
    let mut handles = Vec::new();
    let last = {
        let mut procs = PROCS.lock();
        match procs.get_mut(&pid) {
            Some(p) => {
                let code = *p.exit_code.get_or_insert(code);
                p.threads.retain(|&t| t != me);
                if p.threads.is_empty() {
                    p.state = State::Exited(code);
                    handles = core::mem::take(&mut p.handles);
                    let cr3 = core::mem::replace(&mut p.cr3, 0);
                    // A vfork child's memory is its parent's.
                    Some(if core::mem::replace(&mut p.shared_vm, false) { 0 } else { cr3 })
                } else {
                    p.kill = true;
                    None
                }
            }
            None => Some(0),
        }
    };
    match last {
        Some(cr3) => {
            for h in handles.into_iter().flatten() {
                h.release();
            }
            // Ports made by the kernel on the process's behalf, too.
            for id in crate::ipc::owned_by(pid) {
                crate::ipc::close(id);
            }
            teardown(pid, cr3)
        }
        None => task::exit(),
    }
}

/// The calling thread ends; the process goes on if it has others.
pub(crate) fn exit_thread(code: i64) -> ! {
    let pid = task::current_pid();
    let me = task::current_tid();
    let others = PROCS.lock().get(&pid).is_some_and(|p| p.threads.iter().any(|&t| t != me));
    if !others {
        terminate(code);
    }
    if let Some(p) = PROCS.lock().get_mut(&pid) {
        p.threads.retain(|&t| t != me);
    }
    task::exit()
}

fn teardown(pid: u32, cr3: u64) -> ! {
    cpu::disable_irq();
    // Descriptors (sockets, pipes) are closed before the memory goes.
    cpu::enable_irq();
    crate::linux::forget(pid);
    cpu::disable_irq();
    if cr3 != 0 {
        task::leave_address_space();
        paging::free_address_space(cr3);
    }
    crate::gui::window::process_ended(pid);
    // A Linux parent hears of it (SIGCHLD), and may have said not to wait.
    let ended = PROCS.lock().get(&pid).and_then(|p| match p.state {
        State::Exited(code) => p.parent.map(|parent| (parent, code)),
        State::Running => None,
    });
    let reap = ended.is_some_and(|(parent, code)| crate::linux::signal::child_ended(parent, pid, code));
    settle(pid, reap);
    EXITED.notify_all();
    task::exit();
}

/// The first thread of the calling process.
pub(crate) fn main_thread() -> Option<Tid> {
    let pid = task::current_pid();
    PROCS.lock().get(&pid).map(|p| p.tid)
}

struct ThreadStart {
    frame: TrapFrame,
    fs: u64,
}

/// Starts another thread in the calling process, resuming user mode with
/// the registers of `frame` (a copy of the parent's, as `clone` makes it).
/// The thread is created suspended: wake it with [`task::wake_new`].
pub(crate) fn spawn_thread(frame: TrapFrame, fs: u64) -> Result<Tid, Error> {
    let pid = task::current_pid();
    let (cr3, name) = {
        let procs = PROCS.lock();
        let p = procs.get(&pid).ok_or(Error::Unknown)?;
        if p.kill {
            return Err(Error::Unknown);
        }
        (p.cr3, p.name.clone())
    };
    if PROCS.lock().get(&pid).map_or(0, |p| p.threads.len()) >= 512 {
        return Err(Error::NoMemory);
    }
    let arg = Box::into_raw(Box::new(ThreadStart { frame, fs }));
    match task::spawn_suspended(&name, thread_start, arg as *mut c_void, cr3, pid) {
        Some(tid) => {
            if let Some(p) = PROCS.lock().get_mut(&pid) {
                p.threads.push(tid);
            }
            Ok(tid)
        }
        None => {
            // SAFETY: not handed to any thread; take it back.
            drop(unsafe { Box::from_raw(arg) });
            Err(Error::NoMemory)
        }
    }
}

unsafe extern "C" fn thread_start(arg: *mut c_void) {
    // SAFETY: the Box leaked by spawn_thread, used once.
    let start = unsafe { Box::from_raw(arg as *mut ThreadStart) };
    let frame = start.frame;
    let (_, gs) = crate::arch::syscall::user_bases();
    crate::arch::syscall::set_user_bases(start.fs, gs);
    drop(start);
    // SAFETY: the frame holds user-mode registers of this address space.
    unsafe { resume_user(&frame) }
}

/// Returns to user mode with every register from `frame`, through the
/// common trap return path.
///
/// # Safety
/// `frame` must describe user code and stack of the running address space.
unsafe fn resume_user(frame: &TrapFrame) -> ! {
    let mut f = *frame;
    f.cs = gdt::USER_CODE as u64;
    f.ss = gdt::USER_DATA as u64;
    f.rflags |= 0x200;
    extern "C" {
        fn trap_return();
    }
    // SAFETY: `f` lives on this kernel stack; with interrupts off, rsp is
    // pointed at it and trap_return pops it and leaves with iretq.
    unsafe {
        core::arch::asm!(
            "cli",
            "mov rsp, {f}",
            "jmp {ret}",
            f = in(reg) &f as *const TrapFrame,
            ret = sym trap_return,
            options(noreturn)
        );
    }
}

// ----------------------------------------------------------------------------
// Process control for the shell

/// Waits for a process to end and returns its exit code.
pub fn wait(pid: u32) -> Option<i64> {
    let code = EXITED.wait_for(|| match PROCS.lock().get(&pid) {
        Some(p) => match p.state {
            State::Exited(c) => Some(Some(c)),
            State::Running => None,
        },
        None => Some(None),
    });
    PROCS.lock().remove(&pid);
    code
}

/// Runs a program in the foreground: Ctrl+C stops it.
pub fn run_foreground(path: &Path, args: &str, cwd: Path) -> Result<i64, Error> {
    let pid = spawn(path, args, cwd)?;
    *FOREGROUND.lock() = Some(pid);
    let code = wait(pid);
    *FOREGROUND.lock() = None;
    Ok(code.unwrap_or(-1))
}

/// Runs a Linux program in the foreground under a chosen `argv0`. Multi-call
/// programs such as BusyBox pick which tool to be from `argv[0]`, so this is
/// how the shell runs `ls` out of a single `busybox` image.
pub fn run_applet_foreground(path: &Path, argv0: &str, args: &str, cwd: Path) -> Result<i64, Error> {
    let image = fs::read_file(path).map_err(fs_error)?;
    let pid = start(image, String::from(argv0), args, cwd, Kind::Linux, path, current_app())?;
    *FOREGROUND.lock() = Some(pid);
    let code = wait(pid);
    *FOREGROUND.lock() = None;
    Ok(code.unwrap_or(-1))
}

/// Ctrl+C: the foreground program and the processes it made stop, except
/// Linux programs that handle SIGINT (they get it) or ignore it. Returns
/// whether there was a foreground program.
pub fn interrupt_foreground() -> bool {
    let Some(pid) = *FOREGROUND.lock() else { return false };
    let tree = {
        let procs = PROCS.lock();
        let mut tree = Vec::new();
        let mut todo = alloc::vec![pid];
        while let Some(id) = todo.pop() {
            if procs.get(&id).is_some_and(|p| p.state == State::Running) {
                tree.push(id);
            }
            todo.extend(procs.iter().filter(|(_, c)| c.parent == Some(id)).map(|(&c, _)| c));
        }
        tree
    };
    if tree.is_empty() {
        return false;
    }
    let stop: Vec<u32> = tree.into_iter().filter(|&id| !crate::linux::signal::interrupt(id)).collect();
    {
        let mut procs = PROCS.lock();
        for id in stop {
            if let Some(p) = procs.get_mut(&id) {
                p.kill = true;
            }
        }
    }
    EXITED.notify_all();
    true
}

/// Asks a process to stop, with the processes it made (fork) and theirs.
pub fn kill(pid: u32) -> bool {
    let found = {
        let mut procs = PROCS.lock();
        let found = procs.get(&pid).is_some_and(|p| p.state == State::Running);
        let mut todo = alloc::vec![pid];
        while let Some(id) = todo.pop() {
            if let Some(p) = procs.get_mut(&id) {
                if p.state == State::Running {
                    p.kill = true;
                }
            }
            todo.extend(procs.iter().filter(|(_, c)| c.parent == Some(id)).map(|(&c, _)| c));
        }
        found
    };
    // Wake those waiting for children or for a vfork child, so they see it.
    EXITED.notify_all();
    found
}

pub(crate) fn kill_requested() -> bool {
    let pid = task::current_pid();
    PROCS.lock().get(&pid).is_some_and(|p| p.kill)
}

/// Is `pid` a running process?
pub(crate) fn is_running(pid: u32) -> bool {
    PROCS.lock().get(&pid).is_some_and(|p| p.state == State::Running)
}

/// Stops one process (SIGKILL), not the ones it made.
pub(crate) fn kill_one(pid: u32) {
    if let Some(p) = PROCS.lock().get_mut(&pid) {
        if p.state == State::Running {
            p.kill = true;
        }
    }
    EXITED.notify_all();
}

/// Wakes the threads waiting for a child, to look at their signals.
pub(crate) fn wake_waiters() {
    EXITED.notify_all();
}

/// The first thread of a running process.
pub(crate) fn main_thread_of(pid: u32) -> Option<Tid> {
    PROCS.lock().get(&pid).filter(|p| p.state == State::Running).map(|p| p.tid)
}

/// Called on timer interrupts: a program in user mode that is being
/// stopped stops, and a Linux program gets the signal waiting for it.
pub fn check_kill(frame: &mut TrapFrame) {
    if !frame.is_user() {
        return;
    }
    if kill_requested() {
        cpu::enable_irq();
        terminate(-130);
    }
    crate::linux::signal::at_interrupt(frame);
}

pub fn count() -> usize {
    PROCS.lock().values().filter(|p| p.state == State::Running).count()
}

/// Whether a running process was started from the program file `exe`
/// (an app being updated waits until it is closed).
pub fn running(exe: &Path) -> bool {
    let same = |p: &Path| p.volume.eq_ignore_ascii_case(&exe.volume) && p.inner.eq_ignore_ascii_case(&exe.inner);
    PROCS.lock().values().any(|p| p.state == State::Running && same(&p.exe))
}

// ----------------------------------------------------------------------------
// User memory

/// Checks that `len` bytes at `ptr` are mapped user memory of the running
/// process and returns them.
pub(crate) fn user_slice<'a>(ptr: u64, len: u64) -> Result<&'a mut [u8], Error> {
    if len == 0 {
        return Ok(&mut []);
    }
    let end = ptr.checked_add(len).ok_or(Error::BadAddress)?;
    if ptr == 0 || end > USER_TOP {
        return Err(Error::BadAddress);
    }
    // Pages a Linux program reserved but never touched are made now.
    crate::linux::mm::populate(ptr, len);
    let cr3 = cpu::read_cr3();
    let mut page = mem::align_down(ptr, PAGE_SIZE);
    while page < end {
        if !paging::user_accessible(cr3, page) {
            return Err(Error::BadAddress);
        }
        page += PAGE_SIZE;
    }
    // SAFETY: every page of the range is mapped for user access in the
    // current address space. Other threads of the process may write to it
    // too, as they could on Linux; the kernel only copies bytes.
    Ok(unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len as usize) })
}

/// Reads a NUL-terminated string (as C and Linux pass them), up to `max`
/// bytes. Only the bytes before the NUL need to be mapped and valid UTF-8.
pub(crate) fn user_cstr(ptr: u64, max: u64) -> Result<String, Error> {
    if ptr == 0 {
        return Err(Error::BadAddress);
    }
    let cr3 = cpu::read_cr3();
    let mut out = Vec::new();
    let mut at = ptr;
    while (out.len() as u64) < max {
        if at < USER_TOP && !paging::user_accessible(cr3, mem::align_down(at, PAGE_SIZE)) {
            crate::linux::mm::populate(at, 1);
        }
        if at >= USER_TOP || !paging::user_accessible(cr3, mem::align_down(at, PAGE_SIZE)) {
            return Err(Error::BadAddress);
        }
        // SAFETY: the page holding `at` is mapped for user access.
        let b = unsafe { *(at as *const u8) };
        if b == 0 {
            return String::from_utf8(out).map_err(|_| Error::InvalidName);
        }
        out.push(b);
        at += 1;
    }
    Err(Error::InvalidName)
}

pub(crate) fn user_str<'a>(ptr: u64, len: u64) -> Result<&'a str, Error> {
    if len > 4096 {
        return Err(Error::InvalidName);
    }
    core::str::from_utf8(user_slice(ptr, len)?).map_err(|_| Error::InvalidName)
}

fn user_path(ptr: u64, len: u64) -> Result<Path, Error> {
    let text = user_str(ptr, len)?;
    Path::parse(text, &cwd()?).map_err(fs_error)
}

/// `path`, if the calling process's sandbox lets it change it.
fn writable(path: Path) -> Result<Path, Error> {
    crate::sandbox::check(&path, true).map(|_| path)
}

/// The current folder of the calling process.
pub(crate) fn cwd() -> Result<Path, Error> {
    let pid = task::current_pid();
    PROCS.lock().get(&pid).map(|p| p.cwd.clone()).ok_or(Error::Unknown)
}

pub(crate) fn set_cwd(path: Path) {
    let pid = task::current_pid();
    if let Some(p) = PROCS.lock().get_mut(&pid) {
        p.cwd = path;
    }
}

/// Is `[addr, addr + len)` a page-aligned range of the user half?
fn user_range(addr: u64, len: u64) -> Result<u64, Error> {
    let pages = mem::align_up(len.max(1), PAGE_SIZE) / PAGE_SIZE;
    let end = pages.checked_mul(PAGE_SIZE).and_then(|b| addr.checked_add(b)).ok_or(Error::BadAddress)?;
    if addr == 0 || !addr.is_multiple_of(PAGE_SIZE) || end > USER_TOP {
        return Err(Error::BadAddress);
    }
    Ok(pages)
}

/// Maps fresh pages at a fixed address, if none of them is in use.
pub(crate) fn map_fixed(addr: u64, len: u64) -> Result<(), Error> {
    let pages = user_range(addr, len)?;
    let cr3 = cpu::read_cr3();
    if (0..pages).any(|i| paging::translate_in(cr3, addr + i * PAGE_SIZE).is_some()) {
        return Err(Error::Exists);
    }
    map_user(addr, pages).ok_or(Error::NoMemory)
}

/// Unmaps and frees the user pages of a range (gaps are skipped).
pub(crate) fn unmap_range(addr: u64, len: u64) -> Result<(), Error> {
    let pages = user_range(addr, len)?;
    let cr3 = cpu::read_cr3();
    for i in 0..pages {
        let virt = addr + i * PAGE_SIZE;
        if paging::user_accessible(cr3, virt) {
            if let Some(phys) = paging::unmap_in(cr3, virt) {
                frame::free(phys);
            }
        }
    }
    Ok(())
}

/// Sets the FS or GS base of the calling thread.
pub(crate) fn set_tls(which: u64, base: u64) -> Result<(), Error> {
    if base >= USER_TOP {
        return Err(Error::BadAddress);
    }
    let (fs, gs) = crate::arch::syscall::user_bases();
    match which {
        sys::TLS_FS => crate::arch::syscall::set_user_bases(base, gs),
        sys::TLS_GS => crate::arch::syscall::set_user_bases(fs, base),
        _ => return Err(Error::Unknown),
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// System calls

pub(crate) fn key_code(ev: crate::dev::input::KeyEvent) -> u64 {
    use crate::dev::input::Key;
    let code = match ev.key {
        Key::Char(c) => c as u32,
        Key::Enter => sys::KEY_ENTER,
        Key::Backspace => sys::KEY_BACKSPACE,
        Key::Delete => sys::KEY_DELETE,
        Key::Tab => sys::KEY_TAB,
        Key::Escape => sys::KEY_ESCAPE,
        Key::Up => sys::KEY_UP,
        Key::Down => sys::KEY_DOWN,
        Key::Left => sys::KEY_LEFT,
        Key::Right => sys::KEY_RIGHT,
        Key::Home => sys::KEY_HOME,
        Key::End => sys::KEY_END,
        Key::PageUp => sys::KEY_PAGE_UP,
        Key::PageDown => sys::KEY_PAGE_DOWN,
        Key::Insert => sys::KEY_INSERT,
        Key::F(n) => sys::KEY_F + n as u32,
        // The desktop keeps these for itself.
        Key::PrintScreen | Key::Lock | Key::Overview | Key::PrevDesktop | Key::NextDesktop => 0,
    };
    let mut v = code as u64;
    if ev.ctrl {
        v |= sys::MOD_CTRL;
    }
    if ev.alt {
        v |= sys::MOD_ALT;
    }
    if ev.shift {
        v |= sys::MOD_SHIFT;
    }
    v
}

fn with_handle<T>(h: u64, f: impl FnOnce(&mut Handle) -> T) -> Result<T, Error> {
    let pid = task::current_pid();
    let mut procs = PROCS.lock();
    let p = procs.get_mut(&pid).ok_or(Error::Unknown)?;
    let slot = h.checked_sub(1).and_then(|i| p.handles.get_mut(i as usize)).ok_or(Error::BadHandle)?;
    slot.as_mut().map(f).ok_or(Error::BadHandle)
}

/// Puts a handle in the calling process's table; returns its number.
fn install(handle: Handle) -> Result<u64, Error> {
    let pid = task::current_pid();
    let mut procs = PROCS.lock();
    let p = procs.get_mut(&pid).ok_or(Error::Unknown)?;
    let slot = match p.handles.iter().position(|h| h.is_none()) {
        Some(i) => i,
        None if p.handles.len() < MAX_HANDLES => {
            p.handles.push(None);
            p.handles.len() - 1
        }
        None => return Err(Error::NoMemory),
    };
    p.handles[slot] = Some(handle);
    Ok(slot as u64 + 1)
}

/// The port behind a handle, and whether the process owns it.
fn port_of(h: u64) -> Result<(u32, bool), Error> {
    with_handle(h, |h| match h {
        Handle::Port { id, own } => Ok((*id, *own)),
        Handle::File { .. } => Err(Error::BadHandle),
    })?
}

/// The app process `pid` runs as.
pub(crate) fn app_of(pid: u32) -> Option<crate::sandbox::App> {
    PROCS.lock().get(&pid).and_then(|p| p.app.clone())
}

/// The app the calling thread's process runs as.
pub(crate) fn current_app() -> Option<crate::sandbox::App> {
    app_of(task::current_pid())
}

/// Lets process `pid` use a file outside its sandbox: one the user chose for
/// it in the picker (`write`: and save over it) or dropped on its window.
/// Nothing to do for a program without a sandbox.
pub(crate) fn grant(pid: u32, path: Path, write: bool) {
    if let Some(p) = PROCS.lock().get_mut(&pid) {
        if p.app.is_some() && p.picked.len() < 256 && !p.picked.iter().any(|(q, w)| *q == path && (*w || !write)) {
            p.picked.push((path, write));
        }
    }
}

/// The calling process's sandbox: its app and the files picked for it
/// (None: no sandbox).
pub(crate) fn sandbox() -> Option<(crate::sandbox::App, Vec<(Path, bool)>)> {
    let pid = task::current_pid();
    let procs = PROCS.lock();
    let p = procs.get(&pid)?;
    Some((p.app.clone()?, p.picked.clone()))
}

/// `APP_ENTER`: the calling process becomes `app`, for good.
fn enter_app(app: crate::sandbox::App) -> Result<(), Error> {
    let pid = task::current_pid();
    if let Some(cur) = app_of(pid) {
        return if cur.id == app.id { Ok(()) } else { Err(Error::Denied) };
    }
    let home = crate::sandbox::prepare(&app).ok_or(Error::Io)?;
    let mut procs = PROCS.lock();
    let p = procs.get_mut(&pid).ok_or(Error::Unknown)?;
    match &p.app {
        // Another thread was first.
        Some(cur) if cur.id != app.id => Err(Error::Denied),
        Some(_) => Ok(()),
        None => {
            p.app = Some(app);
            p.cwd = home;
            Ok(())
        }
    }
}

/// The name of a process (0 is the kernel).
pub(crate) fn process_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return Some(String::from("kernel"));
    }
    PROCS.lock().get(&pid).map(|p| p.name.clone())
}

fn call(n: u64, a: [u64; 5]) -> Result<u64, Error> {
    match n {
        sys::WRITE => {
            let data = user_slice(a[1], a[2])?;
            if a[0] == sys::CONSOLE {
                crate::print!("{}", String::from_utf8_lossy(data));
                return Ok(a[2]);
            }
            let (path, pos, write) = with_handle(a[0], |h| h.file().map(|(path, pos, write)| (path.clone(), *pos, write)))??;
            if !write {
                return Err(Error::ReadOnly);
            }
            let data = data.to_vec();
            fs::write_at(&path, pos, &data).map_err(fs_error)?;
            with_handle(a[0], |h| h.file().map(|(_, pos, _)| *pos += data.len() as u64))??;
            Ok(data.len() as u64)
        }
        sys::READ => {
            let (path, pos) = with_handle(a[0], |h| h.file().map(|(path, pos, _)| (path.clone(), *pos)))??;
            let buf = user_slice(a[1], a[2])?;
            let mut tmp = alloc::vec![0u8; buf.len().min(1 << 20)];
            let n = fs::read_at(&path, pos, &mut tmp).map_err(fs_error)?;
            buf[..n].copy_from_slice(&tmp[..n]);
            with_handle(a[0], |h| h.file().map(|(_, pos, _)| *pos += n as u64))??;
            Ok(n as u64)
        }
        sys::READ_KEY => {
            let until = if a[0] == u64::MAX { u64::MAX } else { crate::time::uptime_ms().saturating_add(a[0]) };
            loop {
                if let Some(ev) = crate::dev::input::try_read_key() {
                    return Ok(key_code(ev));
                }
                if kill_requested() {
                    terminate(-130);
                }
                if crate::time::uptime_ms() >= until {
                    return Ok(0);
                }
                task::sleep_ms(5);
            }
        }
        sys::OPEN => {
            let path = user_path(a[0], a[1])?;
            let mode = a[2];
            crate::sandbox::check(&path, mode != sys::MODE_READ)?;
            match fs::stat(&path) {
                Ok(st) if st.kind == justfs::Kind::Dir => return Err(Error::IsDir),
                Ok(_) => {
                    if mode == sys::MODE_WRITE {
                        fs::write_file(&path, &[]).map_err(fs_error)?;
                    }
                }
                Err(fs::Error::NotFound) if mode != sys::MODE_READ => fs::write_file(&path, &[]).map_err(fs_error)?,
                Err(e) => return Err(fs_error(e)),
            }
            let pos = if mode == sys::MODE_APPEND { fs::stat(&path).map(|s| s.size).unwrap_or(0) } else { 0 };
            install(Handle::File { path, pos, write: mode != sys::MODE_READ })
        }
        sys::CLOSE => {
            let pid = task::current_pid();
            let h = {
                let mut procs = PROCS.lock();
                let p = procs.get_mut(&pid).ok_or(Error::Unknown)?;
                let slot = a[0].checked_sub(1).and_then(|i| p.handles.get_mut(i as usize)).ok_or(Error::BadHandle)?;
                slot.take().ok_or(Error::BadHandle)?
            };
            h.release();
            Ok(0)
        }
        sys::SEEK => with_handle(a[0], |h| h.file().map(|(_, pos, _)| *pos = a[1]))?.map(|_| 0),
        sys::FILE_SIZE => {
            let path = with_handle(a[0], |h| h.file().map(|(path, _, _)| path.clone()))??;
            fs::stat(&path).map(|s| s.size).map_err(fs_error)
        }
        sys::PORT_CREATE => {
            let name = user_str(a[0], a[1])?;
            // An app names its ports after itself: it cannot pose as the
            // system's services or another app's.
            if let Some(app) = current_app() {
                if !name.is_empty() && name != app.id && !name.strip_prefix(app.id.as_str()).is_some_and(|r| r.starts_with('.')) {
                    return Err(Error::Denied);
                }
            }
            let id = crate::ipc::create(task::current_pid(), (!name.is_empty()).then_some(name))?;
            install(Handle::Port { id, own: true }).inspect_err(|_| crate::ipc::close(id))
        }
        sys::PORT_LOOKUP => {
            let id = crate::ipc::lookup(user_str(a[0], a[1])?)?;
            install(Handle::Port { id, own: false }).inspect_err(|_| crate::ipc::drop_sender(id))
        }
        sys::PORT_SEND => {
            let id = port_of(a[0])?.0;
            let data = user_slice(a[1], a[2].min(sys::PORT_MSG_MAX + 1))?.to_vec();
            crate::ipc::send(id, data, task::current_pid(), None).map(|_| 0)
        }
        sys::PORT_RECV => {
            let (id, own) = port_of(a[0])?;
            if !own {
                return Err(Error::BadHandle);
            }
            // Check the buffers before taking a message off the queue.
            user_slice(a[1], a[2])?;
            user_slice(a[4], 24)?;
            let timeout = (a[3] != sys::FOREVER).then_some(a[3]);
            let m = crate::ipc::recv(id, task::current_pid(), timeout, &kill_requested)?;
            let reply = match m.reply {
                Some(r) => install(Handle::Port { id: r, own: false }).inspect_err(|_| crate::ipc::drop_sender(r)).unwrap_or(0),
                None => 0,
            };
            let buf = user_slice(a[1], a[2])?;
            let n = m.data.len().min(buf.len());
            buf[..n].copy_from_slice(&m.data[..n]);
            let info = user_slice(a[4], 24)?;
            for (i, w) in [m.data.len() as u64, m.sender as u64, reply].iter().enumerate() {
                info[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
            }
            Ok(n as u64)
        }
        sys::PORT_CALL => {
            let id = port_of(a[0])?.0;
            let data = user_slice(a[1], a[2].min(sys::PORT_MSG_MAX + 1))?.to_vec();
            user_slice(a[3], a[4])?;
            let answer = crate::ipc::call(id, data, task::current_pid(), None, &kill_requested)?;
            let buf = user_slice(a[3], a[4])?;
            let n = answer.len().min(buf.len());
            buf[..n].copy_from_slice(&answer[..n]);
            Ok(answer.len() as u64)
        }
        sys::PROC_NAME => {
            let name = process_name(a[0] as u32).ok_or(Error::NotFound)?;
            let buf = user_slice(a[1], a[2])?;
            let n = name.len().min(buf.len());
            buf[..n].copy_from_slice(&name.as_bytes()[..n]);
            Ok(name.len() as u64)
        }
        sys::APP_ID => {
            let id = current_app().ok_or(Error::NotFound)?.id;
            let buf = user_slice(a[0], a[1])?;
            let n = id.len().min(buf.len());
            buf[..n].copy_from_slice(&id.as_bytes()[..n]);
            Ok(id.len() as u64)
        }
        sys::APP_ENTER => {
            let id = user_str(a[0], a[1])?;
            let name = user_str(a[2], a[3])?.trim();
            if !crate::sandbox::valid_id(id) || name.chars().count() > 64 {
                return Err(Error::InvalidName);
            }
            let name = if name.is_empty() { id } else { name };
            enter_app(crate::sandbox::App { id: id.to_ascii_lowercase(), name: name.to_string(), icon: String::from("window") }).map(|_| 0)
        }
        sys::PICK_FILE => {
            let save = a[0] == sys::PICK_SAVE;
            let suggested = user_str(a[1], a[2])?.to_string();
            user_slice(a[3], a[4])?;
            if !crate::gui::active() {
                return Err(Error::Unknown);
            }
            let who = current_app().map(|a| a.name).unwrap_or_else(current_name);
            let chosen = crate::gui::pick_file(&who, save, &suggested).ok_or(Error::Cancelled)?;
            let path = fs::abs(&chosen).map_err(fs_error)?;
            // What the user chose, the app may use (and save over).
            grant(task::current_pid(), path, true);
            let buf = user_slice(a[3], a[4])?;
            let n = chosen.len().min(buf.len());
            buf[..n].copy_from_slice(&chosen.as_bytes()[..n]);
            Ok(chosen.len() as u64)
        }
        sys::LIST_DIR => {
            let path = user_path(a[0], a[1])?;
            crate::sandbox::check(&path, false)?;
            let mut text = String::new();
            for (e, st) in fs::list_dir(&path).map_err(fs_error)? {
                let kind = if st.kind == justfs::Kind::Dir { 'd' } else { 'f' };
                text.push_str(&alloc::format!("{}\t{}\t{}\n", e.name, kind, st.size));
            }
            let buf = user_slice(a[2], a[3])?;
            let n = text.len().min(buf.len());
            buf[..n].copy_from_slice(&text.as_bytes()[..n]);
            Ok(text.len() as u64)
        }
        sys::REMOVE => fs::remove(&writable(user_path(a[0], a[1])?)?).map(|_| 0).map_err(fs_error),
        sys::MAKE_DIR => fs::make_dir(&writable(user_path(a[0], a[1])?)?).map(|_| 0).map_err(fs_error),
        sys::RENAME => {
            let from = writable(user_path(a[0], a[1])?)?;
            let to = writable(user_path(a[2], a[3])?)?;
            fs::rename(&from, &to).map(|_| 0).map_err(fs_error)
        }
        sys::UPTIME => Ok(crate::time::uptime_ms()),
        sys::SLEEP => {
            task::sleep_ms(a[0].min(3_600_000));
            Ok(0)
        }
        sys::NOW => Ok(crate::time::now()),
        sys::YIELD => {
            task::yield_now();
            Ok(0)
        }
        sys::SPAWN => {
            let name = user_str(a[0], a[1])?.to_string();
            let args = user_str(a[2], a[3])?.to_string();
            let cwd = cwd()?;
            let path = find_program(&name, &cwd).ok_or(Error::NotFound)?;
            crate::sandbox::check(&path, false)?;
            spawn(&path, &args, cwd).map(|p| p as u64)
        }
        sys::WAIT => wait(a[0] as u32).map(|c| c as u64).ok_or(Error::NotFound),
        sys::MAP => {
            let pages = mem::align_up(a[0].max(1), PAGE_SIZE) / PAGE_SIZE;
            if pages > 1 << 18 {
                return Err(Error::NoMemory);
            }
            let pid = task::current_pid();
            let at = {
                let mut procs = PROCS.lock();
                let p = procs.get_mut(&pid).ok_or(Error::Unknown)?;
                let at = p.heap_next;
                p.heap_next += pages * PAGE_SIZE;
                at
            };
            map_user(at, pages).ok_or(Error::NoMemory)?;
            Ok(at)
        }
        sys::SYSINFO => {
            let out = user_slice(a[0], (sys::SYSINFO_WORDS * 8) as u64)?;
            let (free, total) = frame::stats();
            let devs = crate::dev::list();
            let words = [
                total,
                free,
                crate::time::uptime_ms(),
                task::list().len() as u64,
                count() as u64,
                devs.len() as u64,
                devs.iter().filter(|d| d.has_driver()).count() as u64,
                crate::dev::block::list().iter().filter(|d| d.parent.is_none()).count() as u64,
            ];
            for (i, w) in words.iter().enumerate() {
                out[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
            }
            Ok(sys::SYSINFO_WORDS as u64)
        }
        sys::SCREEN => {
            let (cols, rows) = crate::console::size();
            Ok((cols as u64) << 32 | rows as u64)
        }
        sys::GET_CWD => {
            let pid = task::current_pid();
            let cwd = PROCS.lock().get(&pid).map(|p| p.cwd.to_string()).ok_or(Error::Unknown)?;
            let buf = user_slice(a[0], a[1])?;
            let n = cwd.len().min(buf.len());
            buf[..n].copy_from_slice(&cwd.as_bytes()[..n]);
            Ok(cwd.len() as u64)
        }
        sys::SET_CWD => {
            let path = user_path(a[0], a[1])?;
            crate::sandbox::check(&path, false)?;
            if !fs::is_dir(&path) {
                return Err(Error::NotDir);
            }
            set_cwd(path);
            Ok(0)
        }
        sys::SET_TLS => set_tls(a[0], a[1]).map(|_| 0),
        sys::MAP_AT => map_fixed(a[0], a[1]).map(|_| a[0]),
        sys::WIN_CREATE => {
            let title = user_str(a[0], a[1])?.to_string();
            let (w, h) = ((a[2] >> 32) as i32, (a[2] & 0xFFFF_FFFF) as i32);
            crate::gui::window::create(task::current_pid(), &title, w, h, a[3])
        }
        sys::WIN_PRESENT => {
            use crate::gui::window::{MAX_H, MAX_SCALE, MAX_W};
            let (w, h) = ((a[2] >> 32) as i64, (a[2] & 0xFFFF_FFFF) as i64);
            let scale = a[3].clamp(1, MAX_SCALE as u64) as i64;
            if !(1..=MAX_W as i64 * scale).contains(&w) || !(1..=MAX_H as i64 * scale).contains(&h) || a[3] > MAX_SCALE as u64 {
                return Err(Error::BadAddress);
            }
            let pixels = user_slice(a[1], (w * h * 4) as u64)?;
            crate::gui::window::present(task::current_pid(), a[0], pixels, w as i32, h as i32, scale as i32).map(|_| 0)
        }
        sys::WIN_SCALE => crate::gui::window::scale(task::current_pid(), a[0]),
        sys::WIN_EVENT => {
            let out = user_slice(a[1], 32)?;
            match crate::gui::window::next_event(task::current_pid(), a[0], a[2])? {
                Some(ev) => {
                    for (i, w) in ev.iter().enumerate() {
                        out[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
                    }
                    Ok(1)
                }
                None => Ok(0),
            }
        }
        sys::WIN_CLOSE => crate::gui::window::close(task::current_pid(), a[0]).map(|_| 0),
        sys::WIN_DROPPED => {
            let text = crate::gui::window::dropped(task::current_pid(), a[0])?;
            let buf = user_slice(a[1], a[2])?;
            let n = text.len().min(buf.len());
            buf[..n].copy_from_slice(&text.as_bytes()[..n]);
            Ok(text.len() as u64)
        }
        sys::WIN_TITLE => {
            let title = user_str(a[1], a[2])?.to_string();
            crate::gui::window::set_title(task::current_pid(), a[0], &title).map(|_| 0)
        }
        sys::UNMAP => unmap_range(a[0], a[1]).map(|_| 0),
        _ => Err(Error::Unknown),
    }
}

pub fn syscall(frame: &mut TrapFrame) {
    if !frame.is_user() {
        frame.rax = (-(Error::Unknown as i64)) as u64;
        return;
    }
    // System calls may block or take long: let interrupts in.
    cpu::enable_irq();
    let n = frame.rax;
    if n == sys::EXIT {
        terminate(frame.rdi as i64);
    }
    let args = [frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8];
    frame.rax = match call(n, args) {
        Ok(v) => v,
        Err(e) => (-(e as i64)) as u64,
    };
    if kill_requested() {
        terminate(-130);
    }
    cpu::disable_irq();
}

pub fn fault(frame: &mut TrapFrame, what: &str) {
    // A page a Linux program reserved and touches for the first time.
    if frame.vector == 14 && crate::linux::mm::fault(cpu::read_cr2(), frame.error & 2 != 0) {
        return;
    }
    let cr2 = cpu::read_cr2();
    cpu::enable_irq();
    // A Linux program may handle it (SIGSEGV and the like).
    if crate::linux::signal::fault(frame, cr2) {
        cpu::disable_irq();
        return;
    }
    let extra = if frame.vector == 14 { alloc::format!(" (address {:#x})", cpu::read_cr2()) } else { String::new() };
    crate::println!("\x1b[91m{} stopped: {what} at {:#x}{extra}\x1b[0m", current_name(), frame.rip);
    if let Some(title) = crate::gui::window::title_of(task::current_pid()) {
        crate::gui::notify(&title, "L'app si è chiusa per un errore.", "window");
    }
    terminate(-139);
}

/// A running program, for the activity monitor.
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    /// Memory it asked for (heap), in bytes.
    pub memory: u64,
}

pub fn processes() -> Vec<ProcInfo> {
    PROCS
        .lock()
        .iter()
        .filter(|(_, p)| p.state == State::Running)
        .map(|(&pid, p)| ProcInfo { pid, name: p.name.clone(), memory: p.heap_next - sys::USER_HEAP_BASE })
        .collect()
}
