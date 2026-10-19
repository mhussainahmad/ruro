//! ROS 2 topic browser: lists topics announced through rmw_zenoh and adds
//! bridge nodes for them.

use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, bounded};
use egui::{Align, CornerRadius, Frame, Layout, Margin, RichText, Ui};
use ruro_ros2::TopicInfo;
use serde_json::Value;

use crate::app::{Cmd, Studio};
use crate::icons;
use crate::theme::*;
use crate::widgets;

/// How long discovery listens for liveliness tokens.
const DISCOVERY_WINDOW: Duration = Duration::from_millis(1500);

pub struct RosBrowser {
    /// Zenoh router shared with the ROS 2 nodes (`rmw_zenohd` or a ruro daemon).
    pub router: String,
    pub domain: u32,
    pub filter: String,
    pub topics: Vec<TopicInfo>,
    pub state: State,
    pending: Option<Receiver<Result<Vec<TopicInfo>, String>>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Idle,
    Loading,
    Loaded(Instant),
    Failed(String),
}

impl Default for RosBrowser {
    fn default() -> Self {
        let domain = std::env::var("ROS_DOMAIN_ID")
            .ok()
            .and_then(|d| d.parse().ok())
            .unwrap_or(0);
        Self {
            router: ruro_proto::DEFAULT_ENDPOINT.to_owned(),
            domain,
            filter: String::new(),
            topics: Vec::new(),
            state: State::Idle,
            pending: None,
        }
    }
}

impl RosBrowser {
    /// Starts discovery in the background.
    pub fn refresh(&mut self, ctx: &egui::Context) {
        let (tx, rx) = bounded(1);
        let (router, domain, ctx) = (self.router.clone(), self.domain, ctx.clone());
        std::thread::Builder::new()
            .name("ruro-ros2-discovery".into())
            .spawn(move || {
                let result = (|| {
                    let session = ruro_proto::connect(&router).map_err(|e| format!("{e:#}"))?;
                    ruro_ros2::list_topics(&session, domain, DISCOVERY_WINDOW)
                        .map_err(|e| format!("{e:#}"))
                })();
                let _ = tx.send(result);
                ctx.request_repaint();
            })
            .expect("spawn discovery thread");
        self.pending = Some(rx);
        self.state = State::Loading;
    }

    /// Collects a finished discovery, if any.
    pub fn poll(&mut self) {
        let Some(rx) = &self.pending else { return };
        if let Ok(result) = rx.try_recv() {
            self.pending = None;
            match result {
                Ok(mut topics) => {
                    topics.sort_by(|a, b| a.name.cmp(&b.name));
                    self.topics = topics;
                    self.state = State::Loaded(Instant::now());
                }
                Err(e) => self.state = State::Failed(e),
            }
        }
    }

    /// Parameters for a bridge node on `topic`.
    pub fn bridge_params(&self, topic: &TopicInfo) -> Vec<(String, Value)> {
        vec![
            ("topic".into(), Value::String(topic.name.clone())),
            ("domain_id".into(), Value::from(self.domain)),
            ("router".into(), Value::String(self.router.clone())),
            (
                "qos".into(),
                Value::String(
                    if topic.best_effort {
                        "best_effort"
                    } else {
                        "reliable"
                    }
                    .into(),
                ),
            ),
        ]
    }
}

/// Sidebar view.
pub fn show(s: &mut Studio, ui: &mut Ui) {
    s.ros.poll();
    Frame::new()
        .inner_margin(Margin {
            left: 12,
            right: 12,
            top: 0,
            bottom: 8,
        })
        .show(ui, |ui| {
            widgets::prop_row(
                ui,
                "Router",
                "Zenoh router shared with the ROS 2 nodes",
                |ui| widgets::text_field(ui, &mut s.ros.router, "tcp/127.0.0.1:7447"),
            );
            widgets::prop_row(ui, "Domain", "ROS_DOMAIN_ID", |ui| {
                ui.add(egui::DragValue::new(&mut s.ros.domain).range(0..=232))
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let loading = s.ros.state == State::Loading;
                let label = if loading {
                    "Discovering…"
                } else {
                    "Discover topics"
                };
                let resp = ui
                    .add_enabled_ui(!loading, |ui| {
                        widgets::button(
                            ui,
                            Some(icons::REFRESH_CW),
                            label,
                            widgets::ButtonKind::Secondary,
                        )
                    })
                    .inner;
                if resp.clicked() {
                    s.ros.refresh(ui.ctx());
                }
            });
            ui.add_space(6.0);
            widgets::search_field(ui, &mut s.ros.filter, "Filter topics", ui.available_width());
        });

    match &s.ros.state {
        State::Idle => {
            note(
                ui,
                "Lists topics of ROS 2 nodes that use rmw_zenoh (RMW_IMPLEMENTATION=rmw_zenoh_cpp) and share this router.",
            );
            return;
        }
        State::Failed(e) => {
            note(ui, &format!("Discovery failed: {e}"));
            return;
        }
        State::Loaded(_) if s.ros.topics.is_empty() => {
            note(ui, "No ROS 2 topics found on this router and domain.");
            return;
        }
        _ => {}
    }

    let filter = s.ros.filter.to_lowercase();
    let topics: Vec<TopicInfo> = s
        .ros
        .topics
        .iter()
        .filter(|t| {
            filter.is_empty()
                || t.name.to_lowercase().contains(&filter)
                || t.ros_type.to_lowercase().contains(&filter)
        })
        .cloned()
        .collect();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for t in topics {
                topic_card(s, ui, &t);
            }
        });
}

fn topic_card(s: &mut Studio, ui: &mut Ui, t: &TopicInfo) {
    let kinds = ruro_ros2::bridge_kinds(&t.ros_type);
    // Types without a ruro equivalent go through the generic bridge as
    // ros2/Any.
    let mut params = s.ros.bridge_params(t);
    let (sub, publ) = kinds.unwrap_or_else(|| {
        params.push(("type".into(), Value::String(t.ros_type.clone())));
        params.push(("type_hash".into(), Value::String(t.type_hash.clone())));
        ("ros2/sub", "ros2/pub")
    });
    Frame::new()
        .fill(CARD())
        .stroke(egui::Stroke::new(1.0, BORDER()))
        .corner_radius(CornerRadius::same(RADIUS + 1))
        .inner_margin(Margin::same(10))
        .outer_margin(Margin::symmetric(12, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                widgets::icon(
                    ui,
                    icons::RADIO_TOWER,
                    13.0,
                    if kinds.is_some() {
                        ACCENT_TEXT()
                    } else {
                        TEXT_FAINT()
                    },
                );
                ui.label(
                    RichText::new(&t.name)
                        .font(font_mono(12.0))
                        .color(TEXT_STRONG()),
                );
            });
            ui.label(
                RichText::new(&t.ros_type)
                    .font(font(11.5))
                    .color(TEXT_DIM()),
            );
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} pub · {} sub", t.publishers, t.subscribers))
                        .font(font(11.5))
                        .color(TEXT_FAINT()),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::icon_button_sized(
                        ui,
                        icons::ARROW_RIGHT_LEFT,
                        "Add a publisher node for this topic",
                        22.0,
                        13.0,
                        false,
                    )
                    .clicked()
                    {
                        s.push(Cmd::AddConfiguredNode {
                            kind: publ.into(),
                            params: params.clone(),
                        });
                    }
                    if widgets::icon_button_sized(
                        ui,
                        icons::PLUS,
                        "Add a subscriber node for this topic",
                        22.0,
                        13.0,
                        false,
                    )
                    .clicked()
                    {
                        s.push(Cmd::AddConfiguredNode {
                            kind: sub.into(),
                            params: params.clone(),
                        });
                    }
                    if kinds.is_none() {
                        ui.label(
                            RichText::new("as ros2/Any")
                                .font(font(11.0))
                                .color(TEXT_FAINT()),
                        );
                    }
                });
            });
        });
}

fn note(ui: &mut Ui, text: &str) {
    Frame::new()
        .inner_margin(Margin::symmetric(16, 8))
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(text).color(TEXT_FAINT())).wrap());
        });
}
