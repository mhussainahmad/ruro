//! Embeds the path of libpython so the host runs without LD_LIBRARY_PATH
//! when Python is installed outside the default library path.

fn main() {
    let config = pyo3_build_config::get();
    if let Some(dir) = config.lib_dir() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
}
