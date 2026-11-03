//! Messages between programs, through ports (see `arcdk::syscall`).
//!
//! A service makes a named port and answers what comes in:
//!
//! ```ignore
//! let port = Port::create("org.example.echo")?;
//! loop {
//!     let msg = port.recv(None)?;
//!     msg.reply(&msg.data)?;
//! }
//! ```
//!
//! and a client asks it:
//!
//! ```ignore
//! let echo = Port::lookup("org.example.echo")?;
//! let answer = echo.call(b"hello")?;
//! ```

use crate::sys;
use crate::Result;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// A port handle: an own port (to receive from) or one to send to. Closed
/// when dropped.
pub struct Port(u64);

impl Port {
    /// Makes a port others find by `name` (lowercase, dots: `org.example.chat`).
    pub fn create(name: &str) -> Result<Port> {
        sys::check(sys::call(sys::PORT_CREATE, name.as_ptr() as u64, name.len() as u64, 0, 0, 0)).map(Port)
    }

    /// Makes a port without a name, to hand out with messages.
    pub fn private() -> Result<Port> {
        Port::create("")
    }

    /// Finds a named port to send to.
    pub fn lookup(name: &str) -> Result<Port> {
        sys::check(sys::call(sys::PORT_LOOKUP, name.as_ptr() as u64, name.len() as u64, 0, 0, 0)).map(Port)
    }

    /// Queues a message (without waiting for it to be read).
    pub fn send(&self, data: &[u8]) -> Result<()> {
        sys::check(sys::call(sys::PORT_SEND, self.0, data.as_ptr() as u64, data.len() as u64, 0, 0)).map(|_| ())
    }

    /// Sends a request and waits for the answer.
    pub fn call(&self, data: &[u8]) -> Result<Vec<u8>> {
        // Room for the longest answer there can be.
        let mut buf = vec![0u8; sys::PORT_MSG_MAX as usize];
        let n = sys::check(sys::call(sys::PORT_CALL, self.0, data.as_ptr() as u64, data.len() as u64, buf.as_mut_ptr() as u64, buf.len() as u64))? as usize;
        buf.truncate(n);
        Ok(buf)
    }

    /// Waits for a message on an own port; `None` waits as long as it takes.
    pub fn recv(&self, timeout_ms: Option<u64>) -> Result<Message> {
        let mut buf = vec![0u8; sys::PORT_MSG_MAX as usize];
        let mut info = [0u64; 3];
        let n = sys::check(sys::call(
            sys::PORT_RECV,
            self.0,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            timeout_ms.unwrap_or(sys::FOREVER),
            info.as_mut_ptr() as u64,
        ))? as usize;
        buf.truncate(n);
        Ok(Message { data: buf, sender: info[1] as u32, reply: (info[2] != 0).then_some(Port(info[2])) })
    }

    /// The raw handle.
    pub fn handle(&self) -> u64 {
        self.0
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        sys::call(sys::CLOSE, self.0, 0, 0, 0, 0);
    }
}

/// A message received.
pub struct Message {
    pub data: Vec<u8>,
    /// Process id of the sender, as the kernel saw it (0: the kernel).
    pub sender: u32,
    /// Where the sender waits for an answer.
    pub reply: Option<Port>,
}

impl Message {
    /// The message as text.
    pub fn text(&self) -> &str {
        core::str::from_utf8(&self.data).unwrap_or("")
    }

    /// Answers a call (does nothing for a plain message).
    pub fn reply(&self, data: &[u8]) -> Result<()> {
        match &self.reply {
            Some(p) => p.send(data),
            None => Ok(()),
        }
    }
}

/// The name of a running program, such as the sender of a message.
pub fn process_name(pid: u32) -> Result<String> {
    let mut buf = [0u8; 128];
    let n = sys::check(sys::call(sys::PROC_NAME, pid as u64, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0))? as usize;
    Ok(String::from_utf8_lossy(&buf[..n.min(buf.len())]).into_owned())
}
