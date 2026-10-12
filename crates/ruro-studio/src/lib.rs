//! ruro studio: a native editor for building, running and inspecting graphs.
//!
//! The `ruro-studio` binary is a thin wrapper around [`run`]. Programs that
//! want extra visualizers or panels link their crates and call [`run`] from
//! their own `main`; see [`plugin`].

mod app;
mod builtin;
mod canvas;
mod dialogs;
mod diff;
pub mod icons;
mod inspector;
mod link;
mod model;
mod palette;
mod panel;
pub mod plugin;
mod ros;
mod settings;
mod shell;
mod sidebar;
mod source;
mod templates;
#[cfg(test)]
mod testdir;
pub mod theme;
mod view3d;
mod watch;
pub mod widgets;

use std::path::PathBuf;

use clap::Parser;

/// The studio's application id: the Wayland app id and X11 window class of
/// its window, and the name of its desktop file, icons and AppStream
/// metadata (`packaging/linux`), so desktops match the window to its entry.
pub const APP_ID: &str = "io.github.mhussainahmad.ruro.Studio";

#[derive(Parser)]
#[command(
    name = "ruro-studio",
    version,
    about = "Desktop editor for ruro graphs"
)]
struct Cli {
    /// Graphs to open.
    files: Vec<PathBuf>,
    /// Daemon endpoint. Point it at a robot, e.g. tcp/192.168.1.20:7447.
    #[arg(long, default_value = ruro_proto::DEFAULT_ENDPOINT, env = "RURO_CONNECT")]
    connect: String,
    /// Daemon name (discovered automatically if omitted).
    #[arg(long, env = "RURO_DAEMON")]
    daemon: Option<String>,
    /// CA certificate that signed the daemon's certificate, for `tls/` endpoints.
    #[arg(long, env = "RURO_TLS_CA")]
    tls_ca: Option<PathBuf>,
    /// Client certificate, for daemons that require one (mutual TLS).
    #[arg(long, env = "RURO_TLS_CERT")]
    tls_cert: Option<PathBuf>,
    /// Key of the client certificate.
    #[arg(long, env = "RURO_TLS_KEY")]
    tls_key: Option<PathBuf>,
    /// Access token of daemons started with `--token`. Visible to other users
    /// in `ps`; prefer `--token-file`.
    #[arg(long, env = "RURO_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// File whose first line is the access token. Overrides `--token`.
    #[arg(long, env = "RURO_TOKEN_FILE")]
    token_file: Option<PathBuf>,
    /// Do not start a local daemon when none is running.
    #[arg(long)]
    no_spawn: bool,
    /// Use the built-in file picker instead of the desktop's file dialogs.
    #[arg(long)]
    no_native_dialogs: bool,
    /// Do not reopen the graphs that were open last time.
    #[arg(long)]
    fresh: bool,
}

/// Parses the command line and runs the studio until its window closes.
pub fn run() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RURO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,zenoh=warn")),
        )
        .with_target(false)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .compact()
        .init();
    let cli = Cli::parse();
    let token = match &cli.token_file {
        Some(path) => match ruro_proto::read_token_file(path) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("ruro-studio: {e:#}");
                std::process::exit(2);
            }
        },
        None => cli.token.clone().filter(|t| !t.is_empty()),
    };
    let mut files = cli.files.into_iter();
    let opts = app::Options {
        endpoint: cli.connect,
        daemon: cli.daemon,
        security: ruro_proto::Security {
            cert: cli.tls_cert,
            key: cli.tls_key,
            ca: cli.tls_ca,
            token,
        },
        spawn_daemon: !cli.no_spawn,
        file: files.next(),
        store: settings::Store::xdg(),
        native_dialogs: !cli.no_native_dialogs,
    };
    let more: Vec<PathBuf> = files.collect();
    let restore = !cli.fresh && opts.file.is_none();
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ruro studio")
            .with_app_id(APP_ID)
            .with_inner_size([1480.0, 920.0])
            .with_min_inner_size([960.0, 600.0])
            .with_maximized(true),
        ..Default::default()
    };
    eframe::run_native(
        "ruro studio",
        native,
        Box::new(move |cc| {
            let mut studio = app::Studio::new(cc, opts);
            if restore {
                studio.restore_session();
            }
            for f in more {
                studio.push(app::Cmd::Open(f));
            }
            Ok(Box::new(studio))
        }),
    )
}
