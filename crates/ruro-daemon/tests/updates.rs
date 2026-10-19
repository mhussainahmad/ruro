//! What a daemon refuses to update to. (Updates that go through, and their
//! rollback, are tested end to end with the `ruro` binary in ruro-cli.)

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ruro_daemon::sign::SecretKey;
use ruro_proto::{Ctl, Keys, SignedBinary, UpdateRequest, UpdateRole, UploadChunk};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn fake_host(dir: &Path) -> PathBuf {
    let path = dir.join("fake-host");
    std::fs::write(
        &path,
        "#!/bin/sh\nif [ \"$1\" = describe ]; then echo '{\"nodes\":{\"t/x\":{\"kind\":\"t/x\"}}}'; exit 0; fi\nexec sleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct Setup {
    dir: PathBuf,
    name: String,
    session: zenoh::Session,
    key: SecretKey,
    exe: PathBuf,
    _daemon: ruro_daemon::Daemon,
}

impl Setup {
    /// A daemon trusting `key` (or no keys at all), whose "executable" is a
    /// copy of this test binary in a directory of its own.
    fn new(tag: &str, trust: bool) -> Self {
        let dir = std::env::temp_dir().join(format!("ruro-updates-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let exe = dir.join("bin/ruro");
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        let (key, _) = SecretKey::generate().unwrap();
        let keys = dir.join("update-keys");
        std::fs::write(
            &keys,
            format!("# trusted\n{}\n", key.public().to_line("test")),
        )
        .unwrap();
        let name = format!("upd{tag}{}", std::process::id());
        let port = free_port();
        let mut config = ruro_daemon::Config::new();
        config.name = name.clone();
        config.listen = format!("tcp/127.0.0.1:{port}");
        config.host = fake_host(&dir);
        config.executable = exe.clone();
        config.update_keys = trust.then_some(keys);
        let daemon = ruro_daemon::Daemon::start(config).unwrap();
        let session = ruro_proto::connect(&format!("tcp/127.0.0.1:{port}")).unwrap();
        Self {
            dir,
            name,
            session,
            key,
            exe,
            _daemon: daemon,
        }
    }

    fn request(&self, op: Ctl, payload: &str) -> anyhow::Result<String> {
        ruro_proto::request(
            &self.session,
            &Keys::new(&self.name),
            op,
            payload,
            Duration::from_secs(20),
        )
    }

    fn update(
        &self,
        b: &SignedBinary,
        check: bool,
        allow_downgrade: bool,
    ) -> anyhow::Result<String> {
        let req = UpdateRequest {
            binaries: vec![b.clone()],
            allow_downgrade,
            check,
            timeout_s: 10.0,
        };
        self.request(Ctl::Update, &serde_json::to_string(&req).unwrap())
    }

    fn upload(&self, data: &[u8], sha256: &str) -> anyhow::Result<String> {
        let chunk = UploadChunk {
            name: "daemon".into(),
            offset: 0,
            total: data.len() as u64,
            sha256: sha256.into(),
            data: data.to_vec(),
        };
        ruro_proto::request_bytes(
            &self.session,
            &Keys::new(&self.name),
            Ctl::UpdateUpload,
            ruro_proto::encode(&chunk),
            Duration::from_secs(20),
        )
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(&self.name));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn sha256(data: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn err(r: anyhow::Result<String>) -> String {
    format!("{:#}", r.expect_err("refused"))
}

const NEWER: &str = "99.0.0";

#[test]
fn daemons_without_update_keys_refuse_updates() {
    let s = Setup::new("nokeys", false);
    let b = s.key.sign(
        UpdateRole::Daemon,
        NEWER,
        ruro_daemon::TARGET,
        &"0".repeat(64),
    );
    assert!(err(s.update(&b, true, false)).contains("accepts no updates"));
    assert!(err(s.upload(b"x", &sha256(b"x"))).contains("accepts no updates"));
    assert!(!s.exe.with_file_name(".ruro.update").exists());
}

#[test]
fn updates_need_a_trusted_signature_over_what_they_claim() {
    let s = Setup::new("sig", true);
    let sha = sha256(b"anything");
    let target = ruro_daemon::TARGET;
    let good = s.key.sign(UpdateRole::Daemon, NEWER, target, &sha);
    let reply = s.update(&good, true, false).unwrap();
    assert!(
        reply.contains(&format!("accepts daemon {NEWER}")),
        "{reply}"
    );

    // Unsigned, or signed with a key the daemon does not trust.
    let unsigned = SignedBinary {
        signature: String::new(),
        ..good.clone()
    };
    assert!(err(s.update(&unsigned, true, false)).contains("does not match"));
    let (stranger, _) = SecretKey::generate().unwrap();
    let foreign = stranger.sign(UpdateRole::Daemon, NEWER, target, &sha);
    assert!(err(s.update(&foreign, true, false)).contains("does not trust"));
    // A signature moved to another version.
    let moved = SignedBinary {
        version: "100.0.0".into(),
        ..good.clone()
    };
    assert!(err(s.update(&moved, true, false)).contains("does not match"));

    // Another target, signed as such.
    let arm = s.key.sign(
        UpdateRole::Daemon,
        NEWER,
        "riscv64gc-unknown-linux-gnu",
        &sha,
    );
    let e = err(s.update(&arm, true, false));
    assert!(e.contains("built for riscv64gc-unknown-linux-gnu"), "{e}");
}

#[test]
fn updates_must_be_newer_unless_downgrades_are_allowed() {
    let s = Setup::new("ver", true);
    let sha = sha256(b"anything");
    let running = ruro_daemon::version();
    for v in [running, "0.0.1"] {
        let b = s.key.sign(UpdateRole::Daemon, v, ruro_daemon::TARGET, &sha);
        let e = err(s.update(&b, true, false));
        assert!(
            e.contains("not newer") && e.contains("--allow-downgrade"),
            "{e}"
        );
        s.update(&b, true, true).unwrap();
    }
    // A host must go with the daemon's version.
    let host = s
        .key
        .sign(UpdateRole::Host, NEWER, ruro_daemon::TARGET, &sha);
    let e = err(s.update(&host, true, false));
    assert!(
        e.contains("PATH") || e.contains("update them together"),
        "{e}"
    );
}

#[test]
fn updates_wait_for_the_graph_to_stop_or_the_estop() {
    let s = Setup::new("graph", true);
    let b = s.key.sign(
        UpdateRole::Daemon,
        NEWER,
        ruro_daemon::TARGET,
        &sha256(b"x"),
    );
    s.request(
        Ctl::Deploy,
        "name: busy\nnodes:\n  a: { type: t/x, rate: 1 }\n",
    )
    .unwrap();
    let e = err(s.update(&b, true, false));
    assert!(
        e.contains("graph `busy` is running")
            && e.contains("ruro stop")
            && e.contains("ruro estop"),
        "{e}"
    );
    s.request(Ctl::Estop, "engage").unwrap();
    s.update(&b, true, false).unwrap();
    s.request(Ctl::Estop, "release").unwrap();
    s.request(Ctl::Stop, "").unwrap();
    s.update(&b, true, false).unwrap();
}

#[test]
fn staged_binaries_must_be_the_signed_one_for_this_machine() {
    let s = Setup::new("arch", true);
    let original = std::fs::read(&s.exe).unwrap();
    // The same executable, marked for another architecture.
    let mut foreign = original.clone();
    let other: u16 = if std::env::consts::ARCH == "aarch64" {
        62
    } else {
        183
    };
    foreign[18..20].copy_from_slice(&other.to_le_bytes());
    let sha = sha256(&foreign);
    let reply = s.upload(&foreign, &sha).unwrap();
    assert!(reply.contains("stored daemon"), "{reply}");
    assert!(s.exe.with_file_name(".ruro.update").exists());
    let b = s
        .key
        .sign(UpdateRole::Daemon, NEWER, ruro_daemon::TARGET, &sha);
    let e = err(s.update(&b, false, false));
    assert!(
        e.contains("executable for") && e.contains("nothing was replaced"),
        "{e}"
    );
    assert_eq!(std::fs::read(&s.exe).unwrap(), original, "untouched");
    assert!(!s.exe.with_file_name(".ruro.update").exists(), "discarded");

    // An upload that is not the signed binary.
    s.upload(&original, &sha256(&original)).unwrap();
    let e = err(s.update(&b, false, false));
    assert!(e.contains("not the one that was signed"), "{e}");
    // Nothing uploaded at all.
    let e = err(s.update(&b, false, false));
    assert!(e.contains("has not been uploaded"), "{e}");
    assert!(!s.exe.with_file_name("ruro.prev").exists());
}
