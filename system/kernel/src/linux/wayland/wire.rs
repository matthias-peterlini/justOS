//! The Wayland wire format: a message is the object's id, then its size
//! (high 16 bits) and opcode (low 16 bits), then the arguments, each a
//! multiple of four bytes. Descriptors travel beside the bytes (with
//! SCM_RIGHTS), in the order the messages use them.

use crate::linux::fd::Desc;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// The arguments of one request.
pub struct Args<'a> {
    data: &'a [u8],
    at: usize,
    fds: &'a mut VecDeque<Arc<Desc>>,
    /// An argument was missing or malformed.
    pub bad: bool,
}

impl<'a> Args<'a> {
    pub fn new(data: &'a [u8], fds: &'a mut VecDeque<Arc<Desc>>) -> Args<'a> {
        Args { data, at: 0, fds, bad: false }
    }

    pub fn uint(&mut self) -> u32 {
        match self.data.get(self.at..self.at + 4) {
            Some(b) => {
                self.at += 4;
                u32::from_le_bytes([b[0], b[1], b[2], b[3]])
            }
            None => {
                self.bad = true;
                0
            }
        }
    }

    pub fn int(&mut self) -> i32 {
        self.uint() as i32
    }

    /// A string (None for a null one).
    pub fn string(&mut self) -> Option<String> {
        let n = self.uint() as usize;
        if n == 0 {
            return None;
        }
        let b = self.data.get(self.at..self.at + n);
        self.at += n.div_ceil(4) * 4;
        match b {
            // The length counts the terminating zero.
            Some(b) => Some(String::from_utf8_lossy(&b[..n - 1]).into_owned()),
            None => {
                self.bad = true;
                None
            }
        }
    }

    pub fn text(&mut self) -> String {
        self.string().unwrap_or_default()
    }

    pub fn fd(&mut self) -> Option<Arc<Desc>> {
        let d = self.fds.pop_front();
        if d.is_none() {
            self.bad = true;
        }
        d
    }
}

/// Events on their way to a client.
#[derive(Default)]
pub struct Out {
    pub data: Vec<u8>,
    pub fds: Vec<Arc<Desc>>,
}

/// One event being written.
pub struct Event<'a> {
    out: &'a mut Out,
    start: usize,
}

impl Out {
    pub fn event(&mut self, obj: u32, opcode: u16) -> Event<'_> {
        let start = self.data.len();
        self.data.extend_from_slice(&obj.to_le_bytes());
        self.data.extend_from_slice(&(opcode as u32).to_le_bytes());
        Event { out: self, start }
    }
}

impl Event<'_> {
    pub fn uint(self, v: u32) -> Self {
        self.out.data.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn int(self, v: i32) -> Self {
        self.uint(v as u32)
    }

    /// A 24.8 fixed-point number, from whole units.
    pub fn fixed(self, v: i32) -> Self {
        self.int(v.saturating_mul(256))
    }

    pub fn object(self, id: u32) -> Self {
        self.uint(id)
    }

    pub fn string(self, s: &str) -> Self {
        let n = s.len() + 1;
        let d = &mut self.out.data;
        d.extend_from_slice(&(n as u32).to_le_bytes());
        d.extend_from_slice(s.as_bytes());
        d.push(0);
        while !d.len().is_multiple_of(4) {
            d.push(0);
        }
        self
    }

    pub fn array(self, a: &[u8]) -> Self {
        let d = &mut self.out.data;
        d.extend_from_slice(&(a.len() as u32).to_le_bytes());
        d.extend_from_slice(a);
        while !d.len().is_multiple_of(4) {
            d.push(0);
        }
        self
    }

    pub fn fd(self, d: Arc<Desc>) -> Self {
        self.out.fds.push(d);
        self
    }
}

impl Drop for Event<'_> {
    /// Writes the size into the header.
    fn drop(&mut self) {
        let size = (self.out.data.len() - self.start) as u32;
        let at = self.start + 4;
        let op = u32::from_le_bytes([self.out.data[at], self.out.data[at + 1], self.out.data[at + 2], self.out.data[at + 3]]);
        self.out.data[at..at + 4].copy_from_slice(&((size << 16) | (op & 0xffff)).to_le_bytes());
    }
}

/// The next whole message in `buf` from `at`: (object, opcode, arguments,
/// its length), or None until all of it is there.
pub fn next(buf: &[u8], at: usize) -> Option<(u32, u16, &[u8], usize)> {
    let h = buf.get(at..at + 8)?;
    let obj = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
    let w = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
    let size = (w >> 16) as usize;
    if size < 8 {
        // A broken message: take the header and move on.
        return Some((obj, (w & 0xffff) as u16, &[], 8));
    }
    let body = buf.get(at + 8..at + size)?;
    Some((obj, (w & 0xffff) as u16, body, size))
}
