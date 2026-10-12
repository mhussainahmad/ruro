//! Headless UI tests: real pointer and keyboard input against the full studio.

use std::path::PathBuf;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use ruro_proto::{DaemonStatus, GroupMetrics, GroupStatus, Level, NodeMetrics, Preview, TapFrame};

use super::*;

fn examples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn harness_sized(size: egui::Vec2, file: Option<PathBuf>) -> Harness<'static, Studio> {
    ruro_nodes_std::link();
    let mut h = Harness::builder().with_size(size).build_eframe(move |cc| {
        let mut s = Studio::new(
            cc,
            Options {
                endpoint: String::new(),
                daemon: None,
                spawn_daemon: false,
                file: None,
            },
        );
        s.registry = ruro_node::registry();
        if let Some(f) = file {
            s.open(&f);
        }
        s
    });
    h.run_steps(4);
    h
}

fn harness(file: Option<PathBuf>) -> Harness<'static, Studio> {
    harness_sized(egui::vec2(1600.0, 1000.0), file)
}

/// Clicks the header of the canvas node named `id`, as a user would.
fn click_on_canvas(h: &mut Harness<'static, Studio>, id: &str) {
    let st = h.state();
    let nid = st.doc.find(id).unwrap_or_else(|| panic!("no node `{id}`"));
    let rect = st
        .node_rects
        .iter()
        .find(|(n, _)| *n == nid)
        .map(|(_, r)| *r)
        .expect("node was drawn");
    let pos = rect.left_top() + egui::vec2(rect.width() * 0.5, 10.0);
    h.event(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(2);
}

#[test]
fn library_click_adds_and_focuses_a_node() {
    let mut h = harness(None);
    h.get_by_label("Add fake-camera").click();
    h.run_steps(4);
    let st = h.state();
    let ids: Vec<&str> = st.doc.snarl.nodes().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, ["fake_camera"]);
    assert!(st.doc.dirty);
    assert!(st.focused.is_some(), "a new node is focused for editing");
}

#[test]
fn clicking_a_node_opens_it_in_the_inspector() {
    let mut h = harness(Some(examples().join("camera.yaml")));
    assert!(h.query_by_label("radius").is_none());
    click_on_canvas(&mut h, "blur");
    h.run_steps(4);
    let st = h.state();
    assert_eq!(
        st.focused.map(|id| st.doc.snarl[id].id.clone()).as_deref(),
        Some("blur")
    );
    assert!(
        h.query_by_label("Delete node").is_some(),
        "inspector shows the node"
    );
}

#[test]
fn command_palette_adds_nodes_from_the_keyboard() {
    let mut h = harness(None);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    h.run_steps(3);
    assert!(h.state().palette.is_some());
    h.event(egui::Event::Text("add sine".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(4);
    let st = h.state();
    assert!(st.palette.is_none());
    assert_eq!(
        st.doc
            .snarl
            .nodes()
            .map(|n| n.kind.as_str())
            .collect::<Vec<_>>(),
        ["std/sine"]
    );
}

#[test]
fn invalid_graph_is_not_deployed() {
    let dir = std::env::temp_dir().join(format!("ruro-studio-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bad.yaml");
    std::fs::write(
        &path,
        "name: bad\nnodes:\n  b: { type: std/blur, params: { radius: 99 } }\n",
    )
    .unwrap();
    let mut h = harness(Some(path));
    h.state_mut().push(Cmd::Deploy);
    h.run_steps(3);
    let st = h.state();
    assert_eq!(st.panel_tab, PanelTab::Problems);
    assert!(st.issues.iter().any(|i| i.message.contains("outside")));
    assert!(st.pending_deploy.is_none());
}

#[test]
fn deleting_the_focused_node_removes_its_wires() {
    let mut h = harness(Some(examples().join("camera.yaml")));
    click_on_canvas(&mut h, "blur");
    h.run_steps(3);
    h.get_by_label("Delete node").click();
    h.run_steps(3);
    let st = h.state();
    assert_eq!(st.doc.snarl.nodes().count(), 3);
    assert_eq!(st.doc.to_graph(&st.registry).wires.len(), 1);
}

/// Simulates a daemon running the camera example, with live metrics, watches and logs.
fn simulate_live(h: &mut Harness<'static, Studio>) {
    let ctx = h.ctx.clone();
    let st = h.state_mut();
    let ev = |st: &mut Studio, e: Event| st.handle_event(&ctx, e);
    ev(
        st,
        Event::Connected {
            daemon: "rover".into(),
            endpoint: "tcp/127.0.0.1:7447".into(),
        },
    );
    ev(
        st,
        Event::Status(DaemonStatus {
            name: "rover".into(),
            version: "0.1.0".into(),
            pid: 4821,
            uptime_s: 132.0,
            graph: Some("camera-demo".into()),
            groups: vec![
                GroupStatus {
                    name: "camera".into(),
                    nodes: vec!["camera".into()],
                    state: GroupState::Running,
                    pid: Some(4830),
                    restarts: 1,
                    last_exit: Some("killed by signal 9".into()),
                },
                GroupStatus {
                    name: "vision".into(),
                    nodes: vec!["blur".into(), "gray".into(), "stats".into()],
                    state: GroupState::Running,
                    pid: Some(4831),
                    restarts: 0,
                    last_exit: None,
                },
            ],
            recording: None,
            playback: None,
            estopped: false,
        }),
    );
    let m = |id: &str, hz: f64, p50: f64, p99: f64, cpu: f64, age: Option<f64>| NodeMetrics {
        id: id.into(),
        state: ruro_proto::NodeState::Running,
        hz,
        step_p50_us: p50,
        step_p99_us: p99,
        input_age_ms: age,
        cpu_pct: cpu,
        errors: 0,
        last_error: None,
        dropped: 0,
        timing: None,
    };
    ev(
        st,
        Event::Metrics(GroupMetrics {
            group: "camera".into(),
            pid: 4830,
            nodes: vec![m("camera", 30.0, 410.0, 640.0, 2.1, None)],
        }),
    );
    ev(
        st,
        Event::Metrics(GroupMetrics {
            group: "vision".into(),
            pid: 4831,
            nodes: vec![
                m("blur", 30.0, 2900.0, 3800.0, 9.6, Some(0.43)),
                m("gray", 30.0, 610.0, 880.0, 1.9, Some(0.03)),
                m("stats", 30.0, 90.0, 140.0, 0.4, Some(0.03)),
            ],
        }),
    );
    st.deployed_rev = Some(st.doc.structure_rev);

    for (node, port, ty) in [
        ("camera", "image", "std/Image"),
        ("gray", "out", "std/Image"),
        ("stats", "brightness", "std/Scalar"),
    ] {
        st.apply(Cmd::Watch {
            node: node.into(),
            port: port.into(),
            ty: ty.into(),
        });
    }
    let frame = |gray: bool, t: f32| {
        let (w, h) = (320u32, 240u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        let (bx, by) = (
            160.0 + 110.0 * (t * 1.3).sin(),
            120.0 + 80.0 * (t * 1.7).cos(),
        );
        for y in 0..h {
            for x in 0..w {
                let ball = ((x as f32 - bx).powi(2) + (y as f32 - by).powi(2)) < 576.0;
                let (r, g, b) = if ball {
                    (250, 250, 250)
                } else {
                    ((x * 255 / w) as u8, (y * 255 / h) as u8, 160u8)
                };
                let (r, g, b) = if gray {
                    let l = ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8;
                    (l, l, l)
                } else {
                    (r, g, b)
                };
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        Preview::Image {
            width: w,
            height: h,
            rgba,
        }
    };
    ev(
        st,
        Event::Tap {
            node: "camera".into(),
            port: "image".into(),
            frame: TapFrame {
                seq: 1,
                preview: frame(false, 2.0),
            },
        },
    );
    ev(
        st,
        Event::Tap {
            node: "gray".into(),
            port: "out".into(),
            frame: TapFrame {
                seq: 1,
                preview: frame(true, 2.0),
            },
        },
    );
    for i in 0..120 {
        let v = 0.52 + 0.012 * (i as f64 * 0.21).sin() + 0.004 * (i as f64 * 1.3).cos();
        ev(
            st,
            Event::Tap {
                node: "stats".into(),
                port: "brightness".into(),
                frame: TapFrame {
                    seq: i,
                    preview: Preview::Values(vec![("value".into(), v)]),
                },
            },
        );
        // Spread samples over time so the plot has an x extent.
        if let Some(w) = st.watches.iter_mut().find(|w| w.node == "stats") {
            w.shift_history_for_test(0.033);
        }
    }
    for (i, (level, node, msg)) in [
        (Level::Info, None, "group `camera` running 1 node(s)"),
        (Level::Info, None, "group `vision` running 3 node(s)"),
        (Level::Info, Some("blur"), "blur.radius = 6"),
        (
            Level::Warn,
            None,
            "group `camera` exited: killed by signal 9",
        ),
        (Level::Info, None, "group `camera` running 1 node(s)"),
    ]
    .into_iter()
    .enumerate()
    {
        ev(
            st,
            Event::Log(LogLine {
                ts_ns: ruro_msg::now_ns() - (5 - i as u64) * 1_000_000_000,
                level,
                group: if msg.contains("camera") {
                    "camera".into()
                } else {
                    "vision".into()
                },
                node: node.map(str::to_owned),
                message: msg.into(),
            }),
        );
    }
}

/// Renders screenshots of key states to `$RURO_SCREENSHOTS` (skipped when unset).
#[test]
fn screenshots() {
    let Some(dir) = std::env::var_os("RURO_SCREENSHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let save = |h: &mut Harness<'static, Studio>, name: &str| {
        h.run_steps(6);
        let img = h.render().expect("render");
        img.save(dir.join(format!("{name}.png"))).unwrap();
    };
    let size = egui::vec2(1680.0, 1020.0);

    let mut h = Harness::builder()
        .with_size(size)
        .wgpu()
        .build_eframe(|cc| {
            let mut s = Studio::new(
                cc,
                Options {
                    endpoint: String::new(),
                    daemon: None,
                    spawn_daemon: false,
                    file: None,
                },
            );
            s.registry = ruro_node::registry();
            s
        });
    save(&mut h, "01-welcome");

    let file = examples().join("camera.yaml");
    let mut h = Harness::builder()
        .with_size(size)
        .wgpu()
        .build_eframe(move |cc| {
            let mut s = Studio::new(
                cc,
                Options {
                    endpoint: String::new(),
                    daemon: None,
                    spawn_daemon: false,
                    file: None,
                },
            );
            s.registry = ruro_node::registry();
            s.open(&file);
            s
        });
    save(&mut h, "02-editing");

    simulate_live(&mut h);
    h.state_mut().apply(Cmd::Focus(Some("blur".into())));
    save(&mut h, "03-live");

    h.state_mut().apply(Cmd::Activity(Activity::Processes));
    h.state_mut().apply(Cmd::ShowPanel(PanelTab::Output));
    save(&mut h, "04-processes-output");

    h.state_mut().apply(Cmd::CommandPalette);
    h.run_steps(2);
    h.event(egui::Event::Text("watch".into()));
    save(&mut h, "05-command-palette");
}
