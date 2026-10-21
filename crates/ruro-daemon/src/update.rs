//! Over-the-air updates of the daemon and its host binary.
//!
//! 1. `ruro update` has the daemon check the signed binaries
//!    ([`Ctl::Update`](ruro_proto::Ctl::Update) with `check` set), then
//!    uploads them in chunks. Each is staged next to the binary it replaces
//!    (`.<name>.update`), so that installing it is a rename on one file
//!    system.
//! 2. The daemon checks again, now against the staged files: a signature by
//!    a trusted key over the binary's SHA-256, role, version and target;
//!    the target triple and ELF machine; a daemon version newer than the
//!    running one (unless the client allows a downgrade) and a host version
//!    equal to the daemon's; and no graph running unless the e-stop is
//!    engaged.
//! 3. It starts the staged daemon binary on trial, with the daemon's own
//!    arguments but another name, a loopback endpoint and no peers. The
//!    trial must come up and report the signed version and target within
//!    the timeout, else the staged files are removed and nothing changes.
//!    A staged host binary must answer `describe`.
//! 4. It keeps the current binaries as `<name>.prev`, renames the staged
//!    ones into place, stops the graph and asks to be restarted. The `ruro
//!    daemon` process then executes the new binary in place with the same
//!    arguments or, under a service manager (`RURO_RESTART=exit`), exits
//!    with [`EXIT_RESTART`] for it to start the new binary.
//! 5. A handoff file next to the executable tells the new process what
//!    happened and carries the e-stop over. If the new process cannot
//!    start, it puts the previous binaries back and starts them.

use std::ffi::OsString;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use ruro_proto::{SignedBinary, UpdateRequest, UpdateRole, UploadChunk};
use serde::{Deserialize, Serialize};

use crate::sign::PublicKey;
use crate::{Config, State, runtime_dir};

/// Exit status of a daemon that has installed an update and wants its
/// service manager to start it again (`RestartForceExitStatus=` in the
/// systemd unit).
pub const EXIT_RESTART: i32 = 75;

/// `exit` makes a daemon restart after an update by exiting with
/// [`EXIT_RESTART`] instead of executing the new binary itself.
pub const RESTART_ENV: &str = "RURO_RESTART";

/// Overrides the version a daemon reports. For tests, which need a daemon
/// that an update of the same build makes newer; never passed on to the
/// updated daemon.
pub const VERSION_ENV: &str = "RURO_VERSION_OVERRIDE";

/// Set on a trial start: the file to report to, and what to use instead of
/// the daemon's name, endpoint and host.
const TRIAL_ENV: &str = "RURO_UPDATE_TRIAL";
const TRIAL_NAME_ENV: &str = "RURO_UPDATE_TRIAL_NAME";
const TRIAL_LISTEN_ENV: &str = "RURO_UPDATE_TRIAL_LISTEN";
const TRIAL_HOST_ENV: &str = "RURO_UPDATE_TRIAL_HOST";

/// How long a trial gets to exit after reporting.
const TRIAL_EXIT: Duration = Duration::from_secs(10);

/// What a daemon that accepts updates needs.
pub(crate) struct Updates {
    keys: Vec<PublicKey>,
    exe: PathBuf,
    args: Vec<OsString>,
    listen: String,
}

impl Updates {
    /// `None` when the daemon trusts no update keys.
    pub fn from_config(config: &Config) -> Result<Option<Self>> {
        let Some(path) = &config.update_keys else {
            return Ok(None);
        };
        Ok(Some(Self {
            keys: crate::sign::read_trusted(path)?,
            exe: config.executable.clone(),
            args: config.args.clone(),
            listen: config.listen.clone(),
        }))
    }
}

/// The version this daemon reports.
pub fn version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| {
        std::env::var(VERSION_ENV)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
    })
}

/// The target triple this daemon was built for.
pub const TARGET: &str = env!("RURO_TARGET");

fn updates(st: &State) -> Result<&Updates> {
    st.updates.as_ref().with_context(|| {
        format!(
            "`{}` accepts no updates: start it with --update-keys <file> listing the public keys it trusts",
            st.name
        )
    })
}

/// The file a role replaces.
fn target_path(st: &State, u: &Updates, role: UpdateRole) -> Result<PathBuf> {
    let path = match role {
        UpdateRole::Daemon => u.exe.clone(),
        UpdateRole::Host => {
            ensure!(
                st.host.components().count() > 1,
                "`{}` finds its host binary `{}` on PATH; give the daemon --host <path> (or RURO_HOST) to update it",
                st.name,
                st.host.display()
            );
            std::fs::canonicalize(&st.host)
                .with_context(|| format!("the host binary {}", st.host.display()))?
        }
    };
    let dir = path.parent().context("the binary has no directory")?;
    let c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes())?;
    // SAFETY: `c` is a valid NUL-terminated path.
    if unsafe { libc::access(c.as_ptr(), libc::W_OK) } != 0 {
        bail!(
            "`{}` cannot replace {}: its directory is not writable by the daemon (see the book's Updates page)",
            st.name,
            path.display()
        );
    }
    Ok(path)
}

/// Where the replacement of `target` is staged.
fn staged(target: &Path) -> PathBuf {
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    target.with_file_name(format!(".{name}.update"))
}

/// The staged file once an update has taken it, under this process's id.
fn claimed(target: &Path) -> PathBuf {
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    target.with_file_name(format!(".{name}.update.{}", std::process::id()))
}

/// Handles `Ctl::UpdateUpload`.
pub(crate) fn receive(st: &State, chunk: &UploadChunk) -> Result<String> {
    let u = updates(st)?;
    let role = UpdateRole::parse(&chunk.name)
        .with_context(|| format!("unknown binary `{}`: expected daemon or host", chunk.name))?;
    let target = target_path(st, u, role)?;
    let staged = staged(&target);
    let part = staged.with_extension("part");
    crate::upload::store(&part, &staged, role.as_str(), chunk)
}

/// Refuses while a graph runs, unless the e-stop is engaged.
fn refuse_while_running(st: &State) -> Result<()> {
    let graph = st.sup.lock().graph().map(|g| g.name.clone());
    if let Some(g) = graph
        && !st.estop.load(Ordering::Relaxed)
    {
        bail!(
            "graph `{g}` is running on `{}`; stop it (`ruro stop`) or engage the e-stop (`ruro estop`) first: updating restarts the daemon, which stops the graph",
            st.name
        );
    }
    Ok(())
}

fn semver(v: &str) -> Result<semver::Version> {
    semver::Version::parse(v).with_context(|| format!("`{v}` is not a version"))
}

/// Checks signed binaries against this daemon's keys, version, target and
/// state. Returns each with the file it replaces.
fn check(st: &State, u: &Updates, req: &UpdateRequest) -> Result<Vec<(SignedBinary, PathBuf)>> {
    ensure!(!req.binaries.is_empty(), "nothing to update");
    let mut roles: Vec<UpdateRole> = req.binaries.iter().map(|b| b.role).collect();
    roles.sort();
    roles.dedup();
    ensure!(
        roles.len() == req.binaries.len(),
        "more than one binary for the same role"
    );
    refuse_while_running(st)?;
    let running = semver(version())?;
    // A host travels with the daemon it is installed alongside.
    let daemon_version = match req.binaries.iter().find(|b| b.role == UpdateRole::Daemon) {
        Some(b) => semver(&b.version)?,
        None => running.clone(),
    };
    let mut checked = Vec::new();
    for b in &req.binaries {
        let role = b.role.as_str();
        crate::sign::verify(b, &u.keys)?;
        ensure!(
            b.target == TARGET,
            "the {role} binary was built for {}, but `{}` runs on {TARGET}",
            b.target,
            st.name
        );
        let v = semver(&b.version)?;
        match b.role {
            UpdateRole::Daemon if v <= running && !req.allow_downgrade => bail!(
                "`{}` runs {running}; the update carries {v}, which is not newer (--allow-downgrade installs it anyway)",
                st.name
            ),
            UpdateRole::Host if v != daemon_version && !req.allow_downgrade => bail!(
                "the host binary carries {v}, but the daemon on `{}` will run {daemon_version}; update them together (or pass --allow-downgrade)",
                st.name
            ),
            _ => {}
        }
        checked.push((b.clone(), target_path(st, u, b.role)?));
    }
    Ok(checked)
}

/// Handles `Ctl::Update`.
pub(crate) fn update(st: &Arc<State>, req: &UpdateRequest) -> Result<String> {
    let u = updates(st)?;
    let checked = check(st, u, req)?;
    let describe = |b: &SignedBinary| format!("{} {}", b.role.as_str(), b.version);
    let list: Vec<String> = checked.iter().map(|(b, _)| describe(b)).collect();
    if req.check {
        return Ok(format!("`{}` accepts {}", st.name, list.join(" and ")));
    }
    // Checked, tried and installed under a name of this process's own, so
    // an upload arriving meanwhile (another daemon sharing the executable)
    // cannot replace the file after its hash was checked.
    for (_, target) in &checked {
        let _ = std::fs::rename(staged(target), claimed(target));
    }
    let remove_staged = || {
        for (_, target) in &checked {
            let _ = std::fs::remove_file(claimed(target));
        }
    };
    let tried = try_staged(st, u, req, &checked);
    if let Err(e) = tried {
        remove_staged();
        return Err(e.context(format!(
            "nothing was replaced; `{}` still runs {}",
            st.name,
            version()
        )));
    }

    // Install: the current binaries stay as `.prev`.
    let mut replaced: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (_, target) in &checked {
        match install(&claimed(target), target) {
            Ok(prev) => replaced.push((target.clone(), prev)),
            Err(e) => {
                restore(&replaced);
                remove_staged();
                return Err(e.context(format!(
                    "installing {}; the previous binaries are back",
                    target.display()
                )));
            }
        }
    }
    let installed: Vec<String> = checked
        .iter()
        .map(|(b, t)| format!("{} at {}", describe(b), t.display()))
        .collect();
    tracing::warn!("installed {}", installed.join(", "));
    let Some((daemon, _)) = checked.iter().find(|(b, _)| b.role == UpdateRole::Daemon) else {
        return Ok(format!(
            "installed {} (the previous one is kept as .prev); groups use it from the next deploy",
            installed.join(", ")
        ));
    };

    // The restart stops everything; stop it here, where peers are still
    // reachable and recordings can be finished.
    let estop = st.estop.load(Ordering::Relaxed);
    // The binaries are in place: a failure here must not cancel the
    // restart, or the old daemon would run on with the new files installed.
    let stopped = crate::stop_graph(st).unwrap_or_else(|e| {
        tracing::warn!("stopping the graph before the restart: {e:#}");
        Some(format!("stopping the graph: {e:#}"))
    });
    *st.restart.lock() = Some(Restart {
        name: st.name.clone(),
        exe: u.exe.clone(),
        args: u.args.clone(),
        handoff: Handoff {
            from: version().to_owned(),
            to: daemon.version.clone(),
            estop,
            replaced,
            note: None,
            started: false,
        },
    });
    let mut reply = format!(
        "installed {}; `{}` restarts now",
        installed.join(", "),
        st.name
    );
    if let Some(s) = stopped {
        reply.push_str(&format!(" ({s})"));
    }
    Ok(reply)
}

/// Checks the staged files and starts them on trial.
fn try_staged(
    st: &State,
    u: &Updates,
    req: &UpdateRequest,
    checked: &[(SignedBinary, PathBuf)],
) -> Result<()> {
    for (b, target) in checked {
        let role = b.role.as_str();
        let file = claimed(target);
        ensure!(
            file.exists(),
            "the {role} binary has not been uploaded (`ruro update` uploads it)"
        );
        let sum = crate::upload::sha256_file(&file)?;
        ensure!(
            sum.eq_ignore_ascii_case(&b.sha256),
            "the uploaded {role} binary is not the one that was signed (SHA-256 {sum}, signed {})",
            b.sha256
        );
        check_machine(&file).with_context(|| format!("the {role} binary"))?;
    }
    let staged_host = checked
        .iter()
        .find(|(b, _)| b.role == UpdateRole::Host)
        .map(|(_, t)| claimed(t));
    if let Some(host) = &staged_host {
        crate::load_registry(host).context("the new host binary failed `describe`")?;
    }
    if let Some((b, target)) = checked.iter().find(|(b, _)| b.role == UpdateRole::Daemon) {
        let host = staged_host.unwrap_or_else(|| st.host.clone());
        let timeout = Duration::from_secs_f64(req.timeout_s.clamp(1.0, 600.0));
        let report = trial(st, u, &claimed(target), &host, timeout)
            .context("the new daemon binary failed its trial start")?;
        ensure!(
            report.version == b.version && report.target == b.target,
            "the new daemon binary reports version {} for {}, but was signed as {} for {}",
            report.version,
            report.target,
            b.version,
            b.target
        );
    }
    Ok(())
}

/// ELF machine numbers of the architectures ruro builds for.
const MACHINES: &[(u16, &str)] = &[
    (3, "x86"),
    (40, "arm"),
    (62, "x86_64"),
    (183, "aarch64"),
    (243, "riscv64"),
];

/// The architecture of an ELF executable, as in `std::env::consts::ARCH`
/// (`None` for one ruro does not know).
pub fn elf_arch(path: &Path) -> Result<Option<&'static str>> {
    let mut header = Vec::with_capacity(20);
    std::fs::File::open(path)
        .and_then(|f| f.take(20).read_to_end(&mut header))
        .with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        header.len() == 20 && header.starts_with(b"\x7fELF"),
        "{} is not an ELF executable",
        path.display()
    );
    let machine = match header[5] {
        1 => u16::from_le_bytes([header[18], header[19]]),
        2 => u16::from_be_bytes([header[18], header[19]]),
        _ => bail!("{} has a malformed ELF header", path.display()),
    };
    Ok(MACHINES
        .iter()
        .find(|(m, _)| *m == machine)
        .map(|(_, a)| *a))
}

/// Refuses an executable for another architecture than this daemon's.
fn check_machine(path: &Path) -> Result<()> {
    let arch = elf_arch(path)?;
    ensure!(
        arch == Some(std::env::consts::ARCH),
        "it is an executable for {}, but this machine is {}",
        arch.unwrap_or("an unknown architecture"),
        std::env::consts::ARCH
    );
    Ok(())
}

/// What a trial start reports once it is up.
#[derive(Debug, Serialize, Deserialize)]
struct TrialReport {
    version: String,
    target: String,
}

/// A loopback endpoint for a trial, with the same protocol as `listen`.
fn trial_endpoint(listen: &str) -> Result<String> {
    let proto = match listen.split_once('/') {
        Some(("tls", _)) => "tls",
        _ => "tcp",
    };
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    Ok(format!("{proto}/127.0.0.1:{port}"))
}

/// Starts `exe` as a daemon with this daemon's arguments, under another
/// name and endpoint, and waits for it to report.
fn trial(
    st: &State,
    u: &Updates,
    exe: &Path,
    host: &Path,
    timeout: Duration,
) -> Result<TrialReport> {
    let name = format!("{}-trial", st.name);
    let report = runtime_dir(&st.name).join("update-trial.json");
    let _ = std::fs::remove_file(&report);
    let mut cmd = Command::new(exe);
    cmd.args(u.args.iter().skip(1))
        .env(TRIAL_ENV, &report)
        .env(TRIAL_NAME_ENV, &name)
        .env(TRIAL_LISTEN_ENV, trial_endpoint(&u.listen)?)
        .env(TRIAL_HOST_ENV, host)
        .env_remove(VERSION_ENV)
        .stdin(Stdio::null());
    let mut child = spawn(&mut cmd)?;
    let read = || -> Option<TrialReport> {
        serde_json::from_str(&std::fs::read_to_string(&report).ok()?).ok()
    };
    let deadline = Instant::now() + timeout;
    let result = loop {
        if let Some(r) = read() {
            break Ok(r);
        }
        if let Some(status) = child.try_wait()? {
            break read().ok_or_else(|| anyhow!("it exited ({status}) before it was up"));
        }
        if Instant::now() >= deadline {
            break Err(anyhow!(
                "it was not up within {:.0} s",
                timeout.as_secs_f64()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // A trial that reported stops by itself.
    let exited = Instant::now() + TRIAL_EXIT;
    while child.try_wait()?.is_none() {
        if result.is_err() || Instant::now() >= exited {
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_dir_all(runtime_dir(&name));
    result
}

/// Spawns a binary that may have been written a moment ago.
fn spawn(cmd: &mut Command) -> Result<Child> {
    let mut attempts = 0;
    loop {
        match cmd.spawn() {
            // Busy while a process forked meanwhile still holds it open.
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && attempts < 50 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            r => {
                return r.with_context(|| format!("starting {}", cmd.get_program().display()));
            }
        }
    }
}

/// Keeps `target` as `<target>.prev` and renames `staged` over it. Returns
/// the previous binary's path.
fn install(staged: &Path, target: &Path) -> Result<PathBuf> {
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let prev = target.with_file_name(format!("{name}.prev"));
    let tmp = target.with_file_name(format!(".{name}.prev.tmp"));
    let _ = std::fs::remove_file(&tmp);
    if std::fs::hard_link(target, &tmp).is_err() {
        std::fs::copy(target, &tmp).with_context(|| format!("copying {}", target.display()))?;
    }
    std::fs::rename(&tmp, &prev)?;
    std::fs::rename(staged, target)?;
    sync_dir(target)?;
    Ok(prev)
}

/// Puts previous binaries back.
fn restore(replaced: &[(PathBuf, PathBuf)]) {
    for (target, prev) in replaced {
        match std::fs::rename(prev, target) {
            Ok(()) => {
                let _ = sync_dir(target);
            }
            Err(e) => tracing::error!(
                "cannot put {} back as {}: {e}",
                prev.display(),
                target.display()
            ),
        }
    }
}

/// Makes renames in the directory of `path` durable.
fn sync_dir(path: &Path) -> Result<()> {
    let dir = path.parent().context("no directory")?;
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// Passed from a daemon that installed an update to the process that runs
/// it, in a file next to the executable: service managers may remove the
/// runtime directory when the daemon exits to be restarted (systemd's
/// `RuntimeDirectory=`), and the update could write there anyway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handoff {
    from: String,
    to: String,
    /// Whether the e-stop was engaged: the new daemon starts with it engaged.
    estop: bool,
    /// Binaries replaced, with where the previous ones were kept.
    replaced: Vec<(PathBuf, PathBuf)>,
    /// Set once the update was rolled back: why.
    note: Option<String>,
    /// The updated daemon started and is on probation: if it stops before
    /// the probation ends, other than when asked to, the next start finds
    /// this still set and rolls the update back.
    #[serde(default)]
    started: bool,
}

/// `.ruro.handoff-<name>.json` next to the daemon's executable `exe`.
pub fn handoff_file(exe: &Path, name: &str) -> PathBuf {
    let file = exe.file_name().unwrap_or_default().to_string_lossy();
    exe.with_file_name(format!(".{file}.handoff-{name}.json"))
}

impl Handoff {
    /// Leaves the handoff for the next start of the daemon called `name`
    /// from `exe`.
    pub fn save(&self, exe: &Path, name: &str) -> Result<()> {
        let path = handoff_file(exe, name);
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::File::open(&tmp)?.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        sync_dir(&path)
    }

    /// Takes the handoff left for the daemon called `name`, if any.
    pub fn take(exe: &Path, name: &str) -> Option<Self> {
        let path = handoff_file(exe, name);
        let text = std::fs::read_to_string(&path).ok()?;
        let _ = std::fs::remove_file(&path);
        serde_json::from_str(&text)
            .inspect_err(|e| tracing::warn!("ignoring {}: {e}", path.display()))
            .ok()
    }

    /// Whether this is an update that has not been started yet (rather than
    /// the report of one that was rolled back).
    pub fn is_pending(&self) -> bool {
        self.note.is_none() && !self.started
    }

    /// Whether the updated daemon started and then stopped while on
    /// probation, without being asked to.
    pub fn died_on_probation(&self) -> bool {
        self.note.is_none() && self.started
    }

    /// Puts the updated daemon, which has started, on probation until
    /// [`Handoff::accept`].
    pub fn start_probation(mut self, config: &Config) -> Result<()> {
        self.started = true;
        self.save(&config.executable, &config.name)
    }

    /// Ends the probation of the daemon called `name` run from `exe`: the
    /// update stays.
    pub fn accept(exe: &Path, name: &str) {
        let path = handoff_file(exe, name);
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("{}: {e}", path.display());
        }
    }

    /// Carries the outcome and the e-stop into the daemon's configuration.
    pub fn configure(&self, config: &mut Config) {
        config.update_note = Some(match &self.note {
            Some(n) => n.clone(),
            None => format!("updated from {} to {}", self.from, self.to),
        });
        config.estopped |= self.estop;
    }

    /// After the updated daemon failed (`what`: "did not start (...)"):
    /// puts the previous binaries back and starts them in place of this
    /// process. Returns the error to exit with: under a service manager,
    /// which then starts the previous binary, or if executing it failed.
    pub fn roll_back(mut self, config: &Config, what: &str) -> anyhow::Error {
        restore(&self.replaced);
        let note = format!("rolled back to {}: {} {what}", self.from, self.to);
        tracing::error!("{note}");
        self.replaced.clear();
        self.note = Some(note.clone());
        if let Err(e) = self.save(&config.executable, &config.name) {
            tracing::warn!("{e:#}");
        }
        if restart_by_exit() {
            return anyhow!("{note}");
        }
        let e = Command::new(&config.executable)
            .args(config.args.iter().skip(1))
            .exec();
        anyhow!("{note}; starting the previous binary failed: {e}")
    }
}

fn restart_by_exit() -> bool {
    std::env::var(RESTART_ENV).is_ok_and(|v| v == "exit")
}

/// An installed update waiting for the daemon to stop.
pub struct Restart {
    name: String,
    exe: PathBuf,
    args: Vec<OsString>,
    handoff: Handoff,
}

impl Restart {
    /// The version being started.
    pub fn version(&self) -> &str {
        &self.handoff.to
    }

    /// Starts the installed daemon in place of this stopped one: executes
    /// it with the same arguments, or under a service manager exits with
    /// [`EXIT_RESTART`]. Returns only if that failed, after putting the
    /// previous binaries back.
    pub fn exec(self) -> anyhow::Error {
        if let Err(e) = self.handoff.save(&self.exe, &self.name) {
            restore(&self.handoff.replaced);
            return e.context("saving the update handoff; the previous binaries are back");
        }
        if restart_by_exit() {
            tracing::warn!(
                "exiting with status {EXIT_RESTART} for the service manager to start {}",
                self.handoff.to
            );
            std::process::exit(EXIT_RESTART);
        }
        tracing::warn!("starting {} {}", self.exe.display(), self.handoff.to);
        let e = Command::new(&self.exe)
            .args(self.args.iter().skip(1))
            .env_remove(VERSION_ENV)
            .exec();
        restore(&self.handoff.replaced);
        let _ = std::fs::remove_file(handoff_file(&self.exe, &self.name));
        anyhow!(
            "cannot start {}: {e}; the previous binaries are back",
            self.exe.display()
        )
    }
}

/// A trial start of a staged daemon binary, set up by the daemon that is
/// being updated.
pub struct Trial {
    report: PathBuf,
    name: String,
    listen: String,
    host: PathBuf,
}

impl Trial {
    /// The trial this process was started for, if any.
    pub fn from_env() -> Option<Self> {
        let var = |k| std::env::var_os(k);
        Some(Self {
            report: var(TRIAL_ENV)?.into(),
            name: var(TRIAL_NAME_ENV)?.into_string().ok()?,
            listen: var(TRIAL_LISTEN_ENV)?.into_string().ok()?,
            host: var(TRIAL_HOST_ENV)?.into(),
        })
    }

    /// Starts the daemon as configured, apart from the name, endpoint, host
    /// and peers, reports that it is up and stops it.
    pub fn run(self, mut config: Config) -> Result<()> {
        config.name = self.name;
        config.listen = self.listen;
        config.host = self.host;
        config.peers.clear();
        let daemon = crate::Daemon::start(config)?;
        let report = TrialReport {
            version: version().to_owned(),
            target: TARGET.to_owned(),
        };
        let tmp = self.report.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(&report)?)?;
        std::fs::rename(&tmp, &self.report)?;
        daemon.shutdown();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executables_are_checked_for_this_machine() {
        let dir = std::env::temp_dir().join(format!("ruro-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let me = std::env::current_exe().unwrap();
        assert_eq!(elf_arch(&me).unwrap(), Some(std::env::consts::ARCH));
        check_machine(&me).unwrap();

        // The same binary with another ELF machine.
        let mut bytes = std::fs::read(&me).unwrap();
        let other = if std::env::consts::ARCH == "aarch64" {
            62u16
        } else {
            183
        };
        bytes[18..20].copy_from_slice(&other.to_le_bytes());
        let foreign = dir.join("foreign");
        std::fs::write(&foreign, &bytes).unwrap();
        let err = check_machine(&foreign).unwrap_err().to_string();
        assert!(err.contains("but this machine is"), "{err}");

        let script = dir.join("script");
        std::fs::write(&script, "#!/bin/sh\nexit 1\n").unwrap();
        let err = check_machine(&script).unwrap_err().to_string();
        assert!(err.contains("not an ELF executable"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trials_listen_on_loopback_with_the_daemon_protocol() {
        assert!(
            trial_endpoint("tls/0.0.0.0:7447")
                .unwrap()
                .starts_with("tls/127.0.0.1:")
        );
        assert!(
            trial_endpoint("tcp/[::]:7447")
                .unwrap()
                .starts_with("tcp/127.0.0.1:")
        );
        assert!(
            trial_endpoint("udp/1.2.3.4:5")
                .unwrap()
                .starts_with("tcp/127.0.0.1:")
        );
    }

    #[test]
    fn installs_keep_the_previous_binary() {
        let dir = std::env::temp_dir().join(format!("ruro-install-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("ruro");
        std::fs::write(&target, "old").unwrap();
        std::fs::write(staged(&target), "new").unwrap();
        let prev = install(&staged(&target), &target).unwrap();
        assert_eq!(prev, dir.join("ruro.prev"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(&prev).unwrap(), "old");
        assert!(!staged(&target).exists());
        restore(&[(target.clone(), prev.clone())]);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
        assert!(!prev.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
