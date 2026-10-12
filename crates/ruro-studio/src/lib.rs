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
pub mod theme;
mod view3d;
mod watch;
pub mod widgets;

use std::path::PathBuf;

use clap::Parser;

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
        .compact()
        .init();
    let cli = Cli::parse();
    let mut files = cli.files.into_iter();
    let opts = app::Options {
        endpoint: cli.connect,
        daemon: cli.daemon,
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
            .with_app_id("ruro-studio")
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
