//! A small IPv4 network stack: ARP, IPv4, ICMP echo, UDP, TCP ([`tcp`]), a
//! DHCP client and a DNS resolver. Frames come from network drivers through
//! [`crate::dev::net`]; the `netd` thread handles them.

pub mod http;
pub mod tcp;

use crate::dev::net as nic;
use crate::sync::IrqLock;
use crate::task::{self, WaitQueue};
use crate::time;
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Ip(pub [u8; 4]);

impl Ip {
    pub const ANY: Ip = Ip([0, 0, 0, 0]);
    pub const BROADCAST: Ip = Ip([255, 255, 255, 255]);

    pub fn parse(s: &str) -> Option<Ip> {
        let mut out = [0u8; 4];
        let mut parts = s.split('.');
        for b in out.iter_mut() {
            *b = parts.next()?.parse().ok()?;
        }
        parts.next().is_none().then_some(Ip(out))
    }

    fn u32(self) -> u32 {
        u32::from_be_bytes(self.0)
    }
}

impl fmt::Display for Ip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}.{}", self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

type Mac = [u8; 6];
const BROADCAST_MAC: Mac = [0xFF; 6];

#[derive(Clone, Copy, Default)]
pub struct Config {
    pub ip: Ip,
    pub mask: Ip,
    pub gateway: Ip,
    pub dns: Ip,
    pub lease_s: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dhcp {
    Idle,
    Discovering { xid: u32, since: u64 },
    Requesting { xid: u32, since: u64, offer: Ip, server: Ip },
    Bound,
}

struct Iface {
    mac: Mac,
    config: Config,
    dhcp: Dhcp,
    attempts: u32,
}

struct Stack {
    ifaces: BTreeMap<u32, Iface>,
    arp: BTreeMap<Ip, (Mac, u64)>,
    /// (id, seq) -> round-trip start (ms) and the reply time once it arrives.
    pings: BTreeMap<(u16, u16), (u64, Option<u64>)>,
    /// Open UDP ports: received datagrams (source ip, source port, data).
    udp: BTreeMap<u16, Vec<(Ip, u16, Vec<u8>)>>,
    ident: u16,
}

static STACK: IrqLock<Stack> =
    IrqLock::new(Stack { ifaces: BTreeMap::new(), arp: BTreeMap::new(), pings: BTreeMap::new(), udp: BTreeMap::new(), ident: 1 });
/// Signalled on every network event (and by pipes and eventfds of Linux
/// programs, so that poll and epoll wait on a single queue).
pub(crate) static EVENTS: WaitQueue = WaitQueue::new();

fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    for chunk in data.chunks(2) {
        let word = if chunk.len() == 2 { u16::from_be_bytes([chunk[0], chunk[1]]) } else { (chunk[0] as u16) << 8 };
        sum += word as u32;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// The checksum of a TCP or UDP segment, with the IPv4 pseudo-header.
fn checksum_with(src: Ip, dst: Ip, proto: u8, data: &[u8]) -> u16 {
    let mut sum = 0u32;
    for w in [src.0, dst.0] {
        sum += u16::from_be_bytes([w[0], w[1]]) as u32 + u16::from_be_bytes([w[2], w[3]]) as u32;
    }
    sum += proto as u32 + data.len() as u32;
    for chunk in data.chunks(2) {
        let word = if chunk.len() == 2 { u16::from_be_bytes([chunk[0], chunk[1]]) } else { (chunk[0] as u16) << 8 };
        sum += word as u32;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Our address (of the first interface that has one).
pub(crate) fn local_ip() -> Option<Ip> {
    let s = STACK.lock();
    s.ifaces.values().find(|i| i.dhcp == Dhcp::Bound || i.config.ip != Ip::ANY).map(|i| i.config.ip)
}

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}

fn ip_at(b: &[u8], o: usize) -> Ip {
    Ip([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

// ----------------------------------------------------------------------------
// Sending

fn send_ethernet(iface: u32, dst: Mac, ethertype: u16, payload: &[u8]) {
    let src = match STACK.lock().ifaces.get(&iface) {
        Some(i) => i.mac,
        None => return,
    };
    let mut frame = Vec::with_capacity(14 + payload.len().max(46));
    frame.extend_from_slice(&dst);
    frame.extend_from_slice(&src);
    frame.extend_from_slice(&ethertype.to_be_bytes());
    frame.extend_from_slice(payload);
    if frame.len() < 60 {
        frame.resize(60, 0);
    }
    nic::send(iface, &frame);
}

fn send_arp(iface: u32, op: u16, target_mac: Mac, target_ip: Ip) {
    let (mac, ip) = match STACK.lock().ifaces.get(&iface) {
        Some(i) => (i.mac, i.config.ip),
        None => return,
    };
    let mut p = Vec::with_capacity(28);
    p.extend_from_slice(&[0, 1, 0x08, 0, 6, 4]);
    p.extend_from_slice(&op.to_be_bytes());
    p.extend_from_slice(&mac);
    p.extend_from_slice(&ip.0);
    p.extend_from_slice(&target_mac);
    p.extend_from_slice(&target_ip.0);
    let dst = if op == 1 { BROADCAST_MAC } else { target_mac };
    send_ethernet(iface, dst, 0x0806, &p);
}

/// The interface and next-hop MAC for a destination, asking ARP if needed.
fn route(dst: Ip) -> Option<(u32, Mac)> {
    let (iface, hop) = {
        let s = STACK.lock();
        let (&n, i) = s.ifaces.iter().find(|(_, i)| i.dhcp == Dhcp::Bound || i.config.ip != Ip::ANY)?;
        let c = i.config;
        if dst == Ip::BROADCAST {
            return Some((n, BROADCAST_MAC));
        }
        let local = dst.u32() & c.mask.u32() == c.ip.u32() & c.mask.u32();
        (n, if local || c.gateway == Ip::ANY { dst } else { c.gateway })
    };
    for attempt in 0..3 {
        if let Some(&(mac, _)) = STACK.lock().arp.get(&hop) {
            return Some((iface, mac));
        }
        send_arp(iface, 1, [0; 6], hop);
        let until = time::uptime_ms() + 300 * (attempt + 1);
        while time::uptime_ms() < until {
            if STACK.lock().arp.contains_key(&hop) {
                break;
            }
            task::sleep_ms(5);
        }
    }
    STACK.lock().arp.get(&hop).map(|&(mac, _)| (iface, mac))
}

fn ipv4_packet(src: Ip, dst: Ip, proto: u8, payload: &[u8], ident: u16) -> Vec<u8> {
    let total = 20 + payload.len();
    let mut p = vec![0u8; 20];
    p[0] = 0x45;
    p[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    p[4..6].copy_from_slice(&ident.to_be_bytes());
    p[8] = 64;
    p[9] = proto;
    p[12..16].copy_from_slice(&src.0);
    p[16..20].copy_from_slice(&dst.0);
    let c = checksum(&p);
    p[10..12].copy_from_slice(&c.to_be_bytes());
    p.extend_from_slice(payload);
    p
}

fn send_ipv4(dst: Ip, proto: u8, payload: &[u8]) -> Result<(), &'static str> {
    let (iface, mac) = route(dst).ok_or("no route to host (is the network up?)")?;
    let (src, ident) = {
        let mut s = STACK.lock();
        s.ident = s.ident.wrapping_add(1);
        (s.ifaces.get(&iface).map(|i| i.config.ip).unwrap_or(Ip::ANY), s.ident)
    };
    send_ethernet(iface, mac, 0x0800, &ipv4_packet(src, dst, proto, payload, ident));
    Ok(())
}

fn udp_datagram(src_port: u16, dst_port: u16, data: &[u8]) -> Vec<u8> {
    let mut u = Vec::with_capacity(8 + data.len());
    u.extend_from_slice(&src_port.to_be_bytes());
    u.extend_from_slice(&dst_port.to_be_bytes());
    u.extend_from_slice(&((8 + data.len()) as u16).to_be_bytes());
    u.extend_from_slice(&[0, 0]); // no checksum (allowed on IPv4)
    u.extend_from_slice(data);
    u
}

pub fn send_udp(dst: Ip, src_port: u16, dst_port: u16, data: &[u8]) -> Result<(), &'static str> {
    send_ipv4(dst, 17, &udp_datagram(src_port, dst_port, data))
}

// ----------------------------------------------------------------------------
// DHCP

const DHCP_MAGIC: [u8; 4] = [99, 130, 83, 99];

fn dhcp_send(iface: u32, kind: u8, xid: u32, requested: Option<(Ip, Ip)>) {
    let mac = match STACK.lock().ifaces.get(&iface) {
        Some(i) => i.mac,
        None => return,
    };
    let mut d = vec![0u8; 240];
    d[0] = 1; // request
    d[1] = 1; // Ethernet
    d[2] = 6;
    d[4..8].copy_from_slice(&xid.to_be_bytes());
    d[10] = 0x80; // broadcast replies
    d[28..34].copy_from_slice(&mac);
    d[236..240].copy_from_slice(&DHCP_MAGIC);
    d.extend_from_slice(&[53, 1, kind]);
    if let Some((ip, server)) = requested {
        d.extend_from_slice(&[50, 4]);
        d.extend_from_slice(&ip.0);
        d.extend_from_slice(&[54, 4]);
        d.extend_from_slice(&server.0);
    }
    d.extend_from_slice(&[55, 3, 1, 3, 6]); // ask for mask, router, DNS
    d.extend_from_slice(&[12, 6]);
    d.extend_from_slice(b"justos");
    d.push(255);
    let packet = ipv4_packet(Ip::ANY, Ip::BROADCAST, 17, &udp_datagram(68, 67, &d), 0);
    send_ethernet(iface, BROADCAST_MAC, 0x0800, &packet);
}

fn dhcp_receive(iface: u32, d: &[u8]) {
    if d.len() < 240 || d[0] != 2 || d[236..240] != DHCP_MAGIC {
        return;
    }
    let xid = u32::from_be_bytes(d[4..8].try_into().unwrap());
    let yiaddr = ip_at(d, 16);
    let (mut kind, mut mask, mut router, mut dns, mut server, mut lease) = (0, Ip::ANY, Ip::ANY, Ip::ANY, Ip::ANY, 0);
    let mut i = 240;
    while i + 1 < d.len() && d[i] != 255 {
        if d[i] == 0 {
            i += 1;
            continue;
        }
        let (code, len) = (d[i], d[i + 1] as usize);
        let v = &d[(i + 2).min(d.len())..(i + 2 + len).min(d.len())];
        match (code, v.len()) {
            (53, 1..) => kind = v[0],
            (1, 4..) => mask = ip_at(v, 0),
            (3, 4..) => router = ip_at(v, 0),
            (6, 4..) => dns = ip_at(v, 0),
            (54, 4..) => server = ip_at(v, 0),
            (51, 4..) => lease = u32::from_be_bytes(v[..4].try_into().unwrap()),
            _ => {}
        }
        i += 2 + len;
    }
    let mut s = STACK.lock();
    let Some(i) = s.ifaces.get_mut(&iface) else { return };
    match (i.dhcp, kind) {
        (Dhcp::Discovering { xid: x, .. }, 2) if x == xid => {
            i.dhcp = Dhcp::Requesting { xid, since: time::uptime_ms(), offer: yiaddr, server };
            drop(s);
            dhcp_send(iface, 3, xid, Some((yiaddr, server)));
        }
        (Dhcp::Requesting { xid: x, .. }, 5) if x == xid => {
            i.config = Config { ip: yiaddr, mask, gateway: router, dns, lease_s: lease };
            i.dhcp = Dhcp::Bound;
            drop(s);
            crate::info!("net", "net{iface}: address {yiaddr}, gateway {router}, DNS {dns}");
            EVENTS.notify_all();
        }
        (Dhcp::Requesting { xid: x, .. }, 6) if x == xid => {
            i.dhcp = Dhcp::Idle; // NAK: start over
        }
        _ => {}
    }
}

// ----------------------------------------------------------------------------
// Receiving

fn handle_frame(iface: u32, f: &[u8]) {
    if f.len() < 14 {
        return;
    }
    let ethertype = be16(f, 12);
    let p = &f[14..];
    match ethertype {
        0x0806 if p.len() >= 28 => {
            let op = be16(p, 6);
            let sender_mac: Mac = p[8..14].try_into().unwrap();
            let sender_ip = ip_at(p, 14);
            let target_ip = ip_at(p, 24);
            let my_ip = {
                let mut s = STACK.lock();
                s.arp.insert(sender_ip, (sender_mac, time::uptime_ms()));
                s.ifaces.get(&iface).map(|i| i.config.ip).unwrap_or(Ip::ANY)
            };
            if op == 1 && target_ip == my_ip && my_ip != Ip::ANY {
                send_arp(iface, 2, sender_mac, sender_ip);
            }
        }
        0x0800 if p.len() >= 20 => {
            let ihl = ((p[0] & 0xF) as usize) * 4;
            let total = (be16(p, 2) as usize).min(p.len());
            if ihl < 20 || total < ihl {
                return;
            }
            let proto = p[9];
            let src = ip_at(p, 12);
            // Remember who sent it: replies then need no ARP round trip.
            let eth_src: Mac = f[6..12].try_into().unwrap();
            STACK.lock().arp.entry(src).or_insert((eth_src, time::uptime_ms()));
            let body = &p[ihl..total];
            match proto {
                1 if body.len() >= 8 => {
                    let (kind, id, seq) = (body[0], be16(body, 4), be16(body, 6));
                    if kind == 8 {
                        // Echo request: answer with the same payload.
                        let mut reply = body.to_vec();
                        reply[0] = 0;
                        reply[2] = 0;
                        reply[3] = 0;
                        let c = checksum(&reply);
                        reply[2..4].copy_from_slice(&c.to_be_bytes());
                        let _ = send_ipv4(src, 1, &reply);
                    } else if kind == 0 {
                        if let Some(e) = STACK.lock().pings.get_mut(&(id, seq)) {
                            e.1 = Some(time::uptime_ms());
                        }
                        EVENTS.notify_all();
                    }
                }
                6 => tcp::input(src, ip_at(p, 16), body),
                17 if body.len() >= 8 => {
                    let (sport, dport) = (be16(body, 0), be16(body, 2));
                    let len = (be16(body, 4) as usize).clamp(8, body.len());
                    let data = &body[8..len];
                    if dport == 68 {
                        dhcp_receive(iface, data);
                    } else {
                        let mut s = STACK.lock();
                        if let Some(q) = s.udp.get_mut(&dport) {
                            if q.len() < 32 {
                                q.push((src, sport, data.to_vec()));
                            }
                        }
                        drop(s);
                        EVENTS.notify_all();
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// The network thread: picks up new interfaces, runs DHCP, handles frames.
pub fn start() {
    task::spawn("netd", || loop {
        let mut busy = false;
        for info in nic::list() {
            let start_dhcp = {
                let mut s = STACK.lock();
                let iface =
                    s.ifaces.entry(info.index).or_insert(Iface { mac: info.mac, config: Config::default(), dhcp: Dhcp::Idle, attempts: 0 });
                let now = time::uptime_ms();
                match iface.dhcp {
                    Dhcp::Idle => true,
                    Dhcp::Discovering { since, .. } | Dhcp::Requesting { since, .. } => now - since > 3000 && iface.attempts < 10,
                    Dhcp::Bound => false,
                }
            };
            if start_dhcp {
                let xid = (time::uptime_ms() as u32).wrapping_mul(2654435761) ^ info.index;
                {
                    let mut s = STACK.lock();
                    let i = s.ifaces.get_mut(&info.index).unwrap();
                    i.dhcp = Dhcp::Discovering { xid, since: time::uptime_ms() };
                    i.attempts += 1;
                }
                dhcp_send(info.index, 1, xid, None);
            }
            while let Some(frame) = nic::poll(info.index) {
                handle_frame(info.index, &frame);
                busy = true;
            }
        }
        tcp::timers();
        // While packets flow, look again at once; when quiet, rest a little.
        if busy {
            task::yield_now();
        } else {
            task::sleep_ms(1);
        }
    });
}

// ----------------------------------------------------------------------------
// Services for the shell

pub fn config(iface: u32) -> Option<(Config, bool)> {
    STACK.lock().ifaces.get(&iface).map(|i| (i.config, i.dhcp == Dhcp::Bound))
}

/// Waits up to `ms` for any interface to get an address.
pub fn wait_configured(ms: u64) -> bool {
    EVENTS.wait_for_timeout(ms, || STACK.lock().ifaces.values().any(|i| i.dhcp == Dhcp::Bound).then_some(())).is_some()
}

/// Sends one echo request; returns the round-trip time in ms.
pub fn ping(dst: Ip, seq: u16, timeout_ms: u64) -> Result<u64, &'static str> {
    let id: u16 = 0xA2C0;
    let mut msg = vec![8u8, 0, 0, 0];
    msg.extend_from_slice(&id.to_be_bytes());
    msg.extend_from_slice(&seq.to_be_bytes());
    msg.extend_from_slice(b"JustOS ping payload 0123456789abcdef");
    let c = checksum(&msg);
    msg[2..4].copy_from_slice(&c.to_be_bytes());
    let start = time::uptime_ms();
    STACK.lock().pings.insert((id, seq), (start, None));
    let sent = send_ipv4(dst, 1, &msg);
    let result = sent.and_then(|_| {
        EVENTS.wait_for_timeout(timeout_ms, || STACK.lock().pings.get(&(id, seq)).and_then(|e| e.1)).map(|t| t - start).ok_or("timed out")
    });
    STACK.lock().pings.remove(&(id, seq));
    result
}

/// Resolves a host name with the DNS server from DHCP.
pub fn resolve(name: &str) -> Result<Ip, &'static str> {
    if let Some(ip) = Ip::parse(name) {
        return Ok(ip);
    }
    let dns = STACK.lock().ifaces.values().find(|i| i.dhcp == Dhcp::Bound).map(|i| i.config.dns).ok_or("the network is not configured")?;
    if dns == Ip::ANY {
        return Err("no DNS server");
    }
    let port = 50000 + (time::uptime_ms() % 10000) as u16;
    let id = (time::uptime_ms() as u16).wrapping_mul(31);
    let mut q = Vec::new();
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[1, 0, 0, 1, 0, 0, 0, 0, 0, 0]); // recursion desired, one question
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err("bad host name");
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 1, 0, 1]); // type A, class IN
    STACK.lock().udp.insert(port, Vec::new());
    let result = (|| {
        for _ in 0..3 {
            send_udp(dns, port, 53, &q)?;
            let reply = EVENTS.wait_for_timeout(1500, || {
                let mut s = STACK.lock();
                let queue = s.udp.get_mut(&port)?;
                let i = queue.iter().position(|(_, sport, d)| *sport == 53 && d.len() >= 12 && be16(d, 0) == id)?;
                Some(queue.remove(i).2)
            });
            if let Some(r) = reply {
                return parse_dns(&r);
            }
        }
        Err("no answer from the DNS server")
    })();
    STACK.lock().udp.remove(&port);
    result
}

fn parse_dns(r: &[u8]) -> Result<Ip, &'static str> {
    if r[3] & 0xF != 0 {
        return Err("host not found");
    }
    let answers = be16(r, 6);
    let skip_name = |mut i: usize| -> Option<usize> {
        loop {
            let len = *r.get(i)? as usize;
            if len == 0 {
                return Some(i + 1);
            }
            if len & 0xC0 == 0xC0 {
                return Some(i + 2);
            }
            i += len + 1;
        }
    };
    let mut i = skip_name(12).ok_or("bad DNS reply")? + 4;
    for _ in 0..answers {
        i = skip_name(i).ok_or("bad DNS reply")?;
        if i + 10 > r.len() {
            break;
        }
        let (kind, len) = (be16(r, i), be16(r, i + 8) as usize);
        if kind == 1 && len == 4 && i + 14 <= r.len() {
            return Ok(ip_at(r, i + 10));
        }
        i += 10 + len;
    }
    Err("no address for this host")
}

/// (ip, mac, age in s) of the ARP cache.
pub fn arp_table() -> Vec<(Ip, Mac, u64)> {
    let now = time::uptime_ms();
    STACK.lock().arp.iter().map(|(ip, (mac, t))| (*ip, *mac, (now - t) / 1000)).collect()
}

/// Does any interface have an address?
pub fn online() -> bool {
    STACK.lock().ifaces.values().any(|i| i.dhcp == Dhcp::Bound)
}

/// The DNS server from DHCP.
pub fn dns_server() -> Option<Ip> {
    STACK.lock().ifaces.values().find(|i| i.dhcp == Dhcp::Bound).map(|i| i.config.dns).filter(|d| *d != Ip::ANY)
}

// ----------------------------------------------------------------------------
// UDP for sockets

/// Opens UDP port `port` (0: any free one). Returns the port.
pub fn udp_bind(port: u16) -> Result<u16, &'static str> {
    let mut s = STACK.lock();
    let port = if port == 0 {
        let mut p = 40000 + (time::uptime_ms() % 20000) as u16;
        for _ in 0..25000 {
            if !s.udp.contains_key(&p) {
                break;
            }
            p = if p >= 64999 { 40000 } else { p + 1 };
        }
        p
    } else {
        port
    };
    if s.udp.contains_key(&port) {
        return Err("address in use");
    }
    s.udp.insert(port, Vec::new());
    Ok(port)
}

pub fn udp_close(port: u16) {
    STACK.lock().udp.remove(&port);
}

/// The next datagram on `port`: (sender, sender's port, data).
pub fn udp_try_recv(port: u16) -> Option<(Ip, u16, Vec<u8>)> {
    let mut s = STACK.lock();
    let q = s.udp.get_mut(&port)?;
    (!q.is_empty()).then(|| q.remove(0))
}

/// Bytes in the next datagram waiting on `port`, if any.
pub fn udp_peek(port: u16) -> Option<usize> {
    STACK.lock().udp.get(&port).and_then(|q| q.first().map(|d| d.2.len()))
}

/// The address of the first interface that has one (for the interface).
pub fn address() -> Option<Ip> {
    STACK.lock().ifaces.values().find(|i| i.dhcp == Dhcp::Bound).map(|i| i.config.ip)
}
