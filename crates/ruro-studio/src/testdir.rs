//! Scratch directories for tests, removed again when the test ends.

use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A fresh, empty directory under the system temp dir. Dropping it removes
/// the directory and everything in it, also when the test panics.
pub struct TempDir(PathBuf);

impl TempDir {
    /// `name` keeps the directories of concurrent tests apart; the process
    /// id keeps concurrent test runs apart.
    pub fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("ruro-studio-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Self(d)
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn temp_dirs_are_removed_on_drop() {
    let d = TempDir::new("drop-check");
    let path = d.to_path_buf();
    std::fs::write(path.join("f"), "x").unwrap();
    assert!(path.is_dir());
    drop(d);
    assert!(!path.exists());
}
