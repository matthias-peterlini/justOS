//! Files and folders.

use crate::sys;
use crate::Result;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// An open file. Closed when dropped.
pub struct File {
    handle: u64,
}

impl File {
    fn open_mode(path: &str, mode: u64) -> Result<File> {
        let h = sys::check(sys::call(sys::OPEN, path.as_ptr() as u64, path.len() as u64, mode, 0, 0))?;
        Ok(File { handle: h })
    }

    pub fn open(path: &str) -> Result<File> {
        Self::open_mode(path, sys::MODE_READ)
    }

    /// Creates the file, or empties it.
    pub fn create(path: &str) -> Result<File> {
        Self::open_mode(path, sys::MODE_WRITE)
    }

    pub fn append(path: &str) -> Result<File> {
        Self::open_mode(path, sys::MODE_APPEND)
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        sys::check(sys::call(sys::READ, self.handle, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0)).map(|n| n as usize)
    }

    pub fn write(&mut self, data: &[u8]) -> Result<usize> {
        sys::check(sys::call(sys::WRITE, self.handle, data.as_ptr() as u64, data.len() as u64, 0, 0)).map(|n| n as usize)
    }

    pub fn seek(&mut self, pos: u64) -> Result<()> {
        sys::check(sys::call(sys::SEEK, self.handle, pos, 0, 0, 0)).map(|_| ())
    }

    pub fn size(&self) -> Result<u64> {
        sys::check(sys::call(sys::FILE_SIZE, self.handle, 0, 0, 0, 0))
    }

    pub fn read_to_end(&mut self) -> Result<Vec<u8>> {
        let mut out = vec![0u8; self.size()? as usize];
        let mut at = 0;
        while at < out.len() {
            let n = self.read(&mut out[at..])?;
            if n == 0 {
                break;
            }
            at += n;
        }
        out.truncate(at);
        Ok(out)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        sys::call(sys::CLOSE, self.handle, 0, 0, 0, 0);
    }
}

pub fn read(path: &str) -> Result<Vec<u8>> {
    File::open(path)?.read_to_end()
}

pub fn read_to_string(path: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&read(path)?).into_owned())
}

pub fn write(path: &str, data: &[u8]) -> Result<()> {
    File::create(path)?.write(data).map(|_| ())
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

pub fn list_dir(path: &str) -> Result<Vec<Entry>> {
    let mut cap = 4096;
    loop {
        let mut buf = vec![0u8; cap];
        let n =
            sys::check(sys::call(sys::LIST_DIR, path.as_ptr() as u64, path.len() as u64, buf.as_mut_ptr() as u64, cap as u64, 0))? as usize;
        if n > cap {
            cap = n;
            continue;
        }
        let text = String::from_utf8_lossy(&buf[..n]).into_owned();
        return Ok(text
            .lines()
            .filter_map(|l| {
                let mut p = l.split('\t');
                Some(Entry { name: p.next()?.into(), is_dir: p.next()? == "d", size: p.next()?.parse().ok()? })
            })
            .collect());
    }
}

pub fn remove(path: &str) -> Result<()> {
    sys::check(sys::call(sys::REMOVE, path.as_ptr() as u64, path.len() as u64, 0, 0, 0)).map(|_| ())
}

pub fn make_dir(path: &str) -> Result<()> {
    sys::check(sys::call(sys::MAKE_DIR, path.as_ptr() as u64, path.len() as u64, 0, 0, 0)).map(|_| ())
}

pub fn rename(from: &str, to: &str) -> Result<()> {
    sys::check(sys::call(sys::RENAME, from.as_ptr() as u64, from.len() as u64, to.as_ptr() as u64, to.len() as u64, 0)).map(|_| ())
}

/// The program's current folder (`sys:/home`).
pub fn current_dir() -> String {
    let mut buf = [0u8; 512];
    let n = sys::call(sys::GET_CWD, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0, 0).max(0) as usize;
    String::from_utf8_lossy(&buf[..n.min(buf.len())]).into_owned()
}

pub fn set_current_dir(path: &str) -> Result<()> {
    sys::check(sys::call(sys::SET_CWD, path.as_ptr() as u64, path.len() as u64, 0, 0, 0)).map(|_| ())
}
