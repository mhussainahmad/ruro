//! ruro studio: a native editor for building, running and inspecting graphs.

mod app;
mod canvas;
mod icons;
mod inspector;
mod link;
mod model;
mod palette;
mod panel;
mod shell;
mod sidebar;
mod theme;
mod watch;
mod widgets;

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "ruro-studio",
    version,
    about = "Desktop editor for ruro graphs"
)]
struct Cli {
    /// Graph to open.
    file: Option<PathBuf>,
    /// Daemon endpoint. Point it at a robot, e.g. tcp/192.168.1.20:7447.
    #[arg(long, default_value = ruro_proto::DEFAULT_ENDPOINT, env = "RURO_CONNECT")]
    connect: String,
    /// Daemon name (discovered automatically if omitted).
    #[arg(long, env = "RURO_DAEMON")]
    daemon: Option<String>,
    /// Do not start a local daemon when none is running.
    #[arg(long)]
    no_spawn: bool,
}

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RURO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,zenoh=warn")),
        )
        .with_target(false)
        .compact()
        .init();
    let cli = Cli::parse();
    let opts = app::Options {
        endpoint: cli.connect,
        daemon: cli.daemon,
        spawn_daemon: !cli.no_spawn,
        file: cli.file,
    };
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
        Box::new(|cc| Ok(Box::new(app::Studio::new(cc, opts)))),
    )
}
