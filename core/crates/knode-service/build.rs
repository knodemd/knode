//! Sets `cfg(semantic)` when search by meaning is built in: the `semantic` feature is on and
//! the target is one knode-embed builds for (every target but aarch64 Linux, see
//! knodemd/knode#108).

fn main() {
    println!("cargo::rustc-check-cfg=cfg(semantic)");
    let feature = std::env::var_os("CARGO_FEATURE_SEMANTIC").is_some();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if feature && !(arch == "aarch64" && os == "linux") {
        println!("cargo::rustc-cfg=semantic");
    }
}
