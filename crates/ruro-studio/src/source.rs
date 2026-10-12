//! Opening a node's implementation in an editor.
//!
//! Node descriptors carry `path:line` from the derive macro. The path is as
//! the compiler saw it: absolute for dependencies, relative to the workspace
//! for workspace crates.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Splits `path:line` into its parts.
pub fn parse(source: &str) -> (&str, Option<u32>) {
    match source.rsplit_once(':') {
        Some((path, line)) if line.chars().all(|c| c.is_ascii_digit()) && !line.is_empty() => {
            (path, line.parse().ok())
        }
        _ => (source, None),
    }
}

/// Finds the file on disk: as given, then relative to the working directory
/// and to the directories above the studio executable (a workspace build).
pub fn locate(path: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let p = Path::new(path);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    roots.iter().map(|r| r.join(p)).find(|c| c.is_file())
}

fn search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.extend(cwd.ancestors().take(4).map(Path::to_path_buf));
    }
    if let Ok(exe) = std::env::current_exe() {
        roots.extend(exe.ancestors().skip(1).take(5).map(Path::to_path_buf));
    }
    roots
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
}

/// How an editor is started for `file` at `line`.
pub fn editor_command(file: &Path, line: Option<u32>) -> Command {
    let target = match line {
        Some(l) => format!("{}:{l}", file.display()),
        None => file.display().to_string(),
    };
    for (program, goto) in [
        ("code", true),
        ("codium", true),
        ("cursor", true),
        ("zed", false),
    ] {
        if on_path(program) {
            let mut c = Command::new(program);
            if goto {
                c.arg("--goto");
            }
            c.arg(&target);
            return c;
        }
    }
    // A graphical editor named by the user.
    if let Some(visual) = std::env::var_os("VISUAL").filter(|v| !v.is_empty()) {
        let mut c = Command::new(visual);
        if let Some(l) = line {
            c.arg(format!("+{l}"));
        }
        c.arg(file);
        return c;
    }
    let mut c = Command::new("xdg-open");
    c.arg(file);
    c
}

/// Opens `source` (`path:line`). Returns a message for the user.
pub fn open(source: &str) -> Result<String, String> {
    let (path, line) = parse(source);
    let file =
        locate(path, &search_roots()).ok_or_else(|| format!("Source file not found: {path}"))?;
    let mut cmd = editor_command(&file, line);
    let program = cmd.get_program().to_string_lossy().to_string();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| format!("Opening {} in {program}", crate::app::file_name(&file)))
        .map_err(|e| format!("Cannot start {program}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_locations() {
        assert_eq!(parse("src/a.rs:12"), ("src/a.rs", Some(12)));
        assert_eq!(parse("/x/y.rs"), ("/x/y.rs", None));
        assert_eq!(parse("C:odd"), ("C:odd", None));
    }

    #[test]
    fn finds_workspace_relative_files() {
        // Node sources of workspace crates are relative to the workspace root.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let found = locate(
            "nodes/ruro-nodes-std/src/image.rs",
            &[PathBuf::from("/nope"), root],
        );
        assert!(found.is_some());
        assert!(locate("no/such/file.rs", &[PathBuf::from(".")]).is_none());
    }

    #[test]
    fn derive_records_the_source() {
        ruro_nodes_std::link();
        let reg = ruro_node::registry();
        let src = reg.get("std/blur").and_then(|d| d.source.clone()).unwrap();
        let (path, line) = parse(&src);
        assert!(path.ends_with("image.rs"), "{src}");
        assert!(line.is_some());
    }

    #[test]
    fn editor_command_targets_the_line() {
        let c = editor_command(Path::new("/tmp/a.rs"), Some(7));
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into()).collect();
        assert!(args.iter().any(|a| a.contains("a.rs")));
    }
}
