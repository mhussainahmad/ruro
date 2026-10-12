//! Extending the studio with visualizers and panels.
//!
//! The studio is built from a library, so a program can link extra crates
//! into its own studio binary, the same way a custom host links node crates:
//!
//! ```ignore
//! // my-studio/src/main.rs
//! fn main() -> eframe::Result<()> {
//!     my_visualizers::link();
//!     ruro_studio::run()
//! }
//! ```
//!
//! A **visualizer** draws a watched output port in the Watch panel. Register
//! one with [`inventory::submit!`]:
//!
//! ```no_run
//! use ruro_studio::plugin::{PreviewKind, Visualizer, VisualizerKind, inventory};
//!
//! struct Gauge { value: f64 }
//!
//! impl Visualizer for Gauge {
//!     fn push(&mut self, _ctx: &egui::Context, preview: &ruro_studio::plugin::Preview) {
//!         if let ruro_studio::plugin::Preview::Values(v) = preview {
//!             self.value = v.first().map_or(0.0, |(_, x)| *x);
//!         }
//!     }
//!     fn show(&mut self, ui: &mut egui::Ui) {
//!         ui.add(egui::ProgressBar::new(self.value as f32));
//!     }
//! }
//!
//! inventory::submit! {
//!     VisualizerKind {
//!         id: "my/gauge",
//!         title: "Gauge",
//!         icon: ruro_studio::icons::GAUGE,
//!         score: |ty, kind| u8::from(ty == "std/Scalar" && kind == PreviewKind::Values) * 20,
//!         create: |_| Box::new(Gauge { value: 0.0 }),
//!     }
//! }
//! # fn main() {}
//! ```
//!
//! The highest-scoring visualizer is used for a port; users can switch to any
//! other that scores above zero from the tile header.
//!
//! A **panel** adds a tab to the bottom panel. It sees the open graph and the
//! live state of the daemon, and can ask the studio to focus or watch things
//! through its [`PanelContext`]:
//!
//! ```no_run
//! use ruro_studio::plugin::{PanelContext, PanelKind, StudioPanel, inventory};
//! use ruro_studio::theme::{TEXT_DIM, font};
//!
//! struct NodeCount;
//!
//! impl StudioPanel for NodeCount {
//!     fn show(&mut self, cx: &mut PanelContext<'_>, ui: &mut egui::Ui) {
//!         let text = format!("{} nodes, {} running", cx.graph.nodes.len(), cx.metrics.len());
//!         ui.label(egui::RichText::new(text).font(font(12.5)).color(TEXT_DIM()));
//!         if let Some(first) = cx.graph.nodes.keys().next().cloned() {
//!             if ui.button("Select the first node").clicked() {
//!                 cx.focus(&first);
//!             }
//!         }
//!     }
//! }
//!
//! inventory::submit! {
//!     PanelKind { id: "my/count", title: "Count", create: || Box::new(NodeCount) }
//! }
//! # fn main() {}
//! ```

use std::collections::HashMap;

use egui::Ui;
use ruro_graph::{Graph, Registry};
use ruro_proto::{DaemonStatus, NodeMetrics};

pub use inventory;
pub use ruro_proto::Preview;

/// The shape of a preview, for choosing a visualizer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Image,
    Values,
    Text,
    Points,
    Frames,
}

impl PreviewKind {
    pub fn of(p: &Preview) -> Self {
        match p {
            Preview::Image { .. } => Self::Image,
            Preview::Values(_) => Self::Values,
            Preview::Text(_) => Self::Text,
            Preview::Points { .. } => Self::Points,
            Preview::Frames(_) => Self::Frames,
        }
    }
}

/// The port a visualizer is created for.
pub struct WatchInfo<'a> {
    pub node: &'a str,
    pub port: &'a str,
    /// Message type, e.g. `std/Image`.
    pub ty: &'a str,
}

/// Draws the live values of one watched port.
pub trait Visualizer {
    /// Called for every new preview of the port.
    fn push(&mut self, ctx: &egui::Context, preview: &Preview);
    /// Draws the tile body into the space `ui` offers.
    fn show(&mut self, ui: &mut Ui);
    /// Preferred tile width at the given tile height.
    fn width(&self, height: f32) -> f32 {
        let _ = height;
        420.0
    }
    /// The colour theme changed; drop anything cached with colours in it.
    fn theme_changed(&mut self) {}
}

/// A visualizer registration.
pub struct VisualizerKind {
    /// Unique id, e.g. `std/plot`.
    pub id: &'static str,
    /// Name shown in the tile's visualizer menu.
    pub title: &'static str,
    /// A glyph from [`crate::icons`].
    pub icon: &'static str,
    /// How well this visualizer suits a port of type `ty` producing previews
    /// of `kind`: 0 if it cannot show it, higher is better.
    pub score: fn(ty: &str, kind: PreviewKind) -> u8,
    pub create: fn(&WatchInfo) -> Box<dyn Visualizer>,
}

inventory::collect!(VisualizerKind);

/// Every registered visualizer, in id order.
pub fn visualizers() -> Vec<&'static VisualizerKind> {
    let mut v: Vec<&'static VisualizerKind> = inventory::iter::<VisualizerKind>().collect();
    v.sort_by_key(|k| k.id);
    v
}

/// Visualizers able to show `ty`/`kind`, best first.
pub fn candidates(ty: &str, kind: PreviewKind) -> Vec<&'static VisualizerKind> {
    let mut v: Vec<(u8, &'static VisualizerKind)> = visualizers()
        .into_iter()
        .map(|k| ((k.score)(ty, kind), k))
        .filter(|(s, _)| *s > 0)
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(b.1.id)));
    v.into_iter().map(|(_, k)| k).collect()
}

/// What a panel can see, and ask of the studio.
pub struct PanelContext<'a> {
    /// The graph in the active editor tab.
    pub graph: &'a Graph,
    pub registry: &'a Registry,
    /// Status of the connected daemon, if any.
    pub status: Option<&'a DaemonStatus>,
    /// Latest metrics of running nodes, by node id.
    pub metrics: &'a HashMap<String, NodeMetrics>,
    /// True while the open graph is the one running.
    pub live: bool,
    pub(crate) requests: Vec<Request>,
}

pub(crate) enum Request {
    Focus(String),
    Watch {
        node: String,
        port: String,
        ty: String,
    },
}

impl PanelContext<'_> {
    /// Selects a node on the canvas and shows it in the inspector.
    pub fn focus(&mut self, node: &str) {
        self.requests.push(Request::Focus(node.to_owned()));
    }

    /// Starts watching an output port.
    pub fn watch(&mut self, node: &str, port: &str, ty: &str) {
        self.requests.push(Request::Watch {
            node: node.to_owned(),
            port: port.to_owned(),
            ty: ty.to_owned(),
        });
    }
}

/// A tab in the bottom panel.
pub trait StudioPanel {
    fn show(&mut self, cx: &mut PanelContext<'_>, ui: &mut Ui);
}

/// A panel registration.
pub struct PanelKind {
    pub id: &'static str,
    /// Tab title.
    pub title: &'static str,
    pub create: fn() -> Box<dyn StudioPanel>,
}

inventory::collect!(PanelKind);

/// Every registered panel, in id order.
pub fn panels() -> Vec<&'static PanelKind> {
    let mut v: Vec<&'static PanelKind> = inventory::iter::<PanelKind>().collect();
    v.sort_by_key(|k| k.id);
    v
}
