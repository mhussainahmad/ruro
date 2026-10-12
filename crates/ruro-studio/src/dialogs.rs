//! Native file dialogs through the XDG desktop portal.
//!
//! Dialogs run on a worker thread so the UI keeps drawing; the chosen path
//! comes back through a channel and is turned into a command.

use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender, unbounded};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Open,
    SaveAs,
    Compare,
}

pub struct Dialogs {
    tx: Sender<(Purpose, Option<PathBuf>)>,
    rx: Receiver<(Purpose, Option<PathBuf>)>,
    ctx: egui::Context,
    /// A dialog is open; another request waits for it.
    pub busy: bool,
}

impl Dialogs {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = unbounded();
        Self {
            tx,
            rx,
            ctx,
            busy: false,
        }
    }

    /// Shows a dialog. `dir` is where it starts; `name` the suggested file name.
    pub fn show(&mut self, purpose: Purpose, dir: Option<PathBuf>, name: Option<String>) {
        if self.busy {
            return;
        }
        self.busy = true;
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("ruro-dialog".into())
            .spawn(move || {
                let mut d = rfd::FileDialog::new().add_filter("ruro graph", &["yaml", "yml"]);
                if let Some(dir) = dir {
                    d = d.set_directory(dir);
                }
                let path = match purpose {
                    Purpose::Open => d.set_title("Open Graph").pick_file(),
                    Purpose::Compare => d.set_title("Compare With Graph").pick_file(),
                    Purpose::SaveAs => {
                        if let Some(n) = name {
                            d = d.set_file_name(n);
                        }
                        d.set_title("Save Graph As").save_file()
                    }
                };
                let _ = tx.send((purpose, path));
                ctx.request_repaint();
            });
        if spawned.is_err() {
            self.busy = false;
        }
    }

    /// The answer to the open dialog, once the user has picked (or cancelled).
    pub fn poll(&mut self) -> Option<(Purpose, Option<PathBuf>)> {
        let r = self.rx.try_recv().ok()?;
        self.busy = false;
        Some(r)
    }
}
