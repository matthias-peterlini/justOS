//! Programs: starting, waiting, ending; system information.

use crate::sys;
use crate::Result;

pub fn exit(code: i32) -> ! {
    sys::call(sys::EXIT, code as i64 as u64, 0, 0, 0, 0);
    unreachable!("the kernel does not return from exit")
}

/// Starts a program (`name` from sys:/apps, or a path). Returns its id.
pub fn spawn(name: &str, args: &str) -> Result<u32> {
    sys::check(sys::call(sys::SPAWN, name.as_ptr() as u64, name.len() as u64, args.as_ptr() as u64, args.len() as u64, 0)).map(|p| p as u32)
}

/// Waits for a program to end; returns its exit code.
pub fn wait(pid: u32) -> Result<i64> {
    let r = sys::call(sys::WAIT, pid as u64, 0, 0, 0, 0);
    if r == -(sys::Error::NotFound as i64) {
        return Err(sys::Error::NotFound);
    }
    Ok(r)
}

pub fn yield_now() {
    sys::call(sys::YIELD, 0, 0, 0, 0, 0);
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SysInfo {
    pub memory_total: u64,
    pub memory_free: u64,
    pub uptime_ms: u64,
    pub threads: u64,
    pub programs: u64,
    pub devices: u64,
    pub devices_with_driver: u64,
    pub disks: u64,
}

pub fn sysinfo() -> SysInfo {
    let mut w = [0u64; sys::SYSINFO_WORDS];
    sys::call(sys::SYSINFO, w.as_mut_ptr() as u64, 0, 0, 0, 0);
    SysInfo {
        memory_total: w[0],
        memory_free: w[1],
        uptime_ms: w[2],
        threads: w[3],
        programs: w[4],
        devices: w[5],
        devices_with_driver: w[6],
        disks: w[7],
    }
}
