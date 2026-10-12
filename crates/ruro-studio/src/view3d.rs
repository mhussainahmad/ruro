//! A small 3D view for point clouds, laser scans, poses and transform trees.
//!
//! Everything is projected on the CPU and drawn with the egui painter, which
//! is plenty for the ≤20k points a preview carries. The world is Z-up, as in
//! ROS: the ground grid lies in the XY plane.

use std::collections::{BTreeMap, VecDeque};

use egui::{
    Align2, Color32, CornerRadius, Mesh, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2,
};
use ruro_msg::FramePreview;

use crate::icons;
use crate::plugin::{Preview, Visualizer};
use crate::theme::*;
use crate::widgets;

type V3 = [f32; 3];

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt().max(1e-9);
    scale(a, 1.0 / l)
}

/// Rotates `v` by the unit quaternion `q = [x, y, z, w]`.
fn rotate(q: [f32; 4], v: V3) -> V3 {
    let u = [q[0], q[1], q[2]];
    let t = scale(cross(u, v), 2.0);
    add(add(v, scale(t, q[3])), cross(u, t))
}

fn quat_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let (ax, ay, az, aw) = (a[0], a[1], a[2], a[3]);
    let (bx, by, bz, bw) = (b[0], b[1], b[2], b[3]);
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

/// A rigid transform: rotation then translation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub t: V3,
    pub q: [f32; 4],
}

impl Pose {
    pub const IDENTITY: Pose = Pose {
        t: [0.0; 3],
        q: [0.0, 0.0, 0.0, 1.0],
    };

    pub fn apply(&self, v: V3) -> V3 {
        add(rotate(self.q, v), self.t)
    }

    pub fn then(&self, child: &Pose) -> Pose {
        Pose {
            t: self.apply(child.t),
            q: quat_mul(self.q, child.q),
        }
    }
}

/// Orbit camera around a target point.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
    pub target: V3,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            yaw: -2.2,
            pitch: 0.55,
            dist: 6.0,
            target: [0.0; 3],
        }
    }
}

const FOV_Y: f32 = 50.0 * std::f32::consts::PI / 180.0;

/// The camera's basis and projection for one frame.
pub struct Projector {
    eye: V3,
    right: V3,
    up: V3,
    forward: V3,
    focal: f32,
    center: Pos2,
    near: f32,
}

impl Projector {
    pub fn new(cam: &Camera, rect: Rect) -> Self {
        let dir = [
            cam.pitch.cos() * cam.yaw.cos(),
            cam.pitch.cos() * cam.yaw.sin(),
            cam.pitch.sin(),
        ];
        let eye = add(cam.target, scale(dir, cam.dist));
        let forward = norm(sub(cam.target, eye));
        let right = norm(cross(forward, [0.0, 0.0, 1.0]));
        let up = cross(right, forward);
        Self {
            eye,
            right,
            up,
            forward,
            focal: rect.height() / 2.0 / (FOV_Y / 2.0).tan(),
            center: rect.center(),
            near: (cam.dist * 0.01).max(1e-3),
        }
    }

    /// Camera-space coordinates: right, up, depth.
    fn view(&self, p: V3) -> V3 {
        let r = sub(p, self.eye);
        [dot(r, self.right), dot(r, self.up), dot(r, self.forward)]
    }

    fn screen(&self, v: V3) -> Pos2 {
        self.center + vec2(v[0], -v[1]) * (self.focal / v[2])
    }

    /// Screen position of `p`, if it is in front of the camera.
    pub fn project(&self, p: V3) -> Option<Pos2> {
        let v = self.view(p);
        (v[2] > self.near).then(|| self.screen(v))
    }

    /// Screen segment of `a`-`b`, clipped to the near plane.
    pub fn segment(&self, a: V3, b: V3) -> Option<[Pos2; 2]> {
        let (mut va, mut vb) = (self.view(a), self.view(b));
        if va[2] <= self.near && vb[2] <= self.near {
            return None;
        }
        let clip = |inside: V3, outside: V3| {
            let t = (self.near - inside[2]) / (outside[2] - inside[2]);
            add(inside, scale(sub(outside, inside), t))
        };
        if va[2] <= self.near {
            va = clip(vb, va);
        } else if vb[2] <= self.near {
            vb = clip(va, vb);
        }
        Some([self.screen(va), self.screen(vb)])
    }
}

/// Approximation of the Turbo colour map, `t` in 0..=1.
pub fn turbo(t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0) as f64;
    let r = 0.13572138
        + t * (4.61539260
            + t * (-42.66032258 + t * (132.13108234 + t * (-152.94239396 + t * 59.28637943))));
    let g = 0.09140261
        + t * (2.19418839
            + t * (4.84296658 + t * (-14.18503333 + t * (4.27729857 + t * 2.82956604))));
    let b = 0.10667330
        + t * (12.64194608
            + t * (-60.58204836 + t * (110.36276771 + t * (-89.90310912 + t * 27.34824973))));
    let c = |x: f64| (x.clamp(0.0, 1.0) * 255.0) as u8;
    Color32::from_rgb(c(r), c(g), c(b))
}

/// Axis colours: X red, Y green, Z blue.
fn axis_colors() -> [Color32; 3] {
    if is_light() {
        [
            Color32::from_rgb(0xd1, 0x34, 0x38),
            Color32::from_rgb(0x2a, 0x8f, 0x3c),
            Color32::from_rgb(0x00, 0x6a, 0xd0),
        ]
    } else {
        [
            Color32::from_rgb(0xf0, 0x5a, 0x5a),
            Color32::from_rgb(0x5c, 0xc8, 0x6c),
            Color32::from_rgb(0x4f, 0x9c, 0xff),
        ]
    }
}

/// Most positions kept in a pose's trail.
const TRAIL: usize = 600;

#[derive(Default)]
pub struct Scene {
    points: Vec<V3>,
    points_frame: String,
    /// Latest pose of every frame seen, by name.
    frames: BTreeMap<String, FramePreview>,
    /// Recent world positions of frames that move, for drawing paths.
    trails: BTreeMap<String, VecDeque<V3>>,
    cam: Camera,
    fitted: bool,
    /// Scene radius at the last fit.
    fit_radius: f32,
    /// The user moved the camera, so new data no longer refits the view.
    user_moved: bool,
    z_range: (f32, f32),
}

impl Scene {
    /// World pose of frame `name`, following parents to a root.
    pub fn world(&self, name: &str) -> Pose {
        let mut chain = Vec::new();
        let mut cur = name;
        while let Some(f) = self.frames.get(cur) {
            if chain.len() > 32 {
                break;
            }
            chain.push(Pose {
                t: f.translation,
                q: f.rotation,
            });
            if f.parent.is_empty() || f.parent == cur {
                break;
            }
            cur = &f.parent;
        }
        chain
            .iter()
            .rev()
            .fold(Pose::IDENTITY, |acc, p| acc.then(p))
    }

    fn bounds(&self) -> Option<(V3, V3)> {
        let pose = self.world(&self.points_frame);
        let pts = self
            .points
            .iter()
            .map(|p| pose.apply(*p))
            .chain(self.frames.keys().map(|n| self.world(n).t));
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        let mut any = false;
        for p in pts {
            any = true;
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
        any.then_some((lo, hi))
    }

    /// Frames the camera on everything in the scene.
    pub fn fit(&mut self) {
        self.cam = Camera::default();
        if let Some((lo, hi)) = self.bounds() {
            self.cam.target = scale(add(lo, hi), 0.5);
            self.cam.target[2] = self.cam.target[2].min(lo[2].max(0.0));
            let radius = (dot(sub(hi, lo), sub(hi, lo)).sqrt() * 0.5).max(0.5);
            self.cam.dist = (radius / (FOV_Y / 2.0).tan() * 0.95).clamp(1.0, 5_000.0);
            self.fit_radius = radius;
        }
        self.user_moved = false;
    }

    /// Keeps the scene framed as data arrives, until the user moves the camera.
    fn auto_fit(&mut self) {
        if self.user_moved {
            return;
        }
        let radius = self
            .bounds()
            .map_or(0.0, |(lo, hi)| dot(sub(hi, lo), sub(hi, lo)).sqrt() * 0.5);
        if !self.fitted || radius > self.fit_radius * 1.25 {
            self.fit();
            self.fitted = true;
        }
    }

    #[cfg(test)]
    pub fn camera(&self) -> Camera {
        self.cam
    }

    fn grid_step(&self) -> f32 {
        let raw = self.cam.dist / 6.0;
        let base = 10f32.powf(raw.log10().floor());
        [1.0, 2.0, 5.0, 10.0]
            .into_iter()
            .map(|m| base * m)
            .find(|s| *s >= raw)
            .unwrap_or(base * 10.0)
    }

    fn draw(&self, ui: &Ui, rect: Rect) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(3), EDITOR());
        let proj = Projector::new(&self.cam, rect);

        // Ground grid around the target.
        let step = self.grid_step();
        let n = 12;
        let cx = (self.cam.target[0] / step).round() * step;
        let cy = (self.cam.target[1] / step).round() * step;
        let ext = step * n as f32;
        // Lines fade out with distance, so the grid has no hard edge.
        let (minor, major) = (GRID_DOT(), BORDER_STRONG());
        for i in -n..=n {
            let off = i as f32 * step;
            for (along_x, coord) in [(false, cx + off), (true, cy + off)] {
                let is_major = (coord / step).round() as i64 % 5 == 0;
                let color = if is_major { major } else { minor };
                for k in -n..n {
                    let (t0, t1) = (k as f32 * step, (k + 1) as f32 * step);
                    let mid = (t0 + t1) * 0.5;
                    let fade = (1.0 - (off * off + mid * mid).sqrt() / ext).clamp(0.0, 1.0);
                    if fade <= 0.02 {
                        continue;
                    }
                    let (a, b) = if along_x {
                        ([cx + t0, coord, 0.0], [cx + t1, coord, 0.0])
                    } else {
                        ([coord, cy + t0, 0.0], [coord, cy + t1, 0.0])
                    };
                    if let Some(seg) = proj.segment(a, b) {
                        painter.line_segment(
                            seg,
                            Stroke::new(1.0, color.gamma_multiply(fade.powf(0.8))),
                        );
                    }
                }
            }
        }

        // Points, coloured by height.
        let pose = self.world(&self.points_frame);
        if !self.points.is_empty() {
            let (z0, z1) = self.z_range;
            let span = (z1 - z0).max(1e-3);
            let r = (1.6 + 600.0 / (self.points.len() as f32 + 300.0)).clamp(1.2, 3.0);
            let mut mesh = Mesh::default();
            for p in &self.points {
                let w = pose.apply(*p);
                let Some(s) = proj.project(w) else { continue };
                if !rect.contains(s) {
                    continue;
                }
                let c = turbo((w[2] - z0) / span);
                let i = mesh.vertices.len() as u32;
                for d in [vec2(-r, -r), vec2(r, -r), vec2(r, r), vec2(-r, r)] {
                    mesh.colored_vertex(s + d * 0.5, c);
                }
                mesh.add_triangle(i, i + 1, i + 2);
                mesh.add_triangle(i, i + 2, i + 3);
            }
            painter.add(Shape::mesh(mesh));
        }

        // Pose trails.
        for trail in self.trails.values() {
            let pts: Vec<Pos2> = trail.iter().filter_map(|p| proj.project(*p)).collect();
            if pts.len() > 1 {
                painter.add(Shape::line(
                    pts,
                    Stroke::new(1.5, ACCENT_TEXT().gamma_multiply(0.7)),
                ));
            }
        }

        // World origin and frames, with links from parents.
        let axis = axis_colors();
        let len = self.cam.dist * 0.07;
        let triad = |pose: &Pose, len: f32, width: f32| {
            for (i, color) in axis.iter().enumerate() {
                let mut e = [0.0; 3];
                e[i] = len;
                if let Some(seg) = proj.segment(pose.t, pose.apply(e)) {
                    painter.line_segment(seg, Stroke::new(width, *color));
                }
            }
        };
        triad(&Pose::IDENTITY, len * 1.4, 1.5);
        for (name, f) in &self.frames {
            let w = self.world(name);
            if !f.parent.is_empty()
                && let Some(seg) = proj.segment(self.world(&f.parent).t, w.t)
            {
                dashed(&painter, seg, Stroke::new(1.0, TEXT_FAINT()));
            }
            triad(&w, len, 2.0);
            if let Some(s) = proj.project(w.t) {
                painter.text(
                    s + vec2(6.0, -6.0),
                    Align2::LEFT_BOTTOM,
                    name,
                    font(11.5),
                    TEXT_DIM(),
                );
            }
        }

        // Orientation gizmo in the corner.
        let g = pos2(rect.right() - 34.0, rect.bottom() - 34.0);
        painter.circle_filled(g, 22.0, CARD().gamma_multiply(0.85));
        let mut ends: Vec<(f32, usize, Vec2)> = (0..3)
            .map(|i| {
                let mut e = [0.0; 3];
                e[i] = 1.0;
                let v = [dot(e, proj.right), dot(e, proj.up), dot(e, proj.forward)];
                (v[2], i, vec2(v[0], -v[1]) * 16.0)
            })
            .collect();
        ends.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, i, d) in ends {
            painter.line_segment([g, g + d], Stroke::new(2.0, axis[i]));
            painter.text(
                g + d * 1.25,
                Align2::CENTER_CENTER,
                ["x", "y", "z"][i],
                font_medium(9.5),
                axis[i],
            );
        }
    }
}

/// Text on a soft chip, legible over the scene.
fn hud(ui: &Ui, pos: Pos2, align: Align2, text: String, font: egui::FontId, color: Color32) {
    let g = ui.painter().layout_no_wrap(text, font, color);
    let rect = align.anchor_size(pos, g.size() + vec2(12.0, 6.0));
    ui.painter()
        .rect_filled(rect, CornerRadius::same(RADIUS), CARD().gamma_multiply(0.9));
    ui.painter().galley(rect.min + vec2(6.0, 3.0), g, color);
}

/// A dashed line.
fn dashed(painter: &egui::Painter, [a, b]: [Pos2; 2], stroke: Stroke) {
    let len = (b - a).length();
    if len < 1.0 {
        return;
    }
    let dir = (b - a) / len;
    let mut t = 0.0;
    while t < len {
        let e = (t + 4.0).min(len);
        painter.line_segment([a + dir * t, a + dir * e], stroke);
        t += 8.0;
    }
}

impl Visualizer for Scene {
    fn push(&mut self, _ctx: &egui::Context, preview: &Preview) {
        match preview {
            Preview::Points { frame, points } => {
                self.points.clone_from(points);
                self.points_frame.clone_from(frame);
                let pose = self.world(frame);
                let (mut z0, mut z1) = (f32::MAX, f32::MIN);
                for p in points {
                    let z = pose.apply(*p)[2];
                    z0 = z0.min(z);
                    z1 = z1.max(z);
                }
                self.z_range = (z0, z1);
            }
            Preview::Frames(frames) => {
                for f in frames {
                    self.frames.insert(f.name.clone(), f.clone());
                }
                // A message with a single frame is a moving pose: keep its path.
                if let [f] = frames.as_slice() {
                    let w = self.world(&f.name).t;
                    let trail = self.trails.entry(f.name.clone()).or_default();
                    if trail
                        .back()
                        .is_none_or(|last| dot(sub(*last, w), sub(*last, w)) > 1e-6)
                    {
                        trail.push_back(w);
                        if trail.len() > TRAIL {
                            trail.pop_front();
                        }
                    }
                }
            }
            _ => return,
        }
        self.auto_fit();
    }

    fn show(&mut self, ui: &mut Ui) {
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let shift = ui.input(|i| i.modifiers.shift);
        if resp.dragged_by(egui::PointerButton::Primary) && !shift {
            let d = resp.drag_delta();
            self.cam.yaw -= d.x * 0.008;
            self.cam.pitch = (self.cam.pitch + d.y * 0.008).clamp(-1.54, 1.54);
            self.user_moved = true;
        } else if resp.dragged_by(egui::PointerButton::Secondary)
            || resp.dragged_by(egui::PointerButton::Middle)
            || (resp.dragged_by(egui::PointerButton::Primary) && shift)
        {
            let d = resp.drag_delta();
            let proj = Projector::new(&self.cam, rect);
            let k = self.cam.dist * 2.0 * (FOV_Y / 2.0).tan() / rect.height().max(1.0);
            self.user_moved = true;
            self.cam.target = add(
                self.cam.target,
                add(scale(proj.right, -d.x * k), scale(proj.up, d.y * k)),
            );
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.cam.dist = (self.cam.dist * (-scroll * 0.0015).exp()).clamp(0.05, 10_000.0);
                self.user_moved = true;
                // The panel around the tile must not scroll as well.
                ui.ctx().input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
            }
        }
        if resp.double_clicked() {
            self.fit();
        }
        self.draw(ui, rect);

        // Summary and controls over the view.
        let summary = match (self.points.len(), self.frames.len()) {
            (0, 0) => String::new(),
            (0, f) => format!("{f} frame{}", if f == 1 { "" } else { "s" }),
            (p, 0) => format!("{p} points · {}", self.points_frame),
            (p, f) => format!("{p} points · {f} frames"),
        };
        if !summary.is_empty() {
            hud(
                ui,
                rect.left_top() + vec2(6.0, 6.0),
                Align2::LEFT_TOP,
                summary,
                font_mono(11.0),
                TEXT_DIM(),
            );
        }
        if resp.hovered() || !self.user_moved {
            hud(
                ui,
                rect.left_bottom() + vec2(6.0, -6.0),
                Align2::LEFT_BOTTOM,
                "Drag to orbit · Shift-drag to pan · Scroll to zoom".into(),
                font(10.5),
                TEXT_FAINT(),
            );
        }
        let btn = Rect::from_min_size(rect.right_top() + vec2(-28.0, 4.0), Vec2::splat(24.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(btn));
        if widgets::icon_button_sized(
            &mut child,
            icons::FOCUS,
            "Fit view (double-click)",
            24.0,
            13.0,
            false,
        )
        .clicked()
        {
            self.fit();
        }
    }

    fn width(&self, height: f32) -> f32 {
        (height * 1.45).max(420.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(name: &str, parent: &str, t: V3, q: [f32; 4]) -> FramePreview {
        FramePreview {
            name: name.into(),
            parent: parent.into(),
            translation: t,
            rotation: q,
        }
    }

    #[test]
    fn quaternion_rotation() {
        // 90 degrees about Z maps X to Y.
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let v = rotate([0.0, 0.0, s, s], [1.0, 0.0, 0.0]);
        assert!((v[0]).abs() < 1e-5 && (v[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn frames_chain_to_world() {
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let mut scene = Scene::default();
        scene.push(
            &egui::Context::default(),
            &Preview::Frames(vec![
                frame("base", "", [1.0, 0.0, 0.0], [0.0, 0.0, s, s]),
                frame("laser", "base", [1.0, 0.0, 0.5], [0.0, 0.0, 0.0, 1.0]),
            ]),
        );
        let w = scene.world("laser").t;
        assert!(
            (w[0] - 1.0).abs() < 1e-5 && (w[1] - 1.0).abs() < 1e-5 && (w[2] - 0.5).abs() < 1e-5
        );
        assert_eq!(scene.world("unknown"), Pose::IDENTITY);
    }

    #[test]
    fn projection_and_clipping() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0));
        let cam = Camera {
            yaw: 0.0,
            pitch: 0.0,
            dist: 10.0,
            target: [0.0; 3],
        };
        let p = Projector::new(&cam, rect);
        // The target projects to the centre.
        let c = p.project([0.0; 3]).unwrap();
        assert!((c - rect.center()).length() < 1e-3);
        // Up is up on screen.
        assert!(p.project([0.0, 0.0, 1.0]).unwrap().y < c.y);
        // Behind the camera is not drawn, but a segment crossing it is clipped.
        assert!(p.project([20.0, 0.0, 0.0]).is_none());
        assert!(p.segment([20.0, 0.0, 0.0], [0.0, 0.0, 0.0]).is_some());
        assert!(p.segment([20.0, 0.0, 0.0], [30.0, 0.0, 0.0]).is_none());
    }

    #[test]
    fn fit_frames_the_points() {
        let mut scene = Scene::default();
        let points: Vec<V3> = (0..100).map(|i| [i as f32 * 0.1, 0.0, 0.0]).collect();
        scene.push(
            &egui::Context::default(),
            &Preview::Points {
                frame: "map".into(),
                points,
            },
        );
        let cam = scene.camera();
        assert!((cam.target[0] - 4.95).abs() < 1e-3);
        assert!(cam.dist > 5.0);
        assert_eq!(turbo(0.0), turbo(-1.0));
    }
}
