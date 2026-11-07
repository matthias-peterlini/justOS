//! Renders a local HTML file to a PPM image, for working on the engine on
//! the host: `cargo run --target x86_64-unknown-linux-gnu -p justweb
//! --example render -- page.html 1024 out.ppm`. Stylesheets and pictures
//! are read from paths next to the page, or fetched with curl for an
//! http(s) address.

use justgfx::{Canvas, Fonts, Rect};
use justweb::{Document, WebFonts};
use std::path::Path;

fn local(base: &Path, url: &str) -> Option<Vec<u8>> {
    if url.starts_with("http") {
        let out = std::process::Command::new("curl").args(["-sSL", "--compressed", "-m", "20", "-A", "Mozilla/5.0 justOS", url]).output().ok()?;
        return out.status.success().then_some(out.stdout);
    }
    let rel = url.strip_prefix("file://").unwrap_or(url);
    let rel = rel.split(['?', '#']).next()?;
    std::fs::read(base.join(Path::new(rel).file_name()?)).ok()
}

fn main() {
    let kb: usize = std::env::var("STACK_KB").ok().and_then(|v| v.parse().ok()).unwrap_or(8192);
    std::thread::Builder::new().stack_size(kb << 10).spawn(run).unwrap().join().unwrap();
}

fn run() {
    let args: Vec<String> = std::env::args().collect();
    let page = Path::new(&args[1]);
    let width: i32 = args.get(2).and_then(|w| w.parse().ok()).unwrap_or(1024);
    let out = args.get(3).map_or("out.ppm", |s| s.as_str());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../rootfs/system/webfonts.bin");
    let raw = justzip::gunzip(&std::fs::read(root).expect("webfonts.bin")).expect("gzip");
    let fonts = WebFonts::new(Fonts::parse(Box::leak(raw.into_boxed_slice())).expect("atlas"));
    let base = page.parent().unwrap_or(Path::new("."));
    let address = if args[1].starts_with("http") { args[1].clone() } else { format!("file://{}", page.display()) };
    let body = local(base, &args[1]).or_else(|| std::fs::read(page).ok()).expect("page");
    let mut doc = Document::new(&address, &body, "text/html");
    for _ in 0..3 {
        for s in doc.wanted_sheets() {
            let text = local(base, &s).map(|b| String::from_utf8_lossy(&b).into_owned());
            doc.add_sheet(&s, text.as_deref());
        }
    }
    let t = std::time::Instant::now();
    doc.lay_out(&fonts, width, 800);
    for p in doc.wanted_pictures() {
        let pic = local(base, &p).and_then(|b| justweb::image::decode(&b));
        if pic.is_none() {
            eprintln!("no picture: {p}");
        }
        doc.add_picture(&p, pic);
    }
    if doc.needs_layout(width) {
        doc.lay_out(&fonts, width, 800);
    }
    let h = doc.height().clamp(100, 6000);
    eprintln!("layout {:?}, height {}", t.elapsed(), doc.height());
    let mut c = Canvas::new(width, h, doc.background());
    doc.paint(&mut c, &fonts, Rect { x: 0, y: 0, w: width, h }, 0, false);
    let mut ppm = format!("P6\n{} {}\n255\n", width, h).into_bytes();
    for p in &c.px {
        ppm.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
    }
    std::fs::write(out, ppm).unwrap();
}
