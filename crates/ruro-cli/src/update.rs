//! `ruro keygen`, `ruro sign` and `ruro update`: signed updates of daemons.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use ruro_daemon::sign::SecretKey;
use ruro_proto::{Ctl, DaemonStatus, Keys, SignedBinary, UpdateRequest, UpdateRole, UploadChunk};
use zenoh::Wait;

/// Bytes per upload request.
const CHUNK: usize = 1 << 20;

#[derive(Args)]
pub struct KeygenArgs {
    /// Writes `<NAME>.key` (secret, readable only by you) and `<NAME>.pub`.
    #[arg(default_value = "ruro-update")]
    name: PathBuf,
    /// Comment stored with the public key.
    #[arg(long, default_value = "")]
    comment: String,
}

#[derive(Args)]
pub struct SignArgs {
    /// Binary to sign. The signature is written to `<BINARY>.sig`.
    binary: PathBuf,
    /// Secret key made with `ruro keygen`.
    #[arg(long, env = "RURO_UPDATE_KEY")]
    key: PathBuf,
    /// The binary is a host (`ruro-host` or your own), not the daemon.
    #[arg(long)]
    host: bool,
    /// Version the binary reports (default: asked from the binary with
    /// `--version`, when it runs on this machine).
    #[arg(long)]
    binary_version: Option<String>,
    /// Target triple it was built for (default: this machine's, with the
    /// architecture of the binary's ELF header).
    #[arg(long)]
    target: Option<String>,
}

#[derive(Args)]
pub struct UpdateArgs {
    /// New `ruro` binary, signed with `ruro sign` (`<BINARY>.sig` next to it).
    binary: Option<PathBuf>,
    /// New host binary for the daemon's default host, signed with
    /// `ruro sign --host`.
    #[arg(long)]
    host_binary: Option<PathBuf>,
    /// Daemon to update (same as --daemon).
    #[arg(long)]
    to: Option<String>,
    /// Also update every other daemon reachable through the connected one,
    /// before it.
    #[arg(long)]
    peers: bool,
    /// Install a daemon binary that is not newer than the running one.
    #[arg(long)]
    allow_downgrade: bool,
    /// Seconds the new daemon gets to start, on trial and after the restart.
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
    /// Only check that the daemons would accept the binaries.
    #[arg(long)]
    check: bool,
}

pub fn keygen(args: KeygenArgs) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let with = |ext: &str| {
        let mut p = args.name.clone().into_os_string();
        p.push(ext);
        PathBuf::from(p)
    };
    let (secret, public) = (with(".key"), with(".pub"));
    for p in [&secret, &public] {
        if p.exists() {
            bail!("{} exists; not overwriting it", p.display());
        }
    }
    let (key, text) = SecretKey::generate()?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&secret)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .with_context(|| format!("writing {}", secret.display()))?;
    std::fs::write(
        &public,
        format!("{}\n", key.public().to_line(&args.comment)),
    )
    .with_context(|| format!("writing {}", public.display()))?;
    println!(
        "wrote {} (keep it secret) and {}\nadd the public key to the --update-keys file of each daemon",
        secret.display(),
        public.display()
    );
    Ok(())
}

/// The version a binary prints for `--version` (`ruro 0.1.0`).
fn version_of(binary: &Path) -> Result<String> {
    let out = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .with_context(|| format!("running `{} --version`", binary.display()))?;
    anyhow::ensure!(
        out.status.success(),
        "`{} --version` failed",
        binary.display()
    );
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .nth(1)
        .map(str::to_owned)
        .context("no version in its output")
}

/// This machine's target triple with the architecture of `binary`.
fn target_of(binary: &Path) -> Result<String> {
    let own = ruro_daemon::TARGET;
    let arch =
        ruro_daemon::update::elf_arch(binary)?.context("unknown architecture; pass --target")?;
    let rest = own.split_once('-').map_or("", |(_, r)| r);
    match arch {
        // Their triples name more than the architecture.
        "arm" | "x86" => bail!("cannot tell the target of a 32-bit binary; pass --target"),
        a => Ok(format!("{a}-{rest}")),
    }
}

pub fn sign(args: SignArgs) -> Result<()> {
    let key = SecretKey::read(&args.key)?;
    let target = match args.target {
        Some(t) => t,
        None => target_of(&args.binary)?,
    };
    let version = match args.binary_version {
        Some(v) => v,
        None => version_of(&args.binary).context("pass --binary-version")?,
    };
    let data = std::fs::read(&args.binary)
        .with_context(|| format!("reading {}", args.binary.display()))?;
    let role = if args.host {
        UpdateRole::Host
    } else {
        UpdateRole::Daemon
    };
    let signed = key.sign(role, &version, &target, &crate::fleet::sha256_hex(&data));
    let out = sig_path(&args.binary);
    std::fs::write(&out, serde_json::to_string_pretty(&signed)? + "\n")
        .with_context(|| format!("writing {}", out.display()))?;
    println!(
        "signed {} as {} {version} for {target} with key {}…: {}",
        args.binary.display(),
        role.as_str(),
        key.public().short(),
        out.display()
    );
    Ok(())
}

fn sig_path(binary: &Path) -> PathBuf {
    let mut p = binary.as_os_str().to_owned();
    p.push(".sig");
    PathBuf::from(p)
}

/// A binary to send, with its signature.
struct Binary {
    path: PathBuf,
    signed: SignedBinary,
    data: Vec<u8>,
}

fn load(path: &Path, role: UpdateRole) -> Result<Binary> {
    let sig = sig_path(path);
    let text = std::fs::read_to_string(&sig).with_context(|| {
        format!(
            "reading {}: sign the binary first (`ruro sign --key <key>{} {}`)",
            sig.display(),
            if role == UpdateRole::Host {
                " --host"
            } else {
                ""
            },
            path.display()
        )
    })?;
    let signed: SignedBinary =
        serde_json::from_str(&text).with_context(|| format!("{} is malformed", sig.display()))?;
    if signed.role != role {
        bail!(
            "{} signs {} as the {}, not the {}",
            sig.display(),
            path.display(),
            signed.role.as_str(),
            role.as_str()
        );
    }
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let sum = crate::fleet::sha256_hex(&data);
    if sum != signed.sha256 {
        bail!(
            "{} does not match its signature {} (signed SHA-256 {}, file {sum})",
            path.display(),
            sig.display(),
            signed.sha256
        );
    }
    Ok(Binary {
        path: path.to_owned(),
        signed,
        data,
    })
}

/// Every daemon that answers through `session`.
fn daemons(session: &zenoh::Session) -> Result<BTreeSet<String>> {
    let replies = session
        .get("ruro/*/ctl/ping")
        .timeout(Duration::from_millis(1500))
        .wait()
        .map_err(|e| anyhow!("discovery failed: {e}"))?;
    let mut names = BTreeSet::new();
    while let Ok(reply) = replies.recv() {
        if let Ok(sample) = reply.result()
            && let Ok(Ok(name)) =
                serde_json::from_slice::<ruro_proto::CtlReply>(&sample.payload().to_bytes())
        {
            names.insert(name);
        }
    }
    Ok(names)
}

/// Opens a session with the daemon the CLI connects to.
pub type Connect<'a> = &'a dyn Fn() -> Result<zenoh::Session>;

pub fn update(connect: Connect, default: Option<&str>, args: UpdateArgs) -> Result<()> {
    let mut binaries = Vec::new();
    if let Some(p) = &args.binary {
        binaries.push(load(p, UpdateRole::Daemon)?);
    }
    if let Some(p) = &args.host_binary {
        binaries.push(load(p, UpdateRole::Host)?);
    }
    if binaries.is_empty() {
        bail!("nothing to update: give a ruro binary, --host-binary, or both");
    }
    let session = connect()?;
    let target = ruro_proto::discover(&session, args.to.as_deref().or(default), crate::DISCOVERY)?;
    let target = target.daemon().to_owned();
    // Peers first: updating the connected daemon restarts the router the
    // others are reached through.
    let mut order: Vec<String> = Vec::new();
    if args.peers {
        order.extend(daemons(&session)?.into_iter().filter(|d| *d != target));
    }
    order.push(target);

    let request = |check| UpdateRequest {
        binaries: binaries.iter().map(|b| b.signed.clone()).collect(),
        allow_downgrade: args.allow_downgrade,
        check,
        timeout_s: args.timeout,
    };
    // Everyone is checked before anyone is updated.
    for name in &order {
        let reply = ruro_proto::request(
            &session,
            &Keys::new(name),
            Ctl::Update,
            &serde_json::to_string(&request(true))?,
            crate::TIMEOUT,
        )?;
        println!("{reply}");
    }
    if args.check {
        return Ok(());
    }
    let mut failed = Vec::new();
    for name in &order {
        if let Err(e) = update_one(connect, &session, name, &binaries, &request(false), &args) {
            eprintln!("error: `{name}`: {e:#}");
            failed.push(name.clone());
        }
    }
    if !failed.is_empty() {
        bail!("not updated: {}", failed.join(", "));
    }
    Ok(())
}

fn update_one(
    connect: Connect,
    session: &zenoh::Session,
    name: &str,
    binaries: &[Binary],
    req: &UpdateRequest,
    args: &UpdateArgs,
) -> Result<()> {
    let keys = Keys::new(name);
    for b in binaries {
        upload(session, &keys, b)?;
    }
    let reply = ruro_proto::request(
        session,
        &keys,
        Ctl::Update,
        &serde_json::to_string(req)?,
        Duration::from_secs_f64(args.timeout + 30.0),
    )?;
    println!("{reply}");
    let Some(daemon) = binaries
        .iter()
        .find(|b| b.signed.role == UpdateRole::Daemon)
    else {
        return Ok(());
    };
    let replied = Instant::now();
    let deadline = replied + Duration::from_secs_f64(args.timeout + 15.0);
    let st = wait_restart(connect, session, name, replied, deadline)?;
    if st.version != daemon.signed.version {
        bail!(
            "came back running {} instead of {}: {}",
            st.version,
            daemon.signed.version,
            st.update.as_deref().unwrap_or("no reason given")
        );
    }
    match &st.update {
        Some(note) => println!("`{name}` runs {} ({note})", st.version),
        None => println!("`{name}` runs {}", st.version),
    }
    Ok(())
}

fn upload(session: &zenoh::Session, keys: &Keys, b: &Binary) -> Result<()> {
    let total = b.data.len() as u64;
    let role = b.signed.role.as_str();
    for (i, part) in b.data.chunks(CHUNK).enumerate() {
        let chunk = UploadChunk {
            name: role.to_owned(),
            offset: (i * CHUNK) as u64,
            total,
            sha256: b.signed.sha256.clone(),
            data: part.to_vec(),
        };
        ruro_proto::request_bytes(
            session,
            keys,
            Ctl::UpdateUpload,
            ruro_proto::encode(&chunk),
            Duration::from_secs(30),
        )
        .with_context(|| format!("uploading {}", b.path.display()))?;
        eprint!(
            "\ruploading {role} to `{}`: {}/{total} bytes",
            keys.daemon(),
            i * CHUNK + part.len()
        );
    }
    eprintln!();
    Ok(())
}

/// Waits for a status of `name` from a daemon started after `replied`. The
/// connected daemon may be the one restarting, so the session is opened
/// again whenever it goes quiet.
fn wait_restart(
    connect: Connect,
    session: &zenoh::Session,
    name: &str,
    replied: Instant,
    deadline: Instant,
) -> Result<DaemonStatus> {
    let mut session = session.clone();
    let mut last: Option<DaemonStatus> = None;
    while Instant::now() < deadline {
        let sub = match session.declare_subscriber(Keys::new(name).status()).wait() {
            Ok(s) => s,
            Err(_) => {
                session = reconnect(connect, deadline)?;
                continue;
            }
        };
        let mut heard = false;
        let quiet = Instant::now() + Duration::from_secs(2);
        while Instant::now() < quiet.min(deadline) {
            let Ok(Some(s)) = sub.recv_timeout(Duration::from_millis(200)) else {
                continue;
            };
            heard = true;
            let Ok(st) = ruro_proto::decode_status(&s.payload().to_bytes()) else {
                continue;
            };
            // Started after the reply: the old process is gone.
            if st.uptime_s < replied.elapsed().as_secs_f64() {
                return Ok(st);
            }
            last = Some(st);
        }
        if !heard {
            session = reconnect(connect, deadline)?;
        }
    }
    bail!(
        "did not come back within the timeout{}",
        match last {
            Some(st) => format!(" (last seen running {})", st.version),
            None => String::new(),
        }
    )
}

fn reconnect(connect: Connect, deadline: Instant) -> Result<zenoh::Session> {
    loop {
        match connect() {
            Ok(s) => return Ok(s),
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(300)),
        }
    }
}
