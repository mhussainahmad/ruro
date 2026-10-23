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

/// The processes this process started that are still running (Linux).
#[cfg(target_os = "linux")]
pub fn child_pids() -> Vec<i32> {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };
    let mut pids: Vec<i32> = tasks
        .filter_map(|t| t.ok())
        .filter_map(|t| std::fs::read_to_string(t.path().join("children")).ok())
        .flat_map(|c| {
            c.split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect::<Vec<i32>>()
        })
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Marks every open file descriptor but standard input, output and error
/// close-on-exec, so that a program executed in place of this one inherits
/// none of this one's sockets or locks.
#[cfg(unix)]
pub fn close_on_exec_all() {
    let fds: Vec<i32> =
        match std::fs::read_dir("/proc/self/fd").or_else(|_| std::fs::read_dir("/dev/fd")) {
            Ok(dir) => dir
                .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                .collect(),
            Err(_) => (3..1024).collect(),
        };
    for fd in fds.into_iter().filter(|fd| *fd > 2) {
        // SAFETY: fcntl on a descriptor number; harmless if it is not open.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}
