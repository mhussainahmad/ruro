// A window, not a console, on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> eframe::Result<()> {
    ruro_studio::run()
}
