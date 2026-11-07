//! What `justc` builds: a single `.jt` file or a project folder with an
//! `Just.toml`, as in JustScript's `jpk-bundler`.
//!
//! ```toml
//! [package]
//! name = "counter"
//! version = "0.1.0"
//! entry = "src/main.jt"
//! assets = "assets"
//! title = "Counter"                    # optional display name
//! app_id = "dev.justos.counter"         # optional, defaults to dev.justos.<name>
//! ```

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use justc_core::jpk::{Jpk, Asset};
use justc_core::{compile_with_modules, CompileFailure, CompileOptions, Diagnostic, SourceFile};
use justrt::fs;

pub const MANIFEST_FILE: &str = "Just.toml";

/// A JustScript source.
pub fn is_source(path: &str) -> bool {
    path.ends_with(".jt")
}

/// `a/b` from parts, keeping a volume or a root as it is.
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else if dir.ends_with('/') || dir.ends_with(':') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// The folder part of a path ("" for a bare name).
pub fn parent(path: &str) -> &str {
    match path.trim_end_matches('/').rfind('/') {
        Some(i) if path[..i].ends_with(':') || i == 0 => &path[..=i],
        Some(i) => &path[..i],
        None => "",
    }
}

/// The last name of a path.
pub fn file_name(path: &str) -> &str {
    let p = path.trim_end_matches('/');
    let p = p.rsplit('/').next().unwrap_or(p);
    p.rsplit(':').next().unwrap_or(p)
}

pub fn is_dir(path: &str) -> bool {
    fs::list_dir(path).is_ok()
}

/// Everything needed to produce a package.
pub struct Input {
    pub name: String,
    pub version: String,
    pub title: String,
    pub app_id: String,
    /// The main file as diagnostics show it.
    pub source_name: String,
    pub source: String,
    /// Folder of the main file, where `import`ed modules are looked up.
    pub source_dir: String,
    pub assets: Vec<Asset>,
    /// Project folder (for `build/`), or the folder of a single file.
    pub root: String,
    pub is_project: bool,
}

fn read_text(path: &str) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("{path}: {}", e.as_str()))?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{path}: not UTF-8 text"))?;
    Ok(text.replace("\r\n", "\n"))
}

/// The title, or `name` with its first letter capitalised.
fn display_title(title: &str, name: &str) -> String {
    if !title.trim().is_empty() {
        return title.trim().to_owned();
    }
    let mut chars = name.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

impl Input {
    /// A `.jt` file, or a folder with a `Just.toml`.
    pub fn resolve(path: &str) -> Result<Input, String> {
        if is_dir(path) {
            return Input::from_project(path);
        }
        let source = read_text(path)?;
        let file = file_name(path);
        let name = file.rsplit_once('.').map_or(file, |(stem, _)| stem).to_owned();
        Ok(Input {
            title: display_title("", &name),
            app_id: justc_core::jpk::app_id(&name),
            name,
            version: "0.1.0".into(),
            source_name: file.to_owned(),
            source,
            source_dir: parent(path).to_owned(),
            assets: Vec::new(),
            root: parent(path).to_owned(),
            is_project: false,
        })
    }

    fn from_project(root: &str) -> Result<Input, String> {
        let manifest = join(root, MANIFEST_FILE);
        let text = read_text(&manifest).map_err(|e| format!("{e} (a project folder needs an {MANIFEST_FILE})"))?;
        let mut name = file_name(root).to_owned();
        let (mut version, mut entry, mut assets) = (String::from("0.1.0"), String::from("src/main.jt"), String::from("assets"));
        let (mut title, mut app_id) = (String::new(), None);
        let mut section = String::new();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(s) = line.strip_prefix('[').and_then(|l| l.split_once(']')).map(|(s, _)| s) {
                section = s.trim().to_owned();
                continue;
            }
            let at = |e: String| format!("{manifest}:{}: {e}", n + 1);
            let (key, value) = line.split_once('=').ok_or_else(|| at("expected `key = \"value\"`".into()))?;
            let value = parse_value(value).map_err(at)?;
            if section == "package" {
                match key.trim() {
                    "name" => name = value,
                    "version" => version = value,
                    "entry" => entry = value,
                    "assets" => assets = value,
                    "title" => title = value,
                    "app_id" => {
                        justc_core::jpk::check_app_id(&value).map_err(at)?;
                        app_id = Some(value);
                    }
                    _ => {}
                }
            }
        }
        let entry_path = join(root, &entry);
        let mut list = Vec::new();
        let assets_dir = join(root, &assets);
        if is_dir(&assets_dir) {
            collect_assets(&assets_dir, "", &mut list)?;
        }
        Ok(Input {
            title: display_title(&title, &name),
            app_id: app_id.unwrap_or_else(|| justc_core::jpk::app_id(&name)),
            source: read_text(&entry_path)?,
            source_dir: parent(&entry_path).to_owned(),
            source_name: entry,
            name,
            version,
            assets: list,
            root: root.to_owned(),
            is_project: true,
        })
    }

    /// Compiles into a package; also returns the warnings and the source
    /// they point into.
    pub fn compile(&self, opts: CompileOptions) -> Result<(Jpk, Vec<Diagnostic>, SourceFile), CompileFailure> {
        let dir = self.source_dir.clone();
        let prefix = String::from(parent(&self.source_name));
        let mut load = |module: &str| -> Result<(String, String), String> {
            let file = format!("{module}.jt");
            let path = join(&dir, &file);
            if fs::File::open(&path).is_ok() {
                let text = read_text(&path)?;
                return Ok((join(&prefix, &file), text));
            }
            Err(format!("cannot find module `{module}`: there is no {module}.jt next to this file"))
        };
        let out = compile_with_modules(&self.source_name, &self.source, opts, &mut load)?;
        let mut pkg = Jpk::new(out.program);
        pkg.manifest.set("name", &self.name);
        pkg.manifest.set("title", &self.title);
        pkg.manifest.set("app_id", &self.app_id);
        pkg.manifest.set("version", &self.version);
        pkg.manifest.set("entry", &self.source_name);
        pkg.manifest.set("profile", if opts.debug_info { "debug" } else { "release" });
        pkg.manifest.set("target", "jpk");
        pkg.manifest.set("compiler", format!("justc {} (justOS)", justc_core::VERSION));
        pkg.assets = self.assets.clone();
        Ok((pkg, out.warnings, out.source))
    }
}

/// A manifest value: a TOML basic string (`"..."`, with `\n`, `\t`, `\"`
/// and `\\`) or a bare word, then an optional comment.
fn parse_value(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let Some(body) = raw.strip_prefix('"') else {
        return Ok(raw.split_once('#').map_or(raw, |(v, _)| v).trim().to_owned());
    };
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                let rest = chars.as_str().trim();
                return if rest.is_empty() || rest.starts_with('#') {
                    Ok(out)
                } else {
                    Err(format!("unexpected `{rest}` after the value"))
                };
            }
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(c @ ('"' | '\\')) => out.push(c),
                other => return Err(format!("unknown escape `\\{}`", other.map(String::from).unwrap_or_default())),
            },
            c => out.push(c),
        }
    }
    Err("unterminated string".into())
}

/// Asset files, named by their path below the assets folder.
fn collect_assets(dir: &str, prefix: &str, out: &mut Vec<Asset>) -> Result<(), String> {
    let mut entries = fs::list_dir(dir).map_err(|e| format!("{dir}: {}", e.as_str()))?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    for e in entries {
        let path = join(dir, &e.name);
        let name = if prefix.is_empty() { e.name.clone() } else { format!("{prefix}/{}", e.name) };
        if e.is_dir {
            collect_assets(&path, &name, out)?;
        } else {
            let data = fs::read(&path).map_err(|err| format!("{path}: {}", err.as_str()))?;
            out.push(Asset { name, data });
        }
    }
    Ok(())
}

/// A new project in `dir`, as `justc new` makes it everywhere.
pub fn scaffold(dir: &str, name: &str) -> Result<(), String> {
    let io = |path: &str, e: justrt::Error| format!("{path}: {}", e.as_str());
    if fs::File::open(&join(dir, MANIFEST_FILE)).is_ok() {
        return Err(format!("{dir} already contains a project"));
    }
    for d in [String::from(dir), join(dir, "src"), join(dir, "assets")] {
        match fs::make_dir(&d) {
            Ok(()) | Err(justrt::Error::Exists) => {}
            Err(e) => return Err(io(&d, e)),
        }
    }
    let manifest = format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nentry = \"src/main.jt\"\nassets = \"assets\"\n");
    let main = join(&join(dir, "src"), "main.jt");
    fs::write(&join(dir, MANIFEST_FILE), manifest.as_bytes()).map_err(|e| io(dir, e))?;
    fs::write(&main, TEMPLATE.replace("{{name}}", name).as_bytes()).map_err(|e| io(&main, e))?;
    Ok(())
}

const TEMPLATE: &str = r#"# {{name}} — JustScript application

let clicks = 0

fn click(n)
    clicks += n
    print("clicked, total = {clicks}")

print("Hello from {{name}}!")

view
    window "{{name}}"
        column
            label "You clicked {clicks} times"
            row
                button "Click me" -> click(1)
                button "Reset" -> click(-clicks)
"#;
