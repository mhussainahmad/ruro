//! Node canvas: how graph nodes, ports and wires are drawn and edited.

use std::collections::{HashMap, HashSet};

use egui::{
    Align2, Color32, CornerRadius, Margin, Painter, Pos2, Rect, Sense, Stroke, Style, Ui, Vec2,
    pos2, vec2,
};
use egui_snarl::ui::{
    AnyPins, BackgroundPattern, NodeLayout, PinInfo, PinPlacement, SnarlStyle, SnarlViewer,
    WireStyle,
};
use egui_snarl::{InPin, InPinId, NodeId, OutPin, OutPinId, Snarl};
use ruro_graph::{NodeDescriptor, Registry};
use ruro_proto::{NodeMetrics, NodeState};

use crate::app::Cmd;
use crate::diff::{Mark, Marks};
use crate::icons;
use crate::model::GNode;
use crate::theme::*;
use crate::widgets;

/// Requests from the canvas that the app performs after drawing.
pub enum Action {
    Cmd(Cmd),
    Toast(String),
    StructureChanged,
}

/// Screen positions of ports drawn last frame: `(node, is_output, index)`.
#[derive(Default)]
pub struct PinPositions {
    rows: Vec<(NodeId, bool, usize, f32)>,
    pub pins: HashMap<(NodeId, bool, usize), Pos2>,
}

impl PinPositions {
    #[cfg(test)]
    pub fn output(&self, node: NodeId, i: usize) -> Option<Pos2> {
        self.pins.get(&(node, true, i)).copied()
    }

    #[cfg(test)]
    pub fn input(&self, node: NodeId, i: usize) -> Option<Pos2> {
        self.pins.get(&(node, false, i)).copied()
    }
}

pub struct Viewer<'a> {
    pub registry: &'a Registry,
    /// Interfaces of composite nodes, by node id.
    pub composites: &'a HashMap<String, NodeDescriptor>,
    pub metrics: &'a HashMap<String, NodeMetrics>,
    pub watched: &'a HashSet<(String, String)>,
    /// True while the doc's graph is the one running on the daemon.
    pub live: bool,
    /// Node shown in the inspector; drawn highlighted.
    pub focused: Option<NodeId>,
    /// Nodes the studio selected (select all, paste); drawn highlighted.
    pub marked: &'a HashSet<NodeId>,
    /// Comparison or overlay highlights.
    pub marks: &'a Marks,
    /// The canvas shows a graph with overlays applied: nodes can be moved
    /// and inspected, but not added, removed or rewired.
    pub read_only: bool,
    /// Node under the pointer this frame, reported back to the app.
    pub hovered: Option<NodeId>,
    /// When set, the view is fitted to the graph inside this screen rect.
    pub fit: Option<Rect>,
    /// The graph-to-screen transform this frame.
    pub transform: Option<egui::emath::TSTransform>,
    /// Screen rect of every node drawn this frame.
    pub rects: Vec<(NodeId, Rect)>,
    pub pins: PinPositions,
    pub actions: Vec<Action>,
}

/// Shown when wiring is tried while an overlay is on the canvas.
const READ_ONLY: &str = "An overlay is shown. Select the base graph to edit wires.";

/// Approximate on-canvas size of a node, for fitting before nodes are measured.
const NODE_EXTENT: Vec2 = vec2(330.0, 150.0);

/// Spacing of the background dot grid, in graph units.
const GRID: f32 = 24.0;

pub fn style() -> SnarlStyle {
    SnarlStyle {
        node_layout: Some(NodeLayout::coil()),
        pin_placement: Some(PinPlacement::Edge),
        pin_size: Some(9.0),
        pin_stroke: Some(Stroke::new(2.0, CARD())),
        wire_width: Some(2.0),
        wire_style: Some(WireStyle::AxisAligned {
            corner_radius: 14.0,
        }),
        wire_smoothness: Some(1.0),
        header_drag_space: Some(vec2(8.0, 0.0)),
        collapsible: Some(false),
        node_frame: Some(egui::Frame {
            inner_margin: Margin::symmetric(12, 8),
            outer_margin: Margin::ZERO,
            corner_radius: CornerRadius::same(RADIUS_LG),
            fill: CARD(),
            stroke: Stroke::new(1.0, BORDER_STRONG()),
            shadow: egui::Shadow {
                offset: [0, 4],
                blur: 14,
                spread: 0,
                color: shadow(70),
            },
        }),
        header_frame: Some(egui::Frame {
            inner_margin: Margin::symmetric(12, 7),
            outer_margin: Margin::ZERO,
            corner_radius: CornerRadius {
                nw: RADIUS_LG,
                ne: RADIUS_LG,
                sw: 0,
                se: 0,
            },
            fill: CARD_HEADER(),
            stroke: Stroke::NONE,
            shadow: egui::Shadow::NONE,
        }),
        bg_frame: Some(egui::Frame {
            inner_margin: Margin::ZERO,
            outer_margin: Margin::ZERO,
            corner_radius: CornerRadius::ZERO,
            fill: EDITOR(),
            stroke: Stroke::NONE,
            shadow: egui::Shadow::NONE,
        }),
        bg_pattern: Some(BackgroundPattern::NoPattern),
        select_stoke: Some(Stroke::new(1.0, ACCENT())),
        select_fill: Some(ACCENT().gamma_multiply(0.06)),
        min_scale: Some(0.3),
        max_scale: Some(2.0),
        ..SnarlStyle::new()
    }
}

/// Live state of a node; for a composite, of all the nodes inside it.
fn node_state(metrics: &HashMap<String, NodeMetrics>, node: &GNode) -> Option<NodeState> {
    if !node.is_composite() {
        return metrics.get(&node.id).map(|m| m.state);
    }
    let prefix = format!("{}{}", node.id, ruro_graph::composite::SEPARATOR);
    let states: Vec<NodeState> = metrics
        .iter()
        .filter(|(id, _)| id.starts_with(&prefix))
        .map(|(_, m)| m.state)
        .collect();
    if states.is_empty() {
        None
    } else if states.contains(&NodeState::Failed) {
        Some(NodeState::Failed)
    } else if states.iter().all(|s| *s == NodeState::Running) {
        Some(NodeState::Running)
    } else {
        Some(NodeState::Starting)
    }
}

/// Metrics of a node, summed over the inside of a composite.
pub fn node_metrics(metrics: &HashMap<String, NodeMetrics>, node: &GNode) -> Option<NodeMetrics> {
    if !node.is_composite() {
        return metrics.get(&node.id).cloned();
    }
    let prefix = format!("{}{}", node.id, ruro_graph::composite::SEPARATOR);
    let inner: Vec<&NodeMetrics> = metrics
        .iter()
        .filter(|(id, _)| id.starts_with(&prefix))
        .map(|(_, m)| m)
        .collect();
    let first = inner.first()?;
    Some(NodeMetrics {
        id: node.id.clone(),
        state: node_state(metrics, node).unwrap_or(first.state),
        hz: inner.iter().map(|m| m.hz).fold(0.0, f64::max),
        step_p50_us: inner.iter().map(|m| m.step_p50_us).sum(),
        step_p99_us: inner.iter().map(|m| m.step_p99_us).sum(),
        input_age_ms: None,
        cpu_pct: inner.iter().map(|m| m.cpu_pct).sum(),
        errors: inner.iter().map(|m| m.errors).sum(),
        last_error: inner.iter().find_map(|m| m.last_error.clone()),
        dropped: inner.iter().map(|m| m.dropped).sum(),
        // Wakeup jitter is per timer, so it does not add up across nodes.
        timing: None,
    })
}

impl<'a> Viewer<'a> {
    fn desc(&self, node: &GNode) -> Option<&'a NodeDescriptor> {
        if node.is_composite() {
            self.composites.get(&node.id)
        } else {
            self.registry.get(&node.kind)
        }
    }

    fn insert(
        &mut self,
        snarl: &mut Snarl<GNode>,
        desc: &NodeDescriptor,
        pos: egui::Pos2,
    ) -> NodeId {
        let base = desc.short_name().replace('-', "_");
        let taken = |id: &str| snarl.nodes().any(|n| n.id == id);
        let id = if taken(&base) {
            (2..)
                .map(|i| format!("{base}{i}"))
                .find(|id| !taken(id))
                .unwrap()
        } else {
            base
        };
        self.actions.push(Action::StructureChanged);
        snarl.insert_node(pos, GNode::new(id, desc.kind.clone()))
    }

    fn output_type(&self, snarl: &Snarl<GNode>, pin: OutPinId) -> Option<String> {
        Some(
            self.desc(&snarl[pin.node])?
                .outputs
                .get(pin.output)?
                .ty
                .clone(),
        )
    }

    fn input_type(&self, snarl: &Snarl<GNode>, pin: InPinId) -> Option<String> {
        Some(
            self.desc(&snarl[pin.node])?
                .inputs
                .get(pin.input)?
                .ty
                .clone(),
        )
    }

    fn state(&self, node: &GNode) -> Option<NodeState> {
        self.live.then(|| node_state(self.metrics, node)).flatten()
    }

    /// Remembers where a port row is drawn, to place its pin after layout.
    fn record_row(&mut self, ui: &Ui, node: NodeId, output: bool, index: usize) {
        let to_screen = ui
            .ctx()
            .layer_transform_to_global(ui.layer_id())
            .unwrap_or_default();
        let y = (to_screen * ui.min_rect()).center().y;
        self.pins.rows.push((node, output, index, y));
    }

    /// A menu listing node types, filtered. Returns the picked type.
    fn palette_menu(
        &self,
        ui: &mut Ui,
        filter: impl Fn(&NodeDescriptor) -> bool,
    ) -> Option<NodeDescriptor> {
        ui.set_min_width(230.0);
        let mut picked = None;
        let mut category = "";
        let mut any = false;
        for d in self.registry.iter().filter(|d| filter(d)) {
            any = true;
            if d.category() != category {
                category = d.category();
                ui.label(
                    egui::RichText::new(category.to_uppercase())
                        .font(font_semibold(10.5))
                        .color(TEXT_FAINT()),
                );
            }
            let color = d
                .outputs
                .first()
                .or(d.inputs.first())
                .map_or(TEXT_DIM(), |p| type_color(&p.ty));
            if widgets::list_row(ui, kind_icon(&d.kind), color, d.short_name(), "", false)
                .on_hover_text(&d.doc)
                .clicked()
            {
                picked = Some(d.clone());
                ui.close();
            }
        }
        if !any {
            ui.label(egui::RichText::new("No compatible node types").color(TEXT_FAINT()));
        }
        picked
    }
}

impl SnarlViewer<GNode> for Viewer<'_> {
    fn title(&mut self, node: &GNode) -> String {
        node.id.clone()
    }

    fn node_frame(
        &mut self,
        default: egui::Frame,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        snarl: &Snarl<GNode>,
    ) -> egui::Frame {
        let n = &snarl[node];
        let mark = self.marks.nodes.get(&n.id).copied();
        match (self.state(n), mark) {
            (Some(NodeState::Failed), _) => default.stroke(Stroke::new(1.0, ERR())),
            (_, Some(Mark::Added)) => default.stroke(Stroke::new(1.5, OK())),
            (_, Some(Mark::Changed)) => default.stroke(Stroke::new(1.5, WARN())),
            _ if self.focused == Some(node) || self.marked.contains(&node) => {
                default.stroke(Stroke::new(1.0, ACCENT()))
            }
            _ => default,
        }
    }

    fn final_node_rect(
        &mut self,
        node: NodeId,
        rect: Rect,
        ui: &mut Ui,
        _snarl: &mut Snarl<GNode>,
    ) {
        // The canvas is a zoomed layer: compare in screen space.
        let to_screen = ui
            .ctx()
            .layer_transform_to_global(ui.layer_id())
            .unwrap_or_default();
        let screen = to_screen * rect;
        if ui
            .ctx()
            .pointer_interact_pos()
            .is_some_and(|p| screen.contains(p))
        {
            self.hovered = Some(node);
        }
        self.rects.push((node, screen));
        for (n, output, i, y) in std::mem::take(&mut self.pins.rows) {
            if n == node {
                let x = if output {
                    screen.right()
                } else {
                    screen.left()
                };
                self.pins.pins.insert((n, output, i), pos2(x, y));
            } else {
                self.pins.rows.push((n, output, i, y));
            }
        }
    }

    fn show_header(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut Ui,
        snarl: &mut Snarl<GNode>,
    ) {
        let n = &snarl[node];
        let desc = self.desc(n);
        let composite = n.is_composite();
        let tint = if composite {
            ACCENT_TEXT()
        } else {
            desc.and_then(|d| d.outputs.first().or(d.inputs.first()))
                .map_or(TEXT_DIM(), |p| type_color(&p.ty))
        };
        let mut open = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let glyph = if composite {
                icons::BOXES
            } else {
                kind_icon(&n.kind)
            };
            widgets::icon(ui, glyph, 14.0, tint);
            ui.label(
                egui::RichText::new(&n.id)
                    .font(font_medium(13.0))
                    .color(TEXT_STRONG()),
            );
            if composite {
                let label = match n.kind.strip_prefix(ruro_graph::GRAPH_PREFIX) {
                    Some(path) => path.rsplit('/').next().unwrap_or(path).to_owned(),
                    None => "subgraph".to_owned(),
                };
                ui.label(
                    egui::RichText::new(label)
                        .font(font(11.5))
                        .color(TEXT_FAINT()),
                );
            } else {
                let short = desc.map_or(n.kind.as_str(), |d| d.short_name());
                if short.replace('-', "_") != n.id {
                    ui.label(
                        egui::RichText::new(short)
                            .font(font(11.5))
                            .color(TEXT_FAINT()),
                    );
                }
            }
            if let Some(g) = n.group.as_deref().filter(|g| !g.is_empty()) {
                ui.label(
                    egui::RichText::new(format!("{} {g}", icons::LAYERS))
                        .font(font(11.0))
                        .color(TEXT_FAINT()),
                )
                .on_hover_text(format!("Runs in process `{g}`"));
            }
            if let Some(state) = self.state(n) {
                let c = match state {
                    NodeState::Running => OK(),
                    NodeState::Failed => ERR(),
                    _ => WARN(),
                };
                widgets::dot(ui, c, 3.5);
            }
            if composite {
                open = widgets::icon_button_sized(
                    ui,
                    icons::OPEN_IN,
                    "Open subgraph",
                    18.0,
                    12.0,
                    false,
                )
                .clicked();
            }
        });
        if open {
            self.actions
                .push(Action::Cmd(Cmd::OpenSubgraph(snarl[node].id.clone())));
        }
    }

    fn inputs(&mut self, node: &GNode) -> usize {
        self.desc(node).map_or(0, |d| d.inputs.len())
    }

    fn outputs(&mut self, node: &GNode) -> usize {
        self.desc(node).map_or(0, |d| d.outputs.len())
    }

    #[allow(refining_impl_trait)]
    fn show_input(&mut self, pin: &InPin, ui: &mut Ui, snarl: &mut Snarl<GNode>) -> PinInfo {
        let Some(port) = self
            .desc(&snarl[pin.id.node])
            .and_then(|d| d.inputs.get(pin.id.input))
        else {
            return PinInfo::circle().with_fill(IDLE());
        };
        ui.label(
            egui::RichText::new(&port.name)
                .font(font(12.5))
                .color(TEXT()),
        )
        .on_hover_text(&port.ty);
        self.record_row(ui, pin.id.node, false, pin.id.input);
        PinInfo::circle().with_fill(type_color(&port.ty))
    }

    #[allow(refining_impl_trait)]
    fn show_output(&mut self, pin: &OutPin, ui: &mut Ui, snarl: &mut Snarl<GNode>) -> PinInfo {
        let n = &snarl[pin.id.node];
        let Some(port) = self.desc(n).and_then(|d| d.outputs.get(pin.id.output)) else {
            return PinInfo::circle().with_fill(IDLE());
        };
        let color = type_color(&port.ty);
        let key = (n.id.clone(), port.name.clone());
        let watched = self.watched.contains(&key);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let (glyph, tip) = if watched {
                (icons::EYE, "Stop watching")
            } else {
                (icons::EYE_OFF, "Watch live values")
            };
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
            let c = if watched {
                color
            } else if resp.hovered() {
                TEXT()
            } else {
                TEXT_FAINT()
            };
            widgets::paint_icon(ui, rect, glyph, 12.5, c);
            if resp
                .on_hover_text(tip)
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                self.actions.push(Action::Cmd(if watched {
                    Cmd::Unwatch {
                        node: key.0.clone(),
                        port: key.1.clone(),
                    }
                } else {
                    Cmd::Watch {
                        node: key.0.clone(),
                        port: key.1.clone(),
                        ty: port.ty.clone(),
                    }
                }));
            }
            ui.label(
                egui::RichText::new(&port.name)
                    .font(font(12.5))
                    .color(TEXT()),
            )
            .on_hover_text(&port.ty);
        });
        self.record_row(ui, pin.id.node, true, pin.id.output);
        PinInfo::circle()
            .with_fill(color)
            .with_wire_color(color.gamma_multiply(0.75))
    }

    fn has_body(&mut self, node: &GNode) -> bool {
        self.desc(node).is_none()
    }

    fn show_body(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut Ui,
        snarl: &mut Snarl<GNode>,
    ) {
        let n = &snarl[node];
        let text = if n.is_composite() {
            "subgraph cannot be loaded".to_owned()
        } else {
            format!("unknown type {}", n.kind)
        };
        ui.label(egui::RichText::new(text).font(font(11.5)).color(ERR()));
    }

    fn has_footer(&mut self, node: &GNode) -> bool {
        self.live && node_metrics(self.metrics, node).is_some()
    }

    fn show_footer(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut Ui,
        snarl: &mut Snarl<GNode>,
    ) {
        let Some(m) = node_metrics(self.metrics, &snarl[node]) else {
            return;
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let stat = |ui: &mut Ui, value: String, unit: &str| {
                ui.label(
                    egui::RichText::new(value)
                        .font(font_mono(11.5))
                        .color(TEXT()),
                );
                ui.label(
                    egui::RichText::new(unit)
                        .font(font(10.5))
                        .color(TEXT_FAINT()),
                );
                ui.add_space(6.0);
            };
            stat(ui, format!("{:.1}", m.hz), "Hz");
            stat(ui, widgets::fmt_us(m.step_p99_us), "p99");
            stat(ui, format!("{:.0}%", m.cpu_pct), "cpu");
        });
        match (&m.state, &m.last_error) {
            (NodeState::Failed, Some(e)) => {
                ui.label(
                    egui::RichText::new(widgets::truncate(e, 44))
                        .font(font(11.5))
                        .color(ERR()),
                )
                .on_hover_text(e);
            }
            (_, Some(e)) if m.errors > 0 => {
                ui.label(
                    egui::RichText::new(format!(
                        "{} errors · {}",
                        m.errors,
                        widgets::truncate(e, 32)
                    ))
                    .font(font(11.5))
                    .color(WARN()),
                )
                .on_hover_text(e);
            }
            _ => {}
        }
    }

    fn current_transform(
        &mut self,
        to_global: &mut egui::emath::TSTransform,
        snarl: &mut Snarl<GNode>,
    ) {
        if let Some(view) = self.fit.take() {
            let mut bb = Rect::NOTHING;
            for (_, pos, _) in snarl.nodes_pos_ids() {
                bb.extend_with(pos);
                bb.extend_with(pos + NODE_EXTENT);
            }
            if !bb.is_finite() {
                *to_global =
                    egui::emath::TSTransform::new(view.min.to_vec2() + vec2(40.0, 40.0), 1.0);
            } else {
                let bb = bb.expand(48.0);
                let scale = (view.width() / bb.width())
                    .min(view.height() / bb.height())
                    .clamp(0.4, 1.0);
                *to_global = egui::emath::TSTransform::new(
                    view.center().to_vec2() - bb.center().to_vec2() * scale,
                    scale,
                );
            }
        }
        self.transform = Some(*to_global);
    }

    fn draw_background(
        &mut self,
        _background: Option<&BackgroundPattern>,
        viewport: &Rect,
        _snarl_style: &SnarlStyle,
        _style: &Style,
        painter: &Painter,
        _snarl: &Snarl<GNode>,
    ) {
        // A sparse dot grid that stays quiet at every zoom level.
        let (x0, y0) = (
            (viewport.left() / GRID).floor() as i64,
            (viewport.top() / GRID).floor() as i64,
        );
        let (x1, y1) = (
            (viewport.right() / GRID).ceil() as i64,
            (viewport.bottom() / GRID).ceil() as i64,
        );
        if (x1 - x0) * (y1 - y0) <= 40_000 {
            let dot = GRID_DOT();
            for gx in x0..=x1 {
                for gy in y0..=y1 {
                    painter.circle_filled(pos2(gx as f32 * GRID, gy as f32 * GRID), 1.0, dot);
                }
            }
        }
        // Nodes removed since the compared version, as dashed outlines.
        for (id, kind, [x, y]) in &self.marks.ghosts {
            let rect = Rect::from_min_size(pos2(*x, *y), vec2(240.0, 64.0));
            painter.rect_filled(
                rect,
                CornerRadius::same(RADIUS_LG),
                ERR().gamma_multiply(0.05),
            );
            dashed_rect(painter, rect, Stroke::new(1.0, ERR().gamma_multiply(0.7)));
            painter.text(
                rect.left_top() + vec2(14.0, 14.0),
                Align2::LEFT_TOP,
                format!("{}  {id}", icons::TRASH_2),
                font_medium(13.0),
                ERR().gamma_multiply(0.85),
            );
            painter.text(
                rect.left_top() + vec2(14.0, 36.0),
                Align2::LEFT_TOP,
                format!("{kind} · removed"),
                font(11.5),
                TEXT_FAINT(),
            );
        }
    }

    fn has_graph_menu(&mut self, _pos: egui::Pos2, _snarl: &mut Snarl<GNode>) -> bool {
        !self.read_only
    }

    fn show_graph_menu(&mut self, pos: egui::Pos2, ui: &mut Ui, snarl: &mut Snarl<GNode>) {
        if let Some(d) = self.palette_menu(ui, |_| true) {
            self.insert(snarl, &d, pos);
        }
    }

    fn has_dropped_wire_menu(&mut self, _src: AnyPins, _snarl: &mut Snarl<GNode>) -> bool {
        !self.read_only
    }

    fn show_dropped_wire_menu(
        &mut self,
        pos: egui::Pos2,
        ui: &mut Ui,
        src: AnyPins,
        snarl: &mut Snarl<GNode>,
    ) {
        match src {
            AnyPins::Out(pins) => {
                let Some(&from) = pins.first() else { return };
                let Some(ty) = self.output_type(snarl, from) else {
                    return;
                };
                if let Some(d) = self.palette_menu(ui, |d| d.inputs.iter().any(|p| p.ty == ty)) {
                    let input = d.inputs.iter().position(|p| p.ty == ty).unwrap();
                    let node = self.insert(snarl, &d, pos);
                    snarl.connect(from, InPinId { node, input });
                }
            }
            AnyPins::In(pins) => {
                let Some(&to) = pins.first() else { return };
                let Some(ty) = self.input_type(snarl, to) else {
                    return;
                };
                if let Some(d) = self.palette_menu(ui, |d| d.outputs.iter().any(|p| p.ty == ty)) {
                    let output = d.outputs.iter().position(|p| p.ty == ty).unwrap();
                    let node = self.insert(snarl, &d, pos);
                    snarl.connect(OutPinId { node, output }, to);
                }
            }
        }
    }

    fn has_node_menu(&mut self, _node: &GNode) -> bool {
        true
    }

    fn show_node_menu(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut Ui,
        snarl: &mut Snarl<GNode>,
    ) {
        ui.set_min_width(240.0);
        let n = snarl[node].clone();
        let row = |ui: &mut Ui, glyph: &str, label: &str, color: Color32| {
            widgets::list_row(ui, glyph, color, label, "", false)
        };
        let cmd = |ui: &mut Ui, actions: &mut Vec<Action>, c: Cmd| {
            actions.push(Action::Cmd(Cmd::Focus(Some(n.id.clone()))));
            actions.push(Action::Cmd(c));
            ui.close();
        };
        if n.is_composite() {
            if row(ui, icons::OPEN_IN, "Open Subgraph", TEXT_DIM()).clicked() {
                cmd(ui, &mut self.actions, Cmd::OpenSubgraph(n.id.clone()));
            }
            if row(ui, icons::UNGROUP, "Ungroup", TEXT_DIM()).clicked() {
                cmd(ui, &mut self.actions, Cmd::Ungroup(n.id.clone()));
            }
            ui.separator();
        }
        if row(ui, icons::SCAN_EYE, "Watch All Outputs", TEXT_DIM()).clicked() {
            if let Some(d) = self.desc(&n) {
                for p in &d.outputs {
                    self.actions.push(Action::Cmd(Cmd::Watch {
                        node: n.id.clone(),
                        port: p.name.clone(),
                        ty: p.ty.clone(),
                    }));
                }
            }
            ui.close();
        }
        if !self.read_only
            && row(ui, icons::LAYERS, "Run in Its Own Process", TEXT_DIM())
                .on_hover_text("Isolates crashes; costs one shared-memory hop per wire")
                .clicked()
        {
            snarl[node].group = Some(n.id.clone());
            self.actions.push(Action::StructureChanged);
            ui.close();
        }
        if !n.is_composite()
            && self.desc(&n).is_some_and(|d| d.source.is_some())
            && row(ui, icons::FILE_CODE, "Open Node Source", TEXT_DIM()).clicked()
        {
            cmd(ui, &mut self.actions, Cmd::OpenNodeSource(n.id.clone()));
        }
        ui.separator();
        if row(ui, icons::COPY_PLUS, "Duplicate", TEXT_DIM()).clicked() {
            cmd(ui, &mut self.actions, Cmd::Duplicate);
        }
        if row(ui, icons::GROUP, "Group into Subgraph", TEXT_DIM()).clicked() {
            cmd(ui, &mut self.actions, Cmd::GroupSelection);
        }
        ui.separator();
        if !self.read_only && row(ui, icons::TRASH_2, "Delete", ERR()).clicked() {
            snarl.remove_node(node);
            self.actions.push(Action::StructureChanged);
            ui.close();
        }
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<GNode>) {
        if self.read_only {
            self.actions.push(Action::Toast(READ_ONLY.into()));
            return;
        }
        let (Some(a), Some(b)) = (
            self.output_type(snarl, from.id),
            self.input_type(snarl, to.id),
        ) else {
            return;
        };
        if a != b {
            self.actions
                .push(Action::Toast(format!("Cannot connect {a} to {b}")));
            return;
        }
        if from.id.node != to.id.node {
            snarl.connect(from.id, to.id);
            self.actions.push(Action::StructureChanged);
        }
    }

    fn disconnect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<GNode>) {
        if self.read_only {
            self.actions.push(Action::Toast(READ_ONLY.into()));
            return;
        }
        snarl.disconnect(from.id, to.id);
        self.actions.push(Action::StructureChanged);
    }

    fn drop_outputs(&mut self, pin: &OutPin, snarl: &mut Snarl<GNode>) {
        if self.read_only {
            return;
        }
        snarl.drop_outputs(pin.id);
        self.actions.push(Action::StructureChanged);
    }

    fn drop_inputs(&mut self, pin: &InPin, snarl: &mut Snarl<GNode>) {
        if self.read_only {
            return;
        }
        snarl.drop_inputs(pin.id);
        self.actions.push(Action::StructureChanged);
    }
}

/// A rectangle outlined with dashes.
fn dashed_rect(painter: &Painter, rect: Rect, stroke: Stroke) {
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    for i in 0..4 {
        let (a, b) = (corners[i], corners[(i + 1) % 4]);
        let len = (b - a).length();
        let dir = (b - a) / len;
        let mut t = 0.0;
        while t < len {
            painter.line_segment([a + dir * t, a + dir * (t + 5.0).min(len)], stroke);
            t += 9.0;
        }
    }
}
