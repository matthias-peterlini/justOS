//! The rest of the C runtime (`msvcrt`) and the `kernel32` calls around it:
//! files, the keyboard, strings, numbers, time and maths.
//!
//! A file is read whole when it is opened and written back when it is closed
//! (or flushed): seeking and mixing reads with writes are then trivial, and
//! the programs winrun runs work on small files.

#![allow(non_snake_case, static_mut_refs)]

use crate::win::{alloc_bytes, c_len, console_write, format_c};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use arcc_core::float as m;
use core::ffi::c_void;

/// The address of `name`, if it is one of the functions here.
pub fn resolve(name: &str) -> Option<u64> {
    let f: u64 = match name {
        // stdio: files
        "fopen" => fopen as *const () as u64,
        "fclose" => fclose as *const () as u64,
        "fread" => fread as *const () as u64,
        "fgetc" | "getc" => fgetc as *const () as u64,
        "getchar" => getchar as *const () as u64,
        "ungetc" => ungetc as *const () as u64,
        "fgets" => fgets as *const () as u64,
        "gets" => gets as *const () as u64,
        "fseek" | "_fseeki64" => fseek as *const () as u64,
        "ftell" | "_ftelli64" => ftell as *const () as u64,
        "rewind" => rewind as *const () as u64,
        "feof" => feof as *const () as u64,
        "ferror" => ferror as *const () as u64,
        "clearerr" => clearerr as *const () as u64,
        "fflush" => fflush as *const () as u64,
        "setvbuf" | "setbuf" => ret0 as *const () as u64,
        "remove" | "_unlink" | "unlink" => remove as *const () as u64,
        "rename" => rename as *const () as u64,
        "_fileno" | "fileno" => fileno as *const () as u64,
        "_isatty" | "isatty" => isatty as *const () as u64,
        // stdio: formatted
        "sprintf" => core::ptr::addr_of!(WINRUN_SPRINTF) as u64,
        "snprintf" | "_snprintf" => core::ptr::addr_of!(WINRUN_SNPRINTF) as u64,
        "vsprintf" => vsprintf as *const () as u64,
        "vsnprintf" | "_vsnprintf" => vsnprintf as *const () as u64,
        "printf" => core::ptr::addr_of!(WINRUN_PRINTF) as u64,
        "vprintf" => vprintf as *const () as u64,
        "sscanf" => core::ptr::addr_of!(WINRUN_SSCANF) as u64,
        "scanf" => core::ptr::addr_of!(WINRUN_SCANF) as u64,
        "fscanf" => core::ptr::addr_of!(WINRUN_FSCANF) as u64,
        "vsscanf" => vsscanf as *const () as u64,
        "vfscanf" => vfscanf as *const () as u64,
        // strings
        "strcpy" => strcpy as *const () as u64,
        "strncpy" => strncpy as *const () as u64,
        "strcat" => strcat as *const () as u64,
        "strncat" => strncat as *const () as u64,
        "strchr" => strchr as *const () as u64,
        "strrchr" => strrchr as *const () as u64,
        "strstr" => strstr as *const () as u64,
        "strdup" | "_strdup" => strdup as *const () as u64,
        "strtok" => strtok as *const () as u64,
        "strspn" => strspn as *const () as u64,
        "strcspn" => strcspn as *const () as u64,
        "strpbrk" => strpbrk as *const () as u64,
        "memchr" => memchr as *const () as u64,
        "_stricmp" | "stricmp" | "_strcmpi" | "lstrcmpiA" => stricmp as *const () as u64,
        "_strnicmp" | "strnicmp" => strnicmp as *const () as u64,
        "lstrcmpA" => lstrcmpA as *const () as u64,
        "lstrlenA" => lstrlenA as *const () as u64,
        "lstrlenW" => lstrlenW as *const () as u64,
        "lstrcpyA" => strcpy as *const () as u64,
        "lstrcatA" => strcat as *const () as u64,
        "_strlwr" | "strlwr" => strlwr as *const () as u64,
        "_strupr" | "strupr" => strupr as *const () as u64,
        "wcscpy" => wcscpy as *const () as u64,
        "wcscmp" => wcscmp as *const () as u64,
        // characters
        "toupper" => toupper as *const () as u64,
        "tolower" => tolower as *const () as u64,
        "isalpha" => isalpha as *const () as u64,
        "isdigit" => isdigit as *const () as u64,
        "isalnum" => isalnum as *const () as u64,
        "isspace" => isspace as *const () as u64,
        "isupper" => isupper as *const () as u64,
        "islower" => islower as *const () as u64,
        "isxdigit" => isxdigit as *const () as u64,
        "ispunct" => ispunct as *const () as u64,
        "isprint" => isprint as *const () as u64,
        "iscntrl" => iscntrl as *const () as u64,
        // numbers
        "atoi" | "atol" | "_wtoi" => atoi as *const () as u64,
        "atoll" | "_atoi64" => atoll as *const () as u64,
        "strtol" => strtol as *const () as u64,
        "strtoul" => strtoul as *const () as u64,
        "strtoll" | "_strtoi64" => strtoll as *const () as u64,
        "strtoull" | "_strtoui64" => strtoull as *const () as u64,
        "atof" => atof as *const () as u64,
        "strtod" => strtod as *const () as u64,
        "strtof" => strtof as *const () as u64,
        "abs" | "labs" => abs as *const () as u64,
        "llabs" | "_abs64" => llabs as *const () as u64,
        "qsort" => qsort as *const () as u64,
        "bsearch" => bsearch as *const () as u64,
        "rand" => rand as *const () as u64,
        "srand" => srand as *const () as u64,
        "getenv" => ret0 as *const () as u64,
        "system" => system as *const () as u64,
        // time
        "time" | "_time64" => time as *const () as u64,
        "_time32" => time as *const () as u64,
        "clock" => clock as *const () as u64,
        "difftime" | "_difftime64" => difftime as *const () as u64,
        "localtime" | "_localtime64" | "gmtime" | "_gmtime64" => localtime as *const () as u64,
        "mktime" | "_mktime64" => mktime as *const () as u64,
        // maths
        "sqrt" => sqrt as *const () as u64,
        "sin" => sin as *const () as u64,
        "cos" => cos as *const () as u64,
        "tan" => tan as *const () as u64,
        "asin" => asin as *const () as u64,
        "acos" => acos as *const () as u64,
        "atan" => atan as *const () as u64,
        "atan2" => atan2 as *const () as u64,
        "exp" => exp as *const () as u64,
        "log" => log as *const () as u64,
        "log10" => log10 as *const () as u64,
        "log2" => log2 as *const () as u64,
        "pow" => pow as *const () as u64,
        "floor" => floor as *const () as u64,
        "ceil" => ceil as *const () as u64,
        "round" => round as *const () as u64,
        "trunc" => trunc as *const () as u64,
        "fabs" => fabs as *const () as u64,
        "fmod" => fmod as *const () as u64,
        "hypot" | "_hypot" => hypot as *const () as u64,
        "ldexp" => ldexp as *const () as u64,
        "sqrtf" => sqrtf as *const () as u64,
        "sinf" => sinf as *const () as u64,
        "cosf" => cosf as *const () as u64,
        "fabsf" => fabsf as *const () as u64,
        "floorf" => floorf as *const () as u64,
        "atan2f" => atan2f as *const () as u64,
        "powf" => powf as *const () as u64,
        // kernel32: files and the console
        "CreateFileA" => CreateFileA as *const () as u64,
        "CreateFileW" => CreateFileW as *const () as u64,
        "ReadFile" => ReadFile as *const () as u64,
        "CloseHandle" => CloseHandle as *const () as u64,
        "GetFileSize" => GetFileSize as *const () as u64,
        "GetFileSizeEx" => GetFileSizeEx as *const () as u64,
        "SetFilePointer" => SetFilePointer as *const () as u64,
        "SetFilePointerEx" => SetFilePointerEx as *const () as u64,
        "SetEndOfFile" => SetEndOfFile as *const () as u64,
        "DeleteFileA" => DeleteFileA as *const () as u64,
        "DeleteFileW" => DeleteFileW as *const () as u64,
        "GetFileAttributesA" => GetFileAttributesA as *const () as u64,
        "GetFileAttributesW" => GetFileAttributesW as *const () as u64,
        "CreateDirectoryA" => CreateDirectoryA as *const () as u64,
        "CreateDirectoryW" => CreateDirectoryW as *const () as u64,
        "GetCurrentDirectoryA" => GetCurrentDirectoryA as *const () as u64,
        "SetCurrentDirectoryA" => SetCurrentDirectoryA as *const () as u64,
        "ReadConsoleA" => ReadConsoleA as *const () as u64,
        "ReadConsoleW" => ReadConsoleW as *const () as u64,
        "WriteConsoleW" => WriteConsoleW as *const () as u64,
        "GetLocalTime" | "GetSystemTime" => GetLocalTime as *const () as u64,
        "GetStartupInfoA" | "GetStartupInfoW" => GetStartupInfo as *const () as u64,
        "GetEnvironmentVariableA" | "GetEnvironmentVariableW" => ret0 as *const () as u64,
        "SetEnvironmentVariableA" | "SetEnvironmentVariableW" => ret1 as *const () as u64,
        "IsDebuggerPresent" => ret0 as *const () as u64,
        "OutputDebugStringA" | "OutputDebugStringW" => ret0 as *const () as u64,
        "SetConsoleTitleA" | "SetConsoleTitleW" | "SetConsoleTextAttribute" | "SetConsoleCtrlHandler" => ret1 as *const () as u64,
        _ => return None,
    };
    Some(f)
}

extern "win64" fn ret0() -> u64 {
    0
}
extern "win64" fn ret1() -> u64 {
    1
}

// ----------------------------------------------------------------------------
// Paths

/// A Windows path as a JustOS one: `\` becomes `/` and a drive (`C:\x`) is
/// the system volume.
pub fn path(p: &str) -> String {
    let p = p.replace('\\', "/");
    let b = p.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return alloc::format!("sys:{}", if p.len() > 2 { &p[2..] } else { "/" });
    }
    p
}

/// A NUL-terminated byte string.
pub fn cstr(p: *const u8) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: a NUL-terminated string from the program.
    let bytes = unsafe { core::slice::from_raw_parts(p, c_len(p)) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// A NUL-terminated UTF-16 string.
pub fn wstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut v = Vec::new();
    let mut q = p;
    // SAFETY: NUL-terminated.
    unsafe {
        while *q != 0 {
            v.push(*q);
            q = q.add(1);
        }
    }
    String::from_utf16_lossy(&v)
}

// ----------------------------------------------------------------------------
// Open files

const MAGIC: u64 = 0x454C_4946_4E55_5241; // "ARUNFILE"

/// An open file: a `FILE*` and a Win32 `HANDLE` both point at one of these.
pub struct CFile {
    magic: u64,
    path: String,
    data: Vec<u8>,
    pos: usize,
    append: bool,
    write: bool,
    dirty: bool,
    eof: bool,
    err: bool,
}

impl CFile {
    fn open(p: &str, read: bool, write: bool, truncate: bool, create: bool, append: bool) -> Option<*mut CFile> {
        let p = path(p);
        let data = match justrt::fs::read(&p) {
            Ok(d) if !truncate => d,
            Ok(_) => Vec::new(),
            Err(_) if create => Vec::new(),
            Err(_) => return None,
        };
        let _ = read;
        let pos = if append { data.len() } else { 0 };
        let f = Box::new(CFile { magic: MAGIC, path: p, data, pos, append, write, dirty: truncate || (create && write), eof: false, err: false });
        let ptr = Box::into_raw(f);
        if write {
            // Make sure the file exists (and is empty when asked) right away.
            // SAFETY: just created.
            if !unsafe { (*ptr).flush() } {
                // SAFETY: from Box::into_raw above.
                drop(unsafe { Box::from_raw(ptr) });
                return None;
            }
        }
        Some(ptr)
    }

    fn read(&mut self, buf: &mut [u8]) -> usize {
        let n = buf.len().min(self.data.len().saturating_sub(self.pos));
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        if n < buf.len() {
            self.eof = true;
        }
        n
    }

    fn write(&mut self, bytes: &[u8]) -> usize {
        if !self.write {
            self.err = true;
            return 0;
        }
        if self.append {
            self.pos = self.data.len();
        }
        let end = self.pos + bytes.len();
        if end > self.data.len() {
            self.data.resize(end, 0);
        }
        self.data[self.pos..end].copy_from_slice(bytes);
        self.pos = end;
        self.dirty = true;
        bytes.len()
    }

    fn flush(&mut self) -> bool {
        if !self.dirty {
            return true;
        }
        self.dirty = false;
        justrt::fs::write(&self.path, &self.data).is_ok()
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }
}

/// What a `FILE*` or a `HANDLE` stands for.
pub enum Stream {
    In,
    Out,
    File(&'static mut CFile),
}

/// The stream behind `FILE*` / `HANDLE` `h`: the three slots of `_iob`, the
/// small console handles of `GetStdHandle`, or an open file.
pub fn stream(h: u64) -> Option<Stream> {
    let base = crate::win::iob_base();
    if h >= base && h < base + 48 * 3 {
        return Some(if h == base { Stream::In } else { Stream::Out });
    }
    match h {
        1 | 2 => return Some(Stream::Out),
        3 => return Some(Stream::In),
        0 => return None,
        _ => {}
    }
    if !h.is_multiple_of(8) {
        return None;
    }
    // SAFETY: open files are ours; anything else would be a bad handle the
    // program passed, read only for its first word.
    let f = unsafe { &mut *(h as *mut CFile) };
    (f.magic == MAGIC).then_some(Stream::File(f))
}

/// Writes to a stream; returns the bytes written.
pub fn write_to(h: u64, data: &[u8]) -> usize {
    match stream(h) {
        Some(Stream::File(f)) => f.write(data),
        _ => {
            console_write(data);
            data.len()
        }
    }
}

fn close(h: u64) -> bool {
    match stream(h) {
        Some(Stream::File(f)) => {
            let ok = f.flush();
            f.magic = 0;
            // SAFETY: an open file comes from Box::into_raw in CFile::open.
            drop(unsafe { Box::from_raw(f as *mut CFile) });
            ok
        }
        Some(_) => true,
        None => false,
    }
}

// ----------------------------------------------------------------------------
// The keyboard: whole lines, typed with the usual editing, then read a byte
// at a time.

static mut STDIN: Vec<u8> = Vec::new();
static mut STDIN_AT: usize = 0;
static mut STDIN_EOF: bool = false;

fn stdin_fill() -> bool {
    // SAFETY: programs are single-threaded.
    unsafe {
        if STDIN_AT < STDIN.len() {
            return true;
        }
        if STDIN_EOF {
            return false;
        }
        match justrt::console::read_line() {
            Some(line) => {
                STDIN = line.into_bytes();
                STDIN.push(b'\n');
                STDIN_AT = 0;
                true
            }
            None => {
                STDIN_EOF = true;
                false
            }
        }
    }
}

fn stdin_peek() -> Option<u8> {
    // SAFETY: as above.
    stdin_fill().then(|| unsafe { STDIN[STDIN_AT] })
}

fn stdin_next() -> Option<u8> {
    let b = stdin_peek()?;
    // SAFETY: as above.
    unsafe { STDIN_AT += 1 };
    Some(b)
}

fn getc(h: u64) -> Option<u8> {
    match stream(h)? {
        Stream::In => stdin_next(),
        Stream::Out => None,
        Stream::File(f) => {
            let mut b = [0u8];
            (f.read(&mut b) == 1).then_some(b[0])
        }
    }
}

// ----------------------------------------------------------------------------
// stdio

extern "win64" fn fopen(name: *const u8, mode: *const u8) -> u64 {
    let mode = cstr(mode);
    let plus = mode.contains('+');
    let f = match mode.chars().next() {
        Some('r') => CFile::open(&cstr(name), true, plus, false, false, false),
        Some('w') => CFile::open(&cstr(name), plus, true, true, true, false),
        Some('a') => CFile::open(&cstr(name), plus, true, false, true, true),
        _ => None,
    };
    match f {
        Some(p) => p as u64,
        None => {
            crate::win::set_errno(2); // ENOENT
            0
        }
    }
}
extern "win64" fn fclose(f: u64) -> i32 {
    if close(f) {
        0
    } else {
        -1
    }
}
extern "win64" fn fflush(f: u64) -> i32 {
    match stream(f) {
        Some(Stream::File(f)) => {
            if f.flush() {
                0
            } else {
                -1
            }
        }
        _ => 0,
    }
}
extern "win64" fn fread(ptr: *mut u8, size: u64, count: u64, f: u64) -> u64 {
    let total = (size * count) as usize;
    if total == 0 {
        return 0;
    }
    // SAFETY: the program owns `total` bytes at `ptr`.
    let buf = unsafe { core::slice::from_raw_parts_mut(ptr, total) };
    let n = match stream(f) {
        Some(Stream::File(f)) => f.read(buf),
        Some(Stream::In) => {
            let mut n = 0;
            while n < total {
                match stdin_next() {
                    Some(b) => {
                        buf[n] = b;
                        n += 1;
                    }
                    None => break,
                }
            }
            n
        }
        _ => 0,
    };
    n as u64 / size
}
extern "win64" fn fgetc(f: u64) -> i32 {
    getc(f).map_or(-1, |b| b as i32)
}
extern "win64" fn getchar() -> i32 {
    stdin_next().map_or(-1, |b| b as i32)
}
extern "win64" fn ungetc(c: i32, f: u64) -> i32 {
    if c < 0 {
        return -1;
    }
    match stream(f) {
        Some(Stream::File(f)) if f.pos > 0 => {
            f.pos -= 1;
            f.data[f.pos] = c as u8;
            f.eof = false;
            c
        }
        // SAFETY: single-threaded; put the byte back in front of the line.
        Some(Stream::In) => unsafe {
            if STDIN_AT > 0 {
                STDIN_AT -= 1;
                STDIN[STDIN_AT] = c as u8;
            } else {
                STDIN.insert(0, c as u8);
            }
            c
        },
        _ => -1,
    }
}
extern "win64" fn fgets(buf: *mut u8, n: i32, f: u64) -> *mut u8 {
    if n <= 0 {
        return core::ptr::null_mut();
    }
    let mut i = 0usize;
    while i + 1 < n as usize {
        match getc(f) {
            Some(b) => {
                // SAFETY: inside the caller's buffer of `n` bytes.
                unsafe { *buf.add(i) = b };
                i += 1;
                if b == b'\n' {
                    break;
                }
            }
            None => break,
        }
    }
    if i == 0 {
        return core::ptr::null_mut();
    }
    // SAFETY: i < n.
    unsafe { *buf.add(i) = 0 };
    buf
}
extern "win64" fn gets(buf: *mut u8) -> *mut u8 {
    let mut i = 0usize;
    loop {
        match stdin_next() {
            Some(b'\n') => break,
            Some(b) => {
                // SAFETY: gets trusts the caller's buffer, as in C.
                unsafe { *buf.add(i) = b };
                i += 1;
            }
            None if i == 0 => return core::ptr::null_mut(),
            None => break,
        }
    }
    // SAFETY: as above.
    unsafe { *buf.add(i) = 0 };
    buf
}
extern "win64" fn fseek(f: u64, off: i64, whence: i32) -> i32 {
    match stream(f) {
        Some(Stream::File(f)) => {
            let base = match whence {
                0 => 0,
                1 => f.pos as i64,
                _ => f.data.len() as i64,
            };
            let to = base + off;
            if to < 0 {
                return -1;
            }
            f.pos = to as usize;
            f.eof = false;
            0
        }
        _ => -1,
    }
}
extern "win64" fn ftell(f: u64) -> i64 {
    match stream(f) {
        Some(Stream::File(f)) => f.pos as i64,
        _ => -1,
    }
}
extern "win64" fn rewind(f: u64) {
    fseek(f, 0, 0);
}
extern "win64" fn feof(f: u64) -> i32 {
    match stream(f) {
        Some(Stream::File(f)) => f.eof as i32,
        // SAFETY: single-threaded.
        Some(Stream::In) => unsafe { STDIN_EOF as i32 },
        _ => 0,
    }
}
extern "win64" fn ferror(f: u64) -> i32 {
    match stream(f) {
        Some(Stream::File(f)) => f.err as i32,
        _ => 0,
    }
}
extern "win64" fn clearerr(f: u64) {
    if let Some(Stream::File(f)) = stream(f) {
        f.eof = false;
        f.err = false;
    }
}
extern "win64" fn remove(name: *const u8) -> i32 {
    if justrt::fs::remove(&path(&cstr(name))).is_ok() {
        0
    } else {
        -1
    }
}
extern "win64" fn rename(from: *const u8, to: *const u8) -> i32 {
    if justrt::fs::rename(&path(&cstr(from)), &path(&cstr(to))).is_ok() {
        0
    } else {
        -1
    }
}
extern "win64" fn fileno(f: u64) -> i32 {
    match stream(f) {
        Some(Stream::In) => 0,
        Some(Stream::Out) => 1,
        _ => 3,
    }
}
extern "win64" fn isatty(fd: i32) -> i32 {
    (fd <= 2) as i32
}

// ----------------------------------------------------------------------------
// Formatted output into memory, and the shims for the variadic functions
//
// Each shim stores the register arguments in the caller's shadow space, where
// the stack arguments follow them, and passes a pointer to the first variadic
// one: exactly a Windows `va_list`.

core::arch::global_asm!(
    r#"
    .global WINRUN_PRINTF
WINRUN_PRINTF:
    mov [rsp + 8], rcx
    mov [rsp + 16], rdx
    mov [rsp + 24], r8
    mov [rsp + 32], r9
    lea rdx, [rsp + 16]
    jmp {printf}

    .global WINRUN_SPRINTF
WINRUN_SPRINTF:
    mov [rsp + 8], rcx
    mov [rsp + 16], rdx
    mov [rsp + 24], r8
    mov [rsp + 32], r9
    lea r8, [rsp + 24]
    jmp {sprintf}

    .global WINRUN_SNPRINTF
WINRUN_SNPRINTF:
    mov [rsp + 8], rcx
    mov [rsp + 16], rdx
    mov [rsp + 24], r8
    mov [rsp + 32], r9
    lea r9, [rsp + 32]
    jmp {snprintf}

    .global WINRUN_SCANF
WINRUN_SCANF:
    mov [rsp + 8], rcx
    mov [rsp + 16], rdx
    mov [rsp + 24], r8
    mov [rsp + 32], r9
    lea rdx, [rsp + 16]
    jmp {scanf}

    .global WINRUN_SSCANF
WINRUN_SSCANF:
    mov [rsp + 8], rcx
    mov [rsp + 16], rdx
    mov [rsp + 24], r8
    mov [rsp + 32], r9
    lea r8, [rsp + 24]
    jmp {sscanf}

    .global WINRUN_FSCANF
WINRUN_FSCANF:
    mov [rsp + 8], rcx
    mov [rsp + 16], rdx
    mov [rsp + 24], r8
    mov [rsp + 32], r9
    lea r8, [rsp + 24]
    jmp {fscanf}
"#,
    printf = sym vprintf,
    sprintf = sym vsprintf,
    snprintf = sym vsnprintf,
    scanf = sym vscanf,
    sscanf = sym vsscanf,
    fscanf = sym vfscanf,
);

extern "C" {
    static WINRUN_PRINTF: u8;
    static WINRUN_SPRINTF: u8;
    static WINRUN_SNPRINTF: u8;
    static WINRUN_SCANF: u8;
    static WINRUN_SSCANF: u8;
    static WINRUN_FSCANF: u8;
}

extern "win64" fn vprintf(fmt: *const u8, va: *const u64) -> i32 {
    let out = format_c(fmt, va);
    console_write(&out);
    out.len() as i32
}
extern "win64" fn vsprintf(buf: *mut u8, fmt: *const u8, va: *const u64) -> i32 {
    let out = format_c(fmt, va);
    // SAFETY: as in C, the caller's buffer is big enough.
    unsafe {
        core::ptr::copy_nonoverlapping(out.as_ptr(), buf, out.len());
        *buf.add(out.len()) = 0;
    }
    out.len() as i32
}
extern "win64" fn vsnprintf(buf: *mut u8, n: u64, fmt: *const u8, va: *const u64) -> i32 {
    let out = format_c(fmt, va);
    if n > 0 && !buf.is_null() {
        let k = out.len().min(n as usize - 1);
        // SAFETY: at most n bytes, the last one the NUL.
        unsafe {
            core::ptr::copy_nonoverlapping(out.as_ptr(), buf, k);
            *buf.add(k) = 0;
        }
    }
    out.len() as i32
}

// ----------------------------------------------------------------------------
// scanf

/// Where `scanf` reads from.
enum Src {
    Str(*const u8),
    Stream(u64),
}

impl Src {
    fn peek(&mut self) -> Option<u8> {
        match self {
            // SAFETY: a NUL-terminated string.
            Src::Str(p) => Some(unsafe { **p }).filter(|&b| b != 0),
            Src::Stream(h) => match stream(*h)? {
                Stream::In => stdin_peek(),
                Stream::File(f) => f.peek(),
                Stream::Out => None,
            },
        }
    }
    fn bump(&mut self) {
        match self {
            // SAFETY: only after peek returned a byte (not the NUL).
            Src::Str(p) => *p = unsafe { p.add(1) },
            Src::Stream(h) => {
                getc(*h);
            }
        }
    }
    fn skip_space(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.bump();
        }
    }
}

extern "win64" fn vscanf(fmt: *const u8, va: *const u64) -> i32 {
    scan(&mut Src::Stream(crate::win::iob_base()), fmt, va)
}
extern "win64" fn vsscanf(s: *const u8, fmt: *const u8, va: *const u64) -> i32 {
    scan(&mut Src::Str(s), fmt, va)
}
extern "win64" fn vfscanf(f: u64, fmt: *const u8, va: *const u64) -> i32 {
    scan(&mut Src::Stream(f), fmt, va)
}

fn scan(src: &mut Src, fmt: *const u8, va: *const u64) -> i32 {
    // SAFETY: `fmt` is NUL-terminated; each argument is one pointer slot.
    let at = |i: usize| unsafe { *fmt.add(i) };
    let mut arg = 0usize;
    let mut next = || {
        // SAFETY: as above.
        let v = unsafe { *va.add(arg) };
        arg += 1;
        v
    };
    let mut i = 0usize;
    let mut count = 0i32;
    let mut read_any = false;
    loop {
        let c = at(i);
        if c == 0 {
            break;
        }
        if c.is_ascii_whitespace() {
            src.skip_space();
            i += 1;
            continue;
        }
        if c != b'%' || at(i + 1) == b'%' {
            if c == b'%' {
                i += 1;
            }
            src.skip_space();
            if src.peek() != Some(c) {
                break;
            }
            src.bump();
            i += 1;
            continue;
        }
        i += 1;
        let skip = at(i) == b'*';
        if skip {
            i += 1;
        }
        let mut width = 0usize;
        while at(i).is_ascii_digit() {
            width = width * 10 + (at(i) - b'0') as usize;
            i += 1;
        }
        let width = if width == 0 { usize::MAX } else { width };
        // Length: h, hh, l, ll, L, z, j, I64.
        let mut len = 0i32; // -2 hh, -1 h, 0 int, 1 l, 2 ll
        loop {
            match at(i) {
                b'h' => len -= 1,
                b'l' => len += 1,
                b'L' | b'q' | b'j' | b'z' => len = 2,
                b'I' if at(i + 1) == b'6' && at(i + 2) == b'4' => {
                    len = 2;
                    i += 2;
                }
                _ => break,
            }
            i += 1;
        }
        let conv = at(i);
        i += 1;
        if conv != b'c' && conv != b'[' && conv != b'n' {
            src.skip_space();
        }
        if src.peek().is_none() && conv != b'n' {
            return if read_any || count > 0 { count } else { -1 };
        }
        match conv {
            b'd' | b'i' | b'u' | b'x' | b'X' | b'o' => {
                let mut s = String::new();
                while s.len() < width {
                    match src.peek() {
                        Some(b) if b.is_ascii_hexdigit() || ((b == b'-' || b == b'+') && s.is_empty()) || (b == b'x' || b == b'X') => {
                            if !b.is_ascii_digit() && conv != b'x' && conv != b'X' && conv != b'i' && b != b'-' && b != b'+' {
                                break;
                            }
                            s.push(b as char);
                            src.bump();
                        }
                        _ => break,
                    }
                }
                let base = match conv {
                    b'x' | b'X' => 16,
                    b'o' => 8,
                    b'i' => 0,
                    _ => 10,
                };
                let (v, used) = parse_int(s.as_bytes(), base);
                if used == 0 {
                    break;
                }
                read_any = true;
                if !skip {
                    let p = next();
                    // SAFETY: the program passed a pointer of the right size.
                    unsafe {
                        match len {
                            ..=-2 => *(p as *mut i8) = v as i8,
                            -1 => *(p as *mut i16) = v as i16,
                            0 | 1 => *(p as *mut i32) = v as i32,
                            _ => *(p as *mut i64) = v,
                        }
                    }
                    count += 1;
                }
            }
            b'f' | b'e' | b'g' | b'E' | b'G' | b'a' => {
                let mut s = String::new();
                while s.len() < width {
                    match src.peek() {
                        Some(b) if b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E') => {
                            s.push(b as char);
                            src.bump();
                        }
                        _ => break,
                    }
                }
                let (v, used) = parse_float(s.as_bytes());
                if used == 0 {
                    break;
                }
                read_any = true;
                if !skip {
                    let p = next();
                    // SAFETY: float* or double* as the length says.
                    unsafe {
                        if len >= 1 {
                            *(p as *mut f64) = v;
                        } else {
                            *(p as *mut f32) = v as f32;
                        }
                    }
                    count += 1;
                }
            }
            b's' => {
                let p = if skip { 0 } else { next() } as *mut u8;
                let mut n = 0usize;
                while n < width {
                    match src.peek() {
                        Some(b) if !b.is_ascii_whitespace() => {
                            if !p.is_null() {
                                // SAFETY: the program's buffer, as in C.
                                unsafe { *p.add(n) = b };
                            }
                            n += 1;
                            src.bump();
                        }
                        _ => break,
                    }
                }
                if !p.is_null() {
                    // SAFETY: as above.
                    unsafe { *p.add(n) = 0 };
                    count += 1;
                }
                read_any = true;
            }
            b'c' => {
                let n = if width == usize::MAX { 1 } else { width };
                let p = if skip { 0 } else { next() } as *mut u8;
                for k in 0..n {
                    let Some(b) = src.peek() else { break };
                    if !p.is_null() {
                        // SAFETY: room for `n` characters.
                        unsafe { *p.add(k) = b };
                    }
                    src.bump();
                }
                if !p.is_null() {
                    count += 1;
                }
                read_any = true;
            }
            b'[' => {
                let neg = at(i) == b'^';
                if neg {
                    i += 1;
                }
                let start = i;
                if at(i) == b']' {
                    i += 1;
                }
                while at(i) != 0 && at(i) != b']' {
                    i += 1;
                }
                // SAFETY: the set lies inside the format string.
                let set: Vec<u8> = (start..i).map(|k| unsafe { *fmt.add(k) }).collect();
                if at(i) == b']' {
                    i += 1;
                }
                let p = if skip { 0 } else { next() } as *mut u8;
                let mut n = 0usize;
                while n < width {
                    match src.peek() {
                        Some(b) if in_set(&set, b) != neg => {
                            if !p.is_null() {
                                // SAFETY: the program's buffer.
                                unsafe { *p.add(n) = b };
                            }
                            n += 1;
                            src.bump();
                        }
                        _ => break,
                    }
                }
                if n == 0 {
                    break;
                }
                if !p.is_null() {
                    // SAFETY: as above.
                    unsafe { *p.add(n) = 0 };
                    count += 1;
                }
                read_any = true;
            }
            b'n' => {
                // Not counted; the position is not tracked for streams.
                let _ = next();
            }
            _ => break,
        }
    }
    count
}

fn in_set(set: &[u8], b: u8) -> bool {
    let mut k = 0;
    while k < set.len() {
        if k + 2 < set.len() && set[k + 1] == b'-' {
            if (set[k]..=set[k + 2]).contains(&b) {
                return true;
            }
            k += 3;
        } else {
            if set[k] == b {
                return true;
            }
            k += 1;
        }
    }
    false
}

// ----------------------------------------------------------------------------
// Numbers from text

/// Parses an integer the way `strtol` does; returns it and the bytes used
/// (0: no number). `base` 0 picks it from the prefix.
fn parse_int(s: &[u8], mut base: u32) -> (i64, usize) {
    let mut i = 0;
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'-' || s[i] == b'+') {
        neg = s[i] == b'-';
        i += 1;
    }
    let hex_prefix = i + 1 < s.len() && s[i] == b'0' && (s[i + 1] | 0x20) == b'x';
    if (base == 0 || base == 16) && hex_prefix && s.get(i + 2).is_some_and(|b| b.is_ascii_hexdigit()) {
        base = 16;
        i += 2;
    } else if base == 0 {
        base = if i < s.len() && s[i] == b'0' { 8 } else { 10 };
    }
    let start = i;
    let mut v: u64 = 0;
    while i < s.len() {
        let d = match s[i] {
            c @ b'0'..=b'9' => (c - b'0') as u32,
            c @ b'a'..=b'z' => (c - b'a') as u32 + 10,
            c @ b'A'..=b'Z' => (c - b'A') as u32 + 10,
            _ => break,
        };
        if d >= base {
            break;
        }
        v = v.wrapping_mul(base as u64).wrapping_add(d as u64);
        i += 1;
    }
    if i == start {
        return (0, 0);
    }
    (if neg { (v as i64).wrapping_neg() } else { v as i64 }, i)
}

fn parse_float(s: &[u8]) -> (f64, usize) {
    let mut i = 0;
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'-' || s[i] == b'+') {
        neg = s[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut mant: f64 = 0.0;
    let mut digits = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        mant = mant * 10.0 + (s[i] - b'0') as f64;
        i += 1;
        digits += 1;
    }
    let mut scale = 0i32;
    if i < s.len() && s[i] == b'.' {
        i += 1;
        while i < s.len() && s[i].is_ascii_digit() {
            mant = mant * 10.0 + (s[i] - b'0') as f64;
            scale -= 1;
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        // "inf" and "nan".
        let rest = &s[start..];
        if rest.len() >= 3 && rest[..3].eq_ignore_ascii_case(b"inf") {
            return (if neg { f64::NEG_INFINITY } else { f64::INFINITY }, start + 3);
        }
        if rest.len() >= 3 && rest[..3].eq_ignore_ascii_case(b"nan") {
            return (f64::NAN, start + 3);
        }
        return (0.0, 0);
    }
    if i < s.len() && (s[i] | 0x20) == b'e' {
        let (e, used) = parse_int(&s[i + 1..], 10);
        if used > 0 {
            scale += e as i32;
            i += 1 + used;
        }
    }
    let v = if scale >= 0 { mant * m::powi(10.0, scale) } else { mant / m::powi(10.0, -scale) };
    (if neg { -v } else { v }, i)
}

/// The bytes of a C string, without its NUL.
fn bytes<'a>(p: *const u8) -> &'a [u8] {
    // SAFETY: NUL-terminated.
    unsafe { core::slice::from_raw_parts(p, c_len(p)) }
}

extern "win64" fn atoi(s: *const u8) -> i32 {
    parse_int(bytes(s), 10).0 as i32
}
extern "win64" fn atoll(s: *const u8) -> i64 {
    parse_int(bytes(s), 10).0
}
fn strto(s: *const u8, end: *mut *const u8, base: i32) -> i64 {
    let (v, used) = parse_int(bytes(s), base as u32);
    if !end.is_null() {
        // SAFETY: a place for the end pointer.
        unsafe { *end = s.add(used) };
    }
    v
}
extern "win64" fn strtol(s: *const u8, end: *mut *const u8, base: i32) -> i32 {
    strto(s, end, base).clamp(i32::MIN as i64, i32::MAX as i64) as i32
}
extern "win64" fn strtoul(s: *const u8, end: *mut *const u8, base: i32) -> u32 {
    strto(s, end, base) as u32
}
extern "win64" fn strtoll(s: *const u8, end: *mut *const u8, base: i32) -> i64 {
    strto(s, end, base)
}
extern "win64" fn strtoull(s: *const u8, end: *mut *const u8, base: i32) -> u64 {
    strto(s, end, base) as u64
}
extern "win64" fn atof(s: *const u8) -> f64 {
    parse_float(bytes(s)).0
}
extern "win64" fn strtod(s: *const u8, end: *mut *const u8) -> f64 {
    let (v, used) = parse_float(bytes(s));
    if !end.is_null() {
        // SAFETY: a place for the end pointer.
        unsafe { *end = s.add(used) };
    }
    v
}
extern "win64" fn strtof(s: *const u8, end: *mut *const u8) -> f32 {
    strtod(s, end) as f32
}
extern "win64" fn abs(v: i32) -> i32 {
    v.wrapping_abs()
}
extern "win64" fn llabs(v: i64) -> i64 {
    v.wrapping_abs()
}

// ----------------------------------------------------------------------------
// Sorting and random numbers

type Cmp = extern "win64" fn(*const c_void, *const c_void) -> i32;

extern "win64" fn qsort(base: *mut u8, n: u64, size: u64, cmp: Cmp) {
    let (n, size) = (n as usize, size as usize);
    if n < 2 || size == 0 {
        return;
    }
    // Sort an index with the program's comparison, then lay the elements out
    // in that order.
    let mut idx: Vec<usize> = (0..n).collect();
    // SAFETY: elements i and j are inside the array.
    idx.sort_by(|&i, &j| cmp(unsafe { base.add(i * size) } as *const c_void, unsafe { base.add(j * size) } as *const c_void).cmp(&0));
    // SAFETY: the array holds n * size bytes.
    let copy: Vec<u8> = unsafe { core::slice::from_raw_parts(base, n * size) }.to_vec();
    for (k, &i) in idx.iter().enumerate() {
        // SAFETY: as above.
        unsafe { core::ptr::copy_nonoverlapping(copy.as_ptr().add(i * size), base.add(k * size), size) };
    }
}
extern "win64" fn bsearch(key: *const c_void, base: *const u8, n: u64, size: u64, cmp: Cmp) -> *const u8 {
    let (mut lo, mut hi) = (0u64, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        // SAFETY: mid < n.
        let p = unsafe { base.add((mid * size) as usize) };
        match cmp(key, p as *const c_void) {
            0 => return p,
            r if r < 0 => hi = mid,
            _ => lo = mid + 1,
        }
    }
    core::ptr::null()
}

static mut SEED: u32 = 1;

extern "win64" fn srand(seed: u32) {
    // SAFETY: single-threaded.
    unsafe { SEED = seed };
}
extern "win64" fn rand() -> i32 {
    // The Microsoft C runtime's generator (RAND_MAX = 32767).
    // SAFETY: single-threaded.
    unsafe {
        SEED = SEED.wrapping_mul(214_013).wrapping_add(2_531_011);
        ((SEED >> 16) & 0x7FFF) as i32
    }
}
extern "win64" fn system(_cmd: *const u8) -> i32 {
    -1
}

// ----------------------------------------------------------------------------
// Time

/// Seconds since 1970 (JustOS counts from 2000).
fn unix_now() -> i64 {
    justrt::time::now() as i64 + 946_684_800
}

extern "win64" fn time(out: *mut i64) -> i64 {
    let t = unix_now();
    if !out.is_null() {
        // SAFETY: a place for the time.
        unsafe { *out = t };
    }
    t
}
extern "win64" fn clock() -> i32 {
    justrt::time::uptime_ms() as i32 // CLOCKS_PER_SEC = 1000
}
extern "win64" fn difftime(a: i64, b: i64) -> f64 {
    (a - b) as f64
}

static mut TM: [i32; 9] = [0; 9];

extern "win64" fn localtime(t: *const i64) -> *mut i32 {
    // SAFETY: a pointer to a time_t.
    let secs = if t.is_null() { unix_now() } else { unsafe { *t } };
    let d = justrt::time::date((secs - 946_684_800).max(0) as u64);
    let mut yday = d.day as i32 - 1;
    for mo in 1..d.month {
        yday += justrt::time::days_in_month(d.year, mo) as i32;
    }
    // SAFETY: single-threaded; one static struct tm, as in C.
    unsafe {
        TM = [d.sec as i32, d.min as i32, d.hour as i32, d.day as i32, d.month as i32 - 1, d.year as i32 - 1900, (d.weekday as i32 + 1) % 7, yday, 0];
        core::ptr::addr_of_mut!(TM) as *mut i32
    }
}
extern "win64" fn mktime(tm: *const i32) -> i64 {
    // SAFETY: a struct tm of 9 ints.
    let tm = unsafe { core::slice::from_raw_parts(tm, 9) };
    let year = (tm[5] + 1900) as i64;
    let mut days = 0i64;
    for y in 1970..year {
        days += if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 366 } else { 365 };
    }
    for mo in 1..=tm[4] as u64 {
        days += justrt::time::days_in_month(year as u64, mo) as i64;
    }
    days += tm[3] as i64 - 1;
    days * 86400 + tm[2] as i64 * 3600 + tm[1] as i64 * 60 + tm[0] as i64
}

// ----------------------------------------------------------------------------
// Maths

extern "win64" fn sqrt(x: f64) -> f64 {
    m::sqrt(x)
}
extern "win64" fn sin(x: f64) -> f64 {
    m::sin(x)
}
extern "win64" fn cos(x: f64) -> f64 {
    m::cos(x)
}
extern "win64" fn tan(x: f64) -> f64 {
    m::tan(x)
}
extern "win64" fn asin(x: f64) -> f64 {
    m::asin(x)
}
extern "win64" fn acos(x: f64) -> f64 {
    m::acos(x)
}
extern "win64" fn atan(x: f64) -> f64 {
    m::atan(x)
}
extern "win64" fn atan2(y: f64, x: f64) -> f64 {
    m::atan2(y, x)
}
extern "win64" fn exp(x: f64) -> f64 {
    m::exp(x)
}
extern "win64" fn log(x: f64) -> f64 {
    m::ln(x)
}
extern "win64" fn log10(x: f64) -> f64 {
    m::log10(x)
}
extern "win64" fn log2(x: f64) -> f64 {
    m::log2(x)
}
extern "win64" fn pow(x: f64, y: f64) -> f64 {
    m::powf(x, y)
}
extern "win64" fn floor(x: f64) -> f64 {
    m::floor(x)
}
extern "win64" fn ceil(x: f64) -> f64 {
    m::ceil(x)
}
extern "win64" fn round(x: f64) -> f64 {
    m::round(x)
}
extern "win64" fn trunc(x: f64) -> f64 {
    m::trunc(x)
}
extern "win64" fn fabs(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & !(1 << 63))
}
extern "win64" fn fmod(x: f64, y: f64) -> f64 {
    if y == 0.0 {
        return f64::NAN;
    }
    x - m::trunc(x / y) * y
}
extern "win64" fn hypot(x: f64, y: f64) -> f64 {
    m::hypot(x, y)
}
extern "win64" fn ldexp(x: f64, n: i32) -> f64 {
    m::ldexp(x, n)
}
extern "win64" fn sqrtf(x: f32) -> f32 {
    m::sqrt(x as f64) as f32
}
extern "win64" fn sinf(x: f32) -> f32 {
    m::sin(x as f64) as f32
}
extern "win64" fn cosf(x: f32) -> f32 {
    m::cos(x as f64) as f32
}
extern "win64" fn fabsf(x: f32) -> f32 {
    f32::from_bits(x.to_bits() & !(1 << 31))
}
extern "win64" fn floorf(x: f32) -> f32 {
    m::floor(x as f64) as f32
}
extern "win64" fn atan2f(y: f32, x: f32) -> f32 {
    m::atan2(y as f64, x as f64) as f32
}
extern "win64" fn powf(x: f32, y: f32) -> f32 {
    m::powf(x as f64, y as f64) as f32
}

// ----------------------------------------------------------------------------
// Strings

extern "win64" fn strcpy(dst: *mut u8, src: *const u8) -> *mut u8 {
    // SAFETY: as in C, dst has room for src and its NUL.
    unsafe { core::ptr::copy(src, dst, c_len(src) + 1) };
    dst
}
extern "win64" fn strncpy(dst: *mut u8, src: *const u8, n: u64) -> *mut u8 {
    let mut i = 0usize;
    // SAFETY: at most n bytes of dst; src read up to its NUL.
    unsafe {
        while i < n as usize && *src.add(i) != 0 {
            *dst.add(i) = *src.add(i);
            i += 1;
        }
        while i < n as usize {
            *dst.add(i) = 0;
            i += 1;
        }
    }
    dst
}
extern "win64" fn strcat(dst: *mut u8, src: *const u8) -> *mut u8 {
    // SAFETY: as in C.
    unsafe { strcpy(dst.add(c_len(dst)), src) };
    dst
}
extern "win64" fn strncat(dst: *mut u8, src: *const u8, n: u64) -> *mut u8 {
    // SAFETY: as in C: dst has room for n more bytes and a NUL.
    unsafe {
        let d = dst.add(c_len(dst));
        let k = c_len(src).min(n as usize);
        core::ptr::copy(src, d, k);
        *d.add(k) = 0;
    }
    dst
}
extern "win64" fn strchr(s: *const u8, c: i32) -> *const u8 {
    let mut p = s;
    // SAFETY: NUL-terminated; the NUL itself can be searched for.
    unsafe {
        loop {
            if *p == c as u8 {
                return p;
            }
            if *p == 0 {
                return core::ptr::null();
            }
            p = p.add(1);
        }
    }
}
extern "win64" fn strrchr(s: *const u8, c: i32) -> *const u8 {
    let b = bytes(s);
    if c as u8 == 0 {
        // SAFETY: the NUL ends the string.
        return unsafe { s.add(b.len()) };
    }
    // SAFETY: inside the string.
    b.iter().rposition(|&x| x == c as u8).map_or(core::ptr::null(), |i| unsafe { s.add(i) })
}
extern "win64" fn strstr(hay: *const u8, needle: *const u8) -> *const u8 {
    let (h, n) = (bytes(hay), bytes(needle));
    if n.is_empty() {
        return hay;
    }
    // SAFETY: inside the string.
    h.windows(n.len()).position(|w| w == n).map_or(core::ptr::null(), |i| unsafe { hay.add(i) })
}
extern "win64" fn strdup(s: *const u8) -> *mut u8 {
    let n = c_len_safe(s);
    let p = alloc_bytes(n + 1) as *mut u8;
    if !p.is_null() {
        // SAFETY: room for n bytes and the NUL.
        unsafe { core::ptr::copy_nonoverlapping(s, p, n + 1) };
    }
    p
}
fn c_len_safe(s: *const u8) -> usize {
    // SAFETY: NUL-terminated.
    unsafe { c_len(s) }
}

static mut TOK: *mut u8 = core::ptr::null_mut();

extern "win64" fn strtok(s: *mut u8, delim: *const u8) -> *mut u8 {
    let d = bytes(delim);
    // SAFETY: single-threaded; the string is the program's and NUL-terminated.
    unsafe {
        let mut p = if s.is_null() { TOK } else { s };
        if p.is_null() {
            return p;
        }
        while *p != 0 && d.contains(&*p) {
            p = p.add(1);
        }
        if *p == 0 {
            TOK = core::ptr::null_mut();
            return core::ptr::null_mut();
        }
        let start = p;
        while *p != 0 && !d.contains(&*p) {
            p = p.add(1);
        }
        if *p == 0 {
            TOK = core::ptr::null_mut();
        } else {
            *p = 0;
            TOK = p.add(1);
        }
        start
    }
}
extern "win64" fn strspn(s: *const u8, set: *const u8) -> u64 {
    let set = bytes(set);
    bytes(s).iter().take_while(|b| set.contains(b)).count() as u64
}
extern "win64" fn strcspn(s: *const u8, set: *const u8) -> u64 {
    let set = bytes(set);
    bytes(s).iter().take_while(|b| !set.contains(b)).count() as u64
}
extern "win64" fn strpbrk(s: *const u8, set: *const u8) -> *const u8 {
    let set = bytes(set);
    // SAFETY: inside the string.
    bytes(s).iter().position(|b| set.contains(b)).map_or(core::ptr::null(), |i| unsafe { s.add(i) })
}
extern "win64" fn memchr(s: *const u8, c: i32, n: u64) -> *const u8 {
    // SAFETY: n bytes are readable.
    let b = unsafe { core::slice::from_raw_parts(s, n as usize) };
    // SAFETY: inside the block.
    b.iter().position(|&x| x == c as u8).map_or(core::ptr::null(), |i| unsafe { s.add(i) })
}
fn icmp(a: &[u8], b: &[u8], n: usize) -> i32 {
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0).to_ascii_lowercase();
        let y = b.get(i).copied().unwrap_or(0).to_ascii_lowercase();
        if x != y {
            return x as i32 - y as i32;
        }
        if x == 0 {
            break;
        }
    }
    0
}
extern "win64" fn stricmp(a: *const u8, b: *const u8) -> i32 {
    icmp(bytes(a), bytes(b), usize::MAX)
}
extern "win64" fn strnicmp(a: *const u8, b: *const u8, n: u64) -> i32 {
    icmp(bytes(a), bytes(b), n as usize)
}
extern "win64" fn lstrcmpA(a: *const u8, b: *const u8) -> i32 {
    bytes(a).cmp(bytes(b)) as i32
}
extern "win64" fn lstrlenA(s: *const u8) -> i32 {
    if s.is_null() {
        0
    } else {
        bytes(s).len() as i32
    }
}
extern "win64" fn lstrlenW(s: *const u16) -> i32 {
    let mut n = 0;
    // SAFETY: NUL-terminated.
    while !s.is_null() && unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    n as i32
}
extern "win64" fn strlwr(s: *mut u8) -> *mut u8 {
    // SAFETY: the program's own NUL-terminated string.
    unsafe { core::slice::from_raw_parts_mut(s, c_len(s)) }.make_ascii_lowercase();
    s
}
extern "win64" fn strupr(s: *mut u8) -> *mut u8 {
    // SAFETY: as above.
    unsafe { core::slice::from_raw_parts_mut(s, c_len(s)) }.make_ascii_uppercase();
    s
}
extern "win64" fn wcscpy(dst: *mut u16, src: *const u16) -> *mut u16 {
    let n = lstrlenW(src) as usize;
    // SAFETY: as in C.
    unsafe { core::ptr::copy(src, dst, n + 1) };
    dst
}
extern "win64" fn wcscmp(a: *const u16, b: *const u16) -> i32 {
    let mut i = 0;
    // SAFETY: both NUL-terminated.
    unsafe {
        loop {
            let (x, y) = (*a.add(i), *b.add(i));
            if x != y || x == 0 {
                return x as i32 - y as i32;
            }
            i += 1;
        }
    }
}

extern "win64" fn toupper(c: i32) -> i32 {
    (c as u8).to_ascii_uppercase() as i32 | (c & !0xFF)
}
extern "win64" fn tolower(c: i32) -> i32 {
    (c as u8).to_ascii_lowercase() as i32 | (c & !0xFF)
}
fn class(c: i32, f: fn(&u8) -> bool) -> i32 {
    ((0..=255).contains(&c) && f(&(c as u8))) as i32
}
extern "win64" fn isalpha(c: i32) -> i32 {
    class(c, u8::is_ascii_alphabetic)
}
extern "win64" fn isdigit(c: i32) -> i32 {
    class(c, u8::is_ascii_digit)
}
extern "win64" fn isalnum(c: i32) -> i32 {
    class(c, u8::is_ascii_alphanumeric)
}
extern "win64" fn isspace(c: i32) -> i32 {
    class(c, |b| b.is_ascii_whitespace() || *b == 0x0B)
}
extern "win64" fn isupper(c: i32) -> i32 {
    class(c, u8::is_ascii_uppercase)
}
extern "win64" fn islower(c: i32) -> i32 {
    class(c, u8::is_ascii_lowercase)
}
extern "win64" fn isxdigit(c: i32) -> i32 {
    class(c, u8::is_ascii_hexdigit)
}
extern "win64" fn ispunct(c: i32) -> i32 {
    class(c, u8::is_ascii_punctuation)
}
extern "win64" fn isprint(c: i32) -> i32 {
    class(c, |b| (0x20..0x7F).contains(b))
}
extern "win64" fn iscntrl(c: i32) -> i32 {
    class(c, u8::is_ascii_control)
}

// ----------------------------------------------------------------------------
// kernel32: files, folders and the console

const INVALID: u64 = u64::MAX;

fn create_file(name: &str, access: u32, disposition: u32) -> u64 {
    let read = access & 0x8000_0000 != 0 || access & 1 != 0;
    let write = access & 0x4000_0000 != 0 || access & 2 != 0;
    let exists = justrt::fs::read(&path(name)).is_ok();
    let (truncate, create) = match disposition {
        1 if exists => {
            crate::win::set_last_error(80); // ERROR_FILE_EXISTS
            return INVALID;
        }
        1 | 2 => (true, true),
        3 => (false, false),
        4 => (false, true),
        5 => (true, false),
        _ => (false, false),
    };
    match CFile::open(name, read, write, truncate, create, false) {
        Some(f) => f as u64,
        None => {
            crate::win::set_last_error(2); // ERROR_FILE_NOT_FOUND
            INVALID
        }
    }
}

extern "win64" fn CreateFileA(name: *const u8, access: u32, _share: u32, _sec: u64, disposition: u32, _flags: u32, _template: u64) -> u64 {
    create_file(&cstr(name), access, disposition)
}
extern "win64" fn CreateFileW(name: *const u16, access: u32, _share: u32, _sec: u64, disposition: u32, _flags: u32, _template: u64) -> u64 {
    create_file(&wstr(name), access, disposition)
}
extern "win64" fn ReadFile(h: u64, buf: *mut u8, n: u32, got: *mut u32, _ovl: u64) -> u32 {
    // SAFETY: the program's buffer of n bytes.
    let out = unsafe { core::slice::from_raw_parts_mut(buf, n as usize) };
    let k = match stream(h) {
        Some(Stream::File(f)) => f.read(out),
        Some(Stream::In) => {
            let mut k = 0;
            while k < out.len() {
                match stdin_next() {
                    Some(b) => {
                        out[k] = b;
                        k += 1;
                        if b == b'\n' {
                            break;
                        }
                    }
                    None => break,
                }
            }
            k
        }
        _ => return 0,
    };
    if !got.is_null() {
        // SAFETY: a place for the count.
        unsafe { *got = k as u32 };
    }
    1
}
extern "win64" fn CloseHandle(h: u64) -> u32 {
    close(h) as u32
}
extern "win64" fn GetFileSize(h: u64, high: *mut u32) -> u32 {
    match stream(h) {
        Some(Stream::File(f)) => {
            if !high.is_null() {
                // SAFETY: a place for the high half.
                unsafe { *high = 0 };
            }
            f.data.len() as u32
        }
        _ => u32::MAX,
    }
}
extern "win64" fn GetFileSizeEx(h: u64, out: *mut u64) -> u32 {
    match stream(h) {
        Some(Stream::File(f)) => {
            // SAFETY: a LARGE_INTEGER.
            unsafe { *out = f.data.len() as u64 };
            1
        }
        _ => 0,
    }
}
fn seek(h: u64, dist: i64, method: u32) -> Option<u64> {
    match stream(h)? {
        Stream::File(f) => {
            let base = match method {
                0 => 0,
                1 => f.pos as i64,
                _ => f.data.len() as i64,
            };
            let to = base.checked_add(dist).filter(|&v| v >= 0)?;
            f.pos = to as usize;
            Some(to as u64)
        }
        _ => None,
    }
}
extern "win64" fn SetFilePointer(h: u64, dist: i32, high: *mut i32, method: u32) -> u32 {
    // SAFETY: the optional high half of the distance.
    let d = if high.is_null() { dist as i64 } else { (unsafe { *high } as i64) << 32 | dist as u32 as i64 };
    match seek(h, d, method) {
        Some(p) => {
            if !high.is_null() {
                // SAFETY: as above.
                unsafe { *high = (p >> 32) as i32 };
            }
            p as u32
        }
        None => u32::MAX,
    }
}
extern "win64" fn SetFilePointerEx(h: u64, dist: i64, out: *mut u64, method: u32) -> u32 {
    match seek(h, dist, method) {
        Some(p) => {
            if !out.is_null() {
                // SAFETY: a LARGE_INTEGER.
                unsafe { *out = p };
            }
            1
        }
        None => 0,
    }
}
extern "win64" fn SetEndOfFile(h: u64) -> u32 {
    match stream(h) {
        Some(Stream::File(f)) => {
            f.data.truncate(f.pos);
            f.dirty = true;
            1
        }
        _ => 0,
    }
}
extern "win64" fn DeleteFileA(name: *const u8) -> u32 {
    justrt::fs::remove(&path(&cstr(name))).is_ok() as u32
}
extern "win64" fn DeleteFileW(name: *const u16) -> u32 {
    justrt::fs::remove(&path(&wstr(name))).is_ok() as u32
}
fn attributes(name: &str) -> u32 {
    let p = path(name);
    if justrt::fs::list_dir(&p).is_ok() {
        0x10 // FILE_ATTRIBUTE_DIRECTORY
    } else if justrt::fs::read(&p).is_ok() {
        0x80 // FILE_ATTRIBUTE_NORMAL
    } else {
        u32::MAX // INVALID_FILE_ATTRIBUTES
    }
}
extern "win64" fn GetFileAttributesA(name: *const u8) -> u32 {
    attributes(&cstr(name))
}
extern "win64" fn GetFileAttributesW(name: *const u16) -> u32 {
    attributes(&wstr(name))
}
extern "win64" fn CreateDirectoryA(name: *const u8, _sec: u64) -> u32 {
    justrt::fs::make_dir(&path(&cstr(name))).is_ok() as u32
}
extern "win64" fn CreateDirectoryW(name: *const u16, _sec: u64) -> u32 {
    justrt::fs::make_dir(&path(&wstr(name))).is_ok() as u32
}
extern "win64" fn GetCurrentDirectoryA(n: u32, buf: *mut u8) -> u32 {
    let dir = justrt::fs::current_dir();
    if (n as usize) < dir.len() + 1 || buf.is_null() {
        return dir.len() as u32 + 1;
    }
    // SAFETY: room for the path and its NUL.
    unsafe {
        core::ptr::copy_nonoverlapping(dir.as_ptr(), buf, dir.len());
        *buf.add(dir.len()) = 0;
    }
    dir.len() as u32
}
extern "win64" fn SetCurrentDirectoryA(name: *const u8) -> u32 {
    justrt::fs::set_current_dir(&path(&cstr(name))).is_ok() as u32
}
extern "win64" fn ReadConsoleA(_h: u64, buf: *mut u8, n: u32, got: *mut u32, _ctl: u64) -> u32 {
    ReadFile(3, buf, n, got, 0)
}
extern "win64" fn ReadConsoleW(_h: u64, buf: *mut u16, n: u32, got: *mut u32, _ctl: u64) -> u32 {
    let mut k = 0usize;
    while k < n as usize {
        match stdin_next() {
            Some(b) => {
                // SAFETY: n UTF-16 units fit in the buffer.
                unsafe { *buf.add(k) = b as u16 };
                k += 1;
                if b == b'\n' {
                    break;
                }
            }
            None => break,
        }
    }
    if !got.is_null() {
        // SAFETY: a place for the count.
        unsafe { *got = k as u32 };
    }
    1
}
extern "win64" fn WriteConsoleW(_h: u64, buf: *const u16, n: u32, written: *mut u32, _r: u64) -> u32 {
    // SAFETY: n UTF-16 units.
    let s = String::from_utf16_lossy(unsafe { core::slice::from_raw_parts(buf, n as usize) });
    console_write(s.as_bytes());
    if !written.is_null() {
        // SAFETY: a place for the count.
        unsafe { *written = n };
    }
    1
}
extern "win64" fn GetLocalTime(out: *mut u16) {
    let d = justrt::time::date(justrt::time::now());
    let ms = (justrt::time::uptime_ms() % 1000) as u16;
    let v = [d.year as u16, d.month as u16, ((d.weekday + 1) % 7) as u16, d.day as u16, d.hour as u16, d.min as u16, d.sec as u16, ms];
    // SAFETY: a SYSTEMTIME of eight WORDs.
    unsafe { core::ptr::copy_nonoverlapping(v.as_ptr(), out, 8) };
}
extern "win64" fn GetStartupInfo(out: *mut u32) {
    // SAFETY: a STARTUPINFO (104 bytes); cb first, everything else zero.
    unsafe {
        core::ptr::write_bytes(out as *mut u8, 0, 104);
        *out = 104;
    }
}
