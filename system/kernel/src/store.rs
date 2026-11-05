//! The store: programs to install into `sys:/apps`.
//!
//! Two sources:
//!
//! - **On this computer**, `sys:/store/<id>`: the JustOS apps and games that
//!   come with the system (`info`, `app`, `icon.qoi`).
//! - **From the internet**: the catalog (`sys:/system/store-catalog.json`,
//!   refreshed from the JustOS repository) lists real Linux and Windows
//!   programs by their GitHub project; installing one asks GitHub for its
//!   latest release, downloads the right file over HTTPS, unpacks it (tar.gz,
//!   zip, gz) and checks that JustOS can run it (a static x86-64 Linux ELF,
//!   or a 64-bit Windows PE). Any GitHub project can also be searched and
//!   installed the same way.
//!
//! An installed program is `sys:/apps/<command>` (Linux), `<command>.exe`
//! (Windows) or `<id>.app` (JustOS), with `<id>.info` (`key=value` lines) and
//! `<id>.qoi` (its icon) next to it.

use crate::fs::{self, Error, Path};
use crate::net::http;
use crate::sync::IrqLock;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use justjson::Value;

const STORE: &str = "sys:/store";
const APPS: &str = "sys:/apps";
const CATALOG_FILE: &str = "sys:/system/store-catalog.json";
pub const CATALOG_URL: &str = "https://raw.githubusercontent.com/matthias-peterlini/justos/main/rootfs/system/store-catalog.json";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Platform {
    #[default]
    Justos,
    Linux,
    Windows,
}

impl Platform {
    pub fn name(self) -> &'static str {
        match self {
            Platform::Justos => "JustOS",
            Platform::Linux => "Linux",
            Platform::Windows => "Windows",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Platform::Justos => "native",
            Platform::Linux => "linux",
            Platform::Windows => "windows",
        }
    }

    fn parse(s: &str) -> Platform {
        match s {
            "linux" => Platform::Linux,
            "windows" => Platform::Windows,
            _ => Platform::Justos,
        }
    }
}

/// What a package's `info` says.
#[derive(Clone, Default)]
pub struct PackageInfo {
    pub id: String,
    pub name: String,
    pub category: String,
    pub about: String,
    pub version: String,
    pub developer: String,
    /// More about it, for its page in the store.
    pub details: String,
    pub platform: Platform,
    /// An JustOS program (else Linux or Windows).
    pub native: bool,
    /// Has a window (else it runs in the terminal).
    pub gui: bool,
    /// What to type in the terminal to start it (text programs).
    pub command: String,
    /// The program file, when it is not named after the id.
    pub program: String,
    /// Size of the program in bytes.
    pub size: u64,
    /// The icon colour (for generated icons).
    pub color: u32,
    /// Where it was downloaded from.
    pub source: String,
    /// What opening it types in the terminal (text programs).
    pub open: String,
    /// Who the app is to the sandbox (`app_id=`; else `dev.justos.<id>`).
    pub app_id: String,
    /// What it says it will ask for (`permissions=files:Immagini, camera`),
    /// shown on its page; an update that adds one waits for the user.
    pub permissions: Vec<String>,
}

pub fn parse_info(text: &str, id: &str) -> PackageInfo {
    let mut p = PackageInfo { id: id.to_string(), name: id.to_string(), native: true, ..Default::default() };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim().to_string();
        match k.trim() {
            "name" => p.name = v,
            "category" => p.category = v,
            "about" => p.about = v,
            "version" => p.version = v,
            "developer" => p.developer = v,
            "details" => p.details = v,
            "type" => p.platform = Platform::parse(&v),
            "gui" => p.gui = v == "yes",
            "command" => p.command = v,
            "program" => p.program = v,
            "color" => p.color = parse_color(&v),
            "source" => p.source = v,
            "open" => p.open = v,
            "app_id" if crate::sandbox::valid_id(&v) => p.app_id = v.to_ascii_lowercase(),
            "permissions" => p.permissions = v.split(',').map(str::trim).filter(|s| crate::sandbox::known(s)).map(String::from).collect(),
            _ => {}
        }
    }
    if p.app_id.is_empty() {
        p.app_id = crate::sandbox::default_id(id);
    }
    p.native = p.platform == Platform::Justos;
    if p.command.is_empty() {
        p.command = id.to_string();
    }
    if p.open.is_empty() {
        p.open = p.command.clone();
    }
    p
}

fn parse_color(s: &str) -> u32 {
    u32::from_str_radix(s.trim_start_matches('#'), 16).map(|c| 0xFF00_0000 | c).unwrap_or(0xFF5B_5B6B)
}

fn path(s: &str) -> Result<Path, Error> {
    fs::abs(s)
}

fn read_info(file: &str, id: &str) -> Option<PackageInfo> {
    let bytes = fs::read_file(&path(file).ok()?).ok()?;
    Some(parse_info(&String::from_utf8_lossy(&bytes), id))
}

/// The packages on this computer (`sys:/store`).
pub fn available() -> Vec<PackageInfo> {
    let Ok(dir) = path(STORE) else { return Vec::new() };
    let mut v: Vec<PackageInfo> = fs::list_dir(&dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, st)| st.kind == justfs::Kind::Dir)
        .filter_map(|(e, _)| {
            let mut p = read_info(&format!("{STORE}/{}/info", e.name), &e.name)?;
            p.size = fs::stat(&dir.join(&e.name).join("app")).map(|s| s.size).unwrap_or(0);
            Some(p)
        })
        .collect();
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    v
}

/// Where an installed package's program is.
pub fn program_path(p: &PackageInfo) -> String {
    if !p.program.is_empty() {
        return p.program.clone();
    }
    match p.platform {
        Platform::Justos => format!("{APPS}/{}.app", p.id),
        Platform::Linux => format!("{APPS}/{}", p.id),
        Platform::Windows => format!("{APPS}/{}.exe", p.id),
    }
}

/// The installed package `id`, if it is.
pub fn installed(id: &str) -> Option<PackageInfo> {
    read_info(&format!("{APPS}/{id}.info"), id)
}

/// Every installed package.
pub fn installed_all() -> Vec<PackageInfo> {
    let Ok(dir) = path(APPS) else { return Vec::new() };
    fs::list_dir(&dir).unwrap_or_default().iter().filter_map(|(e, _)| e.name.strip_suffix(".info")).filter_map(installed).collect()
}

/// The icon of a package: "@<file>" if it has a picture of its own.
pub fn icon(id: &str) -> Option<String> {
    let file = format!("{APPS}/{id}.qoi");
    if path(&file).is_ok_and(|p| fs::exists(&p)) {
        return Some(format!("@{file}"));
    }
    let file = format!("{STORE}/{id}/icon.qoi");
    path(&file).is_ok_and(|p| fs::exists(&p)).then(|| format!("@{file}"))
}

/// Installs a package from this computer.
pub fn install(id: &str) -> Result<PackageInfo, Error> {
    let p = read_info(&format!("{STORE}/{id}/info"), id).ok_or(Error::NotFound)?;
    fs::copy_file(&path(&format!("{STORE}/{id}/app"))?, &path(&program_path(&p))?)?;
    fs::copy_file(&path(&format!("{STORE}/{id}/info"))?, &path(&format!("{APPS}/{id}.info"))?)?;
    if let Ok(icon) = path(&format!("{STORE}/{id}/icon.qoi")) {
        if fs::exists(&icon) {
            fs::copy_file(&icon, &path(&format!("{APPS}/{id}.qoi"))?)?;
        }
    }
    Ok(p)
}

pub fn remove(id: &str) -> Result<(), Error> {
    let p = installed(id).ok_or(Error::NotFound)?;
    for f in [program_path(&p), format!("{APPS}/{id}.info"), format!("{APPS}/{id}.qoi")] {
        if let Ok(f) = path(&f) {
            if fs::exists(&f) {
                fs::remove(&f)?;
            }
        }
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// The online catalog

/// A program the catalog offers.
#[derive(Clone, Default)]
pub struct CatalogApp {
    pub id: String,
    pub name: String,
    pub category: String,
    pub platform: Platform,
    pub developer: String,
    pub about: String,
    pub details: String,
    pub license: String,
    /// "owner/repo" on GitHub.
    pub github: String,
    /// Fragments the release asset's name must contain, in order ("*").
    pub asset: String,
    /// A fixed download address instead of GitHub's latest release.
    pub url: String,
    /// The program inside the archive.
    pub file: String,
    /// Its name once installed (what to type).
    pub command: String,
    /// What "Apri" types in the terminal.
    pub open: String,
    pub color: u32,
    pub tags: Vec<String>,
    pub featured: bool,
}

static CATALOG: IrqLock<Option<Vec<CatalogApp>>> = IrqLock::new(None);

fn parse_catalog(v: &Value) -> Vec<CatalogApp> {
    v.get("apps")
        .as_array()
        .iter()
        .filter_map(|a| {
            let id = a.get("id").as_str()?.to_string();
            let command = a.get("command").as_str().unwrap_or(&id).to_string();
            Some(CatalogApp {
                name: a.get("name").as_str().unwrap_or(&id).to_string(),
                category: a.get("category").str().to_string(),
                platform: Platform::parse(a.get("platform").str()),
                developer: a.get("developer").str().to_string(),
                about: a.get("about").str().to_string(),
                details: a.get("details").str().to_string(),
                license: a.get("license").str().to_string(),
                github: a.get("github").str().to_string(),
                asset: a.get("asset").str().to_string(),
                url: a.get("url").str().to_string(),
                file: a.get("file").str().to_string(),
                open: a.get("open").as_str().unwrap_or(&command).to_string(),
                color: parse_color(a.get("color").str()),
                tags: a.get("tags").as_array().iter().filter_map(|t| t.as_str().map(String::from)).collect(),
                featured: a.get("featured").as_bool().unwrap_or(false),
                command,
                id,
            })
        })
        .collect()
}

/// The catalog as last saved on this computer.
pub fn catalog() -> Vec<CatalogApp> {
    if let Some(c) = CATALOG.lock().as_ref() {
        return c.clone();
    }
    let c = path(CATALOG_FILE)
        .and_then(|p| fs::read_file(&p))
        .ok()
        .and_then(|b| justjson::parse_bytes(&b))
        .map(|v| parse_catalog(&v))
        .unwrap_or_default();
    *CATALOG.lock() = Some(c.clone());
    c
}

/// Downloads the newest catalog; returns how many programs it lists.
pub fn refresh_catalog() -> Result<usize, String> {
    let r = http::get(CATALOG_URL)?;
    if !r.ok() {
        return Err(format!("the catalog is not available ({})", r.status));
    }
    let v = justjson::parse_bytes(&r.body).ok_or("the catalog is damaged")?;
    let apps = parse_catalog(&v);
    if apps.is_empty() {
        return Err("the catalog is empty".to_string());
    }
    if let Ok(p) = path(CATALOG_FILE) {
        let _ = fs::write_file(&p, &r.body);
    }
    let n = apps.len();
    *CATALOG.lock() = Some(apps);
    Ok(n)
}

// ----------------------------------------------------------------------------
// GitHub

/// A project found on GitHub.
#[derive(Clone)]
pub struct GhRepo {
    pub full_name: String,
    pub description: String,
    pub stars: u64,
}

fn github_json(url: &str) -> Result<Value, String> {
    let req = http::Request { method: "GET", url, headers: &[("Accept", "application/vnd.github+json")], body: None };
    let r = http::fetch(&req, &mut |_, _| true)?;
    if r.status == 403 || r.status == 429 {
        return Err("GitHub asks to wait a little before searching again".to_string());
    }
    if !r.ok() {
        return Err(format!("GitHub answered {}", r.status));
    }
    justjson::parse_bytes(&r.body).ok_or_else(|| "GitHub sent something unreadable".to_string())
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Searches GitHub for projects (most starred first).
pub fn github_search(query: &str) -> Result<Vec<GhRepo>, String> {
    let url = format!("https://api.github.com/search/repositories?q={}&sort=stars&per_page=12", url_encode(query.trim()));
    let v = github_json(&url)?;
    Ok(v.get("items")
        .as_array()
        .iter()
        .map(|r| GhRepo {
            full_name: r.get("full_name").str().to_string(),
            description: r.get("description").str().to_string(),
            stars: r.get("stargazers_count").as_f64().unwrap_or(0.0) as u64,
        })
        .collect())
}

/// A file of a release.
#[derive(Clone)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub platform: Platform,
}

/// Which platform a release file is for, if JustOS can use it.
pub fn asset_platform(name: &str) -> Option<Platform> {
    let n = name.to_ascii_lowercase();
    let x64 = n.contains("x86_64") || n.contains("amd64") || n.contains("x64") || n.contains("linux64") || n.contains("win64");
    let packed = n.ends_with(".tar.gz") || n.ends_with(".tgz") || n.ends_with(".zip") || n.ends_with(".gz");
    let skip = [".sha256", ".sig", ".asc", ".deb", ".rpm", ".msi", ".pem", ".sbom", ".txt", ".json", ".tar.xz", ".tar.bz2", ".7z", ".apk", ".appimage", ".dmg", ".pkg"];
    if skip.iter().any(|s| n.ends_with(s)) || n.contains("darwin") || n.contains("macos") || n.contains("android") || n.contains("freebsd") {
        return None;
    }
    if !x64 && !(n.contains("linux") && !n.contains("arm") && !n.contains("aarch64") && !n.contains("386") && !n.contains("i686")) {
        return None;
    }
    if n.contains("windows") || n.contains("win64") || n.contains("-win") || n.ends_with(".exe") {
        return Some(Platform::Windows);
    }
    if n.contains("linux") && (packed || !n.contains('.') || n.ends_with("-static")) {
        // musl and static builds run; glibc builds usually need libraries.
        return Some(Platform::Linux);
    }
    None
}

/// The latest release of `repo`: its tag and the files JustOS could use,
/// static Linux builds first.
pub fn github_release(repo: &str) -> Result<(String, Vec<Asset>), String> {
    let v = github_json(&format!("https://api.github.com/repos/{repo}/releases/latest"))?;
    let tag = v.get("tag_name").str().to_string();
    let mut assets: Vec<Asset> = v
        .get("assets")
        .as_array()
        .iter()
        .filter_map(|a| {
            let name = a.get("name").str().to_string();
            let platform = asset_platform(&name)?;
            Some(Asset {
                url: a.get("browser_download_url").str().to_string(),
                size: a.get("size").as_f64().unwrap_or(0.0) as u64,
                platform,
                name,
            })
        })
        .collect();
    let rank = |a: &Asset| {
        let n = a.name.to_ascii_lowercase();
        match a.platform {
            Platform::Linux if n.contains("musl") || n.contains("static") => 0,
            Platform::Linux => 1,
            Platform::Windows if n.contains("gnu") || n.contains("mingw") => 2,
            _ => 3,
        }
    };
    assets.sort_by_key(rank);
    Ok((tag, assets))
}

fn matches_pattern(name: &str, pattern: &str) -> bool {
    let mut rest = name;
    for part in pattern.split('*').filter(|p| !p.is_empty()) {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    true
}

// ----------------------------------------------------------------------------
// Installing from the internet

/// How an installation is going (for the store's progress ring).
#[derive(Clone, Default)]
pub struct Job {
    pub done: u64,
    pub total: Option<u64>,
    pub phase: &'static str,
    /// Set when it is over: Ok, or why it failed.
    pub result: Option<Result<(), String>>,
}

static JOBS: IrqLock<BTreeMap<String, Job>> = IrqLock::new(BTreeMap::new());

pub fn job(id: &str) -> Option<Job> {
    JOBS.lock().get(id).cloned()
}

pub fn clear_job(id: &str) {
    JOBS.lock().remove(id);
}

fn set_phase(id: &str, phase: &'static str) {
    JOBS.lock().entry(id.to_string()).or_default().phase = phase;
}

/// Starts installing a catalog program in the background.
pub fn start_install(app: CatalogApp) {
    JOBS.lock().insert(app.id.clone(), Job { phase: "In attesa", ..Default::default() });
    let id = app.id.clone();
    let spawned = crate::task::spawn("store", move || {
        let r = install_remote(&app);
        if let Err(e) = &r {
            crate::warn!("store", "{}: {e}", app.id);
        }
        if let Some(j) = JOBS.lock().get_mut(&app.id) {
            j.result = Some(r);
        }
    });
    if spawned.is_none() {
        if let Some(j) = JOBS.lock().get_mut(&id) {
            j.result = Some(Err("no memory to start the download".to_string()));
        }
    }
}

fn install_remote(app: &CatalogApp) -> Result<(), String> {
    let id = app.id.clone();
    let mut report = |done: u64, total: Option<u64>| {
        if let Some(j) = JOBS.lock().get_mut(&id) {
            j.done = done;
            j.total = total;
        }
        true
    };
    let (version, url, program) = download_release(app, &mut |phase| set_phase(&app.id, phase), &mut report)?;
    save(app, &version, &url, &program)
}

/// Downloads the newest release of a catalog program and takes the program
/// out: (its version, where it came from, the program). `phase` hears what
/// it is doing, `report` how far the download is.
pub fn download_release(app: &CatalogApp, phase: &mut dyn FnMut(&'static str), report: http::Progress<'_>) -> Result<(String, String, Vec<u8>), String> {
    phase("Cerco la versione");
    let (version, url, asset_name) = if !app.url.is_empty() {
        (String::new(), app.url.clone(), app.url.rsplit('/').next().unwrap_or("").to_string())
    } else {
        let (tag, assets) = github_release(&app.github)?;
        let pick = assets
            .iter()
            .find(|a| !app.asset.is_empty() && matches_pattern(&a.name, &app.asset))
            .or_else(|| if app.asset.is_empty() { assets.iter().find(|a| a.platform == app.platform) } else { None })
            .ok_or_else(|| format!("no {} build in the latest release", app.platform.name()))?;
        (tag, pick.url.clone(), pick.name.clone())
    };
    phase("Scarico");
    let r = http::fetch(&http::Request::get(&url), report)?;
    if !r.ok() {
        return Err(format!("the download failed ({})", r.status));
    }
    phase("Installo");
    let (_, program) = unpack(&r.body, &asset_name, &app.file, &app.command, app.platform)?;
    Ok((version, url, program))
}

/// Where a catalog program is installed.
pub fn program_file(app: &CatalogApp) -> String {
    match app.platform {
        Platform::Windows => format!("{APPS}/{}.exe", app.command),
        _ => format!("{APPS}/{}", app.command),
    }
}

/// The `info` of a catalog program installed from `url`.
pub fn info_text(app: &CatalogApp, version: &str, url: &str) -> String {
    format!(
        "name={}\ncategory={}\nabout={}\nversion={}\ndeveloper={}\ntype={}\ncommand={}\nprogram={}\ncolor={:06x}\nsource={url}\nopen={}\n",
        app.name,
        app.category,
        app.about,
        version.trim_start_matches('v'),
        app.developer,
        app.platform.key(),
        app.command,
        program_file(app),
        app.color & 0xFF_FFFF,
        app.open,
    )
}

/// How to get a newer release of an installed Linux or Windows program:
/// its catalog entry, or the GitHub project it was downloaded from.
pub fn catalog_app_for(p: &PackageInfo) -> Option<CatalogApp> {
    if p.native {
        return None;
    }
    if let Some(app) = catalog().into_iter().find(|a| a.id == p.id && !a.github.is_empty() && a.url.is_empty()) {
        return Some(app);
    }
    // "https://github.com/<owner>/<repo>/releases/download/<tag>/<file>"
    let rest = p.source.strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    if parts.next() != Some("releases") {
        return None;
    }
    Some(CatalogApp {
        id: p.id.clone(),
        name: p.name.clone(),
        category: p.category.clone(),
        platform: p.platform,
        developer: p.developer.clone(),
        about: p.about.clone(),
        github: format!("{owner}/{repo}"),
        command: p.command.clone(),
        open: p.open.clone(),
        color: p.color,
        ..Default::default()
    })
}

/// Writes an installed program with its `info` and icon into `sys:/apps`.
fn save(app: &CatalogApp, version: &str, url: &str, program: &[u8]) -> Result<(), String> {
    let file = program_file(app);
    let fp = path(&file).map_err(|e| e.to_string())?;
    fs::write_file(&fp, program).map_err(|e| format!("{file}: {e}"))?;
    let info = info_text(app, version, url);
    fs::write_file(&path(&format!("{APPS}/{}.info", app.id)).map_err(|e| e.to_string())?, info.as_bytes()).map_err(|e| e.to_string())?;
    let icon = crate::gui::res::make_icon(&app.name, app.color, app.platform);
    let _ = fs::write_file(&path(&format!("{APPS}/{}.qoi", app.id)).map_err(|e| e.to_string())?, &icon);
    Ok(())
}

/// A catalog entry for a GitHub project picked from a search.
pub fn app_from_repo(repo: &GhRepo, platform: Platform) -> CatalogApp {
    let name = repo.full_name.rsplit('/').next().unwrap_or(&repo.full_name).to_string();
    let id = name.to_ascii_lowercase().replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "-");
    CatalogApp {
        color: color_for(&name),
        command: id.clone(),
        open: format!("{id} --help"),
        id,
        name,
        category: String::from("Da GitHub"),
        platform,
        developer: repo.full_name.split('/').next().unwrap_or("").to_string(),
        about: repo.description.clone(),
        github: repo.full_name.clone(),
        ..Default::default()
    }
}

/// A colour for a generated icon, picked from the name.
fn color_for(name: &str) -> u32 {
    let hash = name.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let palette = [0xFF3B_82F6, 0xFFE0_662E, 0xFF10_B981, 0xFF8B_5CF6, 0xFFEF_4444, 0xFF0E_A5E9, 0xFFF5_9E0B, 0xFFEC_4899];
    palette[(hash % palette.len() as u32) as usize]
}

/// Whether a file is a package `install_file` understands (by its name).
pub fn is_package(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    [".deb", ".pkg", ".mpkg", ".zip", ".tar.gz", ".tgz", ".tar", ".gz"].iter().any(|e| l.ends_with(e))
}

/// Installs a package file from this computer (double-clicked in Files):
///
/// - a Debian `.deb` or a `.tar.gz`/`.tgz`/`.tar`/`.zip`/`.gz` with a static
///   x86-64 Linux program or a 64-bit Windows program in it: installed into
///   `sys:/apps` like a store program;
/// - a macOS installer `.pkg`: its payload is unpacked into
///   `sys:/home/Applicazioni macOS/<name>`. macOS programs themselves (Mach-O)
///   cannot run on JustOS; Linux or Windows programs inside are installed.
///
/// Returns what to tell the user.
pub fn install_file(file: &str) -> Result<String, String> {
    let data = fs::read_file(&path(file).map_err(|e| e.to_string())?).map_err(|e| format!("{file}: {e}"))?;
    let name = file.rsplit('/').next().unwrap_or(file);
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".pkg") || lower.ends_with(".mpkg") {
        return install_mac(&data, name);
    }
    let entries = entries(&data, name)?;
    let platform = if entries.iter().any(|(_, d)| d.starts_with(b"\x7fELF")) {
        Platform::Linux
    } else if entries.iter().any(|(_, d)| d.starts_with(b"MZ")) {
        Platform::Windows
    } else {
        return Err("non contiene programmi Linux o Windows".to_string());
    };
    let stem = package_stem(name);
    let (program_name, program) = pick(&entries, "", &stem, platform)?;
    install_program(&program_name, &program, &stem, platform, file)
}

/// "ripgrep_14.1.0-1_amd64.deb" → "ripgrep"; "tool-1.2-x86_64.tar.gz" → "tool-1.2-x86_64".
fn package_stem(name: &str) -> String {
    let mut s = name;
    for ext in [".deb", ".pkg", ".mpkg", ".zip", ".tgz", ".gz", ".tar"] {
        if s.to_ascii_lowercase().ends_with(ext) {
            s = &s[..s.len() - ext.len()];
        }
    }
    if name.to_ascii_lowercase().ends_with(".deb") {
        s = s.split('_').next().unwrap_or(s);
    }
    s.to_ascii_lowercase().replace(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '.', "-")
}

fn install_program(program_name: &str, program: &[u8], id: &str, platform: Platform, source: &str) -> Result<String, String> {
    let base = program_name.rsplit('/').next().unwrap_or(program_name);
    let command = base.strip_suffix(".exe").unwrap_or(base).to_string();
    let app = CatalogApp {
        id: id.to_string(),
        name: command.clone(),
        category: String::from("Installati"),
        platform,
        developer: String::new(),
        about: format!("Installato da {}", source.rsplit('/').next().unwrap_or(source)),
        open: command.clone(),
        color: color_for(&command),
        command: command.clone(),
        ..Default::default()
    };
    save(&app, "", source, program)?;
    Ok(format!("{command} ({}) è installato: scrivi \"{command}\" nel terminale", platform.name()))
}

fn install_mac(data: &[u8], name: &str) -> Result<String, String> {
    let xar = justzip::unxar(data).ok_or("il pacchetto macOS è danneggiato")?;
    let stem = package_stem(name);
    let dest = format!("sys:/home/Applicazioni macOS/{stem}");
    let mut files = 0;
    let mut native = None;
    for payload in xar.iter().filter(|e| e.name == "Payload" || e.name.ends_with("/Payload")) {
        let cpio = if payload.data.starts_with(b"\x1f\x8b") {
            justzip::gunzip(&payload.data).ok_or("il pacchetto macOS è danneggiato")?
        } else if payload.data.starts_with(b"pbzx") {
            return Err("questo pacchetto macOS usa la compressione LZMA (pbzx), non ancora supportata".to_string());
        } else {
            payload.data.clone()
        };
        for e in justzip::uncpio(&cpio).ok_or("il contenuto del pacchetto macOS è danneggiato")? {
            if e.link.is_some() {
                continue;
            }
            let target = format!("{dest}/{}", e.name);
            if e.dir {
                make_dirs(&target);
                continue;
            }
            if let Some((parent, _)) = target.rsplit_once('/') {
                make_dirs(parent);
            }
            let p = path(&target).map_err(|e| e.to_string())?;
            fs::write_file(&p, &e.data).map_err(|err| format!("{target}: {err}"))?;
            files += 1;
            if native.is_none() && (e.data.starts_with(b"\x7fELF") || e.data.starts_with(b"MZ")) {
                native = Some((e.name.clone(), e.data));
            }
        }
    }
    if files == 0 {
        return Err("il pacchetto macOS non contiene file".to_string());
    }
    if let Some((n, d)) = native {
        let platform = if d.starts_with(b"MZ") { Platform::Windows } else { Platform::Linux };
        if let Ok((n, d)) = pick(&[(n, d)], "", "", platform) {
            return install_program(&n, &d, &stem, platform, name);
        }
    }
    Ok(format!("{files} file estratti in {dest}. I programmi macOS non possono girare su JustOS"))
}

/// Makes a directory and those above it (those that exist are kept).
fn make_dirs(dir: &str) {
    let mut at = String::new();
    for part in dir.split('/') {
        if !at.is_empty() && !at.ends_with('/') {
            at.push('/');
        }
        at.push_str(part);
        if part.ends_with(':') || part.is_empty() {
            continue;
        }
        if let Ok(p) = path(&at) {
            if !fs::exists(&p) {
                let _ = fs::make_dir(&p);
            }
        }
    }
}

/// The files of a download or package (archives unpacked).
fn entries(data: &[u8], name: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    let files = |v: Vec<justzip::Entry>| v.into_iter().filter(|e| !e.dir && e.link.is_none()).map(|e| (e.name, e.data)).collect();
    let lower = name.to_ascii_lowercase();
    Ok(if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        let tar = justzip::gunzip(data).ok_or("the archive is damaged")?;
        files(justzip::untar(&tar).ok_or("the archive is damaged")?)
    } else if lower.ends_with(".tar") {
        files(justzip::untar(data).ok_or("the archive is damaged")?)
    } else if lower.ends_with(".deb") {
        let ar = justzip::unar(data).ok_or("the package is damaged")?;
        let member = ar.iter().find(|e| e.name.starts_with("data.tar")).ok_or("the package has no data")?;
        let tar = match member.name.as_str() {
            "data.tar" => member.data.clone(),
            "data.tar.gz" => justzip::gunzip(&member.data).ok_or("the package is damaged")?,
            other => return Err(format!("{other}: questa compressione non è ancora supportata (solo gzip)")),
        };
        files(justzip::untar(&tar).ok_or("the package is damaged")?)
    } else if lower.ends_with(".zip") {
        files(justzip::unzip(data).ok_or("the archive is damaged")?)
    } else if lower.ends_with(".gz") {
        alloc::vec![(name[..name.len() - 3].to_string(), justzip::gunzip(data).ok_or("the file is damaged")?)]
    } else {
        alloc::vec![(name.to_string(), data.to_vec())]
    })
}

/// The program inside a download: unpacks archives and checks the result.
fn unpack(data: &[u8], name: &str, want: &str, command: &str, platform: Platform) -> Result<(String, Vec<u8>), String> {
    pick(&entries(data, name)?, want, command, platform)
}

/// Picks the program among the files and checks that JustOS can run it.
fn pick(entries: &[(String, Vec<u8>)], want: &str, command: &str, platform: Platform) -> Result<(String, Vec<u8>), String> {
    let base = |n: &str| n.rsplit('/').next().unwrap_or(n).to_string();
    let is_program = |d: &[u8]| match platform {
        Platform::Windows => d.starts_with(b"MZ"),
        _ => d.starts_with(b"\x7fELF"),
    };
    let names = |n: &str| {
        let b = base(n);
        let stem = b.strip_suffix(".exe").unwrap_or(&b).to_string();
        (b, stem)
    };
    let pick = entries
        .iter()
        .find(|(n, d)| !want.is_empty() && (names(n).0 == want || names(n).1 == want) && is_program(d))
        .or_else(|| entries.iter().find(|(n, d)| names(n).1 == command && is_program(d)))
        .or_else(|| entries.iter().find(|(n, d)| is_program(d) && (n.contains("bin/") || n.ends_with(".exe"))))
        .or_else(|| entries.iter().find(|(_, d)| is_program(d)))
        .ok_or("no program for this computer in the download")?;
    let d = &pick.1;
    match platform {
        Platform::Windows => {
            let pe = u32::from_le_bytes(d.get(0x3c..0x40).ok_or("not a Windows program")?.try_into().unwrap()) as usize;
            let machine = d.get(pe + 4..pe + 6).map(|m| u16::from_le_bytes([m[0], m[1]]));
            if d.get(pe..pe + 4) != Some(b"PE\0\0") || machine != Some(0x8664) {
                return Err("not a 64-bit Windows program".to_string());
            }
        }
        _ => {
            if d.len() < 64 || d[4] != 2 || u16::from_le_bytes([d[18], d[19]]) != 0x3E {
                return Err("not a 64-bit Linux program".to_string());
            }
            let phoff = u64::from_le_bytes(d[32..40].try_into().unwrap()) as usize;
            let phent = u16::from_le_bytes([d[54], d[55]]) as usize;
            let phnum = u16::from_le_bytes([d[56], d[57]]) as usize;
            let dynamic = (0..phnum).any(|i| d.get(phoff + i * phent..phoff + i * phent + 4) == Some(&3u32.to_le_bytes()[..]));
            if dynamic {
                return Err("this build needs Linux libraries (JustOS runs static programs)".to_string());
            }
        }
    }
    Ok(pick.clone())
}
