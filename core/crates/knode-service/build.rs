//! Sets `cfg(semantic)` when search by meaning is built in, that is when the `semantic`
//! feature is on.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(semantic)");
    if std::env::var_os("CARGO_FEATURE_SEMANTIC").is_some() {
        println!("cargo::rustc-cfg=semantic");
    }
}
