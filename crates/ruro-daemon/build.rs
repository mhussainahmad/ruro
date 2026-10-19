// The target triple, which updates must have been built for.
fn main() {
    let target = std::env::var("TARGET").expect("cargo sets TARGET");
    println!("cargo:rustc-env=RURO_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
