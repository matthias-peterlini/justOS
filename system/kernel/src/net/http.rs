//! An HTTP/1.1 client over TCP, with TLS for `https:` ([`justtls`]):
//! redirects, chunked and sized bodies, gzip, progress reports and an
//! optional proxy (`sys:/system/proxy`, "host:port", reached with CONNECT).
//!
//! Server certificates are checked against Mozilla's roots
//! (`sys:/system/certs.bin`) and any extra ones the user trusts
//! (`sys:/system/certs/*.der`).

use super::tcp::{self, Conn};
use super::Ip;
use crate::sync::IrqLock;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use justtls::tls::Client;
use justtls::x509::{self, Root};

const CONNECT_MS: u64 = 15_000;
const READ_MS: u64 = 30_000;
pub const USER_AGENT: &str = "Mozilla/5.0 (justOS; x86_64) justweb/1.0";

static ROOTS: IrqLock<Option<&'static [Root]>> = IrqLock::new(None);

/// The trusted roots, read once.
pub fn roots() -> &'static [Root] {
    if let Some(r) = *ROOTS.lock() {
        return r;
    }
    let mut v = crate::fs::abs("sys:/system/certs.bin").and_then(|p| crate::fs::read_file(&p)).map(|b| x509::parse_roots(&b)).unwrap_or_default();
    if let Ok(dir) = crate::fs::abs("sys:/system/certs") {
        for (e, _) in crate::fs::list_dir(&dir).unwrap_or_default() {
            if !e.name.ends_with(".der") {
                continue;
            }
            let Ok(der) = crate::fs::read_file(&dir.join(&e.name)) else { continue };
            if let Some(c) = x509::parse(&der) {
                if let Some(key) = x509::parse_spki(c.spki) {
                    v.push(Root { subject: c.subject.to_vec(), key });
                }
            }
        }
    }
    if v.is_empty() {
        crate::warn!("http", "no trusted roots: secure sites will not open");
    }
    let r: &'static [Root] = Box::leak(v.into_boxed_slice());
    *ROOTS.lock() = Some(r);
    r
}

/// A URL split into its parts.
#[derive(Clone, Debug)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    /// Path and query ("/a/b?c").
    pub path: String,
}

impl Url {
    pub fn parse(s: &str) -> Option<Url> {
        let s = s.trim();
        let (https, rest) = if let Some(r) = s.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = s.strip_prefix("http://") {
            (false, r)
        } else {
            return None;
        };
        let (authority, path) = match rest.find(['/', '?', '#']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let authority = authority.rsplit('@').next()?;
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, p.parse().ok()?),
            _ => (authority, if https { 443 } else { 80 }),
        };
        if host.is_empty() {
            return None;
        }
        let path = path.split('#').next().unwrap_or("/");
        let path = if path.starts_with('?') { format!("/{path}") } else if path.is_empty() { "/".to_string() } else { path.to_string() };
        Some(Url { https, host: host.to_ascii_lowercase(), port, path })
    }

    /// `location` (absolute, or relative to this URL).
    pub fn join(&self, location: &str) -> Option<Url> {
        if location.starts_with("http://") || location.starts_with("https://") {
            return Url::parse(location);
        }
        let mut u = self.clone();
        if let Some(rest) = location.strip_prefix("//") {
            return Url::parse(&format!("{}://{rest}", if self.https { "https" } else { "http" }));
        }
        if location.starts_with('/') {
            u.path = location.to_string();
        } else {
            let dir = match self.path.split('?').next().unwrap_or("/").rfind('/') {
                Some(i) => &self.path[..=i],
                None => "/",
            };
            u.path = format!("{dir}{location}");
        }
        Some(u)
    }

    pub fn origin(&self) -> String {
        let default = if self.https { 443 } else { 80 };
        let scheme = if self.https { "https" } else { "http" };
        if self.port == default {
            format!("{scheme}://{}", self.host)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }
}

impl core::fmt::Display for Url {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}{}", self.origin(), self.path)
    }
}

/// A connection, plain or secure.
pub struct Stream {
    conn: Conn,
    tls: Option<Client<'static>>,
    buf: Vec<u8>,
}

fn rng(out: &mut [u8]) {
    crate::rand::fill(out);
}

fn proxy() -> Option<(String, u16)> {
    let p = crate::fs::abs("sys:/system/proxy").and_then(|p| crate::fs::read_file(&p)).ok()?;
    let s = String::from_utf8_lossy(&p).trim().to_string();
    let (h, port) = s.rsplit_once(':')?;
    Some((h.to_string(), port.parse().ok()?))
}

fn resolve(host: &str) -> Result<Ip, String> {
    super::resolve(host).map_err(|e| format!("{host}: {e}"))
}

impl Stream {
    /// Opens a connection to the URL's host (through the proxy if one is
    /// set), with TLS for https.
    pub fn open(url: &Url) -> Result<Stream, String> {
        let conn = match proxy() {
            Some((ph, pp)) => {
                let c = tcp::connect(resolve(&ph)?, pp, CONNECT_MS).map_err(|e| format!("proxy: {e}"))?;
                let req = format!("CONNECT {0}:{1} HTTP/1.1\r\nHost: {0}:{1}\r\nUser-Agent: {USER_AGENT}\r\n\r\n", url.host, url.port);
                tcp::send(c, req.as_bytes(), READ_MS).map_err(|e| e.to_string())?;
                let mut s = Stream { conn: c, tls: None, buf: Vec::new() };
                let head = s.read_head()?;
                let ok = head.split_whitespace().nth(1).is_some_and(|code| code.starts_with('2'));
                if !ok {
                    tcp::close(c);
                    return Err(format!("the proxy refused: {}", head.lines().next().unwrap_or("")));
                }
                c
            }
            None => tcp::connect(resolve(&url.host)?, url.port, CONNECT_MS).map_err(|e| format!("{}: {e}", url.host))?,
        };
        let mut s = Stream { conn, tls: None, buf: Vec::new() };
        if url.https {
            let client = Client::new(&url.host, roots(), crate::time::unix_now(), rng, &["http/1.1"]);
            s.tls = Some(client);
            if let Err(e) = s.handshake() {
                tcp::close(conn);
                return Err(e);
            }
        }
        Ok(s)
    }

    fn flush_tls(&mut self) -> Result<(), String> {
        if let Some(t) = self.tls.as_mut() {
            let out = t.take_output();
            if !out.is_empty() {
                tcp::send(self.conn, &out, READ_MS).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    fn handshake(&mut self) -> Result<(), String> {
        let mut buf = alloc::vec![0u8; 16384];
        loop {
            self.flush_tls()?;
            let t = self.tls.as_ref().unwrap();
            if t.connected() {
                return Ok(());
            }
            let n = tcp::recv(self.conn, &mut buf, READ_MS).map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("the server closed the connection during the handshake".to_string());
            }
            let r = self.tls.as_mut().unwrap().feed(&buf[..n]);
            if let Err(e) = r {
                let _ = self.flush_tls();
                return Err(format!("secure connection failed: {}", e.describe()));
            }
        }
    }

    pub fn write(&mut self, data: &[u8]) -> Result<(), String> {
        match self.tls.as_mut() {
            Some(t) => {
                t.write(data).map_err(|e| e.describe().to_string())?;
                self.flush_tls()
            }
            None => tcp::send(self.conn, data, READ_MS).map(|_| ()).map_err(|e| e.to_string()),
        }
    }

    /// Reads some bytes (0: the other side closed).
    pub fn read(&mut self, out: &mut [u8]) -> Result<usize, String> {
        if !self.buf.is_empty() {
            let n = out.len().min(self.buf.len());
            out[..n].copy_from_slice(&self.buf[..n]);
            self.buf.drain(..n);
            return Ok(n);
        }
        let Some(_) = self.tls else {
            return tcp::recv(self.conn, out, READ_MS).map_err(|e| e.to_string());
        };
        let mut raw = alloc::vec![0u8; 16384];
        loop {
            let t = self.tls.as_mut().unwrap();
            if t.pending() > 0 {
                return Ok(t.read(out));
            }
            if t.peer_closed() {
                return Ok(0);
            }
            let n = tcp::recv(self.conn, &mut raw, READ_MS).map_err(|e| e.to_string())?;
            if n == 0 {
                return Ok(0);
            }
            let r = self.tls.as_mut().unwrap().feed(&raw[..n]);
            self.flush_tls()?;
            r.map_err(|e| e.describe().to_string())?;
        }
    }

    /// Reads up to the blank line after the headers; returns them.
    fn read_head(&mut self) -> Result<String, String> {
        let mut head = Vec::new();
        let mut b = [0u8; 1024];
        loop {
            if let Some(i) = find(&head, b"\r\n\r\n") {
                let rest = head.split_off(i + 4);
                self.buf.splice(0..0, rest);
                return Ok(String::from_utf8_lossy(&head).into_owned());
            }
            if head.len() > 64 * 1024 {
                return Err("the server's reply is too long".to_string());
            }
            let n = self.read(&mut b)?;
            if n == 0 {
                return Err("the server closed the connection".to_string());
            }
            head.extend_from_slice(&b[..n]);
        }
    }

    pub fn close(mut self) {
        if let Some(t) = self.tls.as_mut() {
            t.close();
        }
        let _ = self.flush_tls();
        tcp::close(self.conn);
    }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Where the content finally came from (after redirects).
    pub url: Url,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub struct Request<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    pub body: Option<&'a [u8]>,
}

impl<'a> Request<'a> {
    pub fn get(url: &'a str) -> Request<'a> {
        Request { method: "GET", url, headers: &[], body: None }
    }
}

/// Progress of a download: (bytes so far, total if known). Returning false
/// cancels it.
pub type Progress<'p> = &'p mut dyn FnMut(u64, Option<u64>) -> bool;

/// Receives the body of a reply as it arrives (server-sent events, long
/// downloads). Returning false stops reading.
pub type Sink<'s> = &'s mut dyn FnMut(&[u8]) -> bool;

/// Sends a request without following redirects (to read a `Location`).
pub fn fetch_once(req: &Request<'_>) -> Result<Response, String> {
    let url = Url::parse(req.url).ok_or_else(|| format!("not a web address: {}", req.url))?;
    let (status, headers, body) = once(&url, req.method, req.headers, req.body, &mut |_, _| true, None)?;
    Ok(Response { status, headers, body, url })
}

/// Sends a request and hands a successful reply's body to `sink` piece by
/// piece; an unsuccessful one comes back whole in the response.
#[allow(dead_code)] // for streamed answers
pub fn stream(req: &Request<'_>, sink: Sink<'_>) -> Result<Response, String> {
    let url = Url::parse(req.url).ok_or_else(|| format!("not a web address: {}", req.url))?;
    let (status, headers, body) = once(&url, req.method, req.headers, req.body, &mut |_, _| true, Some(sink))?;
    Ok(Response { status, headers, body, url })
}

/// Sends a request and reads the whole reply, following redirects.
pub fn fetch(req: &Request<'_>, progress: Progress<'_>) -> Result<Response, String> {
    let mut url = Url::parse(req.url).ok_or_else(|| format!("not a web address: {}", req.url))?;
    let mut method = req.method;
    let mut body = req.body;
    for _ in 0..10 {
        let (status, headers, data) = once(&url, method, req.headers, body, progress, None)?;
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            if let Some(loc) = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("location")).map(|(_, v)| v.clone()) {
                url = url.join(&loc).ok_or("bad redirect")?;
                if status == 303 || ((status == 301 || status == 302) && method == "POST") {
                    method = "GET";
                    body = None;
                }
                continue;
            }
        }
        let data = match headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-encoding")).map(|(_, v)| v.to_ascii_lowercase()) {
            Some(e) if e.contains("gzip") => justzip::gunzip(&data).ok_or("bad gzip data")?,
            Some(e) if e.contains("deflate") => justzip::zlib(&data).ok_or("bad deflate data")?,
            _ => data,
        };
        return Ok(Response { status, headers, body: data, url });
    }
    Err("too many redirects".to_string())
}

pub fn get(url: &str) -> Result<Response, String> {
    fetch(&Request::get(url), &mut |_, _| true)
}

type Reply = (u16, Vec<(String, String)>, Vec<u8>);

fn once(url: &Url, method: &str, extra: &[(&str, &str)], body: Option<&[u8]>, progress: Progress<'_>, mut sink: Option<Sink<'_>>) -> Result<Reply, String> {
    let mut s = Stream::open(url)?;
    let host = if url.port == if url.https { 443 } else { 80 } { url.host.clone() } else { format!("{}:{}", url.host, url.port) };
    let mut req = format!("{method} {} HTTP/1.1\r\nHost: {host}\r\n", url.path);
    let has = |name: &str| extra.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    if !has("user-agent") {
        req.push_str(&format!("User-Agent: {USER_AGENT}\r\n"));
    }
    if !has("accept") {
        req.push_str("Accept: */*\r\n");
    }
    req.push_str("Accept-Encoding: gzip\r\nConnection: close\r\n");
    for (k, v) in extra {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    let mut out = req.into_bytes();
    if let Some(b) = body {
        out.extend_from_slice(b);
    }
    s.write(&out)?;
    let head = s.read_head()?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or("bad reply from the server")?;
    let headers: Vec<(String, String)> =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    let get = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str());
    let chunked = get("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let length: Option<u64> = get("content-length").and_then(|v| v.parse().ok());
    let mut data = Vec::new();
    let no_body = method == "HEAD" || status == 204 || status == 304 || (100..200).contains(&status);
    if !(200..300).contains(&status) {
        sink = None;
    }
    if !no_body {
        if chunked {
            read_chunked(&mut s, &mut data, progress, &mut sink)?;
        } else {
            let mut buf = alloc::vec![0u8; 32768];
            if let Some(len) = length {
                data.reserve(len.min(256 << 20) as usize);
            }
            loop {
                if length.is_some_and(|l| data.len() as u64 >= l) {
                    break;
                }
                let n = s.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                if let Some(f) = sink.as_mut() {
                    if !f(&buf[..n]) {
                        s.close();
                        return Ok((status, headers, data));
                    }
                    continue;
                }
                data.extend_from_slice(&buf[..n]);
                if !progress(data.len() as u64, length) {
                    s.close();
                    return Err("cancelled".to_string());
                }
            }
            if let (Some(l), None) = (length, sink.as_ref()) {
                if (data.len() as u64) < l {
                    s.close();
                    return Err("the download was cut short".to_string());
                }
                data.truncate(l as usize);
            }
        }
    }
    s.close();
    Ok((status, headers, data))
}

fn read_line(s: &mut Stream) -> Result<String, String> {
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        if s.read(&mut b)? == 0 {
            return Err("the download was cut short".to_string());
        }
        if b[0] == b'\n' {
            break;
        }
        if b[0] != b'\r' {
            line.push(b[0]);
        }
        if line.len() > 4096 {
            return Err("bad chunk".to_string());
        }
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

fn read_chunked(s: &mut Stream, data: &mut Vec<u8>, progress: Progress<'_>, sink: &mut Option<Sink<'_>>) -> Result<(), String> {
    let mut buf = alloc::vec![0u8; 32768];
    loop {
        let line = read_line(s)?;
        let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| "bad chunk")?;
        if size == 0 {
            // Trailers until the blank line.
            while !read_line(s)?.is_empty() {}
            return Ok(());
        }
        let mut left = size;
        while left > 0 {
            let n = s.read(&mut buf[..left.min(32768)])?;
            if n == 0 {
                return Err("the download was cut short".to_string());
            }
            left -= n;
            if let Some(f) = sink.as_mut() {
                if !f(&buf[..n]) {
                    return Ok(());
                }
                continue;
            }
            data.extend_from_slice(&buf[..n]);
            if !progress(data.len() as u64, None) {
                return Err("cancelled".to_string());
            }
        }
        read_line(s)?;
    }
}
