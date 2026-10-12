//! Persistent settings, recent files and autosave of unsaved documents.
//!
//! Settings live in `$XDG_CONFIG_HOME/ruro/studio.json`. Autosaves of unsaved
//! documents go to `$XDG_STATE_HOME/ruro/autosave/` and are offered for
//! recovery the next time the studio starts.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::theme::ThemeKind;

/// Most entries kept in the recent files list.
pub const MAX_RECENT: usize = 12;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeKind,
    pub sidebar_open: bool,
    pub inspector_open: bool,
    pub panel_open: bool,
    pub sidebar_width: Option<f32>,
    pub inspector_width: Option<f32>,
    pub panel_height: Option<f32>,
    /// Most recently opened or saved graphs, newest first.
    pub recent: Vec<PathBuf>,
    /// Graphs open when the studio last closed.
    pub open_files: Vec<PathBuf>,
    /// Collapsed sidebar and inspector sections.
    pub collapsed: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeKind::Dark,
            sidebar_open: true,
            inspector_open: true,
            panel_open: true,
            sidebar_width: None,
            inspector_width: None,
            panel_height: None,
            recent: Vec::new(),
            open_files: Vec::new(),
            collapsed: Vec::new(),
        }
    }
}

impl Settings {
    /// Moves `path` to the front of the recent list.
    pub fn add_recent(&mut self, path: &Path) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(MAX_RECENT);
    }
}

/// An autosaved document from an earlier session.
#[derive(Debug, Clone)]
pub struct Recovery {
    /// The autosave file.
    pub file: PathBuf,
    /// Where the document was saved, if it ever was.
    pub original: Option<PathBuf>,
    pub name: String,
    pub modified: SystemTime,
}

#[derive(Serialize, Deserialize)]
struct AutosaveMeta {
    original: Option<PathBuf>,
    name: String,
}

/// Where settings and autosaves are kept.
#[derive(Debug, Clone)]
pub struct Store {
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
}

impl Store {
    /// The XDG locations for the current user.
    pub fn xdg() -> Option<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let dir = |var: &str, fallback: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .or_else(|| home.as_ref().map(|h| h.join(fallback)))
        };
        Some(Self {
            config_dir: dir("XDG_CONFIG_HOME", ".config")?.join("ruro"),
            state_dir: dir("XDG_STATE_HOME", ".local/state")?.join("ruro"),
        })
    }

    fn settings_file(&self) -> PathBuf {
        self.config_dir.join("studio.json")
    }

    fn autosave_dir(&self) -> PathBuf {
        self.state_dir.join("autosave")
    }

    pub fn load_settings(&self) -> Settings {
        std::fs::read_to_string(self.settings_file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Writes the settings; `text` is their JSON.
    pub fn save_settings(&self, text: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        write_atomic(&self.settings_file(), text.as_bytes())
    }

    /// File stem for a document's autosave: unique to this process and document.
    fn stem(uid: u64) -> String {
        format!("{}-{uid}", std::process::id())
    }

    pub fn write_autosave(
        &self,
        uid: u64,
        name: &str,
        original: Option<&Path>,
        yaml: &str,
    ) -> std::io::Result<()> {
        let dir = self.autosave_dir();
        std::fs::create_dir_all(&dir)?;
        let stem = Self::stem(uid);
        let meta = AutosaveMeta {
            original: original.map(Path::to_path_buf),
            name: name.to_owned(),
        };
        write_atomic(&dir.join(format!("{stem}.yaml")), yaml.as_bytes())?;
        write_atomic(
            &dir.join(format!("{stem}.json")),
            serde_json::to_string(&meta).unwrap_or_default().as_bytes(),
        )
    }

    pub fn remove_autosave(&self, uid: u64) {
        let dir = self.autosave_dir();
        let stem = Self::stem(uid);
        let _ = std::fs::remove_file(dir.join(format!("{stem}.yaml")));
        let _ = std::fs::remove_file(dir.join(format!("{stem}.json")));
    }

    /// Autosaves left by earlier sessions, newest first.
    pub fn take_recoverable(&self) -> Vec<Recovery> {
        let mine = format!("{}-", std::process::id());
        let mut out: Vec<Recovery> = std::fs::read_dir(self.autosave_dir())
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
            .filter(|p| {
                !p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(&mine))
            })
            .filter_map(|file| {
                let meta: Option<AutosaveMeta> =
                    std::fs::read_to_string(file.with_extension("json"))
                        .ok()
                        .and_then(|s| serde_json::from_str(&s).ok());
                let modified = std::fs::metadata(&file).and_then(|m| m.modified()).ok()?;
                let (original, name) = match meta {
                    Some(m) => (m.original, m.name),
                    None => (None, "untitled".into()),
                };
                Some(Recovery {
                    file,
                    original,
                    name,
                    modified,
                })
            })
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.modified));
        out
    }

    /// Deletes a recovered autosave and its metadata.
    pub fn forget(&self, r: &Recovery) {
        let _ = std::fs::remove_file(&r.file);
        let _ = std::fs::remove_file(r.file.with_extension("json"));
    }
}

/// Writes through a temporary file so a crash never leaves a torn file.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TempDir;

    /// A store in a scratch directory, removed when the guard drops.
    fn store(name: &str) -> (Store, TempDir) {
        let root = TempDir::new(&format!("settings-{name}"));
        let store = Store {
            config_dir: root.join("config"),
            state_dir: root.join("state"),
        };
        (store, root)
    }

    #[test]
    fn settings_roundtrip_and_defaults() {
        let (s, _dir) = store("roundtrip");
        assert_eq!(s.load_settings(), Settings::default());
        let mut settings = Settings {
            theme: ThemeKind::Light,
            panel_height: Some(312.0),
            ..Default::default()
        };
        settings.add_recent(Path::new("/tmp/a.yaml"));
        settings.add_recent(Path::new("/tmp/b.yaml"));
        settings.add_recent(Path::new("/tmp/a.yaml"));
        assert_eq!(settings.recent.len(), 2);
        assert_eq!(settings.recent[0], PathBuf::from("/tmp/a.yaml"));
        s.save_settings(&serde_json::to_string(&settings).unwrap())
            .unwrap();
        assert_eq!(s.load_settings(), settings);
    }

    #[test]
    fn autosaves_from_other_sessions_are_recoverable() {
        let (s, _dir) = store("autosave");
        s.write_autosave(7, "demo", Some(Path::new("/x/demo.yaml")), "name: demo\n")
            .unwrap();
        // Our own autosaves are not offered back to us.
        assert!(s.take_recoverable().is_empty());
        // Pretend another process wrote it.
        let dir = s.autosave_dir();
        for ext in ["yaml", "json"] {
            std::fs::rename(
                dir.join(format!("{}-7.{ext}", std::process::id())),
                dir.join(format!("1-7.{ext}")),
            )
            .unwrap();
        }
        let r = s.take_recoverable();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "demo");
        assert_eq!(r[0].original.as_deref(), Some(Path::new("/x/demo.yaml")));
        s.forget(&r[0]);
        assert!(s.take_recoverable().is_empty());
    }
}
