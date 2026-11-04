//! Network interfaces registered by drivers. Frames are counted and kept in
//! a small receive queue; protocols live in [`crate::net`].

use crate::sync::IrqLock;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use arcdk::{NetDesc, Status, E_INVAL, OK};

pub struct Iface {
    pub device: u32,
    pub mac: [u8; 6],
    pub mtu: u32,
    desc: *const NetDesc,
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    rx: VecDeque<Vec<u8>>,
}

// SAFETY: the descriptor belongs to the driver, which serialises sends itself.
unsafe impl Send for Iface {}

/// Interfaces by number; a removed interface leaves its slot empty, so the
/// numbers of the others never change.
static IFACES: IrqLock<Vec<Option<Iface>>> = IrqLock::new(Vec::new());

pub fn register(device: u32, desc: *const NetDesc) -> Status {
    // SAFETY: the driver keeps the descriptor alive.
    let d = unsafe { &*desc };
    let mut ifaces = IFACES.lock();
    let n = ifaces.len();
    ifaces.push(Some(Iface {
        device,
        mac: d.mac,
        mtu: d.mtu,
        desc,
        rx_packets: 0,
        rx_bytes: 0,
        tx_packets: 0,
        tx_bytes: 0,
        rx: VecDeque::new(),
    }));
    drop(ifaces);
    let m = d.mac;
    crate::info!("net", "net{n}: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5]);
    n as Status
}

pub fn receive(iface: u32, frame: &[u8]) {
    let mut ifaces = IFACES.lock();
    if let Some(Some(i)) = ifaces.get_mut(iface as usize) {
        i.rx_packets += 1;
        i.rx_bytes += frame.len() as u64;
        if i.rx.len() < 64 {
            i.rx.push_back(frame.to_vec());
        }
    }
}

/// Next received frame on an interface.
pub fn poll(iface: u32) -> Option<Vec<u8>> {
    IFACES.lock().get_mut(iface as usize)?.as_mut()?.rx.pop_front()
}

pub fn send(iface: u32, frame: &[u8]) -> Status {
    if !super::pm::awake() {
        return OK; // the card sleeps: the frame is lost, as on a busy wire
    }
    let desc = {
        let mut ifaces = IFACES.lock();
        let Some(Some(i)) = ifaces.get_mut(iface as usize) else { return E_INVAL };
        i.tx_packets += 1;
        i.tx_bytes += frame.len() as u64;
        i.desc
    };
    // SAFETY: the descriptor stays valid while the interface exists.
    unsafe { ((*desc).send)((*desc).ctx, frame.as_ptr(), frame.len()) }
}

pub struct IfaceInfo {
    pub index: u32,
    pub device: u32,
    pub mac: [u8; 6],
    pub mtu: u32,
    pub rx: (u64, u64),
    pub tx: (u64, u64),
}

pub fn list() -> Vec<IfaceInfo> {
    IFACES
        .lock()
        .iter()
        .enumerate()
        .filter_map(|(n, i)| Some((n, i.as_ref()?)))
        .map(|(n, i)| IfaceInfo {
            index: n as u32,
            device: i.device,
            mac: i.mac,
            mtu: i.mtu,
            rx: (i.rx_packets, i.rx_bytes),
            tx: (i.tx_packets, i.tx_bytes),
        })
        .collect()
}

pub fn remove_device(device: u32) {
    for slot in IFACES.lock().iter_mut() {
        if slot.as_ref().is_some_and(|i| i.device == device) {
            *slot = None;
        }
    }
}

#[allow(dead_code)]
pub const SEND_OK: Status = OK;
