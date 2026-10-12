# Studio plugins

The studio can be extended in Rust with:

- **visualizers**, which draw a watched output port in the Watch panel, and
- **panels**, which add a tab to the bottom panel.

Plugins are compiled in. The studio is a library (`ruro_studio`) as well as a binary, and
plugins register themselves with [`inventory`](https://docs.rs/inventory), the same mechanism
ruro uses for node types. To use a plugin crate, build your own studio binary that links it,
just as a [custom host](writing-nodes.md) links node crates:

```toml
# my-studio/Cargo.toml
[dependencies]
ruro-studio = { path = "../ruro/crates/ruro-studio" }
my-visualizers = { path = "../my-visualizers" }
eframe = { version = "0.36", default-features = false }
egui = "0.36"
```

```rust
// my-studio/src/main.rs
fn main() -> eframe::Result<()> {
    my_visualizers::link();
    ruro_studio::run()
}
```

`link()` is an empty public function in the plugin crate. Calling it makes sure the linker keeps
the crate, and with it the registrations.

The built-in image, plot, values, text and 3D views and the Timing panel use exactly this API
(`crates/ruro-studio/src/builtin.rs`).

## Visualizers

A visualizer receives every preview of the port it watches and draws the tile body:

```rust
use ruro_studio::plugin::{Preview, PreviewKind, Visualizer, VisualizerKind, inventory};

/// Shows the first value of a port as a bar.
struct Level {
    value: f64,
}

impl Visualizer for Level {
    fn push(&mut self, _ctx: &egui::Context, preview: &Preview) {
        if let Preview::Values(values) = preview {
            self.value = values.first().map_or(0.0, |(_, v)| *v);
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::ProgressBar::new(self.value as f32).show_percentage());
    }

    // Optional: the tile width for a given height (default 420).
    fn width(&self, _height: f32) -> f32 {
        280.0
    }
}

inventory::submit! {
    VisualizerKind {
        id: "my/level",
        title: "Level",
        icon: ruro_studio::icons::GAUGE,
        // 0 = cannot show this port; otherwise higher wins.
        score: |ty, kind| if ty == "std/Scalar" && kind == PreviewKind::Values { 20 } else { 0 },
        create: |_watch| Box::new(Level { value: 0.0 }),
    }
}

pub fn link() {}
```

What a port's data looks like to a visualizer is decided by the message type's `preview`
(see [Writing nodes](writing-nodes.md)): `Preview::Image`, `Values`, `Text`, `Points` or
`Frames`. When a port starts streaming, the studio picks the visualizer with the highest score
for the port's type and preview kind. The built-in ones score 10 for their main kinds and lower
for fallbacks, so a plugin that scores above 10 for a type becomes its default view. Every
visualizer with a score above zero is listed in the tile's view menu.

`create` receives the watched node, port and message type. Use the theme tokens in
`ruro_studio::theme` (`TEXT()`, `ACCENT()`, `type_color(ty)`, ...) so the view follows the dark
and light themes, and implement `theme_changed` if you cache colours.

## Panels

A panel draws a bottom-panel tab. Through its `PanelContext` it sees the graph in the active
tab (`graph`), the node registry (`registry`), the daemon status (`status`), the live
metrics by node id (`metrics`) and whether the graph is running (`live`), and it can ask the studio to
select a node or start watching a port:

```rust
use ruro_studio::plugin::{PanelContext, PanelKind, StudioPanel, inventory};

struct Errors;

impl StudioPanel for Errors {
    fn show(&mut self, cx: &mut PanelContext<'_>, ui: &mut egui::Ui) {
        let mut failing: Vec<_> = cx.metrics.values().filter(|m| m.errors > 0).collect();
        failing.sort_by_key(|m| std::cmp::Reverse(m.errors));
        for m in failing {
            if ui.link(format!("{}: {} errors", m.id, m.errors)).clicked() {
                cx.focus(&m.id);
            }
        }
    }
}

inventory::submit! {
    PanelKind { id: "my/errors", title: "Errors", create: || Box::new(Errors) }
}
```

Panels appear after the built-in tabs, in id order, and in the command palette as
`View: <title>`.
