//! A TLS client: TLS 1.3 (RFC 8446) and, for servers that do not speak it
//! yet, TLS 1.2 with ECDHE (RFC 5246, 7627, 8422). Ciphers: AES-128/256-GCM
//! and ChaCha20-Poly1305. Key exchange: X25519, P-256 or P-384. Server
//! certificates are checked against the trusted roots.
//!
//! The client does no input or output itself: [`Client::feed`] takes the
//! bytes that came from the server, [`Client::take_output`] gives what to
//! send, [`Client::write`] and [`Client::read`] carry the application's data.

use crate::aes::Gcm;
use crate::chacha;
use crate::ec::{self, Curve};
use crate::hash::{self, Alg, Hasher};
use crate::x25519;
use crate::x509::{self, PublicKey, Root, SigAlg};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Why a connection failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The server's message made no sense.
    Decode,
    /// The server picked something we did not offer.
    Protocol,
    /// The server sent an alert (its description).
    Alert(u8),
    /// A record did not decrypt: tampered with or wrong keys.
    BadRecord,
    /// The certificate check failed.
    Certificate(&'static str),
    /// The server's signature or Finished message was wrong.
    BadSignature,
    /// No cipher or group in common.
    Unsupported,
}

impl Error {
    pub fn describe(self) -> &'static str {
        match self {
            Error::Decode => "the server sent something unreadable",
            Error::Protocol => "the server broke the TLS protocol",
            Error::Alert(40) => "the server refused the connection (handshake failure)",
            Error::Alert(70) => "the server does not support this TLS version",
            Error::Alert(112) => "the server does not know this site name",
            Error::Alert(_) => "the server closed the secure connection with an error",
            Error::BadRecord => "a message could not be decrypted",
            Error::Certificate(why) => why,
            Error::BadSignature => "the server's identity could not be proved",
            Error::Unsupported => "no encryption in common with the server",
        }
    }
}

const CT_CCS: u8 = 20;
const CT_ALERT: u8 = 21;
const CT_HANDSHAKE: u8 = 22;
const CT_APP: u8 = 23;

const HS_CLIENT_HELLO: u8 = 1;
const HS_SERVER_HELLO: u8 = 2;
const HS_NEW_TICKET: u8 = 4;
const HS_ENC_EXT: u8 = 8;
const HS_CERT: u8 = 11;
const HS_SKE: u8 = 12;
const HS_CERT_REQ: u8 = 13;
const HS_HELLO_DONE: u8 = 14;
const HS_CERT_VERIFY: u8 = 15;
const HS_CKE: u8 = 16;
const HS_FINISHED: u8 = 20;
const HS_KEY_UPDATE: u8 = 24;

const X25519: u16 = 0x001d;
const SECP256R1: u16 = 0x0017;
const SECP384R1: u16 = 0x0018;

const HRR_RANDOM: [u8; 32] = [
    0xcf, 0x21, 0xad, 0x74, 0xe5, 0x9a, 0x61, 0x11, 0xbe, 0x1d, 0x8c, 0x02, 0x1e, 0x65, 0xb8, 0x91, 0xc2, 0xa2, 0x11, 0x16, 0x7a, 0xbb, 0x8c,
    0x5e, 0x07, 0x9e, 0x09, 0xe2, 0xc8, 0xa8, 0x33, 0x9c,
];

/// The cipher suites we offer, best first. ChaCha20 leads: our AES uses
/// tables, ChaCha20 has no timing to leak.
const SUITES: [u16; 9] = [0x1303, 0x1301, 0x1302, 0xcca9, 0xcca8, 0xc02b, 0xc02f, 0xc02c, 0xc030];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Aead {
    Aes128,
    Aes256,
    ChaCha,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Suite {
    id: u16,
    aead: Aead,
    hash: Alg,
    tls13: bool,
}

fn suite(id: u16) -> Option<Suite> {
    let (aead, hash, tls13) = match id {
        0x1301 => (Aead::Aes128, Alg::Sha256, true),
        0x1302 => (Aead::Aes256, Alg::Sha384, true),
        0x1303 => (Aead::ChaCha, Alg::Sha256, true),
        0xc02b | 0xc02f => (Aead::Aes128, Alg::Sha256, false),
        0xc02c | 0xc030 => (Aead::Aes256, Alg::Sha384, false),
        0xcca8 | 0xcca9 => (Aead::ChaCha, Alg::Sha256, false),
        _ => return None,
    };
    Some(Suite { id, aead, hash, tls13 })
}

impl Aead {
    fn key_len(self) -> usize {
        match self {
            Aead::Aes128 => 16,
            _ => 32,
        }
    }
}

enum Cipher {
    Gcm(Gcm),
    ChaCha([u8; 32]),
}

/// One direction of a protected connection.
struct Keys {
    cipher: Cipher,
    iv: [u8; 12],
    /// TLS 1.2 GCM: a 4-byte salt and an explicit 8-byte nonce per record.
    explicit: bool,
    seq: u64,
}

impl Keys {
    fn new(aead: Aead, key: &[u8], iv: &[u8], explicit: bool) -> Keys {
        let cipher = match aead {
            Aead::ChaCha => Cipher::ChaCha(key.try_into().unwrap()),
            _ => Cipher::Gcm(Gcm::new(key)),
        };
        let mut v = [0u8; 12];
        v[..iv.len()].copy_from_slice(iv);
        Keys { cipher, iv: v, explicit, seq: 0 }
    }

    fn nonce(&self, explicit: Option<&[u8]>) -> [u8; 12] {
        let mut n = self.iv;
        match explicit {
            Some(e) => n[4..].copy_from_slice(e),
            None => {
                for (i, b) in self.seq.to_be_bytes().iter().enumerate() {
                    n[4 + i] ^= b;
                }
            }
        }
        n
    }

    fn seal(&self, nonce: &[u8; 12], aad: &[u8], data: &mut Vec<u8>) {
        match &self.cipher {
            Cipher::Gcm(g) => g.seal(nonce, aad, data),
            Cipher::ChaCha(k) => chacha::seal(k, nonce, aad, data),
        }
    }

    fn open(&self, nonce: &[u8; 12], aad: &[u8], data: &mut Vec<u8>) -> bool {
        match &self.cipher {
            Cipher::Gcm(g) => g.open(nonce, aad, data),
            Cipher::ChaCha(k) => chacha::open(k, nonce, aad, data),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// Waiting for ServerHello.
    Hello,
    // TLS 1.3
    EncExt,
    Cert13,
    CertVerify,
    Finished13,
    // TLS 1.2
    Cert12,
    Ske,
    HelloDone,
    Ccs12,
    Finished12,
    // Both
    Connected,
    Closed,
}

enum Share {
    X25519([u8; 32]),
    Ec(Curve, Vec<u64>),
}

pub struct Client<'a> {
    host: String,
    roots: &'a [Root],
    now: u64,
    rng: fn(&mut [u8]),
    alpn: Vec<&'static str>,
    state: State,
    suite: Option<Suite>,
    client_random: [u8; 32],
    server_random: [u8; 32],
    session_id: [u8; 32],
    share: Share,
    /// Every handshake message so far (the transcript), until the suite and
    /// its hash are known; then the running hash.
    transcript: Vec<u8>,
    hasher: Option<Hasher>,
    retried: bool,
    // TLS 1.3 secrets.
    handshake_secret: Vec<u8>,
    client_hs: Vec<u8>,
    server_hs: Vec<u8>,
    client_app: Vec<u8>,
    server_app: Vec<u8>,
    // TLS 1.2
    ems: bool,
    master: Vec<u8>,
    server_key: Option<PublicKey>,
    /// TLS 1.2: our public key and the shared secret, until ClientKeyExchange.
    share_tls12: Option<(Vec<u8>, Vec<u8>)>,
    /// TLS 1.2: the server's keys, in use after its ChangeCipherSpec.
    pending_read: Option<Keys>,
    cert_requested: bool,
    chain: Vec<Vec<u8>>,
    read_keys: Option<Keys>,
    write_keys: Option<Keys>,
    /// Bytes from the server not yet made into records.
    inbuf: Vec<u8>,
    /// Handshake bytes not yet made into messages.
    hsbuf: Vec<u8>,
    out: Vec<u8>,
    plain: VecDeque<u8>,
    /// The protocol the server chose with ALPN.
    pub alpn_chosen: Option<String>,
    peer_closed: bool,
    error: Option<Error>,
}

fn put_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn put_u24(v: &mut Vec<u8>, x: usize) {
    v.extend_from_slice(&(x as u32).to_be_bytes()[1..]);
}

/// A vector with a length prefix of `n` bytes.
fn put_vec(v: &mut Vec<u8>, n: usize, data: &[u8]) {
    let len = data.len();
    match n {
        1 => v.push(len as u8),
        2 => put_u16(v, len as u16),
        _ => put_u24(v, len),
    }
    v.extend_from_slice(data);
}

/// Reads big-endian fields from a message.
struct Reader<'b> {
    b: &'b [u8],
}

impl<'b> Reader<'b> {
    fn u8(&mut self) -> Result<u8, Error> {
        let (&x, rest) = self.b.split_first().ok_or(Error::Decode)?;
        self.b = rest;
        Ok(x)
    }

    fn u16(&mut self) -> Result<u16, Error> {
        Ok(((self.u8()? as u16) << 8) | self.u8()? as u16)
    }

    fn u24(&mut self) -> Result<usize, Error> {
        Ok(((self.u8()? as usize) << 16) | ((self.u8()? as usize) << 8) | self.u8()? as usize)
    }

    fn take(&mut self, n: usize) -> Result<&'b [u8], Error> {
        if n > self.b.len() {
            return Err(Error::Decode);
        }
        let (a, rest) = self.b.split_at(n);
        self.b = rest;
        Ok(a)
    }

    fn vec(&mut self, n: usize) -> Result<&'b [u8], Error> {
        let len = match n {
            1 => self.u8()? as usize,
            2 => self.u16()? as usize,
            _ => self.u24()?,
        };
        self.take(len)
    }
}

impl<'a> Client<'a> {
    /// Starts a connection to `host`; the ClientHello is ready in the output.
    /// `now` (seconds since 1970) checks certificate dates (0: not checked).
    pub fn new(host: &str, roots: &'a [Root], now: u64, rng: fn(&mut [u8]), alpn: &[&'static str]) -> Client<'a> {
        let mut client_random = [0u8; 32];
        let mut session_id = [0u8; 32];
        let mut k = [0u8; 32];
        rng(&mut client_random);
        rng(&mut session_id);
        rng(&mut k);
        let mut c = Client {
            host: String::from(host.trim_end_matches('.')),
            roots,
            now,
            rng,
            alpn: alpn.to_vec(),
            state: State::Hello,
            suite: None,
            client_random,
            server_random: [0; 32],
            session_id,
            share: Share::X25519(k),
            transcript: Vec::new(),
            hasher: None,
            retried: false,
            handshake_secret: Vec::new(),
            client_hs: Vec::new(),
            server_hs: Vec::new(),
            client_app: Vec::new(),
            server_app: Vec::new(),
            ems: false,
            master: Vec::new(),
            server_key: None,
            share_tls12: None,
            pending_read: None,
            cert_requested: false,
            chain: Vec::new(),
            read_keys: None,
            write_keys: None,
            inbuf: Vec::new(),
            hsbuf: Vec::new(),
            out: Vec::new(),
            plain: VecDeque::new(),
            alpn_chosen: None,
            peer_closed: false,
            error: None,
        };
        let hello = c.client_hello(None);
        c.send_handshake(&hello);
        c
    }

    /// The first key share: always X25519 (a HelloRetryRequest may ask for
    /// another group).
    fn key_share_entry(&self) -> (u16, Vec<u8>) {
        match &self.share {
            Share::X25519(k) => (X25519, x25519::public(k).to_vec()),
            Share::Ec(..) => (X25519, Vec::new()),
        }
    }

    fn client_hello(&self, retry: Option<(u16, Vec<u8>, Option<Vec<u8>>)>) -> Vec<u8> {
        let mut b = Vec::new();
        put_u16(&mut b, 0x0303);
        b.extend_from_slice(&self.client_random);
        put_vec(&mut b, 1, &self.session_id);
        let mut suites = Vec::new();
        for s in SUITES {
            put_u16(&mut suites, s);
        }
        put_vec(&mut b, 2, &suites);
        put_vec(&mut b, 1, &[0]);
        let mut ext = Vec::new();
        let add = |ext: &mut Vec<u8>, id: u16, data: &[u8]| {
            put_u16(ext, id);
            put_vec(ext, 2, data);
        };
        // server_name
        if x509_ip(&self.host).is_none() {
            let mut sn = Vec::new();
            let mut entry = vec![0u8];
            put_vec(&mut entry, 2, self.host.as_bytes());
            put_vec(&mut sn, 2, &entry);
            add(&mut ext, 0, &sn);
        }
        add(&mut ext, 23, &[]); // extended_master_secret
        add(&mut ext, 0xff01, &[0]); // renegotiation_info
        let mut groups = Vec::new();
        for g in [X25519, SECP256R1, SECP384R1] {
            put_u16(&mut groups, g);
        }
        let mut gl = Vec::new();
        put_vec(&mut gl, 2, &groups);
        add(&mut ext, 10, &gl);
        add(&mut ext, 11, &[1, 0]); // ec_point_formats: uncompressed
        let mut sigs = Vec::new();
        for s in [0x0403u16, 0x0503, 0x0804, 0x0805, 0x0806, 0x0401, 0x0501, 0x0601] {
            put_u16(&mut sigs, s);
        }
        let mut sl = Vec::new();
        put_vec(&mut sl, 2, &sigs);
        add(&mut ext, 13, &sl);
        if !self.alpn.is_empty() {
            let mut protos = Vec::new();
            for p in &self.alpn {
                put_vec(&mut protos, 1, p.as_bytes());
            }
            let mut al = Vec::new();
            put_vec(&mut al, 2, &protos);
            add(&mut ext, 16, &al);
        }
        add(&mut ext, 43, &[4, 0x03, 0x04, 0x03, 0x03]); // supported_versions
        add(&mut ext, 45, &[1, 1]); // psk_key_exchange_modes: psk_dhe_ke
        let (group, public) = match &retry {
            Some((g, p, _)) => (*g, p.clone()),
            None => self.key_share_entry(),
        };
        let mut ks = Vec::new();
        put_u16(&mut ks, group);
        put_vec(&mut ks, 2, &public);
        let mut kl = Vec::new();
        put_vec(&mut kl, 2, &ks);
        add(&mut ext, 51, &kl);
        if let Some((_, _, Some(cookie))) = &retry {
            let mut cl = Vec::new();
            put_vec(&mut cl, 2, cookie);
            add(&mut ext, 44, &cl);
        }
        put_vec(&mut b, 2, &ext);
        let mut msg = vec![HS_CLIENT_HELLO];
        put_u24(&mut msg, b.len());
        msg.extend(b);
        msg
    }

    // ------------------------------------------------------------------
    // Records

    fn record_plain(&mut self, ctype: u8, data: &[u8]) {
        for chunk in data.chunks(16384) {
            self.out.push(ctype);
            put_u16(&mut self.out, 0x0303);
            put_vec(&mut self.out, 2, chunk);
        }
    }

    fn record(&mut self, ctype: u8, data: &[u8]) {
        let tls13 = self.suite.is_some_and(|s| s.tls13);
        let Some(keys) = self.write_keys.as_mut() else {
            self.record_plain(ctype, data);
            return;
        };
        for chunk in data.chunks(16384) {
            let mut rec = Vec::with_capacity(chunk.len() + 40);
            if tls13 {
                let mut inner = chunk.to_vec();
                inner.push(ctype);
                let len = inner.len() + 16;
                let aad = [CT_APP, 3, 3, (len >> 8) as u8, len as u8];
                let nonce = keys.nonce(None);
                keys.seal(&nonce, &aad, &mut inner);
                rec.extend_from_slice(&aad);
                rec.extend(inner);
            } else {
                let mut body = chunk.to_vec();
                let seq = keys.seq.to_be_bytes();
                let mut aad = seq.to_vec();
                aad.extend_from_slice(&[ctype, 3, 3, (chunk.len() >> 8) as u8, chunk.len() as u8]);
                let (nonce, explicit) = if keys.explicit {
                    (keys.nonce(Some(&seq)), Some(seq))
                } else {
                    // ChaCha20 in TLS 1.2: the IV xored with the sequence.
                    let mut n = keys.iv;
                    for (i, b) in seq.iter().enumerate() {
                        n[4 + i] ^= b;
                    }
                    (n, None)
                };
                keys.seal(&nonce, &aad, &mut body);
                let len = body.len() + if explicit.is_some() { 8 } else { 0 };
                rec.extend_from_slice(&[ctype, 3, 3, (len >> 8) as u8, len as u8]);
                if let Some(e) = explicit {
                    rec.extend_from_slice(&e);
                }
                rec.extend(body);
            }
            keys.seq += 1;
            self.out.extend(rec);
        }
    }

    fn send_handshake(&mut self, msg: &[u8]) {
        self.add_transcript(msg);
        self.record(CT_HANDSHAKE, msg);
    }

    fn add_transcript(&mut self, msg: &[u8]) {
        match &mut self.hasher {
            Some(h) => h.update(msg),
            None => self.transcript.extend_from_slice(msg),
        }
    }

    fn transcript_hash(&self) -> Vec<u8> {
        self.hasher.clone().map(|h| h.finish()).unwrap_or_default()
    }

    /// Takes the bytes to send to the server.
    pub fn take_output(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.out)
    }

    /// Bytes arrived from the server.
    pub fn feed(&mut self, data: &[u8]) -> Result<(), Error> {
        if let Some(e) = self.error {
            return Err(e);
        }
        self.inbuf.extend_from_slice(data);
        let r = self.process();
        if let Err(e) = r {
            self.error = Some(e);
            if !matches!(e, Error::Alert(_)) {
                // Tell the server why we are leaving.
                let desc = match e {
                    Error::Certificate(_) => 42,
                    Error::BadRecord => 20,
                    Error::BadSignature => 51,
                    Error::Unsupported => 40,
                    Error::Decode => 50,
                    _ => 10,
                };
                self.record(CT_ALERT, &[2, desc]);
            }
            self.state = State::Closed;
        }
        r
    }

    fn process(&mut self) -> Result<(), Error> {
        loop {
            if self.inbuf.len() < 5 {
                return Ok(());
            }
            let len = u16::from_be_bytes([self.inbuf[3], self.inbuf[4]]) as usize;
            if len > 16384 + 2048 {
                return Err(Error::Decode);
            }
            if self.inbuf.len() < 5 + len {
                return Ok(());
            }
            let rec: Vec<u8> = self.inbuf.drain(..5 + len).collect();
            let ctype = rec[0];
            let body = &rec[5..];
            if ctype == CT_CCS {
                if self.state == State::Ccs12 {
                    self.server_ccs()?;
                }
                // TLS 1.3 sends one for middleboxes: ignore it.
                continue;
            }
            let (ctype, data) = if self.read_keys.is_some() { self.decrypt(ctype, &rec[..5], body)? } else { (ctype, body.to_vec()) };
            match ctype {
                CT_ALERT => {
                    if data.len() >= 2 {
                        if data[1] == 0 {
                            self.peer_closed = true;
                            self.state = State::Closed;
                            return Ok(());
                        }
                        if data[0] == 2 || data[1] != 90 {
                            return Err(Error::Alert(data[1]));
                        }
                    }
                }
                CT_HANDSHAKE => {
                    self.hsbuf.extend(data);
                    while self.hsbuf.len() >= 4 {
                        let n = ((self.hsbuf[1] as usize) << 16) | ((self.hsbuf[2] as usize) << 8) | self.hsbuf[3] as usize;
                        if self.hsbuf.len() < 4 + n {
                            break;
                        }
                        let msg: Vec<u8> = self.hsbuf.drain(..4 + n).collect();
                        self.handshake(&msg)?;
                    }
                }
                CT_APP => {
                    if self.state != State::Connected {
                        return Err(Error::Protocol);
                    }
                    self.plain.extend(data);
                }
                _ => return Err(Error::Protocol),
            }
        }
    }

    fn decrypt(&mut self, ctype: u8, header: &[u8], body: &[u8]) -> Result<(u8, Vec<u8>), Error> {
        let tls13 = self.suite.is_some_and(|s| s.tls13);
        let keys = self.read_keys.as_mut().unwrap();
        if tls13 {
            if ctype != CT_APP {
                return Err(Error::Protocol);
            }
            let mut data = body.to_vec();
            let nonce = keys.nonce(None);
            if !keys.open(&nonce, header, &mut data) {
                return Err(Error::BadRecord);
            }
            keys.seq += 1;
            // Strip the padding; the last non-zero byte is the real type.
            let end = data.iter().rposition(|&b| b != 0).ok_or(Error::Protocol)?;
            let t = data[end];
            data.truncate(end);
            Ok((t, data))
        } else {
            let seq = keys.seq.to_be_bytes();
            let (nonce, ct) = if keys.explicit {
                if body.len() < 8 {
                    return Err(Error::BadRecord);
                }
                (keys.nonce(Some(&body[..8])), &body[8..])
            } else {
                let mut n = keys.iv;
                for (i, b) in seq.iter().enumerate() {
                    n[4 + i] ^= b;
                }
                (n, body)
            };
            if ct.len() < 16 {
                return Err(Error::BadRecord);
            }
            let plen = ct.len() - 16;
            let mut aad = seq.to_vec();
            aad.extend_from_slice(&[ctype, 3, 3, (plen >> 8) as u8, plen as u8]);
            let mut data = ct.to_vec();
            if !keys.open(&nonce, &aad, &mut data) {
                return Err(Error::BadRecord);
            }
            keys.seq += 1;
            Ok((ctype, data))
        }
    }

    // ------------------------------------------------------------------
    // Handshake

    fn handshake(&mut self, msg: &[u8]) -> Result<(), Error> {
        let kind = msg[0];
        let body = &msg[4..];
        match (self.state, kind) {
            (State::Hello, HS_SERVER_HELLO) => self.server_hello(msg),
            (State::EncExt, HS_ENC_EXT) => {
                self.add_transcript(msg);
                self.parse_extensions_alpn(body)?;
                self.state = State::Cert13;
                Ok(())
            }
            (State::Cert13, HS_CERT_REQ) => {
                self.add_transcript(msg);
                self.cert_requested = true;
                Ok(())
            }
            (State::Cert13, HS_CERT) => {
                self.add_transcript(msg);
                let mut r = Reader { b: body };
                r.vec(1)?; // request context
                let mut list = Reader { b: r.vec(3)? };
                let mut chain = Vec::new();
                while !list.b.is_empty() {
                    chain.push(list.vec(3)?.to_vec());
                    list.vec(2)?; // extensions
                }
                self.check_chain(chain)?;
                self.state = State::CertVerify;
                Ok(())
            }
            (State::CertVerify, HS_CERT_VERIFY) => {
                let mut r = Reader { b: body };
                let scheme = r.u16()?;
                let sig = r.vec(2)?;
                let mut content = vec![0x20u8; 64];
                content.extend_from_slice(b"TLS 1.3, server CertificateVerify\0");
                content.extend(self.transcript_hash());
                self.check_signature(scheme, &content, sig, true)?;
                self.add_transcript(msg);
                self.state = State::Finished13;
                Ok(())
            }
            (State::Finished13, HS_FINISHED) => self.server_finished13(msg),
            (State::Connected, HS_NEW_TICKET) => Ok(()),
            (State::Connected, HS_KEY_UPDATE) => self.key_update(body),
            // TLS 1.2
            (State::Cert12, HS_CERT) => {
                self.add_transcript(msg);
                let mut r = Reader { b: body };
                let mut list = Reader { b: r.vec(3)? };
                let mut chain = Vec::new();
                while !list.b.is_empty() {
                    chain.push(list.vec(3)?.to_vec());
                }
                self.check_chain(chain)?;
                self.state = State::Ske;
                Ok(())
            }
            (State::Ske, HS_SKE) => {
                self.add_transcript(msg);
                self.server_key_exchange(body)
            }
            (State::HelloDone, HS_CERT_REQ) => {
                self.add_transcript(msg);
                self.cert_requested = true;
                Ok(())
            }
            (State::HelloDone, HS_HELLO_DONE) => {
                self.add_transcript(msg);
                self.client_finish12()
            }
            (State::Finished12, HS_FINISHED) => {
                let expect = hash::prf12(self.suite.unwrap().hash, &self.master, b"server finished", &self.transcript_hash(), 12);
                if !crate::ct_eq(&expect, body) {
                    return Err(Error::BadSignature);
                }
                self.state = State::Connected;
                Ok(())
            }
            _ => Err(Error::Protocol),
        }
    }

    fn check_chain(&mut self, chain: Vec<Vec<u8>>) -> Result<(), Error> {
        let key = x509::verify_chain(&chain, &self.host, self.now, self.roots).map_err(Error::Certificate)?;
        self.server_key = Some(key);
        self.chain = chain;
        Ok(())
    }

    /// The certificate chain the server sent (leaf first).
    pub fn peer_certificates(&self) -> &[Vec<u8>] {
        &self.chain
    }

    fn check_signature(&self, scheme: u16, content: &[u8], sig: &[u8], tls13: bool) -> Result<(), Error> {
        let key = self.server_key.as_ref().ok_or(Error::Protocol)?;
        let alg = match scheme {
            0x0403 => SigAlg::Ecdsa(Alg::Sha256),
            0x0503 => SigAlg::Ecdsa(Alg::Sha384),
            0x0603 => SigAlg::Ecdsa(Alg::Sha512),
            0x0804 => SigAlg::RsaPss(Alg::Sha256),
            0x0805 => SigAlg::RsaPss(Alg::Sha384),
            0x0806 => SigAlg::RsaPss(Alg::Sha512),
            0x0401 if !tls13 => SigAlg::RsaPkcs1(Alg::Sha256),
            0x0501 if !tls13 => SigAlg::RsaPkcs1(Alg::Sha384),
            0x0601 if !tls13 => SigAlg::RsaPkcs1(Alg::Sha512),
            _ => return Err(Error::Unsupported),
        };
        if x509::verify(key, alg, content, sig) {
            Ok(())
        } else {
            Err(Error::BadSignature)
        }
    }

    fn parse_extensions_alpn(&mut self, body: &[u8]) -> Result<(), Error> {
        let mut r = Reader { b: body };
        let mut ext = Reader { b: r.vec(2)? };
        while !ext.b.is_empty() {
            let id = ext.u16()?;
            let data = ext.vec(2)?;
            if id == 16 {
                let mut a = Reader { b: data };
                let mut list = Reader { b: a.vec(2)? };
                let p = list.vec(1)?;
                self.alpn_chosen = Some(String::from_utf8_lossy(p).into_owned());
            }
        }
        Ok(())
    }

    fn server_hello(&mut self, msg: &[u8]) -> Result<(), Error> {
        let mut r = Reader { b: &msg[4..] };
        let _legacy = r.u16()?;
        let random: [u8; 32] = r.take(32)?.try_into().unwrap();
        let sid = r.vec(1)?;
        let suite_id = r.u16()?;
        let _comp = r.u8()?;
        let s = suite(suite_id).filter(|_| SUITES.contains(&suite_id)).ok_or(Error::Unsupported)?;
        let mut version = 0x0303;
        let mut key_share: Option<(u16, Vec<u8>)> = None;
        let mut cookie = None;
        let mut ems = false;
        let mut alpn = None;
        if !r.b.is_empty() {
            let mut ext = Reader { b: r.vec(2)? };
            while !ext.b.is_empty() {
                let id = ext.u16()?;
                let data = ext.vec(2)?;
                let mut d = Reader { b: data };
                match id {
                    43 => version = d.u16()?,
                    51 => {
                        let g = d.u16()?;
                        // A HelloRetryRequest names only the group.
                        let k = if d.b.is_empty() { Vec::new() } else { d.vec(2)?.to_vec() };
                        key_share = Some((g, k));
                    }
                    44 => cookie = Some(d.vec(2)?.to_vec()),
                    23 => ems = true,
                    16 => {
                        let mut list = Reader { b: d.vec(2)? };
                        alpn = Some(String::from_utf8_lossy(list.vec(1)?).into_owned());
                    }
                    _ => {}
                }
            }
        }
        let tls13 = version == 0x0304;
        if tls13 != s.tls13 {
            return Err(Error::Protocol);
        }
        if tls13 && sid != self.session_id {
            return Err(Error::Protocol);
        }
        // The hash of the suite starts the running transcript.
        if self.hasher.is_none() {
            let mut h = Hasher::new(s.hash);
            h.update(&self.transcript);
            self.hasher = Some(h);
        } else if self.suite.is_some_and(|old| old.id != s.id) {
            return Err(Error::Protocol);
        }
        self.suite = Some(s);
        if tls13 && random == HRR_RANDOM {
            return self.hello_retry(msg, key_share, cookie);
        }
        self.add_transcript(msg);
        self.server_random = random;
        if tls13 {
            let (group, public) = key_share.ok_or(Error::Protocol)?;
            let shared = self.shared_secret(group, &public)?;
            self.tls13_handshake_keys(&shared);
            self.state = State::EncExt;
        } else {
            // A TLS 1.3 client talking 1.2 must check the downgrade marker.
            if random[24..] == *b"DOWNGRD\x01" || random[24..] == *b"DOWNGRD\x00" {
                return Err(Error::Protocol);
            }
            self.ems = ems;
            self.alpn_chosen = alpn;
            self.state = State::Cert12;
        }
        Ok(())
    }

    fn hello_retry(&mut self, msg: &[u8], key_share: Option<(u16, Vec<u8>)>, cookie: Option<Vec<u8>>) -> Result<(), Error> {
        if self.retried {
            return Err(Error::Protocol);
        }
        self.retried = true;
        let (group, _) = key_share.ok_or(Error::Unsupported)?;
        let curve = match group {
            SECP256R1 => Curve::P256,
            SECP384R1 => Curve::P384,
            _ => return Err(Error::Unsupported),
        };
        let mut rnd = [0u8; 48];
        (self.rng)(&mut rnd);
        let (k, public) = ec::keypair(curve, &rnd);
        self.share = Share::Ec(curve, k);
        // The transcript restarts with the hash of the first ClientHello.
        let s = self.suite.unwrap();
        let ch1 = self.transcript_hash();
        let mut h = Hasher::new(s.hash);
        h.update(&[254, 0, 0, s.hash.len() as u8]);
        h.update(&ch1);
        h.update(msg);
        self.hasher = Some(h);
        let hello = self.client_hello(Some((group, public, cookie)));
        // Middlebox compatibility: a ChangeCipherSpec before the new hello.
        self.record_plain(CT_CCS, &[1]);
        self.send_handshake(&hello);
        Ok(())
    }

    fn shared_secret(&self, group: u16, public: &[u8]) -> Result<Vec<u8>, Error> {
        match (&self.share, group) {
            (Share::X25519(k), X25519) => {
                let p: [u8; 32] = public.try_into().map_err(|_| Error::Decode)?;
                let s = x25519::x25519(k, &p);
                if s.iter().all(|&b| b == 0) {
                    return Err(Error::Protocol);
                }
                Ok(s.to_vec())
            }
            (Share::Ec(curve, k), g) if (g == SECP256R1 && *curve == Curve::P256) || (g == SECP384R1 && *curve == Curve::P384) => {
                ec::shared(*curve, k, public).ok_or(Error::Decode)
            }
            _ => Err(Error::Protocol),
        }
    }

    fn tls13_handshake_keys(&mut self, shared: &[u8]) {
        let alg = self.suite.unwrap().hash;
        let zero = vec![0u8; alg.len()];
        let early = hash::hkdf_extract(alg, &[], &zero);
        let empty = hash::digest(alg, &[]);
        let derived = hash::expand_label(alg, &early, "derived", &empty, alg.len());
        self.handshake_secret = hash::hkdf_extract(alg, &derived, shared);
        let th = self.transcript_hash();
        self.client_hs = hash::expand_label(alg, &self.handshake_secret, "c hs traffic", &th, alg.len());
        self.server_hs = hash::expand_label(alg, &self.handshake_secret, "s hs traffic", &th, alg.len());
        self.read_keys = Some(self.traffic_keys(&self.server_hs.clone()));
    }

    fn traffic_keys(&self, secret: &[u8]) -> Keys {
        let s = self.suite.unwrap();
        let key = hash::expand_label(s.hash, secret, "key", &[], s.aead.key_len());
        let iv = hash::expand_label(s.hash, secret, "iv", &[], 12);
        Keys::new(s.aead, &key, &iv, false)
    }

    fn server_finished13(&mut self, msg: &[u8]) -> Result<(), Error> {
        let alg = self.suite.unwrap().hash;
        let fkey = hash::expand_label(alg, &self.server_hs, "finished", &[], alg.len());
        let expect = hash::hmac(alg, &fkey, &self.transcript_hash());
        if !crate::ct_eq(&expect, &msg[4..]) {
            return Err(Error::BadSignature);
        }
        self.add_transcript(msg);
        // Application secrets from the transcript up to the server's Finished.
        let th = self.transcript_hash();
        let empty = hash::digest(alg, &[]);
        let derived = hash::expand_label(alg, &self.handshake_secret, "derived", &empty, alg.len());
        let master = hash::hkdf_extract(alg, &derived, &vec![0u8; alg.len()]);
        self.client_app = hash::expand_label(alg, &master, "c ap traffic", &th, alg.len());
        self.server_app = hash::expand_label(alg, &master, "s ap traffic", &th, alg.len());
        // Our side: (middlebox) ChangeCipherSpec, empty Certificate if
        // asked, Finished, all under the handshake keys.
        if !self.retried {
            self.record_plain(CT_CCS, &[1]);
        }
        self.write_keys = Some(self.traffic_keys(&self.client_hs.clone()));
        if self.cert_requested {
            self.send_handshake(&[HS_CERT, 0, 0, 4, 0, 0, 0, 0]);
        }
        let ckey = hash::expand_label(alg, &self.client_hs, "finished", &[], alg.len());
        let verify = hash::hmac(alg, &ckey, &self.transcript_hash());
        let mut fin = vec![HS_FINISHED];
        put_u24(&mut fin, verify.len());
        fin.extend(verify);
        self.send_handshake(&fin);
        self.write_keys = Some(self.traffic_keys(&self.client_app.clone()));
        self.read_keys = Some(self.traffic_keys(&self.server_app.clone()));
        self.state = State::Connected;
        Ok(())
    }

    fn key_update(&mut self, body: &[u8]) -> Result<(), Error> {
        let alg = self.suite.unwrap().hash;
        self.server_app = hash::expand_label(alg, &self.server_app, "traffic upd", &[], alg.len());
        self.read_keys = Some(self.traffic_keys(&self.server_app.clone()));
        if body.first() == Some(&1) {
            self.record(CT_HANDSHAKE, &[HS_KEY_UPDATE, 0, 0, 1, 0]);
            self.client_app = hash::expand_label(alg, &self.client_app, "traffic upd", &[], alg.len());
            self.write_keys = Some(self.traffic_keys(&self.client_app.clone()));
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // TLS 1.2

    fn server_key_exchange(&mut self, body: &[u8]) -> Result<(), Error> {
        let mut r = Reader { b: body };
        if r.u8()? != 3 {
            return Err(Error::Unsupported); // named_curve only
        }
        let group = r.u16()?;
        let public = r.vec(1)?.to_vec();
        let params_len = body.len() - r.b.len();
        let scheme = r.u16()?;
        let sig = r.vec(2)?;
        let mut content = self.client_random.to_vec();
        content.extend_from_slice(&self.server_random);
        content.extend_from_slice(&body[..params_len]);
        self.check_signature(scheme, &content, sig, false)?;
        // Our key for the group the server chose.
        let (ours, shared) = match group {
            X25519 => {
                let Share::X25519(k) = &self.share else { return Err(Error::Protocol) };
                let p: [u8; 32] = public.as_slice().try_into().map_err(|_| Error::Decode)?;
                (x25519::public(k).to_vec(), x25519::x25519(k, &p).to_vec())
            }
            SECP256R1 | SECP384R1 => {
                let curve = if group == SECP256R1 { Curve::P256 } else { Curve::P384 };
                let mut rnd = [0u8; 48];
                (self.rng)(&mut rnd);
                let (k, pubkey) = ec::keypair(curve, &rnd);
                let s = ec::shared(curve, &k, &public).ok_or(Error::Decode)?;
                (pubkey, s)
            }
            _ => return Err(Error::Unsupported),
        };
        self.share_tls12 = Some((ours, shared));
        self.state = State::HelloDone;
        Ok(())
    }

    fn client_finish12(&mut self) -> Result<(), Error> {
        let s = self.suite.unwrap();
        let (ours, pms) = self.share_tls12.take().ok_or(Error::Protocol)?;
        if self.cert_requested {
            self.send_handshake(&[HS_CERT, 0, 0, 3, 0, 0, 0]);
        }
        let mut cke = vec![HS_CKE];
        put_u24(&mut cke, ours.len() + 1);
        put_vec(&mut cke, 1, &ours);
        self.send_handshake(&cke);
        self.master = if self.ems {
            hash::prf12(s.hash, &pms, b"extended master secret", &self.transcript_hash(), 48)
        } else {
            let mut seed = self.client_random.to_vec();
            seed.extend_from_slice(&self.server_random);
            hash::prf12(s.hash, &pms, b"master secret", &seed, 48)
        };
        let (klen, ivlen) = match s.aead {
            Aead::ChaCha => (32, 12),
            a => (a.key_len(), 4),
        };
        let mut seed = self.server_random.to_vec();
        seed.extend_from_slice(&self.client_random);
        let kb = hash::prf12(s.hash, &self.master, b"key expansion", &seed, 2 * klen + 2 * ivlen);
        let explicit = s.aead != Aead::ChaCha;
        let client = Keys::new(s.aead, &kb[..klen], &kb[2 * klen..2 * klen + ivlen], explicit);
        let server = Keys::new(s.aead, &kb[klen..2 * klen], &kb[2 * klen + ivlen..2 * klen + 2 * ivlen], explicit);
        self.record_plain(CT_CCS, &[1]);
        self.write_keys = Some(client);
        self.pending_read = Some(server);
        let verify = hash::prf12(s.hash, &self.master, b"client finished", &self.transcript_hash(), 12);
        let mut fin = vec![HS_FINISHED, 0, 0, 12];
        fin.extend(verify);
        self.send_handshake(&fin);
        self.state = State::Ccs12;
        Ok(())
    }

    fn server_ccs(&mut self) -> Result<(), Error> {
        self.read_keys = Some(self.pending_read.take().ok_or(Error::Protocol)?);
        self.state = State::Finished12;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Application data

    /// Is the handshake still going on?
    pub fn handshaking(&self) -> bool {
        !matches!(self.state, State::Connected | State::Closed)
    }

    pub fn connected(&self) -> bool {
        self.state == State::Connected
    }

    /// The server closed the connection (close_notify).
    pub fn peer_closed(&self) -> bool {
        self.peer_closed
    }

    /// Queues application data (after the handshake).
    pub fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        if let Some(e) = self.error {
            return Err(e);
        }
        if self.state != State::Connected {
            return Err(Error::Protocol);
        }
        self.record(CT_APP, data);
        Ok(())
    }

    /// Takes decrypted application data.
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        let n = buf.len().min(self.plain.len());
        for (d, s) in buf.iter_mut().zip(self.plain.drain(..n)) {
            *d = s;
        }
        n
    }

    pub fn pending(&self) -> usize {
        self.plain.len()
    }

    /// Says goodbye (close_notify).
    pub fn close(&mut self) {
        if self.state == State::Connected {
            self.record(CT_ALERT, &[1, 0]);
        }
        self.state = State::Closed;
    }

    pub fn version(&self) -> &'static str {
        match self.suite {
            Some(s) if s.tls13 => "TLS 1.3",
            Some(_) => "TLS 1.2",
            None => "",
        }
    }
}

fn x509_ip(host: &str) -> Option<()> {
    let mut parts = 0;
    for p in host.split('.') {
        p.parse::<u8>().ok()?;
        parts += 1;
    }
    (parts == 4).then_some(())
}
