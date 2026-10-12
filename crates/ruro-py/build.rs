//! Lets test binaries find libpython when it lives outside the default
//! library path (e.g. Homebrew or pyenv installs). Extension module builds do
//! not link libpython, so the rpath is harmless there.

fn main() {
    let config = pyo3_build_config::get();
    if let Some(dir) = config.lib_dir() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
}
