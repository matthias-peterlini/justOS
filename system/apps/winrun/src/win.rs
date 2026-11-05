//! The Windows API that programs reach through their import table.
//!
//! Every function here follows the Windows x64 calling convention
//! (`extern "win64"`): arguments in RCX, RDX, R8, R9 then the stack, with 32
//! bytes of shadow space. [`resolve`] maps a `(dll, name)` pair to the address
//! of the matching function or variable; the loader writes that address into
//! the program's import address table.
//!
//! This file has the process itself (start-up, exit, memory, the standard
//! streams and `printf`); [`crate::crt`] the rest of the C runtime (files,
//! the keyboard, strings, maths) and [`crate::gui`] windows and drawing. A
//! function none of them has gets a stub that says so once and returns 0
//! (see `pe.rs`).

#![allow(non_snake_case, static_mut_refs)]

use alloc::string::String;
use alloc::vec::Vec;
use justrt::sys;
use core::ffi::c_void;

// ----------------------------------------------------------------------------
// Process-wide state (winrun is single-threaded)

static mut ARGS: Option<Vec<String>> = None;
/// The three standard C streams, as `msvcrt` lays them out (48 bytes each).
static mut IOB: [u8; 48 * 3] = [0; 48 * 3];
static mut ERRNO: i32 = 0;
static mut FMODE: i32 = 0;
static mut COMMODE: i32 = 0;
static mut MB_CUR_MAX: i32 = 1;
static mut INITENV: u64 = 0;
/// A `struct lconv` whose `decimal_point` points at ".", which `printf` reads.
static mut LCONV: [u8; 128] = [0; 128];
static DOT: &[u8] = b".\0";
static mut LAST_ERROR: u32 = 0;
static mut IMAGE_BASE: u64 = 0;

/// `_acmdln`: the command line, which the start-up code of windowed
/// programs reads to find WinMain's `lpCmdLine`.
static mut ACMDLN: u64 = 0;

pub fn set_args(argv: &[String]) {
    // SAFETY: called once at start-up before the program runs.
    unsafe {
        ARGS = Some(argv.to_vec());
        ACMDLN = GetCommandLineA() as u64;
    }
}

/// Where the program's image sits (its module handle).
pub fn set_image_base(base: u64) {
    // SAFETY: as above.
    unsafe { IMAGE_BASE = base };
}

pub fn set_errno(e: i32) {
    // SAFETY: single-threaded.
    unsafe { ERRNO = e };
}

pub fn set_last_error(e: u32) {
    // SAFETY: single-threaded.
    unsafe { LAST_ERROR = e };
}

pub fn iob_base() -> u64 {
    core::ptr::addr_of!(IOB) as u64
}

pub fn console_write(bytes: &[u8]) {
    sys::call(sys::WRITE, sys::CONSOLE, bytes.as_ptr() as u64, bytes.len() as u64, 0, 0);
}

// ----------------------------------------------------------------------------
// The resolver

/// The address of the function or variable `name` exported from `dll`.
pub fn resolve(dll: &str, name: &str) -> Option<u64> {
    own(dll, name).or_else(|| crate::crt::resolve(name)).or_else(|| crate::gui::resolve(name))
}

fn own(dll: &str, name: &str) -> Option<u64> {
    let f: u64 = match (dll, name) {
        // kernel32
        (_, "ExitProcess") => ExitProcess as *const () as u64,
        (_, "GetLastError") => GetLastError as *const () as u64,
        (_, "SetLastError") => SetLastError as *const () as u64,
        (_, "Sleep") => Sleep as *const () as u64,
        (_, "GetStdHandle") => GetStdHandle as *const () as u64,
        (_, "WriteFile") => WriteFile as *const () as u64,
        (_, "WriteConsoleA") => WriteConsoleA as *const () as u64,
        (_, "InitializeCriticalSection") => noop4 as *const () as u64,
        (_, "EnterCriticalSection") => noop4 as *const () as u64,
        (_, "LeaveCriticalSection") => noop4 as *const () as u64,
        (_, "DeleteCriticalSection") => noop4 as *const () as u64,
        (_, "InitializeCriticalSectionAndSpinCount") => ret1 as *const () as u64,
        (_, "TlsGetValue") => ret0 as *const () as u64,
        (_, "TlsAlloc") => ret0 as *const () as u64,
        (_, "SetUnhandledExceptionFilter") => ret0 as *const () as u64,
        (_, "UnhandledExceptionFilter") => ret0 as *const () as u64,
        (_, "IsDBCSLeadByteEx") => ret0 as *const () as u64,
        (_, "VirtualProtect") => VirtualProtect as *const () as u64,
        (_, "VirtualQuery") => ret0 as *const () as u64,
        (_, "VirtualAlloc") => VirtualAlloc as *const () as u64,
        (_, "VirtualFree") => ret1 as *const () as u64,
        (_, "MultiByteToWideChar") => MultiByteToWideChar as *const () as u64,
        (_, "WideCharToMultiByte") => WideCharToMultiByte as *const () as u64,
        (_, "GetCurrentThreadId") => ret1 as *const () as u64,
        (_, "GetCurrentProcessId") => ret1 as *const () as u64,
        (_, "QueryPerformanceCounter") => QueryPerformanceCounter as *const () as u64,
        (_, "QueryPerformanceFrequency") => QueryPerformanceFrequency as *const () as u64,
        (_, "GetSystemTimeAsFileTime") => GetSystemTimeAsFileTime as *const () as u64,
        (_, "GetTickCount") => GetTickCount as *const () as u64,
        (_, "GetProcessHeap") => ret1 as *const () as u64,
        (_, "HeapAlloc") => HeapAlloc as *const () as u64,
        (_, "HeapFree") => HeapFree as *const () as u64,
        (_, "HeapReAlloc") => HeapReAlloc as *const () as u64,
        (_, "GetModuleHandleA" | "GetModuleHandleW") => GetModuleHandle as *const () as u64,
        (_, "GetModuleFileNameA") => GetModuleFileNameA as *const () as u64,
        (_, "GetProcAddress") => GetProcAddress as *const () as u64,
        (_, "LoadLibraryA" | "LoadLibraryW" | "LoadLibraryExA" | "LoadLibraryExW") => ret1 as *const () as u64,
        (_, "FreeLibrary") => ret1 as *const () as u64,
        (_, "GetCommandLineA") => GetCommandLineA as *const () as u64,
        (_, "GetCommandLineW") => GetCommandLineW as *const () as u64,
        (_, "GetConsoleMode") => ret0 as *const () as u64,
        (_, "SetConsoleMode") => ret1 as *const () as u64,
        (_, "GetConsoleOutputCP" | "GetACP" | "GetOEMCP") => GetACP as *const () as u64,
        (_, "FlushFileBuffers") => ret1 as *const () as u64,
        (_, "GetProcessAffinityMask") => ret1 as *const () as u64,
        (_, "RtlLookupFunctionEntry") => ret0 as *const () as u64,
        (_, "RtlVirtualUnwind") => ret0 as *const () as u64,
        (_, "RtlCaptureContext") => noop4 as *const () as u64,
        (_, "RtlAddFunctionTable") => ret1 as *const () as u64,

        // msvcrt — data
        (_, "_iob") => iob_base(),
        (_, "__iob_func") => __iob_func as *const () as u64,
        (_, "_errno") => _errno as *const () as u64,
        (_, "_fmode") => core::ptr::addr_of!(FMODE) as u64,
        (_, "_commode") => core::ptr::addr_of!(COMMODE) as u64,
        (_, "__initenv") => core::ptr::addr_of!(INITENV) as u64,
        (_, "_acmdln") => core::ptr::addr_of!(ACMDLN) as u64,
        (_, "_ismbblead" | "_ismbbtrail") => ret0 as *const () as u64,

        // msvcrt — start-up and exit
        (_, "__getmainargs") => __getmainargs as *const () as u64,
        (_, "__set_app_type") => noop4 as *const () as u64,
        (_, "__setusermatherr") => noop4 as *const () as u64,
        (_, "_initterm") => _initterm as *const () as u64,
        (_, "_initterm_e") => _initterm_e as *const () as u64,
        (_, "_cexit" | "_c_exit") => noop4 as *const () as u64,
        (_, "_lock" | "_unlock") => noop4 as *const () as u64,
        (_, "_onexit" | "atexit") => _onexit as *const () as u64,
        (_, "signal") => ret0 as *const () as u64,
        (_, "_amsg_exit") => _amsg_exit as *const () as u64,
        (_, "abort") => abort as *const () as u64,
        (_, "exit" | "_exit" | "_Exit") => exit as *const () as u64,
        (_, "__C_specific_handler") => __C_specific_handler as *const () as u64,
        (_, "_set_invalid_parameter_handler" | "_set_thread_local_invalid_parameter_handler") => ret0 as *const () as u64,
        (_, "__lconv_init") => noop4 as *const () as u64,

        // msvcrt — locale / errors
        (_, "___lc_codepage_func") => ret0 as *const () as u64,
        (_, "___mb_cur_max_func") => ___mb_cur_max_func as *const () as u64,
        (_, "localeconv") => localeconv as *const () as u64,
        (_, "strerror") => strerror as *const () as u64,

        // msvcrt — output
        (_, "vfprintf") => vfprintf_core as *const () as u64,
        (_, "fprintf") => core::ptr::addr_of!(WINRUN_FPRINTF_SHIM) as u64,
        (_, "fwrite") => fwrite as *const () as u64,
        (_, "fputc" | "putc") => fputc as *const () as u64,
        (_, "fputs") => fputs as *const () as u64,
        (_, "putchar") => putchar as *const () as u64,
        (_, "puts") => puts as *const () as u64,

        // msvcrt — memory and strings
        (_, "malloc") => malloc as *const () as u64,
        (_, "calloc") => calloc as *const () as u64,
        (_, "realloc") => realloc as *const () as u64,
        (_, "free") => free as *const () as u64,
        (_, "memcpy" | "memmove") => memcpy as *const () as u64,
        (_, "memset") => memset as *const () as u64,
        (_, "memcmp") => memcmp as *const () as u64,
        (_, "strlen") => strlen as *const () as u64,
        (_, "wcslen") => wcslen as *const () as u64,
        (_, "strcmp") => strcmp as *const () as u64,
        (_, "strncmp") => strncmp as *const () as u64,
        _ => return None,
    };
    Some(f)
}

/// winrun provides functions by name; programs that import by ordinal are not
/// supported (MinGW programs import by name).
pub fn resolve_ordinal(_dll: &str, _ordinal: u16) -> Option<u64> {
    None
}

// ----------------------------------------------------------------------------
// Generic stubs

static mut MISSING: Vec<u64> = Vec::new();

/// Where the stub of a function winrun does not have lands: it says so the
/// first time and returns 0 (which most Windows functions read as "failed").
pub extern "win64" fn missing(name: *const u8) -> u64 {
    // SAFETY: single-threaded; `name` is the stub's own NUL-terminated string.
    unsafe {
        if !MISSING.contains(&(name as u64)) {
            MISSING.push(name as u64);
            let n = core::slice::from_raw_parts(name, c_len(name));
            console_write(b"winrun: ");
            console_write(n);
            console_write(b" is not available yet: it does nothing\n");
        }
    }
    0
}

extern "win64" fn ret0() -> u64 {
    0
}
extern "win64" fn ret1() -> u64 {
    1
}
extern "win64" fn noop4(_a: u64, _b: u64, _c: u64, _d: u64) {}

// ----------------------------------------------------------------------------
// kernel32

extern "win64" fn ExitProcess(code: u32) -> ! {
    justrt::process::exit(code as i32)
}
extern "win64" fn GetLastError() -> u32 {
    // SAFETY: single-threaded.
    unsafe { LAST_ERROR }
}
extern "win64" fn SetLastError(e: u32) {
    set_last_error(e);
}
extern "win64" fn Sleep(ms: u32) {
    justrt::time::sleep_ms(ms as u64);
}
extern "win64" fn GetStdHandle(which: u32) -> u64 {
    // -10 = stdin, -11 = stdout, -12 = stderr: small non-null tokens that
    // crt::stream knows.
    match which as i32 {
        -10 => 3,
        -12 => 2,
        _ => 1,
    }
}
extern "win64" fn WriteFile(handle: u64, buf: *const u8, len: u32, written: *mut u32, _ovl: u64) -> u32 {
    // SAFETY: the program passes a buffer of `len` bytes it owns.
    let data = unsafe { core::slice::from_raw_parts(buf, len as usize) };
    let n = crate::crt::write_to(handle, data);
    if !written.is_null() {
        // SAFETY: the caller passes a place for the count (or null).
        unsafe { *written = n as u32 };
    }
    1
}
extern "win64" fn GetModuleHandle(name: u64) -> u64 {
    // SAFETY: single-threaded. Other modules ("kernel32.dll") get a token.
    if name == 0 {
        unsafe { IMAGE_BASE }
    } else {
        1
    }
}
extern "win64" fn GetModuleFileNameA(_module: u64, buf: *mut u8, n: u32) -> u32 {
    // SAFETY: ARGS was set at start-up.
    let name = unsafe { ARGS.as_ref().and_then(|a| a.first().cloned()).unwrap_or_default() };
    if n == 0 || buf.is_null() {
        return 0;
    }
    let k = name.len().min(n as usize - 1);
    // SAFETY: room for n bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(name.as_ptr(), buf, k);
        *buf.add(k) = 0;
    }
    k as u32
}
/// Functions looked up at run time: the ones winrun has, by name.
extern "win64" fn GetProcAddress(_module: u64, name: *const u8) -> u64 {
    if (name as u64) < 0x10000 {
        return 0; // by ordinal
    }
    let name = crate::crt::cstr(name);
    resolve("", &name).unwrap_or(0)
}
extern "win64" fn WriteConsoleA(_h: u64, buf: *const u8, len: u32, written: *mut u32, _r: u64) -> u32 {
    WriteFile(1, buf, len, written, 0)
}
extern "win64" fn VirtualProtect(_addr: u64, _size: u64, _new: u32, old: *mut u32) -> u32 {
    if !old.is_null() {
        // SAFETY: a place for the previous protection flags.
        unsafe { *old = 0x40 }; // PAGE_EXECUTE_READWRITE
    }
    1
}
extern "win64" fn GetCommandLineA() -> *const u8 {
    // SAFETY: ARGS was set at start-up; join the words with spaces, NUL-ended.
    let line = unsafe {
        let args = ARGS.as_ref().map(|a| a.join(" ")).unwrap_or_default();
        let mut bytes = args.into_bytes();
        bytes.push(0);
        bytes.leak()
    };
    line.as_ptr()
}
extern "win64" fn GetCommandLineW() -> *const u16 {
    // SAFETY: as above.
    let line = unsafe { ARGS.as_ref().map(|a| a.join(" ")).unwrap_or_default() };
    let mut wide: Vec<u16> = line.encode_utf16().collect();
    wide.push(0);
    wide.leak().as_ptr()
}
extern "win64" fn GetACP() -> u32 {
    65001 // UTF-8
}
extern "win64" fn QueryPerformanceCounter(out: *mut u64) -> u32 {
    if !out.is_null() {
        // SAFETY: a place for the counter.
        unsafe { *out = justrt::time::uptime_ms() };
    }
    1
}
extern "win64" fn QueryPerformanceFrequency(out: *mut u64) -> u32 {
    if !out.is_null() {
        // SAFETY: a place for the frequency (ticks per second: ms -> 1000).
        unsafe { *out = 1000 };
    }
    1
}
extern "win64" fn GetTickCount() -> u32 {
    justrt::time::uptime_ms() as u32
}
extern "win64" fn GetSystemTimeAsFileTime(out: *mut u64) {
    if !out.is_null() {
        // 100-ns ticks since 1601; close enough from the Unix epoch.
        let secs = justrt::time::now() + 946_684_800; // 2000 -> 1970
        let ticks = (secs + 11_644_473_600) * 10_000_000;
        // SAFETY: a place for the 64-bit file time.
        unsafe { *out = ticks };
    }
}

// ----------------------------------------------------------------------------
// Memory: a size-prefixed allocator over the Rust global allocator

const HDR: usize = 16;

extern "win64" fn malloc(size: u64) -> *mut c_void {
    alloc_bytes(size as usize)
}
extern "win64" fn calloc(n: u64, size: u64) -> *mut c_void {
    let total = (n as usize).saturating_mul(size as usize);
    let p = alloc_bytes(total);
    if !p.is_null() {
        // SAFETY: freshly allocated block of `total` bytes.
        unsafe { core::ptr::write_bytes(p as *mut u8, 0, total) };
    }
    p
}
extern "win64" fn free(ptr: *mut c_void) {
    free_bytes(ptr);
}
extern "win64" fn realloc(ptr: *mut c_void, size: u64) -> *mut c_void {
    if ptr.is_null() {
        return alloc_bytes(size as usize);
    }
    let old = block_size(ptr);
    let new = alloc_bytes(size as usize);
    if !new.is_null() {
        // SAFETY: both blocks are valid for the copied length.
        unsafe { core::ptr::copy_nonoverlapping(ptr as *const u8, new as *mut u8, old.min(size as usize)) };
        free_bytes(ptr);
    }
    new
}
extern "win64" fn HeapAlloc(_heap: u64, flags: u32, size: u64) -> *mut c_void {
    let p = alloc_bytes(size as usize);
    if flags & 8 != 0 && !p.is_null() {
        // HEAP_ZERO_MEMORY.
        unsafe { core::ptr::write_bytes(p as *mut u8, 0, size as usize) };
    }
    p
}
extern "win64" fn HeapFree(_heap: u64, _flags: u32, ptr: *mut c_void) -> u32 {
    free_bytes(ptr);
    1
}
extern "win64" fn HeapReAlloc(_heap: u64, _flags: u32, ptr: *mut c_void, size: u64) -> *mut c_void {
    realloc(ptr, size)
}
extern "win64" fn VirtualAlloc(_addr: u64, size: u64, _typ: u32, _prot: u32) -> *mut c_void {
    alloc_bytes(size as usize)
}

pub fn alloc_bytes(size: usize) -> *mut c_void {
    let total = size + HDR;
    // SAFETY: a non-zero layout; the global allocator uses the MAP syscall.
    let p = unsafe { alloc::alloc::alloc(layout(total)) };
    if p.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: room for the header we just reserved.
    unsafe { *(p as *mut usize) = size };
    unsafe { p.add(HDR) as *mut c_void }
}
fn free_bytes(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: ptr came from alloc_bytes, so the header sits just before it.
    unsafe {
        let base = (ptr as *mut u8).sub(HDR);
        let size = *(base as *const usize);
        alloc::alloc::dealloc(base, layout(size + HDR));
    }
}
fn block_size(ptr: *mut c_void) -> usize {
    // SAFETY: ptr came from alloc_bytes.
    unsafe { *((ptr as *mut u8).sub(HDR) as *const usize) }
}
fn layout(total: usize) -> alloc::alloc::Layout {
    alloc::alloc::Layout::from_size_align(total.max(1), 16).unwrap()
}

// ----------------------------------------------------------------------------
// Strings

extern "win64" fn memcpy(dst: *mut u8, src: *const u8, n: u64) -> *mut u8 {
    // SAFETY: the caller guarantees both ranges are valid for `n` bytes.
    unsafe { core::ptr::copy(src, dst, n as usize) };
    dst
}
extern "win64" fn memset(dst: *mut u8, v: i32, n: u64) -> *mut u8 {
    // SAFETY: the caller guarantees `dst` is valid for `n` bytes.
    unsafe { core::ptr::write_bytes(dst, v as u8, n as usize) };
    dst
}
extern "win64" fn memcmp(a: *const u8, b: *const u8, n: u64) -> i32 {
    // SAFETY: both ranges are valid for `n` bytes.
    let (a, b) = unsafe { (core::slice::from_raw_parts(a, n as usize), core::slice::from_raw_parts(b, n as usize)) };
    for (x, y) in a.iter().zip(b) {
        if x != y {
            return *x as i32 - *y as i32;
        }
    }
    0
}
pub unsafe fn c_len(mut p: *const u8) -> usize {
    let mut n = 0;
    // SAFETY: the string is NUL-terminated.
    while unsafe { *p } != 0 {
        n += 1;
        p = unsafe { p.add(1) };
    }
    n
}
extern "win64" fn strlen(s: *const u8) -> u64 {
    // SAFETY: NUL-terminated.
    unsafe { c_len(s) as u64 }
}
extern "win64" fn wcslen(s: *const u16) -> u64 {
    let mut n = 0;
    let mut p = s;
    // SAFETY: NUL-terminated wide string.
    while unsafe { *p } != 0 {
        n += 1;
        p = unsafe { p.add(1) };
    }
    n
}
extern "win64" fn strcmp(a: *const u8, b: *const u8) -> i32 {
    // SAFETY: both NUL-terminated.
    unsafe { strncmp(a, b, (c_len(a).max(c_len(b)) + 1) as u64) }
}
extern "win64" fn strncmp(a: *const u8, b: *const u8, n: u64) -> i32 {
    for i in 0..n as usize {
        // SAFETY: compared within `n`, stopping at the first NUL.
        let (x, y) = unsafe { (*a.add(i), *b.add(i)) };
        if x != y {
            return x as i32 - y as i32;
        }
        if x == 0 {
            break;
        }
    }
    0
}

// ----------------------------------------------------------------------------
// C runtime start-up

extern "win64" fn __iob_func() -> u64 {
    iob_base()
}
extern "win64" fn _errno() -> *mut i32 {
    core::ptr::addr_of_mut!(ERRNO)
}
extern "win64" fn ___mb_cur_max_func() -> *mut i32 {
    core::ptr::addr_of_mut!(MB_CUR_MAX)
}
extern "win64" fn localeconv() -> *mut u8 {
    // SAFETY: fill decimal_point (the first field) once with ".".
    unsafe {
        let p = core::ptr::addr_of_mut!(LCONV) as *mut u64;
        *p = DOT.as_ptr() as u64;
        core::ptr::addr_of_mut!(LCONV) as *mut u8
    }
}
extern "win64" fn strerror(_code: i32) -> *const u8 {
    c"error".as_ptr() as *const u8
}

/// `int __getmainargs(int *argc, char ***argv, char ***env, int wild, void*)`.
extern "win64" fn __getmainargs(argc: *mut i32, argv: *mut *mut *mut u8, env: *mut *mut *mut u8, _wild: i32, _info: u64) -> i32 {
    // SAFETY: ARGS was set at start-up.
    let args = unsafe { ARGS.clone().unwrap_or_default() };
    // Build a NULL-terminated char* array in the program's heap.
    let n = args.len();
    let arr = alloc_bytes((n + 1) * 8) as *mut *mut u8;
    for (i, a) in args.iter().enumerate() {
        let s = alloc_bytes(a.len() + 1) as *mut u8;
        // SAFETY: freshly allocated with room for the bytes and a NUL.
        unsafe {
            core::ptr::copy_nonoverlapping(a.as_ptr(), s, a.len());
            *s.add(a.len()) = 0;
            *arr.add(i) = s;
        }
    }
    // SAFETY: the final slot ends the array; the out-params are valid.
    unsafe {
        *arr.add(n) = core::ptr::null_mut();
        *argc = n as i32;
        *argv = arr;
        if !env.is_null() {
            let e = alloc_bytes(8) as *mut *mut u8;
            *e = core::ptr::null_mut();
            *env = e;
            INITENV = e as u64;
        }
    }
    0
}

/// `void _initterm(PVFV *first, PVFV *last)`: call each non-null initialiser.
extern "win64" fn _initterm(first: *const u64, last: *const u64) {
    let mut p = first;
    while p < last {
        // SAFETY: the table holds function pointers or null, ended by `last`.
        let f = unsafe { *p };
        if f != 0 {
            let func: extern "win64" fn() = unsafe { core::mem::transmute(f) };
            func();
        }
        // SAFETY: still inside the table.
        p = unsafe { p.add(1) };
    }
}
/// Like `_initterm` but each initialiser returns an error code; stop on the
/// first non-zero.
extern "win64" fn _initterm_e(first: *const u64, last: *const u64) -> i32 {
    let mut p = first;
    while p < last {
        // SAFETY: as in `_initterm`.
        let f = unsafe { *p };
        if f != 0 {
            let func: extern "win64" fn() -> i32 = unsafe { core::mem::transmute(f) };
            let r = func();
            if r != 0 {
                return r;
            }
        }
        // SAFETY: still inside the table.
        p = unsafe { p.add(1) };
    }
    0
}
extern "win64" fn _onexit(func: u64) -> u64 {
    func // pretend it was registered
}
extern "win64" fn _amsg_exit(code: i32) -> ! {
    console_write(b"winrun: the C runtime stopped the program\n");
    justrt::process::exit(code)
}
extern "win64" fn abort() -> ! {
    justrt::process::exit(3)
}
extern "win64" fn exit(code: i32) -> ! {
    justrt::process::exit(code)
}
extern "win64" fn __C_specific_handler(_rec: u64, _frame: u64, _ctx: u64, _disp: u64) -> i32 {
    1 // ExceptionContinueSearch
}

// ----------------------------------------------------------------------------
// Locale conversions (ASCII only)

extern "win64" fn MultiByteToWideChar(_cp: u32, _flags: u32, src: *const u8, src_len: i32, dst: *mut u16, dst_len: i32) -> i32 {
    // SAFETY: the caller passes a source of `src_len` bytes (or -1 for NUL).
    let len = if src_len < 0 { unsafe { c_len(src) + 1 } } else { src_len as usize };
    if dst_len == 0 || dst.is_null() {
        return len as i32;
    }
    let n = len.min(dst_len as usize);
    for i in 0..n {
        // SAFETY: within both buffers.
        unsafe { *dst.add(i) = *src.add(i) as u16 };
    }
    n as i32
}
extern "win64" fn WideCharToMultiByte(
    _cp: u32,
    _flags: u32,
    src: *const u16,
    src_len: i32,
    dst: *mut u8,
    dst_len: i32,
    _d: u64,
    _u: u64,
) -> i32 {
    let len = if src_len < 0 {
        let mut n = 0;
        // SAFETY: NUL-terminated wide string.
        while unsafe { *src.add(n) } != 0 {
            n += 1;
        }
        n + 1
    } else {
        src_len as usize
    };
    if dst_len == 0 || dst.is_null() {
        return len as i32;
    }
    let n = len.min(dst_len as usize);
    for i in 0..n {
        // SAFETY: within both buffers.
        unsafe { *dst.add(i) = *src.add(i) as u8 };
    }
    n as i32
}

// ----------------------------------------------------------------------------
// Output: fwrite / fputc / puts and the printf family

extern "win64" fn fwrite(ptr: *const u8, size: u64, count: u64, file: u64) -> u64 {
    let total = size.saturating_mul(count);
    if total == 0 {
        return 0;
    }
    // SAFETY: the C program owns `total` bytes at `ptr`.
    let data = unsafe { core::slice::from_raw_parts(ptr, total as usize) };
    write_stream(file, data);
    count
}
extern "win64" fn fputc(c: i32, file: u64) -> i32 {
    write_stream(file, &[c as u8]);
    c
}
extern "win64" fn putchar(c: i32) -> i32 {
    console_write(&[c as u8]);
    c
}
/// Writes to a `FILE*`: a file, or the console for stdout and stderr.
fn write_stream(file: u64, data: &[u8]) {
    crate::crt::write_to(file, data);
}
extern "win64" fn fputs(s: *const u8, file: u64) -> i32 {
    // SAFETY: NUL-terminated string.
    let bytes = unsafe { core::slice::from_raw_parts(s, c_len(s)) };
    write_stream(file, bytes);
    0
}
extern "win64" fn puts(s: *const u8) -> i32 {
    // SAFETY: NUL-terminated string.
    let bytes = unsafe { core::slice::from_raw_parts(s, c_len(s)) };
    console_write(bytes);
    console_write(b"\n");
    0
}

/// `int vfprintf(FILE *f, const char *fmt, va_list va)`. On Windows x64 a
/// `va_list` is a pointer to the arguments laid out as 8-byte slots, so this
/// is also the body the `fprintf` shim jumps to.
#[no_mangle]
pub extern "win64" fn vfprintf_core(file: u64, fmt: *const u8, va: *const u64) -> i32 {
    let out = format_c(fmt, va);
    write_stream(file, &out);
    out.len() as i32
}

core::arch::global_asm!(
    r#"
    .global WINRUN_FPRINTF_SHIM
WINRUN_FPRINTF_SHIM:
    mov [rsp + 8], rcx
    mov [rsp + 16], rdx
    mov [rsp + 24], r8
    mov [rsp + 32], r9
    lea r8, [rsp + 24]
    jmp {core}
"#,
    core = sym vfprintf_core,
);

extern "C" {
    static WINRUN_FPRINTF_SHIM: u8;
}

/// Formats a C `printf` string against a Windows `va_list`.
pub fn format_c(fmt: *const u8, va: *const u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut arg = 0usize;
    // SAFETY: `fmt` is a NUL-terminated string; each argument is one 8-byte
    // slot in the `va_list` the caller built.
    let next = |arg: &mut usize| unsafe {
        let v = *va.add(*arg);
        *arg += 1;
        v
    };
    let at = |i: usize| unsafe { *fmt.add(i) };
    loop {
        let c = at(i);
        if c == 0 {
            break;
        }
        if c != b'%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        // Flags.
        let mut left = false;
        let mut zero = false;
        let mut plus = false;
        let mut space = false;
        loop {
            match at(i) {
                b'-' => left = true,
                b'0' => zero = true,
                b'+' => plus = true,
                b' ' => space = true,
                b'#' => {}
                _ => break,
            }
            i += 1;
        }
        // Width.
        let mut width = 0usize;
        if at(i) == b'*' {
            width = next(&mut arg) as i32 as usize;
            i += 1;
        } else {
            while at(i).is_ascii_digit() {
                width = width * 10 + (at(i) - b'0') as usize;
                i += 1;
            }
        }
        // Precision.
        let mut prec: Option<usize> = None;
        if at(i) == b'.' {
            i += 1;
            if at(i) == b'*' {
                prec = Some(next(&mut arg) as i32 as usize);
                i += 1;
            } else {
                let mut p = 0usize;
                while at(i).is_ascii_digit() {
                    p = p * 10 + (at(i) - b'0') as usize;
                    i += 1;
                }
                prec = Some(p);
            }
        }
        // Length modifiers. Every argument is an 8-byte slot, but only the
        // low 32 bits of an `int` (or a Windows `long`) are meaningful.
        let mut wide = false;
        let mut short = 0;
        loop {
            match at(i) {
                b'l' if at(i + 1) == b'l' => {
                    wide = true;
                    i += 1;
                }
                b'z' | b'j' | b't' | b'q' | b'L' => wide = true,
                b'I' if at(i + 1) == b'6' && at(i + 2) == b'4' => {
                    wide = true;
                    i += 2;
                }
                b'I' if at(i + 1) == b'3' && at(i + 2) == b'2' => i += 2,
                b'I' => wide = true,
                b'h' => short += 1,
                b'l' => {}
                _ => break,
            }
            i += 1;
        }
        let conv = at(i);
        i += 1;
        let signed = |v: u64| -> i128 {
            match (wide, short) {
                (true, _) => v as i64 as i128,
                (_, 0) => v as i32 as i128,
                (_, 1) => v as i16 as i128,
                _ => v as i8 as i128,
            }
        };
        let unsigned = |v: u64| -> i128 {
            match (wide, short) {
                (true, _) => v as i128,
                (_, 0) => v as u32 as i128,
                (_, 1) => v as u16 as i128,
                _ => v as u8 as i128,
            }
        };
        let mut s: Vec<u8> = Vec::new();
        match conv {
            b'%' => s.push(b'%'),
            b'c' => s.push(next(&mut arg) as u8),
            b's' => {
                let p = next(&mut arg) as *const u8;
                if p.is_null() {
                    s.extend_from_slice(b"(null)");
                } else {
                    // SAFETY: a NUL-terminated C string.
                    let len = unsafe { c_len(p) };
                    let len = prec.map_or(len, |pr| pr.min(len));
                    // SAFETY: `len` bytes are valid.
                    s.extend_from_slice(unsafe { core::slice::from_raw_parts(p, len) });
                }
            }
            b'd' | b'i' => fmt_int(&mut s, signed(next(&mut arg)), 10, false, plus, space, prec),
            b'u' => fmt_int(&mut s, unsigned(next(&mut arg)), 10, false, false, false, prec),
            b'x' => fmt_int(&mut s, unsigned(next(&mut arg)), 16, false, false, false, prec),
            b'X' => fmt_int(&mut s, unsigned(next(&mut arg)), 16, true, false, false, prec),
            b'o' => fmt_int(&mut s, unsigned(next(&mut arg)), 8, false, false, false, prec),
            b'p' => {
                s.extend_from_slice(b"0x");
                fmt_int(&mut s, next(&mut arg) as i128, 16, false, false, false, None);
            }
            b'f' | b'F' => fmt_float(&mut s, f64::from_bits(next(&mut arg)), prec.unwrap_or(6), plus, space),
            b'g' | b'G' => fmt_general(&mut s, f64::from_bits(next(&mut arg)), prec.unwrap_or(6).max(1), plus, space),
            b'e' | b'E' => fmt_exp(&mut s, f64::from_bits(next(&mut arg)), prec.unwrap_or(6), plus, space, conv == b'E'),
            other => {
                s.push(b'%');
                s.push(other);
            }
        }
        // Padding to the field width.
        if s.len() < width {
            let pad = width - s.len();
            if left {
                out.extend_from_slice(&s);
                out.extend(core::iter::repeat_n(b' ', pad));
            } else if zero && matches!(conv, b'd' | b'i' | b'u' | b'x' | b'X' | b'o' | b'f' | b'F') {
                // Keep a leading sign before the zeros.
                let sign = s.first().is_some_and(|&b| b == b'-' || b == b'+' || b == b' ');
                if sign {
                    out.push(s[0]);
                    out.extend(core::iter::repeat_n(b'0', pad));
                    out.extend_from_slice(&s[1..]);
                } else {
                    out.extend(core::iter::repeat_n(b'0', pad));
                    out.extend_from_slice(&s);
                }
            } else {
                out.extend(core::iter::repeat_n(b' ', pad));
                out.extend_from_slice(&s);
            }
        } else {
            out.extend_from_slice(&s);
        }
    }
    out
}

fn fmt_int(out: &mut Vec<u8>, value: i128, base: u32, upper: bool, plus: bool, space: bool, prec: Option<usize>) {
    let neg = value < 0 && base == 10;
    let mut v = if neg { (-value) as u128 } else { value as u128 };
    let digits = b"0123456789abcdef";
    let mut buf = [0u8; 40];
    let mut n = 0;
    if v == 0 {
        buf[n] = b'0';
        n += 1;
    }
    while v > 0 {
        let d = (v % base as u128) as usize;
        buf[n] = if upper { digits[d].to_ascii_uppercase() } else { digits[d] };
        n += 1;
        v /= base as u128;
    }
    while let Some(p) = prec {
        if n >= p {
            break;
        }
        buf[n] = b'0';
        n += 1;
    }
    if neg {
        out.push(b'-');
    } else if plus {
        out.push(b'+');
    } else if space {
        out.push(b' ');
    }
    for i in (0..n).rev() {
        out.push(buf[i]);
    }
}

fn fmt_float(out: &mut Vec<u8>, value: f64, prec: usize, plus: bool, space: bool) {
    if value.is_nan() {
        out.extend_from_slice(b"nan");
        return;
    }
    let neg = value.is_sign_negative();
    let mut v = if neg { -value } else { value };
    if neg {
        out.push(b'-');
    } else if plus {
        out.push(b'+');
    } else if space {
        out.push(b' ');
    }
    if v.is_infinite() {
        out.extend_from_slice(b"inf");
        return;
    }
    // Round at the requested number of decimals (v >= 0 here).
    let mut scale = 1.0f64;
    for _ in 0..prec {
        scale *= 10.0;
    }
    v += 0.5 / scale;
    // Integer part (truncates toward zero for a non-negative value).
    let int_u = v as u128;
    fmt_int(out, int_u as i128, 10, false, false, false, None);
    if prec > 0 {
        out.push(b'.');
        let mut frac = v - int_u as f64;
        for _ in 0..prec {
            frac *= 10.0;
            let d = frac as u64 % 10;
            out.push(b'0' + d as u8);
            frac -= d as f64;
        }
    }
}

/// Splits a finite, non-zero `v` into a mantissa in [1, 10) and an exponent.
fn decompose(v: f64) -> (f64, i32) {
    let mut e = 0i32;
    let mut m = v.abs();
    while m >= 10.0 {
        m /= 10.0;
        e += 1;
    }
    while m < 1.0 {
        m *= 10.0;
        e -= 1;
    }
    (if v < 0.0 { -m } else { m }, e)
}

/// `%e`: d.ddde±XX.
fn fmt_exp(out: &mut Vec<u8>, value: f64, prec: usize, plus: bool, space: bool, upper: bool) {
    if value == 0.0 || !value.is_finite() {
        fmt_float(out, value, prec, plus, space);
        if value == 0.0 {
            out.extend_from_slice(if upper { b"E+00" } else { b"e+00" });
        }
        return;
    }
    let (mut m, mut e) = decompose(value);
    // Rounding may carry into a new digit (9.9996 -> 10.000).
    let mut probe = Vec::new();
    fmt_float(&mut probe, m.abs(), prec, false, false);
    if probe.starts_with(b"10") {
        m /= 10.0;
        e += 1;
    }
    fmt_float(out, m, prec, plus, space);
    out.push(if upper { b'E' } else { b'e' });
    out.push(if e < 0 { b'-' } else { b'+' });
    if e.abs() < 10 {
        out.push(b'0');
    }
    fmt_int(out, e.abs() as i128, 10, false, false, false, None);
}

/// `%g`: the shorter of `%f` and `%e`, without trailing zeros.
fn fmt_general(out: &mut Vec<u8>, value: f64, prec: usize, plus: bool, space: bool) {
    let e = if value == 0.0 || !value.is_finite() { 0 } else { decompose(value).1 };
    let mut s = Vec::new();
    if e < -4 || e >= prec as i32 {
        fmt_exp(&mut s, value, prec - 1, plus, space, false);
        // Trim zeros in the mantissa.
        if let Some(epos) = s.iter().position(|&b| b == b'e') {
            let (mant, exp) = s.split_at(epos);
            let mut mant = mant.to_vec();
            if mant.contains(&b'.') {
                while mant.last() == Some(&b'0') {
                    mant.pop();
                }
                if mant.last() == Some(&b'.') {
                    mant.pop();
                }
            }
            mant.extend_from_slice(exp);
            s = mant;
        }
    } else {
        fmt_float(&mut s, value, (prec as i32 - 1 - e).max(0) as usize, plus, space);
        if s.contains(&b'.') {
            while s.last() == Some(&b'0') {
                s.pop();
            }
            if s.last() == Some(&b'.') {
                s.pop();
            }
        }
    }
    out.extend_from_slice(&s);
}
