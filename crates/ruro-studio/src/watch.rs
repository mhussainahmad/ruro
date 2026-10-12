//! Live views of watched output ports. Each tile hosts a [`Visualizer`]
//! chosen from the registered ones for the port's type and data.

use std::time::Instant;

use egui::{CornerRadius, Frame, Margin, RichText, Stroke, Ui};
use ruro_proto::TapFrame;

use crate::icons;
use crate::plugin::{self, PreviewKind, Visualizer, VisualizerKind, WatchInfo};
use crate::theme::*;
use crate::widgets;

pub struct Watch {
    /// Node and port as shown in the open graph.
    pub node: String,
    pub port: String,
    pub ty: String,
    /// The running port it streams from (differs inside composites).
    pub tap: (String, String),
    view: Option<(&'static VisualizerKind, Box<dyn Visualizer>)>,
    /// Shape of the last preview.
    kind: Option<PreviewKind>,
    frames: u64,
    last: Option<Instant>,
    rate: f64,
}

impl Watch {
    pub fn new(node: String, port: String, ty: String, tap: (String, String)) -> Self {
        Self {
            node,
            port,
            ty,
            tap,
            view: None,
            kind: None,
            frames: 0,
            last: None,
            rate: 0.0,
        }
    }

    pub fn key(&self) -> (String, String) {
        (self.node.clone(), self.port.clone())
    }

    /// Id of the visualizer in use, if data has arrived.
    #[cfg(test)]
    pub fn visualizer(&self) -> Option<&'static str> {
        self.view.as_ref().map(|(k, _)| k.id)
    }

    fn info(&self) -> WatchInfo<'_> {
        WatchInfo {
            node: &self.node,
            port: &self.port,
            ty: &self.ty,
        }
    }

    /// Switches to the visualizer `kind`.
    pub fn use_visualizer(&mut self, kind: &'static VisualizerKind) {
        let v = (kind.create)(&self.info());
        self.view = Some((kind, v));
    }

    pub fn theme_changed(&mut self) {
        if let Some((_, v)) = &mut self.view {
            v.theme_changed();
        }
    }

    pub fn push(&mut self, ctx: &egui::Context, frame: TapFrame) {
        let now = Instant::now();
        if let Some(last) = self.last {
            let dt = (now - last).as_secs_f64().max(1e-3);
            self.rate = if self.rate == 0.0 {
                1.0 / dt
            } else {
                self.rate * 0.9 + 0.1 / dt
            };
        }
        self.last = Some(now);
        self.frames += 1;
        let kind = PreviewKind::of(&frame.preview);
        let usable = self
            .view
            .as_ref()
            .is_some_and(|(k, _)| (k.score)(&self.ty, kind) > 0);
        if !usable && let Some(best) = plugin::candidates(&self.ty, kind).first() {
            self.use_visualizer(best);
        }
        self.kind = Some(kind);
        if let Some((_, v)) = &mut self.view {
            v.push(ctx, &frame.preview);
        }
    }

    /// Preferred tile width at the given height.
    pub fn width(&self, height: f32) -> f32 {
        self.view.as_ref().map_or(320.0, |(_, v)| v.width(height))
    }

    /// Draws the tile. Returns true if the user closed it.
    pub fn show(&mut self, ui: &mut Ui, size: egui::Vec2) -> bool {
        let mut close = false;
        Frame::new()
            .fill(EDITOR())
            .stroke(Stroke::new(1.0, BORDER_STRONG()))
            .corner_radius(CornerRadius::same(RADIUS + 1))
            .inner_margin(Margin::same(0))
            .show(ui, |ui| {
                ui.set_width(size.x);
                ui.set_height(size.y);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    self.header(ui, &mut close);
                    widgets::hairline(ui);
                    Frame::new()
                        .inner_margin(Margin::same(8))
                        .show(ui, |ui| self.body(ui));
                });
            });
        close
    }

    fn header(&mut self, ui: &mut Ui, close: &mut bool) {
        let candidates = self
            .kind
            .map(|k| plugin::candidates(&self.ty, k))
            .unwrap_or_default();
        let mut switch = None;
        Frame::new()
            .fill(CARD())
            .corner_radius(CornerRadius {
                nw: RADIUS + 1,
                ne: RADIUS + 1,
                sw: 0,
                se: 0,
            })
            .inner_margin(Margin::symmetric(10, 0))
            .show(ui, |ui| {
                ui.set_height(30.0);
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    widgets::dot(ui, type_color(&self.ty), 3.5);
                    ui.label(
                        RichText::new(format!("{}.{}", self.node, self.port))
                            .font(font_mono(12.0))
                            .color(TEXT_STRONG()),
                    );
                    ui.label(RichText::new(&self.ty).font(font(11.0)).color(TEXT_FAINT()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if widgets::icon_button_sized(
                            ui,
                            icons::X,
                            "Stop watching",
                            20.0,
                            12.0,
                            false,
                        )
                        .clicked()
                        {
                            *close = true;
                        }
                        if candidates.len() > 1
                            && let Some((current, _)) = &self.view
                        {
                            let resp = widgets::icon_button_sized(
                                ui,
                                current.icon,
                                &format!("{} — change view", current.title),
                                20.0,
                                12.5,
                                false,
                            );
                            egui::Popup::menu(&resp).show(|ui| {
                                ui.set_min_width(160.0);
                                for k in &candidates {
                                    if widgets::list_row(
                                        ui,
                                        k.icon,
                                        TEXT_DIM(),
                                        k.title,
                                        "",
                                        k.id == current.id,
                                    )
                                    .clicked()
                                    {
                                        switch = Some(*k);
                                        ui.close();
                                    }
                                }
                            });
                        }
                        if self.frames > 0 {
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(format!("{:.0} fps", self.rate))
                                    .font(font_mono(11.0))
                                    .color(TEXT_DIM()),
                            );
                        }
                    });
                });
            });
        if let Some(k) = switch {
            self.use_visualizer(k);
        }
    }

    fn body(&mut self, ui: &mut Ui) {
        match &mut self.view {
            Some((_, v)) => v.show(ui),
            None => {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("Waiting for data…").color(TEXT_FAINT()));
                });
            }
        }
    }
}
