use crate::*;

static TEXT: &[u8] = include_bytes!("../tests/data/text.bin");

#[test]
fn gzip_and_zlib() {
    assert_eq!(gunzip(include_bytes!("../tests/data/text.gz")).unwrap(), TEXT);
    assert_eq!(gunzip(include_bytes!("../tests/data/stored.gz")).unwrap(), TEXT);
    assert_eq!(gunzip(include_bytes!("../tests/data/rnd.gz")).unwrap().len(), 70000);
    assert_eq!(zlib(include_bytes!("../tests/data/text.zlib")).unwrap(), TEXT);
    let mut bad = include_bytes!("../tests/data/text.gz").to_vec();
    let n = bad.len();
    bad[n - 9] ^= 0x55;
    assert!(gunzip(&bad).is_none());
}

#[test]
fn tar_gz() {
    let t = untar(&gunzip(include_bytes!("../tests/data/app.tar.gz")).unwrap()).unwrap();
    let names: alloc::vec::Vec<&str> = t.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"app/bin/tool"));
    assert!(names.iter().any(|n| n.ends_with("/long.txt") && n.len() > 120));
    let tool = t.iter().find(|e| e.name == "app/bin/tool").unwrap();
    assert_eq!(tool.data.len(), 5000);
    assert_eq!(tool.mode, 0o755);
    let link = t.iter().find(|e| e.name == "app/bin/link").unwrap();
    assert_eq!(link.link.as_deref(), Some("tool"));
}

#[test]
fn zip() {
    let z = unzip(include_bytes!("../tests/data/app.zip")).unwrap();
    assert_eq!(z.len(), 3);
    assert_eq!(z[0].name, "pkg/tool.exe");
    assert_eq!(z[0].data.len(), 9000);
    assert_eq!(&z[1].data[..], &TEXT[..5000]);
    assert!(z[2].dir);
}

#[test]
fn ar() {
    let a = unar(include_bytes!("../tests/data/pkg.deb")).unwrap();
    let names: alloc::vec::Vec<&str> = a.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["debian-binary", "control.tar.gz", "data.tar.gz"]);
    assert_eq!(&a[2].data[..], &TEXT[..1001]);
    assert!(unar(b"not an archive").is_none());
}

#[test]
fn cpio() {
    for data in [&include_bytes!("../tests/data/payload.newc")[..], &include_bytes!("../tests/data/payload.odc")[..]] {
        let c = uncpio(data).unwrap();
        assert_eq!(c.len(), 3);
        assert!(c[0].dir && c[0].name == "Applications");
        assert_eq!(c[1].name, "Applications/tool");
        assert_eq!(&c[1].data[..], &TEXT[..999]);
        assert_eq!(c[1].mode, 0o755);
        assert_eq!(c[2].link.as_deref(), Some("tool"));
    }
}

#[test]
fn xar() {
    let x = unxar(include_bytes!("../tests/data/tool.pkg")).unwrap();
    let names: alloc::vec::Vec<&str> = x.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["tool.pkg/Payload", "tool.pkg/PackageInfo", "tool.pkg"]);
    assert_eq!(&x[1].data[..], b"<pkg-info/>");
    assert!(x[2].dir);
    assert_eq!(uncpio(&x[0].data).unwrap()[1].data.len(), 999);
}
