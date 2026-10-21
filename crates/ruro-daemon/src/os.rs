//! What differs between the systems the daemon's code is built for. The
//! daemon runs on Linux (and macOS); on Windows only the parts clients use
//! (signing, checking binaries) are needed, so the rest reports an error.

use std::path::Path;
use std::process::Command;

/// Replaces this process with `cmd`. Returns only on failure.
pub fn exec(cmd: &mut Command) -> std::io::Error {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.exec()
    }
    #[cfg(not(unix))]
    {
        let _ = cmd;
        std::io::Error::other("replacing the running process needs Linux or macOS")
    }
}

/// Whether this process may create files in `dir`.
pub fn dir_writable(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()) else {
            return false;
        };
        // SAFETY: `c` is a valid NUL-terminated path.
        unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(dir).is_ok_and(|m| !m.permissions().readonly())
    }
}

/// Makes `path` executable by everyone (mode 755).
pub fn make_executable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}
