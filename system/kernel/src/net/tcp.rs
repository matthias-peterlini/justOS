//! TCP (RFC 9293): connections out and in, with a sliding window in both
//! directions, window scaling, retransmission with an adaptive timeout and
//! segments that arrive out of order kept until the gap is filled.
//!
//! Every connection is a [`Tcb`] in one table. Arriving segments
//! ([`input`]) and the timers ([`timers`], from the network thread) change
//! it and produce the segments to send; those go out once the table's lock
//! is released. Callers block on [`super::EVENTS`], which is signalled on
//! every change.

use super::{checksum_with, local_ip, send_ipv4, Ip, EVENTS};
use crate::sync::IrqLock;
use crate::time;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

/// What we advertise and buffer per connection.
const RECV_BUF: usize = 256 * 1024;
const SEND_BUF: usize = 256 * 1024;
/// Our window scale: windows up to 64 KiB << 3.
const OUR_WSCALE: u8 = 3;
const DEFAULT_MSS: usize = 536;
const OUR_MSS: u16 = 1460;
const RTO_MIN: u64 = 200;
const RTO_MAX: u64 = 30_000;
const MAX_RETRIES: u32 = 10;
const SYN_RETRIES: u32 = 5;
const TIME_WAIT_MS: u64 = 2_000;

const FIN: u8 = 0x01;
const SYN: u8 = 0x02;
const RST: u8 = 0x04;
const PSH: u8 = 0x08;
const ACK: u8 = 0x10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
    Closed,
}

/// A segment ready to go: destination, header and payload (checksum still
/// to be filled in, it needs the source address).
struct Out {
    dst: Ip,
    seg: Vec<u8>,
}

pub struct Tcb {
    state: State,
    local_port: u16,
    remote: (Ip, u16),
    /// Oldest unacknowledged sequence number; `send_buf[0]` is its byte.
    snd_una: u32,
    snd_nxt: u32,
    /// The peer's window (already scaled), from the last update.
    snd_wnd: u32,
    snd_wl1: u32,
    snd_wl2: u32,
    rcv_nxt: u32,
    send_buf: VecDeque<u8>,
    recv_buf: VecDeque<u8>,
    /// Segments beyond `rcv_nxt`, by sequence number.
    ooo: BTreeMap<u32, Vec<u8>>,
    mss: usize,
    snd_wscale: u8,
    rcv_wscale: u8,
    /// Smoothed round trip and its variation (ms), and the timeout.
    srtt: u64,
    rttvar: u64,
    rto: u64,
    /// When the oldest unacknowledged segment is retransmitted.
    rtx_at: Option<u64>,
    retries: u32,
    /// A segment being timed: its end and when it left.
    timing: Option<(u32, u64)>,
    /// The application has no more to send: a FIN follows the data.
    shut_wr: bool,
    /// Sequence number of our FIN, once sent.
    fin_seq: Option<u32>,
    /// The peer sent its FIN.
    peer_fin: bool,
    error: Option<&'static str>,
    /// The owner closed the socket; the connection lives on until it ends.
    detached: bool,
    /// Listening sockets: connections ready for `accept`.
    backlog: VecDeque<u32>,
    /// A connection made by a listener, not accepted yet.
    parent: Option<u32>,
    time_wait_until: u64,
    /// The window we last advertised (to send an update when it opens).
    adv_wnd: u32,
}

struct Table {
    conns: BTreeMap<u32, Tcb>,
    next_id: u32,
    next_port: u16,
}

static TABLE: IrqLock<Table> = IrqLock::new(Table { conns: BTreeMap::new(), next_id: 1, next_port: 49152 });

fn seq_lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

fn seq_le(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) <= 0
}

/// An initial sequence number: a clock and the connection's addresses,
/// mixed (RFC 6528 in spirit).
fn initial_seq(local_port: u16, remote: (Ip, u16)) -> u32 {
    let mut x = crate::rand::u64() ^ (time::uptime_ms() << 12);
    x ^= (local_port as u64) << 48 | (remote.1 as u64) << 32 | u32::from_be_bytes(remote.0 .0) as u64;
    x = x.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (x >> 32) as u32
}

impl Tcb {
    fn new(local_port: u16, remote: (Ip, u16), state: State) -> Tcb {
        let iss = initial_seq(local_port, remote);
        Tcb {
            state,
            local_port,
            remote,
            snd_una: iss,
            snd_nxt: iss,
            snd_wnd: DEFAULT_MSS as u32,
            snd_wl1: 0,
            snd_wl2: 0,
            rcv_nxt: 0,
            send_buf: VecDeque::new(),
            recv_buf: VecDeque::new(),
            ooo: BTreeMap::new(),
            mss: DEFAULT_MSS,
            snd_wscale: 0,
            rcv_wscale: 0,
            srtt: 0,
            rttvar: 0,
            rto: 1000,
            rtx_at: None,
            retries: 0,
            timing: None,
            shut_wr: false,
            fin_seq: None,
            peer_fin: false,
            error: None,
            detached: false,
            backlog: VecDeque::new(),
            parent: None,
            time_wait_until: 0,
            adv_wnd: 0,
        }
    }

    /// The window we can offer now, scaled down for the header.
    fn rcv_wnd(&self) -> u32 {
        (RECV_BUF.saturating_sub(self.recv_buf.len())) as u32
    }

    fn header(&mut self, flags: u8, seq: u32, payload: &[u8], options: &[u8]) -> Out {
        let hlen = 20 + options.len();
        let mut s = Vec::with_capacity(hlen + payload.len());
        s.extend_from_slice(&self.local_port.to_be_bytes());
        s.extend_from_slice(&self.remote.1.to_be_bytes());
        s.extend_from_slice(&seq.to_be_bytes());
        let ack = if flags & ACK != 0 { self.rcv_nxt } else { 0 };
        s.extend_from_slice(&ack.to_be_bytes());
        s.push(((hlen / 4) as u8) << 4);
        s.push(flags);
        let wnd = if flags & SYN != 0 { self.rcv_wnd().min(65535) } else { (self.rcv_wnd() >> self.rcv_wscale).min(65535) };
        self.adv_wnd = if flags & SYN != 0 { wnd } else { wnd << self.rcv_wscale };
        s.extend_from_slice(&(wnd as u16).to_be_bytes());
        s.extend_from_slice(&[0, 0, 0, 0]); // checksum, urgent pointer
        s.extend_from_slice(options);
        s.extend_from_slice(payload);
        Out { dst: self.remote.0, seg: s }
    }

    fn syn_options(&self) -> [u8; 8] {
        // MSS, then NOP + window scale.
        let m = OUR_MSS.to_be_bytes();
        [2, 4, m[0], m[1], 1, 3, 3, OUR_WSCALE]
    }

    fn ack_now(&mut self, out: &mut Vec<Out>) {
        let seq = self.snd_nxt;
        let o = self.header(ACK, seq, &[], &[]);
        out.push(o);
    }

    fn arm_timer(&mut self) {
        if self.rtx_at.is_none() {
            self.rtx_at = Some(time::uptime_ms() + self.rto);
        }
    }

    /// Bytes in flight (sent, not acknowledged), SYN and FIN included.
    fn in_flight(&self) -> u32 {
        self.snd_nxt.wrapping_sub(self.snd_una)
    }

    /// Sends what the window allows: new data, then the FIN.
    fn output(&mut self, out: &mut Vec<Out>) {
        match self.state {
            State::Established | State::CloseWait | State::FinWait1 | State::Closing | State::LastAck => {}
            _ => return,
        }
        loop {
            let offset = self.in_flight() as usize;
            let fin_counted = self.fin_seq.is_some() && seq_lt(self.fin_seq.unwrap(), self.snd_nxt);
            let data_off = offset - fin_counted as usize;
            let unsent = self.send_buf.len().saturating_sub(data_off);
            // At least one byte when the peer's window is shut, as a probe
            // (the timer resends it).
            let wnd = (self.snd_wnd as usize).max(if self.in_flight() == 0 { 1 } else { 0 });
            let room = wnd.saturating_sub(offset);
            let n = unsent.min(room).min(self.mss);
            if n > 0 && self.fin_seq.is_none() {
                let data: Vec<u8> = self.send_buf.iter().skip(data_off).take(n).copied().collect();
                let seq = self.snd_nxt;
                let flags = ACK | if n == unsent { PSH } else { 0 };
                let o = self.header(flags, seq, &data, &[]);
                out.push(o);
                self.snd_nxt = self.snd_nxt.wrapping_add(n as u32);
                if self.timing.is_none() {
                    self.timing = Some((self.snd_nxt, time::uptime_ms()));
                }
                self.arm_timer();
                continue;
            }
            if self.shut_wr && self.fin_seq.is_none() && unsent == 0 {
                let seq = self.snd_nxt;
                let o = self.header(FIN | ACK, seq, &[], &[]);
                out.push(o);
                self.fin_seq = Some(seq);
                self.snd_nxt = self.snd_nxt.wrapping_add(1);
                self.state = match self.state {
                    State::CloseWait => State::LastAck,
                    State::Established => State::FinWait1,
                    s => s,
                };
                self.arm_timer();
            }
            break;
        }
    }

    /// The retransmission timer went off: go back to the oldest byte.
    fn retransmit(&mut self, out: &mut Vec<Out>) {
        self.retries += 1;
        let limit = if matches!(self.state, State::SynSent | State::SynReceived) { SYN_RETRIES } else { MAX_RETRIES };
        if self.retries > limit {
            self.error = Some("connection timed out");
            self.state = State::Closed;
            self.rtx_at = None;
            return;
        }
        self.rto = (self.rto * 2).min(RTO_MAX);
        self.rtx_at = Some(time::uptime_ms() + self.rto);
        self.timing = None;
        match self.state {
            State::SynSent => {
                let opts = self.syn_options();
                let seq = self.snd_una;
                let o = self.header(SYN, seq, &[], &opts);
                out.push(o);
            }
            State::SynReceived => {
                let opts = self.syn_options();
                let seq = self.snd_una;
                let o = self.header(SYN | ACK, seq, &[], &opts);
                out.push(o);
            }
            _ => {
                // Go-back-N: forget what was sent after the oldest byte.
                self.snd_nxt = self.snd_una;
                if let Some(f) = self.fin_seq {
                    if !seq_lt(f, self.snd_una) {
                        self.fin_seq = None;
                        self.state = match self.state {
                            State::LastAck => State::CloseWait,
                            State::FinWait1 | State::Closing => State::Established,
                            s => s,
                        };
                        if self.peer_fin && self.state == State::Established {
                            self.state = State::CloseWait;
                        }
                    }
                }
                // Room for one segment at least, even with a closed window.
                let saved = self.snd_wnd;
                self.snd_wnd = self.snd_wnd.max(self.mss as u32);
                self.output(out);
                self.snd_wnd = saved;
            }
        }
    }

    fn update_rtt(&mut self, ack: u32) {
        if let Some((end, sent)) = self.timing {
            if seq_le(end, ack) {
                let r = time::uptime_ms().saturating_sub(sent).max(1);
                if self.srtt == 0 {
                    self.srtt = r;
                    self.rttvar = r / 2;
                } else {
                    let d = self.srtt.abs_diff(r);
                    self.rttvar = (3 * self.rttvar + d) / 4;
                    self.srtt = (7 * self.srtt + r) / 8;
                }
                self.rto = (self.srtt + (4 * self.rttvar).max(10)).clamp(RTO_MIN, RTO_MAX);
                self.timing = None;
            }
        }
    }

    /// The peer acknowledged up to `ack`.
    fn acked(&mut self, ack: u32) {
        if !seq_lt(self.snd_una, ack) || seq_lt(self.snd_nxt, ack) {
            return;
        }
        let mut n = ack.wrapping_sub(self.snd_una) as usize;
        // The SYN and the FIN take a sequence number but no buffer space.
        if matches!(self.state, State::SynSent | State::SynReceived) {
            n = n.saturating_sub(1);
        }
        if let Some(f) = self.fin_seq {
            if seq_lt(f, ack) {
                n = n.saturating_sub(1);
            }
        }
        let n = n.min(self.send_buf.len());
        self.send_buf.drain(..n);
        self.snd_una = ack;
        self.update_rtt(ack);
        self.retries = 0;
        self.rtx_at = if self.snd_una == self.snd_nxt { None } else { Some(time::uptime_ms() + self.rto) };
    }

    /// Data in order goes to the buffer; later data waits for the gap.
    fn accept_data(&mut self, seq: u32, mut data: &[u8]) {
        let mut seq = seq;
        // Trim what we already have.
        if seq_lt(seq, self.rcv_nxt) {
            let skip = self.rcv_nxt.wrapping_sub(seq) as usize;
            if skip >= data.len() {
                return;
            }
            data = &data[skip..];
            seq = self.rcv_nxt;
        }
        let room = RECV_BUF.saturating_sub(self.recv_buf.len());
        let ahead = seq.wrapping_sub(self.rcv_nxt) as usize;
        if ahead >= room {
            return;
        }
        let data = &data[..data.len().min(room - ahead)];
        if seq == self.rcv_nxt {
            self.recv_buf.extend(data);
            self.rcv_nxt = self.rcv_nxt.wrapping_add(data.len() as u32);
            // Segments that were waiting for this one.
            while let Some((&s, _)) = self.ooo.iter().next() {
                if seq_lt(self.rcv_nxt, s) {
                    break;
                }
                let d = self.ooo.remove(&s).unwrap();
                let skip = self.rcv_nxt.wrapping_sub(s) as usize;
                if skip < d.len() {
                    let room = RECV_BUF.saturating_sub(self.recv_buf.len());
                    let take = (d.len() - skip).min(room);
                    self.recv_buf.extend(&d[skip..skip + take]);
                    self.rcv_nxt = self.rcv_nxt.wrapping_add(take as u32);
                }
            }
        } else if self.ooo.len() < 256 {
            self.ooo.entry(seq).or_insert_with(|| data.to_vec());
        }
    }
}

struct Seg<'a> {
    sport: u16,
    dport: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    wnd: u32,
    mss: Option<u16>,
    wscale: Option<u8>,
    data: &'a [u8],
}

fn parse(body: &[u8]) -> Option<Seg<'_>> {
    if body.len() < 20 {
        return None;
    }
    let be16 = |o: usize| u16::from_be_bytes([body[o], body[o + 1]]);
    let be32 = |o: usize| u32::from_be_bytes([body[o], body[o + 1], body[o + 2], body[o + 3]]);
    let off = ((body[12] >> 4) as usize) * 4;
    if off < 20 || off > body.len() {
        return None;
    }
    let (mut mss, mut wscale) = (None, None);
    let mut i = 20;
    while i < off {
        match body[i] {
            0 => break,
            1 => i += 1,
            kind => {
                let len = *body.get(i + 1)? as usize;
                if len < 2 || i + len > off {
                    break;
                }
                match (kind, len) {
                    (2, 4) => mss = Some(be16(i + 2)),
                    (3, 3) => wscale = Some(body[i + 2].min(14)),
                    _ => {}
                }
                i += len;
            }
        }
    }
    Some(Seg {
        sport: be16(0),
        dport: be16(2),
        seq: be32(4),
        ack: be32(8),
        flags: body[13],
        wnd: be16(14) as u32,
        mss,
        wscale,
        data: &body[off..],
    })
}

fn send_all(out: Vec<Out>) {
    for o in out {
        let Some(src) = local_ip() else { continue };
        let mut seg = o.seg;
        let c = checksum_with(src, o.dst, 6, &seg);
        seg[16..18].copy_from_slice(&c.to_be_bytes());
        let _ = send_ipv4(o.dst, 6, &seg);
    }
}

/// A reset for a segment that belongs to no connection.
fn reset_for(src: Ip, s: &Seg<'_>) -> Out {
    let mut t = Tcb::new(s.dport, (src, s.sport), State::Closed);
    if s.flags & ACK != 0 {
        t.header(RST, s.ack, &[], &[])
    } else {
        t.rcv_nxt = s.seq.wrapping_add(s.data.len() as u32 + (s.flags & SYN != 0) as u32 + (s.flags & FIN != 0) as u32);
        t.header(RST | ACK, 0, &[], &[])
    }
}

/// A TCP segment arrived from `src` for us (`dst`).
pub fn input(src: Ip, dst: Ip, body: &[u8]) {
    crate::rand::mix(body.len() as u64);
    if checksum_with(src, dst, 6, body) != 0 {
        return;
    }
    let Some(s) = parse(body) else { return };
    let mut out = Vec::new();
    {
        let mut t = TABLE.lock();
        let tab = &mut *t;
        let id = tab
            .conns
            .iter()
            .find(|(_, c)| c.local_port == s.dport && c.remote == (src, s.sport) && c.state != State::Listen)
            .or_else(|| tab.conns.iter().find(|(_, c)| c.local_port == s.dport && c.state == State::Listen))
            .map(|(&id, _)| id);
        match id {
            None => {
                if s.flags & RST == 0 {
                    out.push(reset_for(src, &s));
                }
            }
            Some(id) => {
                let new = segment(tab, id, src, &s, &mut out);
                if let Some(c) = new {
                    let nid = tab.next_id;
                    tab.next_id += 1;
                    tab.conns.insert(nid, c);
                }
            }
        }
    }
    send_all(out);
    EVENTS.notify_all();
}

/// Handles a segment for connection `id`; a listener returns the new
/// connection a SYN made.
fn segment(tab: &mut Table, id: u32, src: Ip, s: &Seg<'_>, out: &mut Vec<Out>) -> Option<Tcb> {
    let c = tab.conns.get_mut(&id)?;
    match c.state {
        State::Listen => {
            if s.flags & RST != 0 {
                return None;
            }
            if s.flags & ACK != 0 {
                out.push(reset_for(src, s));
                return None;
            }
            if s.flags & SYN == 0 || c.backlog.len() >= 16 {
                return None;
            }
            let mut n = Tcb::new(c.local_port, (src, s.sport), State::SynReceived);
            n.rcv_nxt = s.seq.wrapping_add(1);
            n.mss = s.mss.map_or(DEFAULT_MSS, |m| (m as usize).clamp(64, OUR_MSS as usize));
            if let Some(w) = s.wscale {
                n.snd_wscale = w;
                n.rcv_wscale = OUR_WSCALE;
            }
            n.snd_wnd = s.wnd;
            n.parent = Some(id);
            let opts = n.syn_options();
            let opts: &[u8] = if s.wscale.is_some() { &opts } else { &opts[..4] };
            let seq = n.snd_nxt;
            let o = n.header(SYN | ACK, seq, &[], opts);
            out.push(o);
            n.snd_nxt = n.snd_nxt.wrapping_add(1);
            n.arm_timer();
            return Some(n);
        }
        State::SynSent => {
            if s.flags & ACK != 0 && (seq_le(s.ack, c.snd_una) || seq_lt(c.snd_nxt, s.ack)) {
                if s.flags & RST == 0 {
                    out.push(reset_for(src, s));
                }
                return None;
            }
            if s.flags & RST != 0 {
                if s.flags & ACK != 0 {
                    c.error = Some("connection refused");
                    c.state = State::Closed;
                    c.rtx_at = None;
                }
                return None;
            }
            if s.flags & SYN != 0 {
                c.rcv_nxt = s.seq.wrapping_add(1);
                c.mss = s.mss.map_or(DEFAULT_MSS, |m| (m as usize).clamp(64, OUR_MSS as usize));
                match s.wscale {
                    Some(w) => {
                        c.snd_wscale = w;
                        c.rcv_wscale = OUR_WSCALE;
                    }
                    None => {
                        c.snd_wscale = 0;
                        c.rcv_wscale = 0;
                    }
                }
                if s.flags & ACK != 0 {
                    c.acked(s.ack);
                    c.snd_wnd = s.wnd << c.snd_wscale;
                    c.snd_wl1 = s.seq;
                    c.snd_wl2 = s.ack;
                    c.state = State::Established;
                    c.ack_now(out);
                    c.output(out);
                } else {
                    // Simultaneous open.
                    c.state = State::SynReceived;
                    let opts = c.syn_options();
                    let seq = c.snd_una;
                    let o = c.header(SYN | ACK, seq, &[], &opts);
                    out.push(o);
                }
            }
            return None;
        }
        _ => {}
    }

    // Is the segment within the window?
    let len = s.data.len() as u32 + (s.flags & SYN != 0) as u32 + (s.flags & FIN != 0) as u32;
    let wnd = c.rcv_wnd().max(1);
    let ok = if len == 0 {
        seq_le(c.rcv_nxt, s.seq) && seq_lt(s.seq, c.rcv_nxt.wrapping_add(wnd)) || s.seq == c.rcv_nxt
    } else {
        let end = s.seq.wrapping_add(len - 1);
        (seq_le(c.rcv_nxt, s.seq) && seq_lt(s.seq, c.rcv_nxt.wrapping_add(wnd)))
            || (seq_le(c.rcv_nxt, end) && seq_lt(end, c.rcv_nxt.wrapping_add(wnd)))
    };
    if !ok {
        if s.flags & RST == 0 {
            c.ack_now(out);
        }
        return None;
    }
    if s.flags & RST != 0 {
        c.error = Some(if c.state == State::SynReceived { "connection refused" } else { "connection reset by peer" });
        c.state = State::Closed;
        c.rtx_at = None;
        return None;
    }
    if s.flags & SYN != 0 && c.state != State::SynReceived {
        // A SYN inside the window: the peer lost track (RFC 5961: answer
        // with an ACK).
        c.ack_now(out);
        return None;
    }
    if s.flags & ACK == 0 {
        return None;
    }
    if c.state == State::SynReceived {
        if seq_lt(c.snd_una, s.ack) && seq_le(s.ack, c.snd_nxt) {
            c.acked(s.ack);
            c.state = State::Established;
            c.snd_wnd = s.wnd << c.snd_wscale;
            c.snd_wl1 = s.seq;
            c.snd_wl2 = s.ack;
            if let Some(p) = c.parent {
                if let Some(l) = tab.conns.get_mut(&p) {
                    l.backlog.push_back(id);
                }
            }
        } else {
            out.push(reset_for(src, s));
            return None;
        }
    }
    let c = tab.conns.get_mut(&id)?;
    // Acknowledgements and window updates.
    if seq_lt(c.snd_una, s.ack) && seq_le(s.ack, c.snd_nxt) {
        c.acked(s.ack);
    }
    if seq_lt(c.snd_wl1, s.seq) || (c.snd_wl1 == s.seq && seq_le(c.snd_wl2, s.ack)) {
        c.snd_wnd = s.wnd << c.snd_wscale;
        c.snd_wl1 = s.seq;
        c.snd_wl2 = s.ack;
    }
    let fin_acked = c.fin_seq.is_some_and(|f| seq_lt(f, c.snd_una));
    match c.state {
        State::FinWait1 if fin_acked => {
            c.state = State::FinWait2;
            // A peer that never closes its side is given up on after a while.
            c.time_wait_until = time::uptime_ms() + 60_000;
        }
        State::Closing if fin_acked => {
            c.state = State::TimeWait;
            c.time_wait_until = time::uptime_ms() + TIME_WAIT_MS;
        }
        State::LastAck if fin_acked => {
            c.state = State::Closed;
            c.rtx_at = None;
        }
        _ => {}
    }
    // Data.
    let mut need_ack = false;
    if !s.data.is_empty() && matches!(c.state, State::Established | State::FinWait1 | State::FinWait2) {
        c.accept_data(s.seq, s.data);
        need_ack = true;
    }
    // FIN, once everything before it has arrived.
    if s.flags & FIN != 0 {
        let fin_at = s.seq.wrapping_add(s.data.len() as u32);
        if fin_at == c.rcv_nxt && !c.peer_fin {
            c.rcv_nxt = c.rcv_nxt.wrapping_add(1);
            c.peer_fin = true;
            c.state = match c.state {
                State::Established | State::SynReceived => State::CloseWait,
                State::FinWait1 => {
                    if fin_acked {
                        c.time_wait_until = time::uptime_ms() + TIME_WAIT_MS;
                        State::TimeWait
                    } else {
                        State::Closing
                    }
                }
                State::FinWait2 => {
                    c.time_wait_until = time::uptime_ms() + TIME_WAIT_MS;
                    State::TimeWait
                }
                st => st,
            };
        }
        need_ack = true;
    }
    if need_ack {
        c.ack_now(out);
    }
    c.output(out);
    None
}

/// Runs the timers (from the network thread, every few milliseconds).
pub fn timers() {
    let mut out = Vec::new();
    {
        let mut t = TABLE.lock();
        let now = time::uptime_ms();
        for c in t.conns.values_mut() {
            if c.rtx_at.is_some_and(|at| now >= at) {
                c.retransmit(&mut out);
            }
            // The application read: tell the peer the window opened again.
            if matches!(c.state, State::Established | State::FinWait1 | State::FinWait2)
                && c.adv_wnd < (c.mss as u32) * 2
                && c.rcv_wnd() >= c.adv_wnd + (RECV_BUF as u32) / 4
            {
                c.ack_now(&mut out);
            }
        }
        // Forget connections that are over and nobody holds.
        t.conns.retain(|_, c| match c.state {
            State::TimeWait => now < c.time_wait_until,
            State::FinWait2 => !(c.detached && now >= c.time_wait_until),
            State::Closed => !c.detached,
            _ => true,
        });
        if !out.is_empty() {
            EVENTS.notify_all();
        }
    }
    if !out.is_empty() {
        send_all(out);
        EVENTS.notify_all();
    }
}

// ----------------------------------------------------------------------------
// The interface for sockets

/// A connection (or a listener), by number.
pub type Conn = u32;

/// Starts opening a connection to `ip`:`port` from `local_port` (0: any),
/// without waiting: [`status`] tells how it goes.
pub fn open(ip: Ip, port: u16, local_port: u16) -> Result<Conn, &'static str> {
    if local_ip().is_none() {
        return Err("the network is not configured");
    }
    let mut out = Vec::new();
    let id = {
        let mut t = TABLE.lock();
        let tab = &mut *t;
        let mut lport = if local_port != 0 { local_port } else { tab.next_port };
        if local_port == 0 {
            for _ in 0..16384 {
                lport = if lport == u16::MAX { 49152 } else { lport + 1 };
                if !tab.conns.values().any(|c| c.local_port == lport) {
                    break;
                }
            }
            tab.next_port = lport;
        }
        let mut c = Tcb::new(lport, (ip, port), State::SynSent);
        let opts = c.syn_options();
        let seq = c.snd_nxt;
        let o = c.header(SYN, seq, &[], &opts);
        out.push(o);
        c.snd_nxt = c.snd_nxt.wrapping_add(1);
        c.arm_timer();
        let id = tab.next_id;
        tab.next_id += 1;
        tab.conns.insert(id, c);
        id
    };
    send_all(out);
    Ok(id)
}

/// How a connection being opened is doing: `None` while it is still
/// trying, then `Ok` or the reason it failed.
pub fn status(id: Conn) -> Option<Result<(), &'static str>> {
    let t = TABLE.lock();
    let Some(c) = t.conns.get(&id) else { return Some(Err("connection reset by peer")) };
    match c.state {
        State::SynSent | State::SynReceived => None,
        State::Closed => Some(Err(c.error.unwrap_or("connection failed"))),
        _ => Some(c.error.map_or(Ok(()), Err)),
    }
}

/// Copies received bytes without taking them.
pub fn peek(id: Conn, buf: &mut [u8]) -> usize {
    let t = TABLE.lock();
    let Some(c) = t.conns.get(&id) else { return 0 };
    let n = buf.len().min(c.recv_buf.len());
    for (d, s) in buf.iter_mut().zip(c.recv_buf.iter()) {
        *d = *s;
    }
    n
}

/// Bytes received and not read yet.
pub fn pending(id: Conn) -> usize {
    TABLE.lock().conns.get(&id).map_or(0, |c| c.recv_buf.len())
}

/// Opens a connection to `ip`:`port`; waits up to `timeout_ms`.
pub fn connect(ip: Ip, port: u16, timeout_ms: u64) -> Result<Conn, &'static str> {
    if local_ip().is_none() {
        return Err("the network is not configured");
    }
    let mut out = Vec::new();
    let id = {
        let mut t = TABLE.lock();
        let tab = &mut *t;
        let mut lport = tab.next_port;
        for _ in 0..16384 {
            lport = if lport == u16::MAX { 49152 } else { lport + 1 };
            if !tab.conns.values().any(|c| c.local_port == lport) {
                break;
            }
        }
        tab.next_port = lport;
        let mut c = Tcb::new(lport, (ip, port), State::SynSent);
        let opts = c.syn_options();
        let seq = c.snd_nxt;
        let o = c.header(SYN, seq, &[], &opts);
        out.push(o);
        c.snd_nxt = c.snd_nxt.wrapping_add(1);
        c.arm_timer();
        let id = tab.next_id;
        tab.next_id += 1;
        tab.conns.insert(id, c);
        id
    };
    send_all(out);
    let r = EVENTS.wait_for_timeout(timeout_ms, || {
        let t = TABLE.lock();
        let c = t.conns.get(&id)?;
        match c.state {
            State::SynSent | State::SynReceived => None,
            State::Closed => Some(Err(c.error.unwrap_or("connection failed"))),
            _ => Some(Ok(())),
        }
    });
    match r {
        Some(Ok(())) => Ok(id),
        Some(Err(e)) => {
            TABLE.lock().conns.remove(&id);
            Err(e)
        }
        None => {
            TABLE.lock().conns.remove(&id);
            Err("connection timed out")
        }
    }
}

/// Starts listening on `port`.
pub fn listen(port: u16) -> Result<Conn, &'static str> {
    let mut t = TABLE.lock();
    if t.conns.values().any(|c| c.local_port == port && c.state == State::Listen) {
        return Err("address in use");
    }
    let c = Tcb::new(port, (Ip::ANY, 0), State::Listen);
    let id = t.next_id;
    t.next_id += 1;
    t.conns.insert(id, c);
    Ok(id)
}

/// A connection that arrived on listener `l`, if there is one now.
pub fn try_accept(l: Conn) -> Option<Conn> {
    let mut t = TABLE.lock();
    let id = t.conns.get_mut(&l)?.backlog.pop_front()?;
    if let Some(c) = t.conns.get_mut(&id) {
        c.parent = None;
    }
    Some(id)
}

/// Queues bytes to send; returns how many fitted (0: the buffer is full).
pub fn try_send(id: Conn, data: &[u8]) -> Result<usize, &'static str> {
    let mut out = Vec::new();
    let n = {
        let mut t = TABLE.lock();
        let c = t.conns.get_mut(&id).ok_or("not connected")?;
        if let Some(e) = c.error {
            return Err(e);
        }
        if c.shut_wr || !matches!(c.state, State::Established | State::CloseWait) {
            return Err("the connection is closing");
        }
        let n = data.len().min(SEND_BUF.saturating_sub(c.send_buf.len()));
        c.send_buf.extend(&data[..n]);
        c.output(&mut out);
        n
    };
    send_all(out);
    Ok(n)
}

/// Sends all of `data`, waiting for room in the buffer.
pub fn send(id: Conn, mut data: &[u8], timeout_ms: u64) -> Result<usize, &'static str> {
    let total = data.len();
    while !data.is_empty() {
        let n = try_send(id, data)?;
        data = &data[n..];
        if n == 0 {
            let room = EVENTS.wait_for_timeout(timeout_ms, || {
                let t = TABLE.lock();
                let c = t.conns.get(&id)?;
                (c.error.is_some() || c.send_buf.len() < SEND_BUF).then_some(())
            });
            if room.is_none() {
                return Err("timed out");
            }
        }
    }
    Ok(total)
}

/// What a read finds: bytes, the end of the stream, or nothing yet.
pub enum Read {
    Data(usize),
    Eof,
    Empty,
}

/// Takes received bytes without waiting.
pub fn try_recv(id: Conn, buf: &mut [u8]) -> Result<Read, &'static str> {
    let mut t = TABLE.lock();
    let c = t.conns.get_mut(&id).ok_or("not connected")?;
    if !c.recv_buf.is_empty() {
        let n = buf.len().min(c.recv_buf.len());
        for (dst, src) in buf.iter_mut().zip(c.recv_buf.drain(..n)) {
            *dst = src;
        }
        return Ok(Read::Data(n));
    }
    if c.peer_fin {
        return Ok(Read::Eof);
    }
    if let Some(e) = c.error {
        return Err(e);
    }
    if c.state == State::Closed {
        return Ok(Read::Eof);
    }
    Ok(Read::Empty)
}

/// Waits for bytes; 0 means the peer closed its side.
pub fn recv(id: Conn, buf: &mut [u8], timeout_ms: u64) -> Result<usize, &'static str> {
    let r = EVENTS.wait_for_timeout(timeout_ms, || match try_recv(id, buf) {
        Ok(Read::Empty) => None,
        Ok(Read::Data(n)) => Some(Ok(n)),
        Ok(Read::Eof) => Some(Ok(0)),
        Err(e) => Some(Err(e)),
    });
    r.unwrap_or(Err("timed out"))
}

/// Nothing more to send: a FIN goes once the data is out.
pub fn shutdown(id: Conn) {
    let mut out = Vec::new();
    {
        let mut t = TABLE.lock();
        if let Some(c) = t.conns.get_mut(&id) {
            c.shut_wr = true;
            c.output(&mut out);
        }
    }
    send_all(out);
}

/// Closes the socket. The connection finishes in the background (the data
/// still queued is sent, then the FIN).
pub fn close(id: Conn) {
    shutdown(id);
    let mut t = TABLE.lock();
    let listener = t.conns.get(&id).is_some_and(|c| c.state == State::Listen);
    if listener {
        // Connections nobody accepted go too.
        let pending: Vec<u32> = t.conns.get(&id).map(|c| c.backlog.iter().copied().collect()).unwrap_or_default();
        for p in pending {
            t.conns.remove(&p);
        }
        t.conns.remove(&id);
        return;
    }
    if let Some(c) = t.conns.get_mut(&id) {
        c.detached = true;
        // Data that will never be read: the peer is told with a reset
        // (RFC 9293 3.10.4) and the connection ends here.
        if !c.recv_buf.is_empty() || matches!(c.state, State::SynSent | State::Closed) {
            let rst = (c.state != State::Closed && c.state != State::SynSent).then(|| {
                let seq = c.snd_nxt;
                c.header(RST | ACK, seq, &[], &[])
            });
            t.conns.remove(&id);
            drop(t);
            if let Some(r) = rst {
                send_all(alloc::vec![r]);
            }
        }
    }
}

/// Readiness for `poll`: (readable, writable, error or hang-up).
pub fn ready(id: Conn) -> (bool, bool, bool) {
    let t = TABLE.lock();
    match t.conns.get(&id) {
        None => (true, false, true),
        Some(c) if c.state == State::Listen => (!c.backlog.is_empty(), false, false),
        Some(c) => {
            let readable = !c.recv_buf.is_empty() || c.peer_fin || c.error.is_some() || c.state == State::Closed;
            let open = matches!(c.state, State::Established | State::CloseWait) && !c.shut_wr;
            let writable = open && c.send_buf.len() < SEND_BUF;
            let hup = c.error.is_some() || c.state == State::Closed || (c.peer_fin && c.shut_wr);
            (readable, writable || c.error.is_some(), hup)
        }
    }
}

/// The two ends of a connection: (local port, remote address and port).
pub fn endpoints(id: Conn) -> Option<(u16, Ip, u16)> {
    TABLE.lock().conns.get(&id).map(|c| (c.local_port, c.remote.0, c.remote.1))
}
