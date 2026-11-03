//! The signed index of app updates (`kernel/src/appupdate.rs` reads it).
//!
//! ```text
//! arctool updates <store dir> <out dir> --key <file> --base-url <url> [--serial <n>] [--system <version>]
//! arctool pubkey <key file>
//! ```
//!
//! `<store dir>` holds the built store (`build/root/store/<id>/{app,info,
//! icon.qoi}`); its arcOS apps go in the index. The out folder gets each as
//! `<id>-<version>.app` (and `.qoi`), `index.json` and `index.json.sig`;
//! publish it at `<url>`. The key file is a private key: 32 bytes in hex.
//! The serial defaults to the time, so a newer index always has a higher
//! one; `--system` is the oldest arcOS the apps run on.

use arcjson::Value;
use arctls::{ed25519, sha2::Sha256};
use std::fs;
use std::path::Path;

type Result<T> = std::result::Result<T, String>;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn read_key(file: &str) -> Result<[u8; 32]> {
    let text = fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
    let t = text.trim();
    let bytes: Option<Vec<u8>> = (0..t.len()).step_by(2).map(|i| t.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok())).collect();
    bytes.and_then(|b| b.try_into().ok()).ok_or_else(|| format!("{file}: not a key (32 bytes in hex)"))
}

pub fn cmd_pubkey(args: &[String]) -> Result<()> {
    let [file] = args else { return Err("usage: arctool pubkey <key file>".into()) };
    let pk = ed25519::public_key(&read_key(file)?);
    println!("{}", hex(&pk));
    let list: Vec<String> = pk.iter().map(|b| format!("0x{b:02x}")).collect();
    println!("[{}]", list.join(", "));
    Ok(())
}

fn version_of(info: &str) -> String {
    info.lines().find_map(|l| l.strip_prefix("version=")).unwrap_or("").trim().to_string()
}

fn file_entry(base: &str, name: &str, data: &[u8]) -> Value {
    Value::object(vec![
        ("url", Value::string(&format!("{}/{name}", base.trim_end_matches('/')))),
        ("sha256", Value::string(&hex(&Sha256::digest(data)))),
        ("size", Value::Num(data.len() as f64)),
    ])
}

pub fn cmd_updates(args: &[String]) -> Result<()> {
    let usage = "usage: arctool updates <store dir> <out dir> --key <file> --base-url <url> [--serial <n>] [--system <version>]";
    let (mut pos, mut key, mut base, mut serial, mut system) = (Vec::new(), None, None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--key" => key = it.next().cloned(),
            "--base-url" => base = it.next().cloned(),
            "--system" => system = it.next().cloned(),
            "--serial" => serial = it.next().and_then(|s| s.parse::<u64>().ok()),
            _ => pos.push(a.clone()),
        }
    }
    let ([store, out], Some(key), Some(base)) = (pos.as_slice(), key, base) else { return Err(usage.into()) };
    let seed = read_key(&key)?;
    let serial = serial.unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    fs::create_dir_all(out).map_err(|e| format!("{out}: {e}"))?;
    let mut ids: Vec<String> = fs::read_dir(store)
        .map_err(|e| format!("{store}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("info").is_file() && e.path().join("app").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    let mut apps = Vec::new();
    for id in &ids {
        let dir = Path::new(store).join(id);
        let info = fs::read_to_string(dir.join("info")).map_err(|e| format!("{id}/info: {e}"))?;
        // Linux and Windows programs update from their own releases.
        if !info.lines().any(|l| l.trim() == "type=native") {
            continue;
        }
        let version = version_of(&info);
        if version.is_empty() {
            return Err(format!("{id}/info has no version"));
        }
        let app = fs::read(dir.join("app")).map_err(|e| format!("{id}/app: {e}"))?;
        let app_name = format!("{id}-{version}.app");
        fs::write(Path::new(out).join(&app_name), &app).map_err(|e| format!("{app_name}: {e}"))?;
        let mut entry = vec![("id", Value::string(id)), ("app", file_entry(&base, &app_name, &app))];
        if let Some(min) = &system {
            entry.push(("system", Value::string(min)));
        }
        if let Ok(icon) = fs::read(dir.join("icon.qoi")) {
            let icon_name = format!("{id}-{version}.qoi");
            fs::write(Path::new(out).join(&icon_name), &icon).map_err(|e| format!("{icon_name}: {e}"))?;
            entry.push(("icon", file_entry(&base, &icon_name, &icon)));
        }
        entry.push(("info", Value::string(&info)));
        apps.push(Value::object(entry));
        println!("{id} {version}");
    }
    let count = apps.len();
    let index = format!("{}\n", Value::object(vec![("serial", Value::Num(serial as f64)), ("apps", Value::Arr(apps))]));
    let sig = ed25519::sign(&seed, index.as_bytes());
    fs::write(Path::new(out).join("index.json"), &index).map_err(|e| format!("index.json: {e}"))?;
    fs::write(Path::new(out).join("index.json.sig"), format!("{}\n", hex(&sig))).map_err(|e| format!("index.json.sig: {e}"))?;
    println!("index.json: {count} apps, serial {serial}, key {}", hex(&ed25519::public_key(&seed)));
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn signed_index_verifies() {
        let dir = std::env::temp_dir().join(format!("arctool-updates-{}", std::process::id()));
        let store = dir.join("store/prova");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("info"), "name=Prova\nversion=1.2\ntype=native\n").unwrap();
        std::fs::write(store.join("app"), b"\x7fELF prova").unwrap();
        std::fs::write(dir.join("key"), "07".repeat(32)).unwrap();
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();
        let args = [s(&dir.join("store")), s(&dir.join("out")), "--key".into(), s(&dir.join("key")), "--base-url".into(), "https://example.org/u/".into(), "--serial".into(), "42".into()];
        super::cmd_updates(&args).unwrap();
        let index = std::fs::read(dir.join("out/index.json")).unwrap();
        let sig = std::fs::read_to_string(dir.join("out/index.json.sig")).unwrap();
        let sig: Vec<u8> = (0..128).step_by(2).map(|i| u8::from_str_radix(&sig[i..i + 2], 16).unwrap()).collect();
        assert!(arctls::ed25519::verify(&arctls::ed25519::public_key(&[7; 32]), &index, &sig));
        let v = arcjson::parse_bytes(&index).unwrap();
        assert_eq!(v.get("serial").as_f64(), Some(42.0));
        let app = v.get("apps").at(0);
        assert_eq!(app.get("app").get("url").as_str(), Some("https://example.org/u/prova-1.2.app"));
        assert!(std::fs::read(dir.join("out/prova-1.2.app")).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
