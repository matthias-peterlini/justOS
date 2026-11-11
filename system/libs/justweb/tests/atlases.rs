//! The font atlases shipped with the system have to be in the format the
//! system reads: a stale file (made before a change of the format) leaves
//! the Browser without faces, and nothing else notices.

use justgfx::Fonts;
use std::path::Path;

fn parses(rel: &str, gzipped: bool) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").join(rel);
    let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let data = if gzipped { justzip::gunzip(&raw).unwrap_or_else(|| panic!("{rel}: not gzip")) } else { raw };
    assert!(Fonts::parse(Box::leak(data.into_boxed_slice())).is_some(), "{rel}: not a font atlas of this version (run make assets)");
}

#[test]
fn web_faces() {
    parses("rootfs/system/webfonts.bin", true);
}

#[test]
fn interface_faces() {
    parses("system/kernel/assets/fonts.bin", false);
    parses("system/kernel/assets/fonts@2x.bin.gz", true);
}
