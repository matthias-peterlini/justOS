//! Fetches a page over HTTPS with justtls, from the host (for testing):
//!
//!     cargo run --example fetch --target x86_64-unknown-linux-gnu -- \
//!         HOST PORT PATH ROOT.der [PROXY_HOST:PORT]

use justtls::tls::Client;
use justtls::x509;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

fn rng(out: &mut [u8]) {
    let mut f = std::fs::File::open("/dev/urandom").unwrap();
    f.read_exact(out).unwrap();
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (host, port, path, root) = (&a[1], &a[2], &a[3], &a[4]);
    let der = std::fs::read(root).unwrap();
    // A DER certificate, or a PEM bundle.
    let mut roots = Vec::new();
    let mut add = |d: &[u8]| {
        if let Some(c) = x509::parse(d) {
            if let Some(k) = x509::parse_spki(c.spki) {
                roots.push(x509::Root { subject: c.subject.to_vec(), key: k });
            }
        }
    };
    if der.starts_with(b"-----") {
        let text = String::from_utf8_lossy(&der);
        let mut b64 = String::new();
        for line in text.lines() {
            if line.starts_with("-----BEGIN") {
                b64.clear();
            } else if line.starts_with("-----END") {
                add(&base64(&b64));
            } else {
                b64.push_str(line.trim());
            }
        }
    } else {
        add(&der);
    }
    let mut sock = match a.get(5) {
        Some(proxy) => {
            let mut s = TcpStream::connect(proxy).unwrap();
            write!(s, "CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n").unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                eprint!("proxy: {line}");
                if line == "\r\n" {
                    break;
                }
            }
            s
        }
        None => TcpStream::connect(format!("{host}:{port}")).unwrap(),
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let t0 = std::time::Instant::now();
    let mut c = Client::new(host, &roots, now, rng, &["http/1.1"]);
    let mut buf = vec![0u8; 65536];
    let mut sent_request = false;
    let mut body = Vec::new();
    loop {
        let out = c.take_output();
        if !out.is_empty() {
            sock.write_all(&out).unwrap();
        }
        if c.connected() && !sent_request {
            eprintln!("connected: {} in {:?}, alpn {:?}", c.version(), t0.elapsed(), c.alpn_chosen);
            let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: JustOS\r\n\r\n");
            c.write(req.as_bytes()).unwrap();
            sent_request = true;
            continue;
        }
        let n = sock.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        if let Err(e) = c.feed(&buf[..n]) {
            let out = c.take_output();
            let _ = sock.write_all(&out);
            eprintln!("error: {e:?}: {}", e.describe());
            std::process::exit(1);
        }
        let mut p = vec![0u8; c.pending()];
        let k = c.read(&mut p);
        body.extend_from_slice(&p[..k]);
        if c.peer_closed() {
            break;
        }
    }
    eprintln!("{} bytes in {:?}", body.len(), t0.elapsed());
    let text = String::from_utf8_lossy(&body);
    println!("{}", text.chars().take(400).collect::<String>());
}

fn base64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::new();
    for ch in bytes.chunks(4) {
        let mut v = 0u32;
        for (i, &c) in ch.iter().enumerate() {
            v |= (val(c) as u32) << (18 - 6 * i);
        }
        out.push((v >> 16) as u8);
        if ch.len() > 2 {
            out.push((v >> 8) as u8);
        }
        if ch.len() > 3 {
            out.push(v as u8);
        }
    }
    out
}
