//! The justOS system call interface for user programs.
//!
//! `int 0x80` with the call number in `rax` and up to five arguments in
//! `rdi`, `rsi`, `rdx`, `r10`, `r8`. The result comes back in `rax`:
//! non-negative on success, a negative [`Error`] code otherwise.
//!
//! Files are reached through **handles** that the kernel hands out; paths
//! are justOS paths (`sys:/home/notes.txt`, or relative to the program's
//! current folder). Handle 0 is the console.

pub const EXIT: u64 = 0;
/// (handle, ptr, len) -> bytes written
pub const WRITE: u64 = 1;
/// (timeout_ms) -> key code, or 0 on timeout. `u64::MAX` waits forever.
pub const READ_KEY: u64 = 2;
/// (path, len, mode) -> handle
pub const OPEN: u64 = 3;
/// (handle, ptr, len) -> bytes read
pub const READ: u64 = 4;
/// (handle)
pub const CLOSE: u64 = 5;
/// (handle, position)
pub const SEEK: u64 = 6;
/// (handle) -> size in bytes
pub const FILE_SIZE: u64 = 7;
/// (path, len, buf, cap) -> bytes: "name\tkind\tsize\n" per entry (kind d/f)
pub const LIST_DIR: u64 = 8;
/// (path, len)
pub const REMOVE: u64 = 9;
/// (path, len)
pub const MAKE_DIR: u64 = 10;
/// () -> milliseconds since start
pub const UPTIME: u64 = 11;
/// (ms)
pub const SLEEP: u64 = 12;
/// () -> seconds since 2000-01-01
pub const NOW: u64 = 13;
/// (path, len, args, len) -> process id
pub const SPAWN: u64 = 14;
/// (pid) -> exit code
pub const WAIT: u64 = 15;
pub const YIELD: u64 = 16;
/// (bytes) -> address of fresh zeroed memory
pub const MAP: u64 = 17;
/// (ptr to [u64; SYSINFO_WORDS]) -> number of words written
pub const SYSINFO: u64 = 18;
/// () -> columns << 32 | rows of the console
pub const SCREEN: u64 = 19;
/// (from, len, to, len)
pub const RENAME: u64 = 20;
/// (buf, cap) -> length of the current folder path
pub const GET_CWD: u64 = 21;
/// (path, len)
pub const SET_CWD: u64 = 22;
/// (TLS_FS or TLS_GS, base): sets the base of the FS or GS segment of the
/// calling thread (thread-local storage; the Windows layer keeps its thread
/// block at GS).
pub const SET_TLS: u64 = 23;
/// (address, bytes) -> address: fresh zeroed memory at a fixed, page-aligned
/// address; `Exists` if part of the range is already in use.
pub const MAP_AT: u64 = 24;
/// (address, bytes): gives back memory from MAP or MAP_AT.
pub const UNMAP: u64 = 25;

/// (title, len, width << 32 | height, flags) -> window handle: a window on
/// the desktop with a content area of that size, in units (`WIN_SCALE`
/// pixels each).
pub const WIN_CREATE: u64 = 26;
/// (handle, pixels, width << 32 | height, scale): shows a frame, `width *
/// height` pixels of 0xAARRGGBB (little-endian words) at `scale` pixels per
/// unit (0 or 1: one each; a frame of fewer pixels per unit than the window
/// has is stretched); transparent parts show the window's glass.
pub const WIN_PRESENT: u64 = 27;
/// (handle, ptr to [u64; 4], timeout_ms) -> 1 and the event, or 0 on timeout.
pub const WIN_EVENT: u64 = 28;
/// (handle)
pub const WIN_CLOSE: u64 = 29;
/// (handle, title, len)
pub const WIN_TITLE: u64 = 30;

// Ports: programs (and the system's services) send each other messages of up
// to `PORT_MSG_MAX` bytes. A port belongs to the program that created it,
// which alone receives from it; anyone holding a handle to it may send.

/// (name, len) -> port handle. A name (`just.notify`) makes the port findable
/// with `PORT_LOOKUP`; an empty one makes a private port. `Exists` if taken.
pub const PORT_CREATE: u64 = 31;
/// (name, len) -> handle for sending to a named port.
pub const PORT_LOOKUP: u64 = 32;
/// (handle, ptr, len): queues a message. `Full` if the port has
/// `PORT_QUEUE_MAX` waiting, `Closed` if its owner is gone.
pub const PORT_SEND: u64 = 33;
/// (handle, buf, cap, timeout_ms, ptr to [u64; 3]) -> bytes copied: waits for
/// a message on an own port (`u64::MAX`: forever; `Timeout` otherwise). The
/// words: the message's full length, the sender's process id (as the kernel
/// knows it; 0 for the kernel) and a handle to send the reply to (0 if none).
/// `Closed` when an unnamed port has nothing queued and nobody left to send.
pub const PORT_RECV: u64 = 34;
/// (handle, request, len, buf, cap) -> reply length: sends a request with a
/// private reply port and waits for the answer. `Closed` if the other side
/// ends or drops the request without answering.
pub const PORT_CALL: u64 = 35;
/// (pid, buf, cap) -> length of the name of a process (for the sender of a
/// message).
pub const PROC_NAME: u64 = 36;

// Apps: an installed justOS app runs as itself (its `app_id`, such as
// `dev.justos.disegno`) in a sandbox: it reads and writes only its own folder,
// `sys:/home/.apps/<app_id>`; other folders need the user's permission, and
// single files come through the system's file picker.

/// (buf, cap) -> length of the calling program's app id. `NotFound` if it
/// runs without one (a system program).
pub const APP_ID: u64 = 37;
/// (id, len, name, len): from now on the program runs as app `id` (shown
/// as `name`), in its sandbox. One way: an app cannot become another
/// (`Denied`). For runners such as `just` before they start a package.
pub const APP_ENTER: u64 = 38;
/// (mode, name, len, buf, cap) -> length of the path chosen: the system's
/// file picker, outside the app. `PICK_OPEN` a file to open, `PICK_SAVE` a
/// place to save (`name` is the suggestion). The app may then use that
/// file, and only that. `Cancelled` if the user closed it.
pub const PICK_FILE: u64 = 39;
pub const PICK_OPEN: u64 = 0;
pub const PICK_SAVE: u64 = 1;
/// (handle, buf, cap) -> length: the files of the last drop on the window
/// (`WIN_EV_DROP`), absolute paths one per line. The program may read them
/// (only read: to change one it asks with `PICK_FILE`).
pub const WIN_DROPPED: u64 = 40;
/// (handle) -> pixels per unit the window is shown with: 1, or 2 on a
/// high-density screen (where a frame drawn with twice the pixels each way
/// is sharp). `WIN_EV_RESIZE` says when it changes.
pub const WIN_SCALE: u64 = 41;
/// () -> the theme the user chose for windows (Impostazioni, Aspetto):
/// `THEME_DARK` or `THEME_LIGHT`. Programs draw their content in its
/// colours (justui's `ui` does).
pub const THEME: u64 = 42;
pub const THEME_DARK: u64 = 0;
pub const THEME_LIGHT: u64 = 1;

pub const PORT_MSG_MAX: u64 = 64 << 10;
pub const PORT_QUEUE_MAX: usize = 64;
/// `PORT_RECV` timeout: wait until a message comes.
pub const FOREVER: u64 = u64::MAX;

/// `WIN_CREATE` flag: the user may resize the window (`WIN_EV_RESIZE`).
pub const WIN_RESIZABLE: u64 = 1;
/// `WIN_CREATE` flag: files may be dropped on the window (`WIN_EV_DROP`).
pub const WIN_DROPS: u64 = 2;

// Window events: [kind, a, b, c].
/// x, y, 1 for a double click (2: the right button)
pub const WIN_EV_PRESS: u64 = 1;
pub const WIN_EV_RELEASE: u64 = 2;
/// x, y, 1 while the button is held
pub const WIN_EV_MOVE: u64 = 3;
/// key code (as READ_KEY)
pub const WIN_EV_KEY: u64 = 4;
/// lines (signed; positive: down)
pub const WIN_EV_SCROLL: u64 = 5;
/// The user closed the window: the program should end.
pub const WIN_EV_CLOSE: u64 = 6;
/// width, height, scale: the content area changed size (in units) or the
/// pixels per unit changed.
pub const WIN_EV_RESIZE: u64 = 7;
/// The pointer left the window.
pub const WIN_EV_LEAVE: u64 = 8;
/// x, y, count: files were dropped there (`WIN_DROPPED` says which).
pub const WIN_EV_DROP: u64 = 9;

pub const TLS_FS: u64 = 0;
pub const TLS_GS: u64 = 1;

pub const MODE_READ: u64 = 0;
/// Create or truncate.
pub const MODE_WRITE: u64 = 1;
/// Create if needed, write at the end.
pub const MODE_APPEND: u64 = 2;

pub const CONSOLE: u64 = 0;

/// `SYSINFO` words: total memory, free memory, uptime ms, threads,
/// processes, devices, devices with a driver, disks.
pub const SYSINFO_WORDS: usize = 8;

/// Error codes (returned negated).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum Error {
    NotFound = 1,
    Exists = 2,
    NotDir = 3,
    IsDir = 4,
    NotEmpty = 5,
    NoSpace = 6,
    InvalidName = 7,
    ReadOnly = 8,
    Io = 9,
    BadHandle = 10,
    BadAddress = 11,
    NoMemory = 12,
    BadProgram = 13,
    Unknown = 14,
    Timeout = 15,
    /// The other end of a port is gone.
    Closed = 16,
    /// A port's queue is full.
    Full = 17,
    TooLarge = 18,
    /// The app's sandbox does not allow it (or the user said no).
    Denied = 19,
    /// The user closed the dialog.
    Cancelled = 20,
}

impl Error {
    pub fn from_code(code: i64) -> Error {
        match -code {
            1 => Error::NotFound,
            2 => Error::Exists,
            3 => Error::NotDir,
            4 => Error::IsDir,
            5 => Error::NotEmpty,
            6 => Error::NoSpace,
            7 => Error::InvalidName,
            8 => Error::ReadOnly,
            9 => Error::Io,
            10 => Error::BadHandle,
            11 => Error::BadAddress,
            12 => Error::NoMemory,
            13 => Error::BadProgram,
            15 => Error::Timeout,
            16 => Error::Closed,
            17 => Error::Full,
            18 => Error::TooLarge,
            19 => Error::Denied,
            20 => Error::Cancelled,
            _ => Error::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Error::NotFound => "not found",
            Error::Exists => "already exists",
            Error::NotDir => "not a folder",
            Error::IsDir => "is a folder",
            Error::NotEmpty => "folder not empty",
            Error::NoSpace => "disk full",
            Error::InvalidName => "invalid name",
            Error::ReadOnly => "read-only",
            Error::Io => "I/O error",
            Error::BadHandle => "bad handle",
            Error::BadAddress => "bad address",
            Error::NoMemory => "out of memory",
            Error::BadProgram => "not a program",
            Error::Unknown => "error",
            Error::Timeout => "timed out",
            Error::Closed => "the other side is gone",
            Error::Full => "queue full",
            Error::TooLarge => "too large",
            Error::Denied => "permission denied",
            Error::Cancelled => "cancelled",
        }
    }
}

// Key codes returned by READ_KEY: a Unicode scalar, or one of these, with
// modifier bits on top.
pub const KEY_SPECIAL: u32 = 0x11_0000;
pub const KEY_ENTER: u32 = KEY_SPECIAL + 1;
pub const KEY_BACKSPACE: u32 = KEY_SPECIAL + 2;
pub const KEY_DELETE: u32 = KEY_SPECIAL + 3;
pub const KEY_TAB: u32 = KEY_SPECIAL + 4;
pub const KEY_ESCAPE: u32 = KEY_SPECIAL + 5;
pub const KEY_UP: u32 = KEY_SPECIAL + 6;
pub const KEY_DOWN: u32 = KEY_SPECIAL + 7;
pub const KEY_LEFT: u32 = KEY_SPECIAL + 8;
pub const KEY_RIGHT: u32 = KEY_SPECIAL + 9;
pub const KEY_HOME: u32 = KEY_SPECIAL + 10;
pub const KEY_END: u32 = KEY_SPECIAL + 11;
pub const KEY_PAGE_UP: u32 = KEY_SPECIAL + 12;
pub const KEY_PAGE_DOWN: u32 = KEY_SPECIAL + 13;
pub const KEY_INSERT: u32 = KEY_SPECIAL + 14;
/// F1..F12 are KEY_F + 1..12.
pub const KEY_F: u32 = KEY_SPECIAL + 0x100;

pub const MOD_CTRL: u64 = 1 << 32;
pub const MOD_ALT: u64 = 1 << 33;
pub const MOD_SHIFT: u64 = 1 << 34;

/// Where user programs are loaded and where their stack ends.
pub const USER_BASE: u64 = 0x40_0000;
pub const USER_STACK_TOP: u64 = 0x7FFF_FFF0_0000;
pub const USER_HEAP_BASE: u64 = 0x1000_0000_0000;
