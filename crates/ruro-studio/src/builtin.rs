//! Visualizers and panels that ship with the studio. They use the same
//! registration as third-party ones (see [`crate::plugin`]).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use egui::{ColorImage, RichText, TextureHandle, TextureOptions, Ui};
use egui_plot::{Legend, Line, Plot, PlotPoints};

use crate::icons;
use crate::plugin::{
    PanelContext, PanelKind, Preview, PreviewKind, StudioPanel, Visualizer, VisualizerKind,
    WatchInfo,
};
use crate::theme::*;
use crate::widgets;

/// Seconds of history kept for plotted values.
const HISTORY_S: f64 = 20.0;

thread_local! {
    /// Lets tests spread samples over time without sleeping.
    static CLOCK_OFFSET: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
}

fn now() -> Instant {
    Instant::now() + CLOCK_OFFSET.with(|c| c.get())
}

#[cfg(test)]
pub(crate) fn advance_clock_for_test(secs: f64) {
    CLOCK_OFFSET.with(|c| c.set(c.get() + Duration::from_secs_f64(secs)));
}

// ----- image ------------------------------------------------------------------

struct ImageView {
    name: String,
    tex: Option<(TextureHandle, [usize; 2])>,
}

impl Visualizer for ImageView {
    fn push(&mut self, ctx: &egui::Context, preview: &Preview) {
        let Preview::Image {
            width,
            height,
            rgba,
        } = preview
        else {
            return;
        };
        let size = [*width as usize, *height as usize];
        if rgba.len() != size[0] * size[1] * 4 {
            return;
        }
        let image = ColorImage::from_rgba_unmultiplied(size, rgba);
        match &mut self.tex {
            Some((tex, s)) => {
                tex.set(image, TextureOptions::LINEAR);
                *s = size;
            }
            None => {
                let tex = ctx.load_texture(&self.name, image, TextureOptions::LINEAR);
                self.tex = Some((tex, size));
            }
        }
    }

    fn show(&mut self, ui: &mut Ui) {
        let Some((tex, size)) = &self.tex else { return };
        let avail = ui.available_size();
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

    fn width(&self, height: f32) -> f32 {
        let aspect = self
            .tex
            .as_ref()
            .map_or(4.0 / 3.0, |(_, s)| s[0] as f32 / s[1].max(1) as f32);
        ((height - 40.0) * aspect).max(260.0)
    }
}

// ----- plot ---------------------------------------------------------------------

struct PlotView {
    id: String,
    ty: String,
    start: Instant,
    names: Vec<String>,
    points: Vec<VecDeque<[f64; 2]>>,
}

impl PlotView {
    fn color(&self, i: usize) -> egui::Color32 {
        if i == 0 {
            type_color(&self.ty)
        } else {
            series_color(i - 1)
        }
    }
}

impl Visualizer for PlotView {
    fn push(&mut self, _ctx: &egui::Context, preview: &Preview) {
        let Preview::Values(values) = preview else {
            return;
        };
        let t_now = now();
        let same = self.names.len() == values.len()
            && self.names.iter().zip(values).all(|(a, (b, _))| a == b);
        if !same {
            self.start = t_now;
            self.names = values.iter().map(|(n, _)| n.clone()).collect();
            self.points = vec![VecDeque::new(); values.len()];
        }
        let t = (t_now - self.start).as_secs_f64();
        for (series, (_, v)) in self.points.iter_mut().zip(values) {
            series.push_back([t, *v]);
            while series.front().is_some_and(|p| p[0] < t - HISTORY_S) {
                series.pop_front();
            }
        }
    }

    fn show(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            for (i, (n, p)) in self.names.iter().zip(&self.points).take(4).enumerate() {
                if let Some(v) = p.back() {
                    widgets::dot(ui, self.color(i), 3.0);
                    ui.label(RichText::new(n).font(font(11.5)).color(TEXT_DIM()));
                    ui.label(
                        RichText::new(format!("{:.4}", v[1]))
                            .font(font_mono(12.0))
                            .color(TEXT_STRONG()),
                    );
                    ui.add_space(8.0);
                }
            }
        });
        ui.add_space(4.0);
        let mut plot = Plot::new(&self.id)
            .show_axes([false, true])
            .show_background(false)
            .allow_scroll(false)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_boxed_zoom(false)
            .height(ui.available_height());
        if self.names.len() > 1 {
            plot = plot.legend(Legend::default());
        }
        let colors: Vec<_> = (0..self.names.len()).map(|i| self.color(i)).collect();
        plot.show(ui, |plot| {
            for (i, (name, series)) in self.names.iter().zip(&self.points).enumerate() {
                let pts: Vec<[f64; 2]> = series.iter().copied().collect();
                plot.line(
                    Line::new(name.clone(), PlotPoints::new(pts))
                        .width(1.6)
                        .color(colors[i]),
                );
            }
        });
    }
}

// ----- values table --------------------------------------------------------------

struct ValuesView {
    values: Vec<(String, f64)>,
}

impl Visualizer for ValuesView {
    fn push(&mut self, _ctx: &egui::Context, preview: &Preview) {
        if let Preview::Values(v) = preview {
            self.values.clone_from(v);
        }
    }

    fn show(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (name, v) in &self.values {
                    ui.horizontal(|ui| {
                        ui.set_min_height(20.0);
                        ui.label(RichText::new(name).font(font(12.0)).color(TEXT_DIM()));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!("{v:.6}"))
                                    .font(font_mono(12.0))
                                    .color(TEXT_STRONG()),
                            );
                        });
                    });
                }
            });
    }

    fn width(&self, _height: f32) -> f32 {
        320.0
    }
}

// ----- text --------------------------------------------------------------------------

struct TextView {
    text: String,
}

impl Visualizer for TextView {
    fn push(&mut self, _ctx: &egui::Context, preview: &Preview) {
        self.text = match preview {
            Preview::Text(t) => t.clone(),
            Preview::Values(v) => v
                .iter()
                .map(|(n, x)| format!("{n:<16} {x}"))
                .collect::<Vec<_>>()
                .join("\n"),
            Preview::Points { frame, points } => {
                format!("{} points in frame `{frame}`", points.len())
            }
            Preview::Frames(frames) => frames
                .iter()
                .map(|f| {
                    let [x, y, z] = f.translation;
                    format!("{:<16} x {x:>8.3}  y {y:>8.3}  z {z:>8.3}", f.name)
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Preview::Image { width, height, .. } => format!("{width}×{height} image"),
        };
    }

    fn show(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label(
                    RichText::new(&self.text)
                        .font(font_mono(12.0))
                        .color(TEXT()),
                );
            });
    }
}

inventory::submit! {
    VisualizerKind {
        id: "std/image",
        title: "Image",
        icon: icons::IMAGE,
        score: |_, kind| if kind == PreviewKind::Image { 10 } else { 0 },
        create: |w: &WatchInfo| Box::new(ImageView { name: format!("tap-{}-{}", w.node, w.port), tex: None }),
    }
}

inventory::submit! {
    VisualizerKind {
        id: "std/plot",
        title: "Plot",
        icon: icons::CHART_LINE,
        score: |_, kind| if kind == PreviewKind::Values { 10 } else { 0 },
        create: |w: &WatchInfo| Box::new(PlotView {
            id: format!("plot-{}-{}", w.node, w.port),
            ty: w.ty.to_owned(),
            start: now(),
            names: Vec::new(),
            points: Vec::new(),
        }),
    }
}

inventory::submit! {
    VisualizerKind {
        id: "std/values",
        title: "Values",
        icon: icons::LIST,
        score: |_, kind| if kind == PreviewKind::Values { 5 } else { 0 },
        create: |_| Box::new(ValuesView { values: Vec::new() }),
    }
}

inventory::submit! {
    VisualizerKind {
        id: "std/text",
        title: "Text",
        icon: icons::TERMINAL,
        score: |_, kind| match kind {
            PreviewKind::Text => 10,
            PreviewKind::Image => 0,
            _ => 1,
        },
        create: |_| Box::new(TextView { text: String::new() }),
    }
}

inventory::submit! {
    VisualizerKind {
        id: "std/scene3d",
        title: "3D",
        icon: icons::AXIS_3D,
        score: |_, kind| match kind {
            PreviewKind::Points | PreviewKind::Frames => 10,
            _ => 0,
        },
        create: |_| Box::new(crate::view3d::Scene::default()),
    }
}

// ----- timing panel --------------------------------------------------------------------

/// Running nodes ranked by step time: where a graph spends its time.
struct TimingPanel;

impl StudioPanel for TimingPanel {
    fn show(&mut self, cx: &mut PanelContext<'_>, ui: &mut Ui) {
        if !cx.live || cx.metrics.is_empty() {
            ui.horizontal(|ui| {
                widgets::icon(ui, icons::TIMER, 14.0, TEXT_FAINT());
                ui.label(
                    RichText::new("Run the graph to see where it spends its time.")
                        .color(TEXT_DIM()),
                );
            });
            return;
        }
        let mut rows: Vec<_> = cx.metrics.values().collect();
        rows.sort_by(|a, b| b.step_p99_us.total_cmp(&a.step_p99_us));
        let worst = rows.first().map_or(1.0, |m| m.step_p99_us.max(1.0));
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for m in rows {
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), ROW_H),
                        egui::Sense::click(),
                    );
                    if resp.hovered() {
                        ui.painter().rect_filled(rect, 0.0, HOVER());
                    }
                    let p = ui.painter();
                    let y = rect.center().y;
                    p.text(
                        egui::pos2(rect.left() + 8.0, y),
                        egui::Align2::LEFT_CENTER,
                        &m.id,
                        font_medium(12.5),
                        TEXT_STRONG(),
                    );
                    let bar = egui::Rect::from_min_size(
                        egui::pos2(rect.left() + 180.0, y - 4.0),
                        egui::vec2(
                            ((rect.width() - 400.0).max(40.0) * (m.step_p99_us / worst) as f32)
                                .max(2.0),
                            8.0,
                        ),
                    );
                    p.rect_filled(bar, 2.0, ACCENT().gamma_multiply(0.7));
                    p.text(
                        egui::pos2(rect.right() - 8.0, y),
                        egui::Align2::RIGHT_CENTER,
                        format!(
                            "p50 {}   p99 {}   {:.1} Hz   {:.0}% cpu",
                            widgets::fmt_us(m.step_p50_us),
                            widgets::fmt_us(m.step_p99_us),
                            m.hz,
                            m.cpu_pct
                        ),
                        font_mono(11.5),
                        TEXT_DIM(),
                    );
                    if resp
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        cx.focus(&m.id);
                    }
                }
            });
    }
}

inventory::submit! {
    PanelKind {
        id: "std/timing",
        title: "Timing",
        create: || Box::new(TimingPanel),
    }
}
