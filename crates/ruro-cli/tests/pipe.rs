//! The CLI's output piped into a reader that stops early, like `| head`.

use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};

#[test]
fn printing_into_a_closed_pipe_exits_quietly() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../ruro-record/tests/fixtures/rosbag2_fixture/rosbag2_fixture.mcap"
    );
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let out = Command::new(env!("CARGO_BIN_EXE_ruro"))
        .args(["info", fixture])
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_eq!(out.status.signal(), Some(libc::SIGPIPE), "{:?}", out.status);
}
