//! Records the exact target triple (e.g. `aarch64-unknown-linux-musl`) for `irori version`.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env=IRORI_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
