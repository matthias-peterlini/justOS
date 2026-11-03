fn main() {
    // arcOS programs are self-contained position-independent images.
    println!("cargo:rustc-link-arg-bins=-e");
    println!("cargo:rustc-link-arg-bins=_start");
    println!("cargo:rustc-link-arg-bins=--strip-all");
    println!("cargo:rustc-link-arg-bins=-z");
    println!("cargo:rustc-link-arg-bins=max-page-size=4096");
}
