//! The app a program runs as, and its sandbox (see `arcdk::syscall`).
//!
//! An installed app reads and writes its own folder ([`data_dir`], where it
//! also starts); for anything else it asks:
//!
//! ```ignore
//! // a file the user chooses, in the system's picker
//! let path = arcrt::app::pick_save("disegno.qoi")?;
//! arcrt::fs::write(&path, &bytes)?;
//!
//! // a device, with the system's dialog the first time
//! if arcrt::app::request("camera")? { /* ... */ }
//! ```
//!
//! Folders of the home (`sys:/home/Immagini`) can also be used directly:
//! the first time, the system asks the user.

use crate::ipc::Port;
use crate::sys;
use crate::Result;
use alloc::format;
use alloc::string::String;
use alloc::vec;

/// The app id the program runs as (None: a system program, without a
/// sandbox).
pub fn id() -> Option<String> {
    let mut buf = vec![0u8; 64];
    let n = sys::check(sys::call(sys::APP_ID, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0, 0)).ok()? as usize;
    buf.truncate(n.min(buf.len()));
    String::from_utf8(buf).ok()
}

/// Where the app keeps its files: `sys:/home/.apps/<id>` (for a system
/// program, the current folder).
pub fn data_dir() -> String {
    match id() {
        Some(id) => format!("sys:/home/.apps/{id}"),
        None => crate::fs::current_dir(),
    }
}

/// A file in [`data_dir`].
pub fn data_file(name: &str) -> String {
    format!("{}/{name}", data_dir())
}

/// From now on, runs as app `id` (`org.example.app`), shown as `name`, in
/// its sandbox; there is no way back. For programs that run others' code,
/// such as `arcc`.
pub fn enter(id: &str, name: &str) -> Result<()> {
    sys::check(sys::call(sys::APP_ENTER, id.as_ptr() as u64, id.len() as u64, name.as_ptr() as u64, name.len() as u64, 0)).map(|_| ())
}

fn pick(mode: u64, name: &str) -> Result<String> {
    let mut buf = vec![0u8; 1024];
    let n = sys::check(sys::call(sys::PICK_FILE, mode, name.as_ptr() as u64, name.len() as u64, buf.as_mut_ptr() as u64, buf.len() as u64))? as usize;
    buf.truncate(n.min(buf.len()));
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// The system's file picker: a file to open. The app may then read and
/// write it. `Cancelled` if the user closed the picker.
pub fn pick_open() -> Result<String> {
    pick(sys::PICK_OPEN, "")
}

/// The system's file picker: where to save, with `name` suggested.
pub fn pick_save(name: &str) -> Result<String> {
    pick(sys::PICK_SAVE, name)
}

/// Asks for a permission: `camera`, `microphone`, `location`, `network`
/// or `files:<home folder>`. The system asks the user the first time and
/// remembers; true if allowed.
pub fn request(permission: &str) -> Result<bool> {
    let answer = ask(&format!("{{\"request\":\"{permission}\"}}"))?;
    Ok(answer.contains("\"granted\":true"))
}

/// What the user said about a permission, without asking: `Some(true)`,
/// `Some(false)`, or None (not asked yet).
pub fn permission(permission: &str) -> Result<Option<bool>> {
    let answer = ask(&format!("{{\"check\":\"{permission}\"}}"))?;
    Ok(if answer.contains("\"allow\"") {
        Some(true)
    } else if answer.contains("\"deny\"") {
        Some(false)
    } else {
        None
    })
}

fn ask(request: &str) -> Result<String> {
    let answer = Port::lookup("arc.permissions")?.call(request.as_bytes())?;
    let answer = String::from_utf8_lossy(&answer).into_owned();
    if answer.contains("\"error\"") {
        return Err(crate::Error::InvalidName);
    }
    Ok(answer)
}
