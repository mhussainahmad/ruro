//! Live views of watched output ports.

use std::collections::VecDeque;
use std::time::Instant;

use egui::{
    Color32, ColorImage, CornerRadius, Frame, Margin, RichText, Stroke, TextureHandle,
    TextureOptions, Ui,
};
use egui_plot::{Legend, Line, Plot, PlotPoints};
use ruro_proto::{Preview, TapFrame};

use crate::icons;
use crate::theme::*;
use crate::widgets;

/// Seconds of history kept for plotted values.
const HISTORY_S: f64 = 20.0;

/// Colours for additional series in one plot, after the port's own colour.
const SERIES: [Color32; 5] = [
    Color32::from_rgb(0xd7, 0xba, 0x7d),
    Color32::from_rgb(0x4e, 0xc9, 0xb0),
    Color32::from_rgb(0xc5, 0x86, 0xc0),
    Color32::from_rgb(0xce, 0x91, 0x78),
    Color32::from_rgb(0xb5, 0xce, 0xa8),
];

pub struct Watch {
    pub node: String,
    pub port: String,
    pub ty: String,
    data: Data,
    frames: u64,
    last: Option<Instant>,
    rate: f64,
}

enum Data {
    Waiting,
    Image {
        tex: TextureHandle,
        size: [usize; 2],
    },
    Series {
        start: Instant,
        names: Vec<String>,
        points: Vec<VecDeque<[f64; 2]>>,
    },
    Text(String),
}

impl Watch {
    pub fn new(node: String, port: String, ty: String) -> Self {
        Self {
            node,
            port,
            ty,
            data: Data::Waiting,
            frames: 0,
            last: None,
            rate: 0.0,
        }
    }

    pub fn key(&self) -> (String, String) {
        (self.node.clone(), self.port.clone())
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
        match frame.preview {
            Preview::Image {
                width,
                height,
                rgba,
            } => {
                let size = [width as usize, height as usize];
                if rgba.len() != size[0] * size[1] * 4 {
                    return;
                }
                let image = ColorImage::from_rgba_unmultiplied(size, &rgba);
                match &mut self.data {
                    Data::Image { tex, size: s } => {
                        tex.set(image, TextureOptions::LINEAR);
                        *s = size;
                    }
                    _ => {
                        let name = format!("tap-{}-{}", self.node, self.port);
                        let tex = ctx.load_texture(name, image, TextureOptions::LINEAR);
                        self.data = Data::Image { tex, size };
                    }
                }
            }
            Preview::Values(values) => {
                let same = matches!(&self.data, Data::Series { names, .. }
                    if names.len() == values.len() && names.iter().zip(&values).all(|(a, (b, _))| a == b));
                if !same {
                    self.data = Data::Series {
                        start: now,
                        names: values.iter().map(|(n, _)| n.clone()).collect(),
                        points: vec![VecDeque::new(); values.len()],
                    };
                }
                if let Data::Series { start, points, .. } = &mut self.data {
                    let t = (now - *start).as_secs_f64();
                    for (series, (_, v)) in points.iter_mut().zip(values) {
                        series.push_back([t, v]);
                        while series.front().is_some_and(|p| p[0] < t - HISTORY_S) {
                            series.pop_front();
                        }
                    }
                }
            }
            Preview::Text(t) => self.data = Data::Text(t),
        }
    }

    /// Draws the tile. Returns true if the user closed it.
    pub fn show(&self, ui: &mut Ui, size: egui::Vec2) -> bool {
        let mut close = false;
        Frame::new()
            .fill(EDITOR)
            .stroke(Stroke::new(1.0, BORDER_STRONG))
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

    fn header(&self, ui: &mut Ui, close: &mut bool) {
        Frame::new()
            .fill(CARD)
            .inner_margin(Margin::symmetric(10, 0))
            .show(ui, |ui| {
                ui.set_height(30.0);
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    widgets::dot(ui, type_color(&self.ty), 3.5);
                    ui.label(
                        RichText::new(format!("{}.{}", self.node, self.port))
                            .font(font_mono(12.0))
                            .color(TEXT_STRONG),
                    );
                    ui.label(RichText::new(&self.ty).font(font(11.0)).color(TEXT_FAINT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                        if self.frames > 0 {
                            ui.label(
                                RichText::new(format!("{:.0} fps", self.rate))
                                    .font(font_mono(11.0))
                                    .color(TEXT_DIM),
                            );
                        }
                    });
                });
            });
    }

    fn body(&self, ui: &mut Ui) {
        let avail = ui.available_size();
        match &self.data {
            Data::Waiting => {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("Waiting for data…").color(TEXT_FAINT));
                });
            }
            Data::Image { tex, size } => {
                let aspect = size[0] as f32 / size[1].max(1) as f32;
                let h = avail.y.min(avail.x / aspect);
                ui.vertical_centered(|ui| {
                    ui.add(
                        egui::Image::new(tex)
                            .fit_to_exact_size(egui::vec2(h * aspect, h))
                            .corner_radius(3),
                    );
                });
            }
            Data::Series { names, points, .. } => {
                let color = |i: usize| {
                    if i == 0 {
                        type_color(&self.ty)
                    } else {
                        SERIES[(i - 1) % SERIES.len()]
                    }
                };
                ui.horizontal(|ui| {
                    for (i, (n, p)) in names.iter().zip(points).take(4).enumerate() {
                        if let Some(v) = p.back() {
                            widgets::dot(ui, color(i), 3.0);
                            ui.label(RichText::new(n).font(font(11.5)).color(TEXT_DIM));
                            ui.label(
                                RichText::new(format!("{:.4}", v[1]))
                                    .font(font_mono(12.0))
                                    .color(TEXT_STRONG),
                            );
                            ui.add_space(8.0);
                        }
                    }
                });
                ui.add_space(4.0);
                let mut plot = Plot::new(format!("plot-{}-{}", self.node, self.port))
                    .show_axes([false, true])
                    .show_background(false)
                    .allow_scroll(false)
                    .allow_drag(false)
                    .allow_zoom(false)
                    .allow_boxed_zoom(false)
                    .height(ui.available_height());
                if names.len() > 1 {
                    plot = plot.legend(Legend::default());
                }
                plot.show(ui, |plot| {
                    for (i, (name, series)) in names.iter().zip(points).enumerate() {
                        let pts: Vec<[f64; 2]> = series.iter().copied().collect();
                        plot.line(
                            Line::new(name.clone(), PlotPoints::new(pts))
                                .width(1.6)
                                .color(color(i)),
                        );
                    }
                });
            }
            Data::Text(t) => {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(RichText::new(t).font(font_mono(12.0)));
                });
            }
        }
    }
}

#[cfg(test)]
impl Watch {
    /// Moves the series origin back so the next samples land `secs` later.
    pub fn shift_history_for_test(&mut self, secs: f64) {
        if let Data::Series { start, .. } = &mut self.data {
            *start -= std::time::Duration::from_secs_f64(secs);
        }
    }
}
