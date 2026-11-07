fn main() {
    // Every justOS driver is a self-contained position-independent image.
    println!("cargo:rustc-link-arg-bins=-e");
    println!("cargo:rustc-link-arg-bins=just_driver_entry");
    println!("cargo:rustc-link-arg-bins=--strip-all");
    println!("cargo:rustc-link-arg-bins=-z");
    println!("cargo:rustc-link-arg-bins=max-page-size=4096");
}
