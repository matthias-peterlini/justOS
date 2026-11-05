//! X.509 certificates (RFC 5280): DER parsing, signature checks and the
//! path from a server's certificate to a trusted root.

use crate::ec::{self, Curve};
use crate::hash::{self, Alg};
use crate::rsa;
use alloc::vec::Vec;

// ----------------------------------------------------------------------------
// DER

/// One TLV: tag, contents, and the whole encoding.
#[derive(Clone, Copy)]
pub struct Tlv<'a> {
    pub tag: u8,
    pub body: &'a [u8],
    pub raw: &'a [u8],
}

/// Reads one TLV from the start of `b`; returns it and what follows.
pub fn tlv(b: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
    let tag = *b.first()?;
    let l0 = *b.get(1)? as usize;
    let (len, hdr) = if l0 < 0x80 {
        (l0, 2)
    } else {
        let n = l0 & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | *b.get(2 + i)? as usize;
        }
        (len, 2 + n)
    };
    let end = hdr.checked_add(len)?;
    if end > b.len() {
        return None;
    }
    Some((Tlv { tag, body: &b[hdr..end], raw: &b[..end] }, &b[end..]))
}

/// The children of a constructed TLV.
pub fn children(b: &[u8]) -> Option<Vec<Tlv<'_>>> {
    let mut out = Vec::new();
    let mut rest = b;
    while !rest.is_empty() {
        let (t, r) = tlv(rest)?;
        out.push(t);
        rest = r;
    }
    Some(out)
}

/// An INTEGER's magnitude without the sign byte.
pub fn uint(b: &[u8]) -> &[u8] {
    let z = b.iter().take_while(|&&x| x == 0).count();
    &b[z.min(b.len().saturating_sub(1))..]
}

const OID_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_RSA_PSS: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0a];
const OID_RSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];
const OID_RSA_SHA384: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0c];
const OID_RSA_SHA512: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0d];
const OID_EC: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_P384: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];
const OID_ECDSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
const OID_ECDSA_SHA384: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x03];
const OID_ECDSA_SHA512: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x04];
const OID_SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
const OID_SHA384: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02];
const OID_SHA512: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03];
const OID_SAN: &[u8] = &[0x55, 0x1d, 0x11];
const OID_BASIC: &[u8] = &[0x55, 0x1d, 0x13];

// ----------------------------------------------------------------------------
// Keys and signatures

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublicKey {
    Rsa { n: Vec<u8>, e: Vec<u8> },
    Ec { curve: Curve, point: Vec<u8> },
}

/// A SubjectPublicKeyInfo.
pub fn parse_spki(der: &[u8]) -> Option<PublicKey> {
    let (spki, _) = tlv(der)?;
    let parts = children(spki.body)?;
    let alg = children(parts.first()?.body)?;
    let oid = alg.first()?.body;
    let bits = parts.get(1)?.body;
    let key = bits.get(1..)?; // skip the unused-bits byte
    if oid == OID_RSA {
        let (seq, _) = tlv(key)?;
        let ne = children(seq.body)?;
        Some(PublicKey::Rsa { n: uint(ne.first()?.body).to_vec(), e: uint(ne.get(1)?.body).to_vec() })
    } else if oid == OID_EC {
        let curve = match alg.get(1)?.body {
            o if o == OID_P256 => Curve::P256,
            o if o == OID_P384 => Curve::P384,
            _ => return None,
        };
        Some(PublicKey::Ec { curve, point: key.to_vec() })
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SigAlg {
    RsaPkcs1(Alg),
    RsaPss(Alg),
    Ecdsa(Alg),
}

fn hash_of_pss(params: &[u8]) -> Option<Alg> {
    // RSASSA-PSS-params: [0] hashAlgorithm (default SHA-1, refused).
    let (seq, _) = tlv(params)?;
    let fields = children(seq.body)?;
    let h = fields.iter().find(|t| t.tag == 0xa0)?;
    let (alg, _) = tlv(h.body)?;
    let oid = children(alg.body)?.first()?.body;
    match oid {
        o if o == OID_SHA256 => Some(Alg::Sha256),
        o if o == OID_SHA384 => Some(Alg::Sha384),
        o if o == OID_SHA512 => Some(Alg::Sha512),
        _ => None,
    }
}

fn parse_sigalg(t: &Tlv<'_>) -> Option<SigAlg> {
    let f = children(t.body)?;
    let oid = f.first()?.body;
    Some(match oid {
        o if o == OID_RSA_SHA256 => SigAlg::RsaPkcs1(Alg::Sha256),
        o if o == OID_RSA_SHA384 => SigAlg::RsaPkcs1(Alg::Sha384),
        o if o == OID_RSA_SHA512 => SigAlg::RsaPkcs1(Alg::Sha512),
        o if o == OID_ECDSA_SHA256 => SigAlg::Ecdsa(Alg::Sha256),
        o if o == OID_ECDSA_SHA384 => SigAlg::Ecdsa(Alg::Sha384),
        o if o == OID_ECDSA_SHA512 => SigAlg::Ecdsa(Alg::Sha512),
        o if o == OID_RSA_PSS => SigAlg::RsaPss(hash_of_pss(f.get(1)?.raw)?),
        _ => return None,
    })
}

/// Checks `sig` over `msg`. ECDSA signatures are DER (r, s).
pub fn verify(key: &PublicKey, alg: SigAlg, msg: &[u8], sig: &[u8]) -> bool {
    match (key, alg) {
        (PublicKey::Rsa { n, e }, SigAlg::RsaPkcs1(h)) => rsa::verify_pkcs1(n, e, h, msg, sig),
        (PublicKey::Rsa { n, e }, SigAlg::RsaPss(h)) => rsa::verify_pss(n, e, h, msg, sig),
        (PublicKey::Ec { curve, point }, SigAlg::Ecdsa(h)) => {
            let Some((seq, _)) = tlv(sig) else { return false };
            let Some(rs) = children(seq.body) else { return false };
            if rs.len() != 2 {
                return false;
            }
            ec::verify(*curve, point, &hash::digest(h, msg), uint(rs[0].body), uint(rs[1].body))
        }
        _ => false,
    }
}

// ----------------------------------------------------------------------------
// Certificates

pub struct Cert<'a> {
    pub tbs: &'a [u8],
    pub sig_alg: Option<SigAlg>,
    pub sig: &'a [u8],
    pub issuer: &'a [u8],
    pub subject: &'a [u8],
    pub not_before: u64,
    pub not_after: u64,
    pub spki: &'a [u8],
    pub is_ca: bool,
    /// DNS names and IP addresses it is for.
    pub dns: Vec<&'a [u8]>,
    pub ips: Vec<&'a [u8]>,
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// UTCTime or GeneralizedTime as seconds since 1970.
fn parse_time(t: &Tlv<'_>) -> Option<u64> {
    let s = core::str::from_utf8(t.body).ok()?;
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (year, rest) = match t.tag {
        0x17 => {
            let y = num(0, 2)?;
            (if y >= 50 { 1900 + y } else { 2000 + y }, 2)
        }
        0x18 => (num(0, 4)?, 4),
        _ => return None,
    };
    let (mo, d, h, mi) = (num(rest, rest + 2)?, num(rest + 2, rest + 4)?, num(rest + 4, rest + 6)?, num(rest + 6, rest + 8)?);
    let sec = num(rest + 8, rest + 10).unwrap_or(0);
    let days = days_from_civil(year, mo, d);
    Some((days * 86400 + h * 3600 + mi * 60 + sec).max(0) as u64)
}

pub fn parse(der: &[u8]) -> Option<Cert<'_>> {
    let (cert, _) = tlv(der)?;
    let top = children(cert.body)?;
    let tbs = top.first()?;
    let sig_alg = parse_sigalg(top.get(1)?);
    let sig = top.get(2)?.body.get(1..)?;
    let f = children(tbs.body)?;
    // The version is optional ([0]); everything moves by one when present.
    let o = if f.first()?.tag == 0xa0 { 1 } else { 0 };
    let issuer = f.get(o + 2)?.raw;
    let validity = children(f.get(o + 3)?.body)?;
    let subject = f.get(o + 4)?.raw;
    let spki = f.get(o + 5)?.raw;
    let mut c = Cert {
        tbs: tbs.raw,
        sig_alg,
        sig,
        issuer,
        subject,
        not_before: parse_time(validity.first()?)?,
        not_after: parse_time(validity.get(1)?)?,
        spki,
        is_ca: false,
        dns: Vec::new(),
        ips: Vec::new(),
    };
    if let Some(ext) = f.iter().skip(o + 6).find(|t| t.tag == 0xa3) {
        let (seq, _) = tlv(ext.body)?;
        for e in children(seq.body)? {
            let parts = children(e.body)?;
            let oid = parts.first()?.body;
            let value = parts.last()?.body; // the OCTET STRING (after "critical")
            if oid == OID_SAN {
                let (names, _) = tlv(value)?;
                for n in children(names.body)? {
                    match n.tag {
                        0x82 => c.dns.push(n.body),
                        0x87 => c.ips.push(n.body),
                        _ => {}
                    }
                }
            } else if oid == OID_BASIC {
                let (bc, _) = tlv(value)?;
                c.is_ca = children(bc.body)?.first().is_some_and(|t| t.tag == 0x01 && t.body.first() == Some(&0xff));
            }
        }
    }
    Some(c)
}

/// Does `pattern` (from a certificate, maybe "*.example.com") name `host`?
pub fn name_matches(pattern: &[u8], host: &str) -> bool {
    let p = core::str::from_utf8(pattern).unwrap_or("").trim_end_matches('.');
    let h = host.trim_end_matches('.');
    if let Some(suffix) = p.strip_prefix("*.") {
        // One label only, never a bare suffix.
        match h.split_once('.') {
            Some((first, rest)) => !first.is_empty() && rest.eq_ignore_ascii_case(suffix),
            None => false,
        }
    } else {
        p.eq_ignore_ascii_case(h)
    }
}

fn ip_bytes(host: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut it = host.split('.');
    for b in out.iter_mut() {
        *b = it.next()?.parse().ok()?;
    }
    it.next().is_none().then_some(out)
}

/// A trusted root: its name and key.
pub struct Root {
    pub subject: Vec<u8>,
    pub key: PublicKey,
}

/// The roots in the `ARCR` format of `tools/certs` (subject and SPKI of each).
pub fn parse_roots(b: &[u8]) -> Vec<Root> {
    let mut out = Vec::new();
    if b.get(..4) != Some(b"ARCR") {
        return out;
    }
    let mut i = 8;
    let u16_at = |i: usize| b.get(i..i + 2).map(|x| u16::from_be_bytes([x[0], x[1]]) as usize);
    while let Some(sl) = u16_at(i) {
        let Some(subject) = b.get(i + 2..i + 2 + sl) else { break };
        let Some(kl) = u16_at(i + 2 + sl) else { break };
        let Some(spki) = b.get(i + 4 + sl..i + 4 + sl + kl) else { break };
        if let Some(key) = parse_spki(spki) {
            out.push(Root { subject: subject.to_vec(), key });
        }
        i += 4 + sl + kl;
    }
    out
}

fn signed_by(c: &Cert<'_>, key: &PublicKey) -> bool {
    match c.sig_alg {
        Some(alg) => verify(key, alg, c.tbs, c.sig),
        None => false,
    }
}

/// Checks the server's chain (leaf first) for `host` at time `now`
/// (seconds since 1970, give or take a day; 0 skips the dates). Returns the
/// leaf's key.
pub fn verify_chain(chain: &[Vec<u8>], host: &str, now: u64, roots: &[Root]) -> Result<PublicKey, &'static str> {
    let certs: Vec<Cert<'_>> = chain.iter().map(|d| parse(d)).collect::<Option<_>>().ok_or("bad certificate")?;
    let leaf = certs.first().ok_or("no certificate")?;
    let name_ok = match ip_bytes(host) {
        Some(ip) => leaf.ips.iter().any(|i| *i == ip),
        None => leaf.dns.iter().any(|n| name_matches(n, host)),
    };
    if !name_ok {
        return Err("the certificate is for another site");
    }
    let leaf_key = parse_spki(leaf.spki).ok_or("unsupported key in the certificate")?;
    let mut cur = 0usize;
    for _ in 0..8 {
        let c = &certs[cur];
        // A day of leeway: many computers keep local time in their clock.
        if now != 0 && (now + 86_400 < c.not_before || now > c.not_after + 86_400) {
            return Err("the certificate has expired or is not valid yet");
        }
        // A trusted root that issued it?
        if roots.iter().any(|r| r.subject == c.issuer && signed_by(c, &r.key)) {
            return Ok(leaf_key);
        }
        // Or one of the certificates sent along (a CA).
        let next = certs.iter().enumerate().find(|(i, n)| {
            *i != cur && n.subject == c.issuer && n.is_ca && parse_spki(n.spki).is_some_and(|k| signed_by(c, &k))
        });
        match next {
            Some((i, _)) => cur = i,
            None => return Err("the certificate is not from a trusted authority"),
        }
    }
    Err("the certificate chain is too long")
}
