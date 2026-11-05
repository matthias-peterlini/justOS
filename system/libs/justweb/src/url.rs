//! Web addresses: resolving a link against the page it is on, and
//! form encoding.

use alloc::format;
use alloc::string::{String, ToString};

/// Splits "scheme://host/path?query#frag" into (scheme, authority, rest).
fn split(url: &str) -> Option<(&str, &str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some((scheme, &rest[..end], &rest[end..]))
}

/// `link` resolved against `base` (RFC 3986, the common cases).
pub fn join(base: &str, link: &str) -> String {
    let link = link.trim();
    let link: String = link.chars().filter(|c| !matches!(c, '\n' | '\r' | '\t')).collect();
    if link.is_empty() {
        return base.split('#').next().unwrap_or(base).to_string();
    }
    if let Some(colon) = link.find(':') {
        let scheme = &link[..colon];
        if !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') && !scheme.contains('/') {
            return link;
        }
    }
    let Some((scheme, auth, path)) = split(base) else { return link };
    if let Some(rest) = link.strip_prefix("//") {
        return format!("{scheme}://{rest}");
    }
    if link.starts_with('/') {
        return format!("{scheme}://{auth}{}", normalize(&link));
    }
    let path_only = path.split(['?', '#']).next().unwrap_or("");
    if link.starts_with('?') {
        return format!("{scheme}://{auth}{}{link}", if path_only.is_empty() { "/" } else { path_only });
    }
    if link.starts_with('#') {
        let no_frag = path.split('#').next().unwrap_or("");
        return format!("{scheme}://{auth}{}{link}", if no_frag.is_empty() { "/" } else { no_frag });
    }
    let dir = match path_only.rfind('/') {
        Some(i) => &path_only[..=i],
        None => "/",
    };
    format!("{scheme}://{auth}{}", normalize(&format!("{dir}{link}")))
}

/// Removes "." and ".." segments.
fn normalize(path: &str) -> String {
    let (p, tail) = match path.find(['?', '#']) {
        Some(i) => (&path[..i], &path[i..]),
        None => (path, ""),
    };
    let mut out: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    let segs: alloc::vec::Vec<&str> = p.split('/').collect();
    for (i, s) in segs.iter().enumerate() {
        match *s {
            "." => {
                if i == segs.len() - 1 {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if i == segs.len() - 1 {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let mut r = out.join("/");
    if !r.starts_with('/') {
        r.insert(0, '/');
    }
    r + tail
}

/// The host of an address ("example.com").
pub fn host(url: &str) -> &str {
    let Some((_, auth, _)) = split(url) else { return "" };
    let auth = auth.rsplit('@').next().unwrap_or(auth);
    auth.split(':').next().unwrap_or(auth)
}

/// Percent-encodes a form value (application/x-www-form-urlencoded).
pub fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => out.push(b as char),
            b' ' => out.push('+'),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Decodes %XX escapes.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = alloc::vec::Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(core::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins() {
        let b = "https://a.org/x/y/page.html?q=1#top";
        assert_eq!(join(b, "z.png"), "https://a.org/x/y/z.png");
        assert_eq!(join(b, "../z.png"), "https://a.org/x/z.png");
        assert_eq!(join(b, "/r"), "https://a.org/r");
        assert_eq!(join(b, "//cdn.org/s.css"), "https://cdn.org/s.css");
        assert_eq!(join(b, "?p=2"), "https://a.org/x/y/page.html?p=2");
        assert_eq!(join(b, "#sec"), "https://a.org/x/y/page.html?q=1#sec");
        assert_eq!(join(b, "mailto:a@b"), "mailto:a@b");
        assert_eq!(join("https://a.org", "b"), "https://a.org/b");
        assert_eq!(host(b), "a.org");
        assert_eq!(form_encode("a b&c=é"), "a+b%26c%3D%C3%A9");
        assert_eq!(percent_decode("a%20b%C3%A9"), "a bé");
    }
}
