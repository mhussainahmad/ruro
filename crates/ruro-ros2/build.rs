//! Embeds the bundled interface definitions (`interfaces/<pkg>/msg/*.msg`).

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    // Read at run time: a build script compiled for one checkout can be reused
    // from a shared target directory by another.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
    let root = Path::new(&manifest).join("interfaces");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut entries = Vec::new();
    let mut packages: Vec<_> = std::fs::read_dir(&root)
        .expect("interfaces directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    packages.sort();
    for pkg in packages {
        let name = pkg.file_name().unwrap().to_string_lossy().into_owned();
        let mut files: Vec<_> = std::fs::read_dir(pkg.join("msg"))
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        files.sort();
        for f in files
            .into_iter()
            .filter(|f| f.extension().is_some_and(|e| e == "msg"))
        {
            println!("cargo:rerun-if-changed={}", f.display());
            let stem = f.file_stem().unwrap().to_string_lossy().into_owned();
            entries.push((name.clone(), stem, f));
        }
    }
    let mut out = String::from("/// `(package, message, definition)` of every bundled message.\n");
    out.push_str("pub const BUNDLED: &[(&str, &str, &str)] = &[\n");
    for (pkg, msg, path) in entries {
        let _ = writeln!(
            out,
            "    ({pkg:?}, {msg:?}, include_str!({:?})),",
            path.display().to_string()
        );
    }
    out.push_str("];\n");
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("bundled.rs");
    std::fs::write(dest, out).unwrap();
}
