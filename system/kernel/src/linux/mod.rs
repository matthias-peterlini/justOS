//! Running Linux programs.
//!
//! justOS loads statically linked Linux executables (built against musl or
//! glibc, or by the Go and Rust toolchains) and serves the Linux system
//! calls they make. Programs arrive on the `syscall` instruction (see
//! [`crate::arch::syscall`]) with the call number in `rax` and arguments in
//! `rdi, rsi, rdx, r10, r8, r9`, as on Linux for x86-64.
//!
//! What is there: files and folders (the Linux file tree is a view of the
//! process's justOS volume), the console as a terminal ([`tty`]), memory with
//! demand paging ([`mm`]), threads and futexes ([`thread`]), pipes and
//! eventfds ([`fd`]), TCP and UDP sockets ([`sock`]), local sockets that
//! pass descriptors along ([`unix`]), shared memory ([`shm`]), poll, select
//! and epoll
//! ([`poll`]), clocks and randomness. Dynamically linked programs start in
//! the dynamic loader they name (glibc's ld-linux-x86-64.so.2, found on the
//! volume like any file), which maps their shared libraries with `mmap`.
//! New processes come from fork, vfork and posix_spawn, and execve
//! replaces a program (scripts with `#!` too); parents wait for children
//! with wait4. Signals ([`signal`]) run the program's handlers, as on Linux.

pub mod fd;
pub mod mm;
mod poll;
mod shm;
pub mod signal;
mod sock;
mod thread;
mod tty;
mod unix;
pub mod wayland;

use crate::arch::idt::TrapFrame;
use crate::fs::Path;
use crate::mem::{self, PAGE_SIZE};
use crate::sync::IrqLock;
use crate::task;
use crate::user;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use justdk::syscall::Error as JustError;

/// Seconds between the Unix epoch (1970) and justOS's epoch (2000).
pub const UNIX_2000: u64 = 946_684_800;

// ----------------------------------------------------------------------------
// Errors (Linux errno values, returned negated)

#[allow(dead_code)]
pub mod err {
    pub const EPERM: i64 = 1;
    pub const ENOENT: i64 = 2;
    pub const ESRCH: i64 = 3;
    pub const EINTR: i64 = 4;
    pub const E2BIG: i64 = 7;
    pub const ENOEXEC: i64 = 8;
    pub const EBADF: i64 = 9;
    pub const ECHILD: i64 = 10;
    pub const EAGAIN: i64 = 11;
    pub const ENOMEM: i64 = 12;
    pub const EACCES: i64 = 13;
    pub const EFAULT: i64 = 14;
    pub const EEXIST: i64 = 17;
    pub const ENOTDIR: i64 = 20;
    pub const EISDIR: i64 = 21;
    pub const EINVAL: i64 = 22;
    pub const EMFILE: i64 = 24;
    pub const ENOTTY: i64 = 25;
    pub const EFBIG: i64 = 27;
    pub const ENOSPC: i64 = 28;
    pub const ESPIPE: i64 = 29;
    pub const EPIPE: i64 = 32;
    pub const ERANGE: i64 = 34;
    pub const ENOSYS: i64 = 38;
    pub const ENOTEMPTY: i64 = 39;
    pub const ENOTSOCK: i64 = 88;
    pub const EDESTADDRREQ: i64 = 89;
    pub const EMSGSIZE: i64 = 90;
    pub const EOPNOTSUPP: i64 = 95;
    pub const EAFNOSUPPORT: i64 = 97;
    pub const EADDRINUSE: i64 = 98;
    pub const ENETUNREACH: i64 = 101;
    pub const ECONNRESET: i64 = 104;
    pub const EISCONN: i64 = 106;
    pub const ENOTCONN: i64 = 107;
    pub const ETIMEDOUT: i64 = 110;
    pub const ECONNREFUSED: i64 = 111;
    pub const EALREADY: i64 = 114;
    pub const EINPROGRESS: i64 = 115;
}

pub fn errno(e: JustError) -> i64 {
    match e {
        JustError::NotFound => err::ENOENT,
        JustError::Exists => err::EEXIST,
        JustError::NotDir => err::ENOTDIR,
        JustError::IsDir => err::EISDIR,
        JustError::NotEmpty => err::ENOTEMPTY,
        JustError::NoSpace => err::ENOSPC,
        JustError::InvalidName => err::EINVAL,
        JustError::ReadOnly => err::EACCES,
        JustError::Io => err::EACCES,
        JustError::BadHandle => err::EBADF,
        JustError::BadAddress => err::EFAULT,
        JustError::NoMemory => err::ENOMEM,
        JustError::BadProgram => err::ENOENT,
        JustError::Unknown => err::EINVAL,
        JustError::Timeout => err::ETIMEDOUT,
        JustError::Closed => err::EPIPE,
        JustError::Full => err::EAGAIN,
        JustError::TooLarge => err::EINVAL,
        JustError::Denied => err::EACCES,
        JustError::Cancelled => err::EINTR,
    }
}

// ----------------------------------------------------------------------------
// Per-process state

pub struct LinuxState {
    /// Open descriptors and their close-on-exec flag.
    pub fds: BTreeMap<i32, (Arc<fd::Desc>, bool)>,
    pub brk_start: u64,
    pub brk_cur: u64,
    pub mmap_next: u64,
    /// Areas reserved with mmap (and the stack), by start address.
    pub vmas: BTreeMap<u64, mm::Vma>,
    /// Per thread: where to write 0 and wake when it ends.
    pub clear_tid: BTreeMap<task::Tid, u64>,
    pub sigactions: [[u8; 32]; 65],
    /// Per thread: the signal mask and alternate stack.
    pub sigthreads: BTreeMap<task::Tid, signal::ThreadSig>,
    /// Signals sent and not yet taken.
    pub pending: Vec<signal::Pending>,
    /// The program, as a Linux path.
    pub exe: String,
    /// What the loader placed (the program's and the dynamic loader's
    /// segments, the stack), for /proc/self/maps: start, end, protection,
    /// file offset and name.
    pub images: Vec<mm::Image>,
}

const BRK_BASE: u64 = 0x1000_0000_0000;

impl LinuxState {
    fn new() -> LinuxState {
        let mut fds = BTreeMap::new();
        let console = fd::Desc::new(fd::Kind::Console, fd::O_RDWR);
        for i in 0..3 {
            fds.insert(i, (console.clone(), false));
        }
        LinuxState {
            fds,
            brk_start: BRK_BASE,
            brk_cur: BRK_BASE,
            mmap_next: mm::MMAP_BASE,
            vmas: mm::new_map(),
            clear_tid: BTreeMap::new(),
            sigactions: [[0; 32]; 65],
            sigthreads: BTreeMap::new(),
            pending: Vec::new(),
            exe: String::new(),
            images: Vec::new(),
        }
    }
}

static STATES: IrqLock<BTreeMap<u32, LinuxState>> = IrqLock::new(BTreeMap::new());

/// Drops the Linux state of a process that is ending (closing its
/// descriptors outside the lock).
pub fn forget(pid: u32) {
    let st = STATES.lock().remove(&pid);
    if let Some(s) = &st {
        signal::forgotten(s);
    }
    drop(st);
}

pub fn with_state<T>(f: impl FnOnce(&mut LinuxState) -> T) -> T {
    let pid = task::current_pid();
    let mut states = STATES.lock();
    f(states.entry(pid).or_insert_with(LinuxState::new))
}

/// Like [`with_state`], but only for a process that already has a Linux
/// state (never creates one: native programs fault through here too).
pub fn try_state<T>(f: impl FnOnce(&mut LinuxState) -> T) -> Option<T> {
    let pid = task::current_pid();
    let mut states = STATES.lock();
    states.get_mut(&pid).map(f)
}

/// Runs `f` on another process's Linux state, if it has one.
pub fn with_state_of<T>(pid: u32, f: impl FnOnce(&mut LinuxState) -> T) -> Option<T> {
    STATES.lock().get_mut(&pid).map(f)
}

/// The Linux state of a child made by fork or vfork: a copy of the
/// caller's, with the same open descriptions (as Linux shares them), memory
/// areas and signal setup.
pub fn fork_state(child: u32, tid: task::Tid) {
    let pid = task::current_pid();
    let me = task::current_tid();
    let mut states = STATES.lock();
    let copy = states.get(&pid).map(|st| LinuxState {
        fds: st.fds.clone(),
        brk_start: st.brk_start,
        brk_cur: st.brk_cur,
        mmap_next: st.mmap_next,
        vmas: st.vmas.clone(),
        clear_tid: BTreeMap::new(),
        sigactions: st.sigactions,
        sigthreads: signal::forked(st, me, tid),
        pending: Vec::new(),
        exe: st.exe.clone(),
        images: st.images.clone(),
    });
    if let Some(c) = copy {
        states.insert(child, c);
    }
}

/// The program's path as Linux shows it.
pub fn exe_path() -> String {
    with_state(|st| st.exe.clone())
}

// ----------------------------------------------------------------------------
// Paths: a Linux path names a place in the running process's justOS volume.

pub fn linux_path(text: &str) -> Result<Path, i64> {
    let cwd = user::cwd().map_err(errno)?;
    // An absolute Linux path is rooted at the volume; a relative one hangs
    // off the current folder.
    let base = if text.starts_with('/') { Path::root(&cwd.volume) } else { cwd };
    Path::parse(text, &base).map_err(|e| errno(user::fs_error(e)))
}

/// Turns a justOS path back into the Linux path a program sees ("/a/b").
pub fn show_path(path: &Path) -> String {
    path.inner.clone()
}

pub fn user_bytes<'a>(ptr: u64, len: u64) -> Result<&'a mut [u8], i64> {
    user::user_slice(ptr, len).map_err(errno)
}

// ----------------------------------------------------------------------------
// Loading an executable

/// Loads a Linux program into the current (empty) address space and returns
/// where to start and the initial stack pointer. The stack holds argc, the
/// argument and environment pointers and the auxiliary vector, as the System
/// V ABI lays them out. A dynamically linked program starts in its dynamic
/// loader, which finds the program through the auxiliary vector.
pub fn load(elf: &[u8], name: &str, args: &str, exe: &str) -> Result<(u64, u64), JustError> {
    let argv = core::iter::once(name.to_string()).chain(justrt_split(args)).collect::<Vec<_>>();
    // The Linux view of the program's path ("/apps/rg").
    let exe_linux = match exe.split_once(':') {
        Some((_, rest)) => rest.to_string(),
        None => exe.to_string(),
    };
    tty::reset();
    load_program(elf, name, &argv, &default_env(), &exe_linux)
}

/// Maps a program (and the dynamic loader it names) into the current,
/// empty address space and builds its stack with `argv` and `envp`.
fn load_program(elf: &[u8], name: &str, argv: &[String], envp: &[String], exe_linux: &str) -> Result<(u64, u64), JustError> {
    let info = map_segments(elf, PIE_BASE, exe_linux).map_err(|_| JustError::BadProgram)?;
    let interp = match interpreter(elf) {
        Some(path) => Some(load_interpreter(name, &path)?),
        None => None,
    };
    with_state(|st| {
        st.exe = exe_linux.to_string();
        st.brk_start = info.brk;
        st.brk_cur = info.brk;
    });
    let interp_base = interp.as_ref().map_or(0, |i| i.base);
    let stack = build_stack(&info, interp_base, argv, envp, exe_linux)?;
    Ok((interp.map_or(info.entry, |i| i.entry), stack))
}

/// Variables the shell's `export` added to the environment of the Linux
/// programs justOS starts ("NAME=value"; they replace defaults of the same
/// name).
static EXTRA_ENV: crate::sync::IrqLock<Vec<String>> = crate::sync::IrqLock::new(Vec::new());

/// Sets a variable for the Linux programs started from now on; an empty
/// value takes it away again.
pub fn set_env(name: &str, value: &str) {
    let mut env = EXTRA_ENV.lock();
    env.retain(|v| v.split_once('=').map(|(n, _)| n) != Some(name));
    if !value.is_empty() {
        env.push(alloc::format!("{name}={value}"));
    }
}

pub fn extra_env() -> Vec<String> {
    EXTRA_ENV.lock().clone()
}

/// The environment of a Linux program justOS starts (one started by
/// another Linux program gets what that one passes to execve).
fn default_env() -> Vec<String> {
    let (cols, rows) = crate::console::size();
    let extra = extra_env();
    let set = |v: &String| extra.iter().any(|e| e.split_once('=').map(|(n, _)| n) == v.split_once('=').map(|(n, _)| n));
    ["PATH=/apps:/usr/local/bin:/usr/bin:/bin", "HOME=/home", "USER=root", "LANG=C.UTF-8", "TERM=xterm", "SHELL=/bin/sh", "TMPDIR=/home/tmp"]
        .iter()
        .map(|s| s.to_string())
        .chain([alloc::format!("COLUMNS={cols}"), alloc::format!("LINES={rows}")])
        .chain([alloc::format!("XDG_RUNTIME_DIR={}", wayland::RUNTIME_DIR), alloc::format!("WAYLAND_DISPLAY={}", wayland::DISPLAY)])
        // GTK and GLib: settings kept in memory (there is no dconf service)
        // and no accessibility bus to look for.
        .chain(["GSETTINGS_BACKEND=memory", "NO_AT_BRIDGE=1"].iter().map(|s| s.to_string()))
        .filter(|v| !set(v))
        .chain(extra.iter().cloned())
        .collect()
}

/// The dynamic loader an ELF asks for (its PT_INTERP), if any.
fn interpreter(elf: &[u8]) -> Option<String> {
    const PT_INTERP: u32 = 3;
    let phoff = u64_at(elf, 32)?;
    let phent = u16_at(elf, 54)? as u64;
    let phnum = u16_at(elf, 56)? as u64;
    (0..phnum).find_map(|i| {
        let p = (phoff + i * phent) as usize;
        if u32_at(elf, p)? != PT_INTERP {
            return None;
        }
        let off = u64_at(elf, p + 8)? as usize;
        let len = u64_at(elf, p + 32)? as usize;
        let raw = elf.get(off..off.checked_add(len)?)?;
        let raw = raw.split(|&b| b == 0).next()?;
        core::str::from_utf8(raw).ok().map(String::from)
    })
}

/// Where a program built as a PIE goes (its lowest page).
const PIE_BASE: u64 = 0x10_0000;
/// Where the dynamic loader goes: above the mmap area, below the stack.
const INTERP_BASE: u64 = 0x7000_0000_0000;

/// Maps the dynamic loader `path` (a Linux path on the program's volume).
fn load_interpreter(name: &str, path: &str) -> Result<Loaded, JustError> {
    let Some(bytes) = linux_path(path).ok().and_then(|p| crate::fs::read_file(&p).ok()) else {
        crate::println!("\x1b[91m{name}: needs {path}, which is not on this volume (are the Linux libraries installed?)\x1b[0m");
        return Err(JustError::BadProgram);
    };
    if u16_at(&bytes, 16) != Some(3) || interpreter(&bytes).is_some() {
        crate::println!("\x1b[91m{name}: {path} is not a dynamic loader\x1b[0m");
        return Err(JustError::BadProgram);
    }
    map_segments(&bytes, INTERP_BASE, path).map_err(|_| JustError::BadProgram)
}

struct Loaded {
    entry: u64,
    /// How far the ELF was moved from its own addresses (0 for ET_EXEC).
    base: u64,
    phdr: u64,
    phent: u64,
    phnum: u64,
    /// The first page after the program: where the break starts.
    brk: u64,
}

const PT_LOAD: u32 = 1;
const PT_PHDR: u32 = 6;

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// Maps the loadable segments of an ELF (ET_EXEC at its own addresses, a
/// PIE or shared object moved so its lowest page is at `pie_base`) and
/// returns where the headers landed. `name` is the file, as Linux shows
/// it in /proc/self/maps.
fn map_segments(elf: &[u8], pie_base: u64, name: &str) -> Result<Loaded, ()> {
    if elf.len() < 64 || &elf[..4] != b"\x7fELF" || elf[4] != 2 || elf[5] != 1 {
        return Err(());
    }
    let etype = u16_at(elf, 16).ok_or(())?;
    if etype != 2 && etype != 3 {
        return Err(()); // only ET_EXEC and ET_DYN (PIE)
    }
    if u16_at(elf, 18) != Some(0x3E) {
        return Err(()); // not x86-64
    }
    // A PIE is moved so its lowest page sits at pie_base; an ET_EXEC keeps
    // its own addresses (bias 0).
    let entry = u64_at(elf, 24).ok_or(())?;
    let phoff = u64_at(elf, 32).ok_or(())?;
    let phentsize = u16_at(elf, 54).ok_or(())? as u64;
    let phnum = u16_at(elf, 56).ok_or(())? as u64;
    if phentsize < 56 || phnum == 0 || phnum > 64 {
        return Err(());
    }

    let mut min_vaddr = u64::MAX;
    for i in 0..phnum {
        let p = (phoff + i * phentsize) as usize;
        if u32_at(elf, p).ok_or(())? == PT_LOAD {
            min_vaddr = min_vaddr.min(u64_at(elf, p + 16).ok_or(())?);
        }
    }
    let bias = if etype == 3 { pie_base.wrapping_sub(min_vaddr & !(PAGE_SIZE - 1)) } else { 0 };

    let mut phdr = 0;
    let mut top = 0u64;
    for i in 0..phnum {
        let p = (phoff + i * phentsize) as usize;
        let kind = u32_at(elf, p).ok_or(())?;
        let flags = u32_at(elf, p + 4).ok_or(())?;
        let offset = u64_at(elf, p + 8).ok_or(())?;
        let vaddr = u64_at(elf, p + 16).ok_or(())?.wrapping_add(bias);
        let filesz = u64_at(elf, p + 32).ok_or(())?;
        let memsz = u64_at(elf, p + 40).ok_or(())?;
        if kind == PT_PHDR {
            phdr = vaddr;
        }
        if kind != PT_LOAD || memsz == 0 {
            continue;
        }
        if filesz > memsz || offset.checked_add(filesz).is_none_or(|e| e > elf.len() as u64) {
            return Err(());
        }
        // Map whole pages covering [vaddr, vaddr + memsz), then copy the file
        // part in. Fresh pages are zero, so .bss needs nothing.
        let start = vaddr & !(PAGE_SIZE - 1);
        let end = mem::align_up(vaddr + memsz, PAGE_SIZE);
        top = top.max(end);
        map_maybe(start, end - start).map_err(|_| ())?;
        // PF_R 4, PF_W 2, PF_X 1 as PROT_READ 1, PROT_WRITE 2, PROT_EXEC 4.
        let prot = (flags >> 2 & 1) | (flags & 2) | (flags & 1) << 2;
        let image = mm::Image { start, end, prot, off: offset & !(PAGE_SIZE - 1), name: name.into() };
        with_state(|st| st.images.push(image));
        // SAFETY: the range was just mapped writable in this address space.
        unsafe {
            core::ptr::copy_nonoverlapping(elf.as_ptr().add(offset as usize), vaddr as *mut u8, filesz as usize);
        }
    }
    // If there was no PT_PHDR, the headers are still in the first segment at
    // file offset phoff, which the ABI places at min_vaddr + phoff for a
    // standard layout.
    if phdr == 0 {
        phdr = min_vaddr.wrapping_add(bias).wrapping_add(phoff);
    }
    // The break starts a little after the program (a gap catches strays).
    let brk = if top != 0 { mem::align_up(top, PAGE_SIZE) + 16 * PAGE_SIZE } else { BRK_BASE };
    Ok(Loaded { entry: entry.wrapping_add(bias), base: bias, phdr, phent: phentsize, phnum, brk })
}

/// Maps pages for a range, skipping any page already present (segments can
/// share a page at their boundary).
fn map_maybe(addr: u64, len: u64) -> Result<(), JustError> {
    let mut page = addr;
    let end = addr + len;
    while page < end {
        if crate::mem::paging::translate_in(crate::arch::cpu::read_cr3(), page).is_none() {
            user::map_fixed(page, PAGE_SIZE)?;
        }
        page += PAGE_SIZE;
    }
    Ok(())
}

fn justrt_split(args: &str) -> Vec<String> {
    // The same splitting justrt::split_args does, kept here to avoid a cycle.
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote = None;
    let mut any = false;
    for c in args.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                any = true;
            }
            None if c.is_whitespace() => {
                if any {
                    out.push(core::mem::take(&mut cur));
                    any = false;
                }
            }
            None => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_FLAGS: u64 = 8;
const AT_ENTRY: u64 = 9;
const AT_RANDOM: u64 = 25;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_PLATFORM: u64 = 15;
const AT_HWCAP: u64 = 16;
const AT_CLKTCK: u64 = 17;
const AT_SECURE: u64 = 23;
const AT_EXECFN: u64 = 31;

/// Builds the initial stack: argc, argv, envp, the auxiliary vector and the
/// strings they point at. Returns the stack pointer to start the program on.
/// `interp_base` is where the dynamic loader went (0: none).
fn build_stack(info: &Loaded, interp_base: u64, argv: &[String], envp: &[String], exe: &str) -> Result<u64, JustError> {
    const PAGES: u64 = 64;
    let top = justdk::syscall::USER_STACK_TOP;
    let bottom = top - PAGES * PAGE_SIZE;
    user::map_fixed(bottom, PAGES * PAGE_SIZE)?;
    // The rest of an 8 MiB stack comes page by page as it is used.
    with_state(|st| {
        st.vmas.insert(top - mm::STACK_RESERVE, mm::Vma::anon(bottom, mm::PROT_READ | mm::PROT_WRITE));
        let rw = mm::PROT_READ | mm::PROT_WRITE;
        st.images.push(mm::Image { start: top - mm::STACK_RESERVE, end: top, prot: rw, off: 0, name: "[stack]".into() });
    });

    // String area at the top; collect the addresses as we go.
    let mut p = top;
    let mut push_str = |s: &str| {
        p -= s.len() as u64 + 1;
        // SAFETY: inside the mapped stack; we stay well below its size.
        unsafe {
            core::ptr::copy_nonoverlapping(s.as_ptr(), p as *mut u8, s.len());
            *(p as *mut u8).add(s.len()) = 0;
        }
        p
    };
    let arg_ptrs: Vec<u64> = argv.iter().map(|a| push_str(a)).collect();
    let env_ptrs: Vec<u64> = envp.iter().map(|e| push_str(e)).collect();
    let platform = push_str("x86_64");
    let execfn = push_str(exe);
    // 16 random bytes for AT_RANDOM (stack protector and pointer guard).
    let mut seed = [0u8; 16];
    crate::rand::fill(&mut seed);
    p -= 16;
    p &= !15;
    let rand_at = p;
    // SAFETY: inside the mapped stack.
    unsafe { core::ptr::copy_nonoverlapping(seed.as_ptr(), p as *mut u8, 16) };

    // The word array: argc, argv[...], 0, envp[...], 0, auxv..., must end
    // 16-byte aligned so that after `argc` the stack meets the ABI.
    let mut words: Vec<u64> = Vec::new();
    words.push(arg_ptrs.len() as u64);
    words.extend_from_slice(&arg_ptrs);
    words.push(0);
    words.extend_from_slice(&env_ptrs);
    words.push(0);
    for (k, v) in [
        (AT_PHDR, info.phdr),
        (AT_PHENT, info.phent),
        (AT_PHNUM, info.phnum),
        (AT_PAGESZ, PAGE_SIZE),
        (AT_BASE, interp_base),
        (AT_FLAGS, 0),
        (AT_ENTRY, info.entry),
        (AT_RANDOM, rand_at),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_SECURE, 0),
        (AT_CLKTCK, 100),
        (AT_HWCAP, crate::arch::cpu::cpuid(1, 0).edx as u64),
        (AT_PLATFORM, platform),
        (AT_EXECFN, execfn),
        (AT_NULL, 0),
    ] {
        words.push(k);
        words.push(v);
    }
    let mut sp = p & !15;
    if (sp - words.len() as u64 * 8) & 15 != 0 {
        sp -= 8;
    }
    sp -= words.len() as u64 * 8;
    // SAFETY: inside the mapped stack, which is much larger than this.
    unsafe { core::ptr::copy_nonoverlapping(words.as_ptr(), sp as *mut u64, words.len()) };
    Ok(sp)
}

// ----------------------------------------------------------------------------
// System call dispatch

/// Every Linux system call goes to the journal (the shell's `strace on`):
/// the ones that may wait when they start, all of them with their result.
pub static TRACE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Calls that may wait: traced when they start too, to see where a program
/// stopped.
fn may_wait(n: u64) -> bool {
    matches!(n, 0 | 7 | 19 | 23 | 34 | 35 | 43 | 45 | 47 | 61 | 130 | 202 | 230 | 232 | 247 | 270 | 271 | 281 | 288 | 299 | 441)
}

/// The file name a call is about, for the trace (` "/usr/lib/…"`).
fn traced_path(n: u64, a: &[u64; 6]) -> String {
    let ptr = match n {
        2 | 4 | 6 | 21 | 59 | 80 | 83 | 87 | 89 => a[0],
        257 | 262 | 263 | 267 | 269 | 332 | 439 => a[1],
        _ => return String::new(),
    };
    user::user_cstr(ptr, 200).map(|s| alloc::format!(" \"{s}\"")).unwrap_or_default()
}

pub fn syscall(frame: &mut TrapFrame) {
    crate::arch::cpu::enable_irq();
    let n = frame.rax;
    if n == 15 {
        // rt_sigreturn: the registers come back from the signal frame.
        signal::sys_rt_sigreturn(frame);
        signal::deliver(frame, None);
        crate::arch::cpu::disable_irq();
        return;
    }
    let a = [frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9];
    let trace = TRACE.load(core::sync::atomic::Ordering::Relaxed);
    let named = if trace { traced_path(n, &a) } else { String::new() };
    if trace && may_wait(n) {
        crate::debug!("strace", "{}[{}] {n}({:#x}, {:#x}, {:#x}, {:#x}){named} ...", user::current_name(), task::current_tid(), a[0], a[1], a[2], a[3]);
    }
    // clone, fork and vfork need the caller's registers for the child.
    let ret = match n {
        56 => thread::sys_clone(frame, a[0], a[1], a[2], a[3], a[4]),
        57 => thread::sys_fork(frame),
        58 => thread::sys_vfork(frame),
        _ => dispatch(n, a),
    };
    // clone3 is left to fail on purpose: callers fall back to clone.
    if ret == -err::ENOSYS && n != 435 {
        crate::debug!("linux", "{}: unimplemented syscall {n}", user::current_name());
    }
    if trace {
        crate::debug!("strace", "{}[{}] {n}({:#x}, {:#x}, {:#x}, {:#x}){named} = {ret:#x}", user::current_name(), task::current_tid(), a[0], a[1], a[2], a[3]);
    }
    frame.rax = ret as u64;
    if user::kill_requested() {
        user::terminate(-130);
    }
    signal::deliver(frame, Some(n));
    crate::arch::cpu::disable_irq();
}

fn dispatch(n: u64, a: [u64; 6]) -> i64 {
    let fd0 = a[0] as i32;
    match n {
        0 => fd::sys_read(fd0, a[1], a[2]),
        1 => fd::sys_write(fd0, a[1], a[2]),
        2 => fd::sys_openat(fd::AT_FDCWD, a[0], a[1] as u32),
        3 => fd::sys_close(fd0),
        4 => fd::sys_newfstatat(fd::AT_FDCWD, a[0], a[1], 0),         // stat
        6 => fd::sys_newfstatat(fd::AT_FDCWD, a[0], a[1], fd::AT_SYMLINK_NOFOLLOW), // lstat
        5 => fd::sys_fstat(fd0, a[1]),
        7 => poll::sys_poll(a[0], a[1], a[2] as i32 as i64),
        8 => fd::sys_lseek(fd0, a[1] as i64, a[2] as i32),
        9 => mm::sys_mmap(a[0], a[1], a[2] as u32, a[3] as u32, a[4] as i32, a[5]),
        10 => mm::sys_mprotect(a[0], a[1], a[2] as u32),
        11 => mm::sys_munmap(a[0], a[1]),
        12 => sys_brk(a[0]),
        13 => signal::sys_rt_sigaction(a[0], a[1], a[2], a[3]),
        14 => signal::sys_rt_sigprocmask(a[0], a[1], a[2]),
        34 => signal::sys_pause(),
        16 => fd::sys_ioctl(fd0, a[1], a[2]),
        17 => fd::sys_pread(fd0, a[1], a[2], a[3]),
        18 => fd::sys_pwrite(fd0, a[1], a[2], a[3]),
        19 => sys_readv(fd0, a[1], a[2]),
        20 => sys_writev(fd0, a[1], a[2]),
        21 => fd::sys_faccessat(fd::AT_FDCWD, a[0]),
        22 => fd::sys_pipe2(a[0], 0),
        23 => poll::sys_select_tv(a[0], a[1], a[2], a[3], a[4]),
        24 => {
            task::yield_now();
            0
        }
        25 => mm::sys_mremap(a[0], a[1], a[2], a[3] as u32),
        26 | 27 => 0, // msync, mincore
        28 => mm::sys_madvise(a[0], a[1], a[2]),
        285 => shm::sys_fallocate(fd0, a[1], a[2], a[3]),
        319 => shm::sys_memfd_create(a[0], a[1] as u32),
        32 => fd::sys_dup(fd0),
        33 => fd::sys_dup3(fd0, a[1] as i32, u32::MAX),
        35 => sys_nanosleep(a[0]),
        39 => task::current_pid() as i64,
        41 => sock::sys_socket(a[0], a[1], a[2]),
        42 => sock::sys_connect(fd0, a[1], a[2]),
        43 => sock::sys_accept4(fd0, a[1], a[2], 0),
        44 => sock::sys_sendto(fd0, a[1], a[2], a[3], a[4], a[5]),
        45 => sock::sys_recvfrom(fd0, a[1], a[2], a[3], a[4], a[5]),
        46 => sock::sys_sendmsg(fd0, a[1], a[2]),
        47 => sock::sys_recvmsg(fd0, a[1], a[2]),
        48 => sock::sys_shutdown(fd0, a[1]),
        49 => sock::sys_bind(fd0, a[1], a[2]),
        50 => sock::sys_listen(fd0, a[1]),
        51 => sock::sys_getsockname(fd0, a[1], a[2]),
        52 => sock::sys_getpeername(fd0, a[1], a[2]),
        53 => sock::sys_socketpair(a[0], a[1], a[3]),
        54 => 0, // setsockopt
        55 => sock::sys_getsockopt(fd0, a[1], a[2], a[3], a[4]),
        59 => sys_execve(a[0], a[1], a[2]),
        60 => thread::sys_exit(a[0] as i64),
        61 => sys_wait4(a[0] as i64, a[1], a[2]),
        62 => signal::sys_kill(a[0] as i64, a[1]),
        63 => sys_uname(a[0]),
        72 => fd::sys_fcntl(fd0, a[1], a[2]),
        73 => 0, // flock
        74 | 75 | 306 => fd::sys_fsync(fd0), // fsync, fdatasync, syncfs
        162 => {
            crate::fs::sync_all();
            0
        }
        76 => match linux_arg_path(a[0]) {
            Ok(p) => fd::truncate_path(&p, a[1]),
            Err(e) => e,
        },
        77 => fd::sys_ftruncate(fd0, a[1]),
        78 | 217 => fd::sys_getdents64(fd0, a[1], a[2]),
        79 => sys_getcwd(a[0], a[1]),
        80 => sys_chdir(a[0]),
        81 => sys_fchdir(fd0),
        82 => fd::sys_renameat(fd::AT_FDCWD, a[0], fd::AT_FDCWD, a[1]),
        83 => fd::sys_mkdirat(fd::AT_FDCWD, a[0]),
        84 | 87 => fd::sys_unlinkat(fd::AT_FDCWD, a[0]),
        86 => -err::EPERM, // link: no hard links
        88 => fd::sys_symlinkat(a[0], fd::AT_FDCWD, a[1]),
        89 => fd::sys_readlinkat(fd::AT_FDCWD, a[0], a[1], a[2]),
        90..=94 => 0, // chmod, fchmod, chown, fchown, lchown
        95 => 0o022,  // umask
        96 => sys_gettimeofday(a[0]),
        97 => sys_getrlimit(a[0], a[1]),
        98 => zero_out(a[1], 144), // getrusage
        99 => sys_sysinfo(a[0]),
        100 => zero_out(a[0], 32), // times
        102 | 104 | 107 | 108 => 0, // getuid, getgid, geteuid, getegid: root
        105 | 106 | 109 | 112 | 113 | 114 | 116 | 117 | 119 => 0, // set*id, setpgid, setsid...
        110 => user::parent_pid() as i64,
        111 | 121 | 124 => task::current_pid() as i64,             // getpgrp, getpgid, getsid
        115 => 0,                                                 // getgroups: none
        118 | 120 => sys_getresid(a[0], a[1], a[2]),
        127 => signal::sys_rt_sigpending(a[0]),
        128 => signal::sys_rt_sigtimedwait(a[0], a[1], a[2]),
        130 => signal::sys_rt_sigsuspend(a[0]),
        131 => signal::sys_sigaltstack(a[0], a[1]),
        137 | 138 => sys_statfs(a[1]),
        140 => 20, // getpriority: 20 - nice, nice 0
        141 => 0,  // setpriority
        142 | 144 => 0, // sched_setparam, sched_setscheduler: accepted, changes nothing
        143 => zero_out(a[1], 4), // sched_getparam: priority 0
        145 => 0, // sched_getscheduler: SCHED_OTHER
        146 => if matches!(a[0], 1 | 2) { 99 } else { 0 }, // sched_get_priority_max: FIFO, RR
        147 => if matches!(a[0], 1 | 2) { 1 } else { 0 },  // sched_get_priority_min
        157 => 0, // prctl
        158 => sys_arch_prctl(a[0], a[1]),
        160 => 0, // setrlimit
        186 => thread::sys_gettid(),
        187 => 0, // readahead: the cache reads what is asked
        200 => signal::sys_tgkill(0, a[0] as i64, a[1]),
        201 => sys_time(a[0]),
        202 => thread::sys_futex(a[0], a[1], a[2], a[3], a[4], a[5]),
        203 => 0, // sched_setaffinity
        204 => sys_sched_getaffinity(a[1], a[2]),
        213 => poll::sys_epoll_create1(0),
        221 => 0, // fadvise64: only advice
        218 => thread::sys_set_tid_address(a[0]),
        228 => sys_clock_gettime(a[0], a[1]),
        229 => sys_clock_getres(a[1]),
        230 => sys_clock_nanosleep(a[0], a[1], a[2]),
        231 => user::terminate(a[0] as i64),
        247 => sys_waitid(a[0], a[1], a[2], a[3]),
        232 => poll::sys_epoll_wait(fd0, a[1], a[2] as i32, a[3] as i32 as i64),
        233 => poll::sys_epoll_ctl(fd0, a[1] as u32, a[2] as i32, a[3]),
        234 => signal::sys_tgkill(a[0] as i64, a[1] as i64, a[2]),
        257 => fd::sys_openat(fd0, a[1], a[2] as u32),
        258 => fd::sys_mkdirat(fd0, a[1]),
        262 => fd::sys_newfstatat(fd0, a[1], a[2], a[3]),
        263 => fd::sys_unlinkat(fd0, a[1]),
        264 | 316 => fd::sys_renameat(fd0, a[1], a[2] as i32, a[3]),
        265 => -err::EPERM, // linkat
        266 => fd::sys_symlinkat(a[0], a[1] as i32, a[2]),
        267 => fd::sys_readlinkat(fd0, a[1], a[2], a[3]),
        268 | 260 | 280 => 0,     // fchmodat, fchownat, utimensat
        269 | 439 => fd::sys_faccessat(fd0, a[1]),
        270 => poll::sys_pselect6(a[0], a[1], a[2], a[3], a[4]),
        271 => poll::sys_ppoll(a[0], a[1], a[2]),
        273 | 334 => 0, // set_robust_list, rseq... (glibc asks rseq; 0 = "fine")
        281 => poll::sys_epoll_wait(fd0, a[1], a[2] as i32, a[3] as i32 as i64),
        284 => fd::sys_eventfd2(a[0] as u32, 0),
        288 => sock::sys_accept4(fd0, a[1], a[2], a[3]),
        290 => fd::sys_eventfd2(a[0] as u32, a[1] as u32),
        291 => poll::sys_epoll_create1(a[0] as u32),
        292 => fd::sys_dup3(fd0, a[1] as i32, a[2] as u32),
        293 => fd::sys_pipe2(a[0], a[1] as u32),
        302 => sys_prlimit(a[1], a[3]),
        309 => sys_getcpu(a[0], a[1]),
        318 => sys_getrandom(a[0], a[1]),
        322 if a[0] as i32 == fd::AT_FDCWD || a[4] == 0 => sys_execve(a[1], a[2], a[3]), // execveat
        324 => 0, // membarrier
        332 => fd::sys_statx(fd0, a[1], a[2], a[4]),
        435 => -err::ENOSYS, // clone3: callers fall back to clone
        436 => fd::sys_close_range(a[0] as u32, a[1] as u32, a[2] as u32),
        441 => poll::sys_epoll_pwait2(fd0, a[1], a[2] as i32, a[3]),
        _ => -err::ENOSYS,
    }
}

fn linux_arg_path(ptr: u64) -> Result<Path, i64> {
    let text = user::user_cstr(ptr, 4096).map_err(errno)?;
    linux_path(&text)
}

fn zero_out(ptr: u64, len: u64) -> i64 {
    if ptr == 0 {
        return 0;
    }
    match user_bytes(ptr, len) {
        Ok(b) => {
            b.fill(0);
            0
        }
        Err(e) => e,
    }
}

// ----------------------------------------------------------------------------
// New programs and their end

/// A NULL-ended array of C strings in user memory (argv, envp).
fn user_strings(ptr: u64) -> Result<Vec<String>, i64> {
    let mut out = Vec::new();
    if ptr == 0 {
        return Ok(out);
    }
    for i in 0..65_536u64 {
        let b = user_bytes(ptr + i * 8, 8)?;
        let p = u64::from_le_bytes(b[..8].try_into().unwrap());
        if p == 0 {
            return Ok(out);
        }
        out.push(user::user_cstr(p, 128 * 1024).map_err(|e| -errno(e))?);
    }
    Err(-err::E2BIG)
}

/// execve: the process becomes another program. A script starting with
/// `#!interpreter [arg]` runs the interpreter with the script's path. Past
/// the checks there is no way back: if loading fails, the process ends.
fn sys_execve(path_ptr: u64, argv_ptr: u64, envp_ptr: u64) -> i64 {
    let text = match user::user_cstr(path_ptr, 4096) {
        Ok(t) => t,
        Err(e) => return -errno(e),
    };
    let (mut argv, envp) = match (user_strings(argv_ptr), user_strings(envp_ptr)) {
        (Ok(a), Ok(e)) => (a, e),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let read = |linux: &str| -> Result<(Path, Vec<u8>), i64> {
        let path = linux_path(linux)?;
        if crate::fs::is_dir(&path) {
            return Err(-err::EACCES);
        }
        let data = crate::fs::read_file(&path).map_err(|e| -errno(user::fs_error(e)))?;
        Ok((path, data))
    };
    let (mut path, mut image) = match read(&text) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if image.starts_with(b"#!") {
        let end = image.iter().position(|&b| b == b'\n').unwrap_or(image.len()).min(256);
        let line = String::from_utf8_lossy(&image[2..end]).trim().to_string();
        let (interp, arg) = match line.split_once([' ', '\t']) {
            Some((i, a)) => (i.to_string(), a.trim().to_string()),
            None => (line.clone(), String::new()),
        };
        if interp.is_empty() {
            return -err::ENOEXEC;
        }
        let mut with = alloc::vec![interp.clone()];
        if !arg.is_empty() {
            with.push(arg);
        }
        with.push(text.clone());
        with.extend(argv.into_iter().skip(1));
        argv = with;
        (path, image) = match read(&interp) {
            Ok(v) => v,
            Err(e) => return e,
        };
    }
    if image.len() < 64 || &image[..4] != b"\x7fELF" {
        return -err::ENOEXEC;
    }
    if let Some(ld) = interpreter(&image) {
        if !linux_path(&ld).is_ok_and(|p| crate::fs::exists(&p)) {
            return -err::ENOENT;
        }
    }
    if !user::only_thread() {
        return -err::EAGAIN; // exec from a program with several threads: not yet
    }
    let name = text.rsplit('/').next().unwrap_or(&text).to_string();
    if argv.is_empty() {
        argv.push(name.clone());
    }
    if user::exec_address_space(&name, &path).is_err() {
        return -err::ENOMEM;
    }
    reset_for_exec();
    let loaded = load_program(&image, &name, &argv, &envp, &show_path(&path));
    // Nothing on this stack is dropped once the new program starts.
    drop((image, argv, envp, path, text, name));
    match loaded {
        Ok((entry, sp)) => user::start_loaded(entry, sp),
        Err(_) => user::terminate(-(128 + 9)),
    }
}

/// What execve keeps of the Linux state: descriptors without close-on-exec,
/// the signal mask and ignored signals. The rest starts afresh.
fn reset_for_exec() {
    let me = task::current_tid();
    let closed: Vec<(Arc<fd::Desc>, bool)> = with_state(|st| {
        let gone: Vec<i32> = st.fds.iter().filter(|(_, (_, cloexec))| *cloexec).map(|(&fd, _)| fd).collect();
        let closed = gone.iter().filter_map(|fd| st.fds.remove(fd)).collect();
        st.vmas = mm::new_map();
        st.images.clear();
        st.mmap_next = mm::MMAP_BASE;
        st.clear_tid.clear();
        for act in st.sigactions.iter_mut() {
            // A handler's code is gone; SIG_DFL (0) and SIG_IGN (1) stay.
            if u64::from_le_bytes(act[..8].try_into().unwrap()) > 1 {
                *act = [0; 32];
            }
        }
        signal::exec(st, me);
        closed
    });
    // Closed outside the state lock: closing a socket talks to the network.
    drop(closed);
}

/// The wait status of a child's exit code: a normal exit (code << 8), or
/// the signal that ended it (codes -(128 + signal)).
fn wait_status(code: i64) -> u32 {
    if code < -128 {
        ((-code - 128) & 0x7f) as u32
    } else {
        ((code as u32) & 0xff) << 8
    }
}

fn sys_wait4(pid: i64, status: u64, options: u64) -> i64 {
    const WNOHANG: u64 = 1;
    let want = (pid > 0).then_some(pid as u32);
    match user::wait_child(want, options & WNOHANG != 0, signal::pending_here) {
        Ok(Some((child, code))) => {
            if status != 0 {
                if let Ok(b) = user_bytes(status, 4) {
                    b.copy_from_slice(&wait_status(code).to_le_bytes());
                }
            }
            child as i64
        }
        Ok(None) => 0,
        Err(JustError::Cancelled) => signal::interrupted().unwrap_or(-err::EINTR),
        Err(_) => -err::ECHILD,
    }
}

/// waitid with P_ALL or P_PID and WEXITED, filling a siginfo_t.
fn sys_waitid(idtype: u64, id: u64, info: u64, options: u64) -> i64 {
    const WNOHANG: u64 = 1;
    const P_PID: u64 = 1;
    let want = (idtype == P_PID).then_some(id as u32);
    let got = match user::wait_child(want, options & WNOHANG != 0, signal::pending_here) {
        Ok(g) => g,
        Err(JustError::Cancelled) => return signal::interrupted().unwrap_or(-err::EINTR),
        Err(_) => return -err::ECHILD,
    };
    if info != 0 {
        let Ok(b) = user_bytes(info, 128) else { return -err::EFAULT };
        b.fill(0);
        if let Some((child, code)) = got {
            let killed = code < -128;
            b[0..4].copy_from_slice(&17u32.to_le_bytes()); // si_signo: SIGCHLD
            b[8..12].copy_from_slice(&(if killed { 2u32 } else { 1u32 }).to_le_bytes()); // CLD_KILLED / CLD_EXITED
            b[16..20].copy_from_slice(&child.to_le_bytes());
            let status = if killed { (-code - 128) as u32 } else { (code as u32) & 0xff };
            b[24..28].copy_from_slice(&status.to_le_bytes());
        }
    }
    0
}

// ----------------------------------------------------------------------------
// Memory

fn sys_brk(req: u64) -> i64 {
    with_state(|st| {
        if req == 0 || req < st.brk_start {
            return st.brk_cur as i64;
        }
        let new = mem::align_up(req, PAGE_SIZE);
        let old = mem::align_up(st.brk_cur, PAGE_SIZE);
        if new > old {
            if user::map_fixed(old, new - old).is_err() {
                return st.brk_cur as i64;
            }
        } else if new < old {
            let _ = user::unmap_range(new, old - new);
        }
        st.brk_cur = req;
        req as i64
    })
}

// ----------------------------------------------------------------------------
// Thread-local storage

const ARCH_SET_FS: u64 = 0x1002;
const ARCH_SET_GS: u64 = 0x1001;
const ARCH_GET_FS: u64 = 0x1003;

fn sys_arch_prctl(code: u64, addr: u64) -> i64 {
    let which = match code {
        ARCH_SET_FS => justdk::syscall::TLS_FS,
        ARCH_SET_GS => justdk::syscall::TLS_GS,
        ARCH_GET_FS => {
            let fs = crate::arch::syscall::user_bases().0;
            return match user_bytes(addr, 8) {
                Ok(b) => {
                    b.copy_from_slice(&fs.to_le_bytes());
                    0
                }
                Err(e) => e,
            };
        }
        _ => return -err::EINVAL,
    };
    match user::set_tls(which, addr) {
        Ok(()) => 0,
        Err(e) => -errno(e),
    }
}

// ----------------------------------------------------------------------------
// Vectored I/O

fn sys_readv(fd: i32, iov: u64, count: u64) -> i64 {
    iov_each(iov, count, |ptr, len| fd::sys_read(fd, ptr, len))
}

fn sys_writev(fd: i32, iov: u64, count: u64) -> i64 {
    iov_each(iov, count, |ptr, len| fd::sys_write(fd, ptr, len))
}

/// Runs `f` on each `struct iovec { base, len }`, summing the bytes until one
/// transfer falls short or fails.
fn iov_each(iov: u64, count: u64, mut f: impl FnMut(u64, u64) -> i64) -> i64 {
    let bytes = match user_bytes(iov, count.saturating_mul(16)) {
        Ok(b) => b.to_vec(),
        Err(e) => return e,
    };
    let mut total = 0i64;
    for chunk in bytes.chunks(16) {
        let base = u64::from_le_bytes(chunk[..8].try_into().unwrap());
        let len = u64::from_le_bytes(chunk[8..].try_into().unwrap());
        if len == 0 {
            continue;
        }
        let r = f(base, len);
        if r < 0 {
            return if total > 0 { total } else { r };
        }
        total += r;
        if (r as u64) < len {
            break;
        }
    }
    total
}

// ----------------------------------------------------------------------------
// cwd

fn sys_getcwd(ptr: u64, cap: u64) -> i64 {
    let cwd = match user::cwd() {
        Ok(c) => c,
        Err(e) => return -errno(e),
    };
    let text = show_path(&cwd);
    let bytes = text.as_bytes();
    if bytes.len() + 1 > cap as usize {
        return -err::ERANGE;
    }
    let buf = match user_bytes(ptr, bytes.len() as u64 + 1) {
        Ok(b) => b,
        Err(e) => return e,
    };
    buf[..bytes.len()].copy_from_slice(bytes);
    buf[bytes.len()] = 0;
    bytes.len() as i64 + 1
}

fn sys_chdir(path_ptr: u64) -> i64 {
    let path = match linux_arg_path(path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if !crate::fs::is_dir(&path) {
        return -err::ENOTDIR;
    }
    user::set_cwd(path);
    0
}

fn sys_fchdir(fd: i32) -> i64 {
    match fd::get(fd).map(|d| match &d.kind {
        fd::Kind::Dir { path, .. } => Some(path.clone()),
        _ => None,
    }) {
        Some(Some(p)) => {
            user::set_cwd(p);
            0
        }
        Some(None) => -err::ENOTDIR,
        None => -err::EBADF,
    }
}

// ----------------------------------------------------------------------------
// Time

/// Sleeps; false if a signal handler is to run first (the call fails
/// with EINTR).
fn sleep_interruptible(ms: u64) -> bool {
    // A signal wakes the thread (also one another thread takes): the time
    // left is measured again after every wake-up.
    let until = crate::time::uptime_ms().saturating_add(ms);
    loop {
        if signal::interrupted().is_some() {
            return false;
        }
        let now = crate::time::uptime_ms();
        if now >= until {
            return true;
        }
        task::sleep_ms((until - now).min(1000));
    }
}

fn read_timespec(ptr: u64) -> Result<(u64, u64), i64> {
    let b = user_bytes(ptr, 16)?;
    Ok((
        i64::from_le_bytes(b[0..8].try_into().unwrap()).max(0) as u64,
        i64::from_le_bytes(b[8..16].try_into().unwrap()).max(0) as u64,
    ))
}

fn sys_nanosleep(req: u64) -> i64 {
    match read_timespec(req) {
        Ok((s, ns)) => {
            if sleep_interruptible(s.saturating_mul(1000).saturating_add(ns.div_ceil(1_000_000)).min(86_400_000)) {
                0
            } else {
                -err::EINTR
            }
        }
        Err(e) => e,
    }
}

/// (seconds, nanoseconds) on one of the Linux clocks.
/// Seconds and nanoseconds on clock `id`: the wall clock (realtime and its
/// variants) is the start time plus the uptime, so both move together and
/// neither goes back.
fn clock_now(id: u64) -> (u64, u64) {
    let ns = crate::time::uptime_ns();
    let (s, sub) = (ns / 1_000_000_000, ns % 1_000_000_000);
    match id {
        0 | 5 | 8 | 11 => (crate::time::boot_unix() + s, sub), // realtime
        _ => (s, sub), // monotonic, boottime, cputime
    }
}

fn sys_clock_gettime(id: u64, tp: u64) -> i64 {
    let (s, ns) = clock_now(id);
    match user_bytes(tp, 16) {
        Ok(b) => {
            b[0..8].copy_from_slice(&s.to_le_bytes());
            b[8..16].copy_from_slice(&ns.to_le_bytes());
            0
        }
        Err(e) => e,
    }
}

fn sys_clock_getres(tp: u64) -> i64 {
    if tp == 0 {
        return 0;
    }
    match user_bytes(tp, 16) {
        Ok(b) => {
            // The tick refined by the time stamp counter (see uptime_ns).
            b[0..8].copy_from_slice(&0u64.to_le_bytes());
            b[8..16].copy_from_slice(&1000u64.to_le_bytes());
            0
        }
        Err(e) => e,
    }
}

fn sys_clock_nanosleep(id: u64, flags: u64, req: u64) -> i64 {
    const TIMER_ABSTIME: u64 = 1;
    let (s, ns) = match read_timespec(req) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let want = s.saturating_mul(1000).saturating_add(ns / 1_000_000);
    let ms = if flags & TIMER_ABSTIME != 0 {
        let (ns_, nns) = clock_now(id);
        want.saturating_sub(ns_ * 1000 + nns / 1_000_000)
    } else {
        want
    };
    if sleep_interruptible(ms.min(86_400_000)) {
        0
    } else {
        -err::EINTR
    }
}

fn sys_gettimeofday(tv: u64) -> i64 {
    if tv == 0 {
        return 0;
    }
    let (s, ns) = clock_now(0);
    match user_bytes(tv, 16) {
        Ok(b) => {
            b[0..8].copy_from_slice(&s.to_le_bytes());
            b[8..16].copy_from_slice(&(ns / 1000).to_le_bytes());
            0
        }
        Err(e) => e,
    }
}

fn sys_time(ptr: u64) -> i64 {
    let t = crate::time::unix_now();
    if ptr != 0 {
        if let Ok(b) = user_bytes(ptr, 8) {
            b.copy_from_slice(&t.to_le_bytes());
        }
    }
    t as i64
}

// ----------------------------------------------------------------------------
// The rest: random, uname, limits, CPU, file systems

fn sys_getrandom(ptr: u64, len: u64) -> i64 {
    let buf = match user_bytes(ptr, len.min(1 << 20)) {
        Ok(b) => b,
        Err(e) => return e,
    };
    crate::rand::fill(buf);
    buf.len() as i64
}

fn sys_uname(buf: u64) -> i64 {
    // struct utsname: six fixed 65-byte fields.
    let out = match user_bytes(buf, 65 * 6) {
        Ok(b) => b,
        Err(e) => return e,
    };
    out.fill(0);
    let fields = ["Linux", "justos", "6.1.0-justos", "#1 SMP justOS", "x86_64", "(none)"];
    for (i, f) in fields.iter().enumerate() {
        let at = i * 65;
        out[at..at + f.len()].copy_from_slice(f.as_bytes());
    }
    0
}

fn rlimit(resource: u64) -> (u64, u64) {
    match resource {
        3 => (8 * 1024 * 1024, u64::MAX), // RLIMIT_STACK
        7 => (4096, 4096),                // RLIMIT_NOFILE
        _ => (u64::MAX, u64::MAX),
    }
}

fn sys_getrlimit(resource: u64, ptr: u64) -> i64 {
    let (cur, max) = rlimit(resource);
    match user_bytes(ptr, 16) {
        Ok(b) => {
            b[0..8].copy_from_slice(&cur.to_le_bytes());
            b[8..16].copy_from_slice(&max.to_le_bytes());
            0
        }
        Err(e) => e,
    }
}

/// prlimit64(pid, resource, new, old): limits are fixed here, so a new one
/// is accepted and changes nothing.
fn sys_prlimit(resource: u64, old: u64) -> i64 {
    if old != 0 {
        return sys_getrlimit(resource, old);
    }
    0
}

fn sys_getresid(a: u64, b: u64, c: u64) -> i64 {
    for p in [a, b, c] {
        if p != 0 {
            if let Ok(x) = user_bytes(p, 4) {
                x.copy_from_slice(&0u32.to_le_bytes());
            }
        }
    }
    0
}

fn sys_sched_getaffinity(len: u64, mask: u64) -> i64 {
    if len < 8 {
        return -err::EINVAL;
    }
    match user_bytes(mask, 8) {
        Ok(b) => {
            b.copy_from_slice(&1u64.to_le_bytes()); // one CPU
            8
        }
        Err(e) => e,
    }
}

fn sys_getcpu(cpu: u64, node: u64) -> i64 {
    for p in [cpu, node] {
        if p != 0 {
            if let Ok(b) = user_bytes(p, 4) {
                b.copy_from_slice(&0u32.to_le_bytes());
            }
        }
    }
    0
}

fn sys_sysinfo(ptr: u64) -> i64 {
    let out = match user_bytes(ptr, 112) {
        Ok(b) => b,
        Err(e) => return e,
    };
    out.fill(0);
    let (free, total) = crate::mem::frame::stats();
    out[0..8].copy_from_slice(&(crate::time::uptime_ms() / 1000).to_le_bytes());
    out[32..40].copy_from_slice(&total.to_le_bytes());
    out[40..48].copy_from_slice(&free.to_le_bytes());
    out[80..82].copy_from_slice(&(user::count() as u16).to_le_bytes());
    out[104..108].copy_from_slice(&1u32.to_le_bytes()); // mem_unit
    0
}

fn sys_statfs(ptr: u64) -> i64 {
    let out = match user_bytes(ptr, 120) {
        Ok(b) => b,
        Err(e) => return e,
    };
    out.fill(0);
    out[0..8].copy_from_slice(&0xA2C0u64.to_le_bytes()); // f_type
    out[8..16].copy_from_slice(&4096u64.to_le_bytes()); // f_bsize
    for off in [16, 24, 32] {
        out[off..off + 8].copy_from_slice(&(1u64 << 20).to_le_bytes()); // blocks, free, avail
    }
    out[64..72].copy_from_slice(&255u64.to_le_bytes()); // f_namelen
    0
}
