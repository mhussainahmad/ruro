//! Headless UI tests: real pointer and keyboard input against the full studio.

use std::path::{Path, PathBuf};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use ruro_msg::FramePreview;
use ruro_proto::{
    DaemonStatus, GroupMetrics, GroupState, GroupStatus, Level, NodeMetrics, PlaybackStatus,
    Preview, TapFrame,
};

use super::*;
use crate::link::{Command, Event};

fn examples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

/// A fresh directory for one test.
fn tempdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ruro-studio-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A writable copy of an example graph.
fn copy_example(name: &str, dir: &Path) -> PathBuf {
    let to = dir.join(name);
    std::fs::copy(examples().join(name), &to).unwrap();
    to
}

fn build(size: egui::Vec2, opts: Options, file: Option<PathBuf>) -> Harness<'static, Studio> {
    ruro_nodes_std::link();
    let mut opts = Some(opts);
    let mut h = Harness::builder().with_size(size).build_eframe(move |cc| {
        let mut s = Studio::new(cc, opts.take().unwrap());
        s.registry = ruro_node::registry();
        if let Some(f) = &file {
            s.open(f);
        }
        s
    });
    h.run_steps(4);
    h
}

fn harness(file: Option<PathBuf>) -> Harness<'static, Studio> {
    build(egui::vec2(1600.0, 1000.0), Options::offline(), file)
}

fn node_rect(h: &Harness<'static, Studio>, id: &str) -> egui::Rect {
    let st = h.state();
    let nid = st.doc.find(id).unwrap_or_else(|| panic!("no node `{id}`"));
    st.node_rects
        .iter()
        .find(|(n, _)| *n == nid)
        .map(|(_, r)| *r)
        .expect("node was drawn")
}

fn click_at(h: &mut Harness<'static, Studio>, pos: egui::Pos2, modifiers: egui::Modifiers) {
    h.event(egui::Event::ModifiersChanged(modifiers));
    h.event(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        });
    }
    h.run_steps(2);
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.run_steps(1);
}

/// Clicks the header of the canvas node named `id`, as a user would.
fn click_on_canvas(h: &mut Harness<'static, Studio>, id: &str) {
    let rect = node_rect(h, id);
    click_at(
        h,
        rect.left_top() + egui::vec2(rect.width() * 0.5, 10.0),
        egui::Modifiers::NONE,
    );
}

/// Presses at `from`, moves to `to` in steps and releases, frame by frame.
fn drag(h: &mut Harness<'static, Studio>, from: egui::Pos2, to: egui::Pos2) {
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(egui::Event::PointerMoved(from));
    h.run_steps(1);
    h.event(button(from, true));
    h.run_steps(1);
    for i in 1..=8 {
        let p = from + (to - from) * (i as f32 / 8.0);
        h.event(egui::Event::PointerMoved(p));
        h.run_steps(1);
    }
    h.event(button(to, false));
    h.run_steps(3);
}

fn key(h: &mut Harness<'static, Studio>, modifiers: egui::Modifiers, k: egui::Key) {
    h.key_press_modifiers(modifiers, k);
    h.run_steps(3);
}

const CTRL: egui::Modifiers = egui::Modifiers::COMMAND;

fn ctrl_shift() -> egui::Modifiers {
    egui::Modifiers::COMMAND | egui::Modifiers::SHIFT
}

fn node_ids(st: &Studio) -> Vec<String> {
    let mut v: Vec<String> = st.doc.snarl.nodes().map(|n| n.id.clone()).collect();
    v.sort();
    v
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
    key(&mut h, CTRL, egui::Key::K);
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
fn command_palette_runs_commands_and_dismisses() {
    let mut h = harness(Some(examples().join("camera.yaml")));
    key(&mut h, CTRL, egui::Key::K);
    h.event(egui::Event::Text("toggle panel".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert!(!h.state().panel_open);
    assert!(h.state().palette.is_none());

    // Escape closes without running anything.
    key(&mut h, CTRL, egui::Key::K);
    h.event(egui::Event::Text("toggle panel".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.state().palette.is_none());
    assert!(!h.state().panel_open);

    // Quick open lists graphs and opens one with Enter.
    key(&mut h, CTRL, egui::Key::P);
    let palette = h.state_mut().palette.as_mut().unwrap();
    palette.query = examples().join("arm.yaml").display().to_string();
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(4);
    assert_eq!(h.state().doc.name, "arm-demo");
    assert_eq!(h.state().tabs().len(), 2);
}

#[test]
fn invalid_graph_is_not_deployed() {
    let dir = tempdir("invalid");
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
fn clicking_a_problem_focuses_its_node() {
    let dir = tempdir("problems");
    let path = dir.join("bad.yaml");
    std::fs::write(
        &path,
        "name: bad\nnodes:\n  ok: { type: std/sine, rate: 10 }\n  b: { type: std/blur, params: { radius: 99 } }\n",
    )
    .unwrap();
    let mut h = harness(Some(path));
    h.state_mut().push(Cmd::ShowPanel(PanelTab::Problems));
    h.run_steps(3);
    let label = h.state().issues[0].to_string();
    h.get_by_label(&label).click();
    h.run_steps(3);
    let st = h.state();
    assert_eq!(st.focused.map(|n| st.doc.snarl[n].id.as_str()), Some("b"));
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

#[test]
fn open_edit_undo_redo_and_save_roundtrip() {
    let dir = tempdir("roundtrip");
    let path = copy_example("camera.yaml", &dir);
    let mut h = harness(Some(path.clone()));
    assert!(!h.state().doc.dirty);

    h.get_by_label("Add sine").click();
    h.run_steps(4);
    assert!(h.state().doc.find("sine").is_some());
    assert!(h.state().doc.dirty);

    key(&mut h, CTRL, egui::Key::Z);
    assert!(h.state().doc.find("sine").is_none());
    assert!(!h.state().doc.dirty, "undo returns to the saved state");

    key(&mut h, ctrl_shift(), egui::Key::Z);
    assert!(h.state().doc.find("sine").is_some());
    key(&mut h, CTRL, egui::Key::Z);
    key(&mut h, CTRL, egui::Key::Y);
    assert!(h.state().doc.find("sine").is_some(), "Ctrl+Y redoes too");

    key(&mut h, CTRL, egui::Key::S);
    assert!(!h.state().doc.dirty);
    let saved = ruro_graph::Graph::load(&path).unwrap();
    assert!(saved.nodes.contains_key("sine"));
    assert!(h.state().settings.recent[0].ends_with("camera.yaml"));
}

#[test]
fn dragging_a_node_is_one_undo_step() {
    let mut h = harness(Some(examples().join("camera.yaml")));
    let id = h.state().doc.find("blur").unwrap();
    let before = h.state().doc.snarl.get_node_info(id).unwrap().pos;
    let r = node_rect(&h, "blur");
    let from = r.left_top() + egui::vec2(r.width() * 0.5, 10.0);
    drag(&mut h, from, from + egui::vec2(120.0, 80.0));
    let after = h.state().doc.snarl.get_node_info(id).unwrap().pos;
    assert!((after - before).length() > 50.0, "{before:?} -> {after:?}");
    key(&mut h, CTRL, egui::Key::Z);
    assert_eq!(h.state().doc.snarl.get_node_info(id).unwrap().pos, before);
}

#[test]
fn dragging_between_pins_creates_a_wire() {
    let mut h = harness(None);
    for kind in ["std/fake-camera", "std/blur"] {
        h.state_mut().push(Cmd::AddNode(kind.into()));
        h.run_steps(2);
    }
    // Space the nodes out so the wire is a real drag.
    {
        let st = h.state_mut();
        let blur = st.doc.find("blur").unwrap();
        st.doc.snarl.get_node_info_mut(blur).unwrap().pos = egui::pos2(520.0, 160.0);
        st.fit_pending = true;
    }
    h.run_steps(4);
    let (from, to) = {
        let st = h.state();
        let cam = st.doc.find("fake_camera").unwrap();
        let blur = st.doc.find("blur").unwrap();
        (
            st.pin_pos.output(cam, 0).expect("output pin drawn"),
            st.pin_pos.input(blur, 0).expect("input pin drawn"),
        )
    };
    drag(&mut h, from, to);
    let st = h.state();
    let wires = st.doc.to_graph(&st.registry).wires;
    assert_eq!(wires.len(), 1, "{wires:?}");
    assert_eq!(wires[0].to_string(), "fake_camera.image -> blur.image");
    key(&mut h, CTRL, egui::Key::Z);
    let st = h.state();
    assert!(st.doc.to_graph(&st.registry).wires.is_empty());
}

#[test]
fn inspector_edits_a_parameter() {
    let mut h = harness(Some(examples().join("camera.yaml")));
    click_on_canvas(&mut h, "blur");
    h.run_steps(3);
    h.get_by_label("radius").click();
    h.run_steps(4);
    let st = h.state();
    let blur = st.doc.find("blur").unwrap();
    let radius = st.doc.snarl[blur].params["radius"].as_i64().unwrap();
    assert!(
        radius != 4 && (8..=24).contains(&radius),
        "clicking the slider sets a value near its middle, got {radius}"
    );
    assert!(st.doc.dirty);
    key(&mut h, CTRL, egui::Key::Z);
    let st = h.state();
    assert_eq!(st.doc.snarl[blur].params["radius"], serde_json::json!(4));
}

#[test]
fn copy_paste_cut_and_duplicate() {
    let mut h = harness(Some(examples().join("camera.yaml")));
    click_on_canvas(&mut h, "blur");
    h.event(egui::Event::Copy);
    h.run_steps(3);
    let yaml = h.state().clipboard.clone().expect("copied");
    assert!(yaml.contains("std/blur"));

    h.event(egui::Event::Paste(yaml.clone()));
    h.run_steps(3);
    {
        let st = h.state();
        let copy = st.doc.find("blur2").expect("pasted as blur2");
        assert_eq!(st.doc.snarl[copy].params["radius"], serde_json::json!(4));
        assert_eq!(st.focused, Some(copy));
    }

    key(&mut h, CTRL, egui::Key::D);
    assert!(h.state().doc.find("blur3").is_some());

    // Select everything and copy: wires between copied nodes come along.
    key(&mut h, CTRL, egui::Key::A);
    h.event(egui::Event::Copy);
    h.run_steps(2);
    let all = h.state().clipboard.clone().unwrap();
    h.event(egui::Event::Paste(all));
    h.run_steps(3);
    {
        let st = h.state();
        assert_eq!(st.doc.snarl.nodes().count(), 12);
        assert_eq!(st.doc.to_graph(&st.registry).wires.len(), 6);
    }

    // Cut removes the selection.
    h.event(egui::Event::Cut);
    h.run_steps(3);
    assert_eq!(h.state().doc.snarl.nodes().count(), 6);

    // Text that is not a graph is ignored.
    h.event(egui::Event::Paste("hello".into()));
    h.run_steps(2);
    assert_eq!(h.state().doc.snarl.nodes().count(), 6);
}

#[test]
fn select_all_then_align_and_delete() {
    let mut h = harness(Some(examples().join("camera.yaml")));
    key(&mut h, CTRL, egui::Key::A);
    assert_eq!(h.state().selected_nodes().len(), 4);
    assert!(
        h.query_by_label("Delete 4 nodes").is_some(),
        "selection inspector"
    );
    h.state_mut().push(Cmd::Align(AlignOp::Left));
    h.run_steps(4);
    let lefts: Vec<f32> = ["camera", "blur", "gray", "stats"]
        .iter()
        .map(|n| node_rect(&h, n).left())
        .collect();
    assert!(
        lefts.iter().all(|l| (l - lefts[0]).abs() < 1.0),
        "{lefts:?}"
    );
    key(&mut h, CTRL, egui::Key::Z);
    h.run_steps(2);
    let blur = node_rect(&h, "blur").left();
    assert!((blur - lefts[0]).abs() > 10.0, "align undone");

    // Shift-click builds a selection on the canvas too.
    click_on_canvas(&mut h, "camera");
    let r = node_rect(&h, "stats");
    click_at(
        &mut h,
        r.left_top() + egui::vec2(r.width() * 0.5, 10.0),
        egui::Modifiers::SHIFT,
    );
    assert_eq!(h.state().selected_nodes().len(), 2);
    h.key_press(egui::Key::Delete);
    h.run_steps(3);
    assert_eq!(node_ids(h.state()), ["blur", "gray"]);
}

#[test]
fn tabs_switch_close_and_confirm_unsaved_changes() {
    let mut h = harness(Some(examples().join("camera.yaml")));
    h.state_mut().push(Cmd::Open(examples().join("arm.yaml")));
    h.run_steps(3);
    assert_eq!(h.state().tabs().len(), 2);
    assert_eq!(h.state().doc.name, "arm-demo");

    h.get_by_label("camera.yaml").click();
    h.run_steps(3);
    assert_eq!(h.state().doc.name, "camera-demo");
    // Opening an open file switches to it instead of opening it twice.
    h.state_mut().push(Cmd::Open(examples().join("arm.yaml")));
    h.run_steps(3);
    assert_eq!(h.state().tabs().len(), 2);

    // Each tab has its own history.
    h.get_by_label("Add sine").click();
    h.run_steps(3);
    h.get_by_label("camera.yaml").click();
    h.run_steps(3);
    assert!(!h.state().doc.can_undo());

    // Closing a changed tab asks first.
    h.get_by_label("arm.yaml").click();
    h.run_steps(3);
    key(&mut h, CTRL, egui::Key::W);
    assert!(h.state().confirm_close.is_some());
    assert_eq!(h.state().tabs().len(), 2);
    h.get_by_label("Don't Save").click();
    h.run_steps(3);
    assert_eq!(h.state().tabs().len(), 1);
    assert_eq!(h.state().doc.name, "camera-demo");

    // Middle-click closes an unchanged tab.
    h.state_mut().push(Cmd::NewFromTemplate("pid-loop"));
    h.run_steps(3);
    let uid = h.state().doc.uid;
    h.state_mut().doc.reset_history(true);
    let tab = h.get_by_label("pid-loop.yaml");
    let pos = tab.rect().center();
    h.event(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Middle,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(3);
    assert!(h.state().doc_by_uid(uid).is_none());
}

#[test]
fn templates_open_as_new_documents() {
    let mut h = harness(None);
    h.get_by_label("Template PID loop").click();
    h.run_steps(3);
    let st = h.state();
    assert_eq!(st.doc.name, "pid-loop");
    assert_eq!(node_ids(st), ["arm", "pid", "target"]);
    assert!(st.doc.path.is_none() && st.doc.dirty);
    assert!(st.issues.is_empty(), "{:?}", st.issues);
}

#[test]
fn group_ungroup_and_edit_a_subgraph() {
    let mut h = harness(Some(examples().join("camera.yaml")));
    {
        let st = h.state_mut();
        let blur = st.doc.find("blur").unwrap();
        let gray = st.doc.find("gray").unwrap();
        st.marked = [blur, gray].into_iter().collect();
    }
    key(&mut h, CTRL, egui::Key::G);
    {
        let st = h.state();
        assert_eq!(node_ids(st), ["camera", "group", "stats"]);
        let g = st.doc.to_graph(&st.registry);
        assert_eq!(g.wires.len(), 2, "{:?}", g.wires);
        assert!(st.issues.is_empty(), "{:?}", st.issues);
        let d = &st.doc.composites["group"];
        assert_eq!(d.inputs[0].name, "image");
        assert_eq!(d.outputs[0].name, "out");
    }

    // Open the subgraph in its own tab and edit it there.
    h.state_mut().push(Cmd::OpenSubgraph("group".into()));
    h.run_steps(3);
    assert!(h.state().doc.parent.is_some());
    assert_eq!(node_ids(h.state()), ["blur", "gray"]);
    {
        let st = h.state_mut();
        let blur = st.doc.find("blur").unwrap();
        st.doc.snarl[blur]
            .params
            .insert("radius".into(), serde_json::json!(9));
    }
    h.run_steps(3);
    let parent = h.state().doc.parent.clone().unwrap().0;
    {
        let st = h.state();
        let p = st.doc_by_uid(parent).unwrap();
        let g = p
            .find("group")
            .map(|n| p.snarl[n].sub.clone().unwrap())
            .unwrap();
        assert_eq!(g.nodes["blur"].params["radius"], serde_json::json!(9));
        assert!(p.dirty);
    }

    // Back in the parent, ungroup restores plain nodes.
    h.state_mut().push(Cmd::SwitchTab(parent));
    h.run_steps(2);
    h.state_mut().push(Cmd::Ungroup("group".into()));
    h.run_steps(3);
    let st = h.state();
    assert_eq!(node_ids(st), ["blur", "camera", "gray", "stats"]);
    assert_eq!(st.doc.to_graph(&st.registry).wires.len(), 3);
    key(&mut h, CTRL, egui::Key::Z);
    assert_eq!(node_ids(h.state()), ["camera", "group", "stats"]);
}

#[test]
fn compare_with_saved_marks_changes() {
    let dir = tempdir("compare");
    let path = copy_example("camera.yaml", &dir);
    let mut h = harness(Some(path));
    click_on_canvas(&mut h, "gray");
    h.state_mut().push(Cmd::DeleteSelection);
    h.run_steps(2);
    {
        let st = h.state_mut();
        let blur = st.doc.find("blur").unwrap();
        st.set_param(blur, "radius", serde_json::json!(7));
    }
    h.state_mut().push(Cmd::AddNode("std/sine".into()));
    h.run_steps(2);
    h.state_mut().push(Cmd::CompareWithSaved);
    h.run_steps(3);
    let st = h.state();
    let c = st.compare.as_ref().expect("comparing");
    assert_eq!(c.diff.added, ["sine"]);
    assert_eq!(c.diff.removed[0].0, "gray");
    assert_eq!(c.diff.changed[0].0, "blur");
    assert_eq!(c.diff.wires_removed.len(), 2);
    assert_eq!(st.activity, Activity::Changes);
    assert!(h.query_by_label("Close comparison").is_some());
}

#[test]
fn theme_switch_restyles_and_is_remembered() {
    let dir = tempdir("theme");
    let store = crate::settings::Store {
        config_dir: dir.join("config"),
        state_dir: dir.join("state"),
    };
    let opts = Options {
        store: Some(store.clone()),
        ..Options::offline()
    };
    let mut h = build(egui::vec2(1400.0, 900.0), opts, None);
    h.get_by_label("Switch to light theme").click();
    h.run_steps(3);
    assert_eq!(h.state().theme, ThemeKind::Light);
    assert!(crate::theme::is_light());
    assert_eq!(
        h.ctx.global_style().visuals.panel_fill,
        crate::theme::CHROME()
    );
    h.state_mut().panel_open = false;
    h.run_steps(2);
    h.state_mut().shutdown();
    crate::theme::apply(&h.ctx, ThemeKind::Dark);

    let again = store.load_settings();
    assert_eq!(again.theme, ThemeKind::Light);
    assert!(!again.panel_open);
    let h2 = build(
        egui::vec2(1400.0, 900.0),
        Options {
            store: Some(store),
            ..Options::offline()
        },
        None,
    );
    assert!(!h2.state().panel_open);
    assert_eq!(h2.state().theme, ThemeKind::Light);
    crate::theme::apply(&h2.ctx, ThemeKind::Dark);
}

#[test]
fn unsaved_work_is_autosaved_and_recovered() {
    let dir = tempdir("autosave");
    let store = crate::settings::Store {
        config_dir: dir.join("config"),
        state_dir: dir.join("state"),
    };
    // A crashed session left an autosave behind.
    let autosave = dir.join("state/autosave");
    std::fs::create_dir_all(&autosave).unwrap();
    std::fs::write(
        autosave.join("1-3.yaml"),
        "name: lost\nnodes:\n  s: { type: std/sine, rate: 5 }\n",
    )
    .unwrap();
    std::fs::write(
        autosave.join("1-3.json"),
        r#"{"original": null, "name": "lost"}"#,
    )
    .unwrap();

    let mut h = build(
        egui::vec2(1400.0, 900.0),
        Options {
            store: Some(store.clone()),
            ..Options::offline()
        },
        None,
    );
    assert_eq!(h.state().recovery.len(), 1);
    h.get_by_label("Restore").click();
    h.run_steps(3);
    let st = h.state();
    assert_eq!(st.doc.name, "lost");
    assert!(st.doc.dirty);
    assert!(st.recovery.is_empty());
    assert!(!autosave.join("1-3.yaml").exists());

    // This session's unsaved work is written for the next one.
    h.state_mut().shutdown();
    let mine = std::fs::read_dir(&autosave).unwrap().count();
    assert_eq!(mine, 2, "yaml and metadata");
}

#[test]
fn deploy_flattens_and_watches_resolve_into_composites() {
    let mut h = harness(Some(examples().join("camera.yaml")));
    {
        let st = h.state_mut();
        st.marked = [st.doc.find("blur").unwrap(), st.doc.find("gray").unwrap()]
            .into_iter()
            .collect();
    }
    key(&mut h, CTRL, egui::Key::G);
    let ctx = h.ctx.clone();
    h.state_mut().handle_event(
        &ctx,
        Event::Connected {
            daemon: "rover".into(),
            endpoint: "tcp/127.0.0.1:7447".into(),
        },
    );
    h.state().link.take_sent();
    h.state_mut().push(Cmd::Deploy);
    h.run_steps(2);
    let sent = h.state().link.take_sent();
    let Some(Command::Deploy(yaml)) = sent.first() else {
        panic!("{sent:?}")
    };
    let g = ruro_graph::Graph::from_yaml(yaml).unwrap();
    assert!(g.nodes.contains_key("group__blur"), "{yaml}");
    assert!(!g.has_composites());

    h.state_mut().push(Cmd::Watch {
        node: "group".into(),
        port: "out".into(),
        ty: "std/Image".into(),
    });
    h.run_steps(2);
    let sent = h.state().link.take_sent();
    assert!(
        matches!(&sent[..], [Command::Watch { node, port }] if node == "group__gray" && port == "out"),
        "{sent:?}"
    );
}

#[test]
fn record_and_replay_controls_send_requests() {
    let mut h = harness(Some(examples().join("camera.yaml")));
    simulate_live(&mut h);
    h.run_steps(2);
    h.state().link.take_sent();

    h.get_by_label("Record every wire to a file").click();
    h.run_steps(3);
    assert!(h.state().record_draft.is_some());
    h.get_by_label("Start recording").click();
    h.run_steps(3);
    let sent = h.state().link.take_sent();
    assert!(
        matches!(&sent[..], [Command::Record(p)] if p.starts_with("camera-demo-") && p.ends_with(".mcap")),
        "{sent:?}"
    );

    // The daemon reports the recording and a replay in progress.
    set_status(&mut h, Some("rec.mcap"), Some(playback(false)));
    h.run_steps(3);
    assert_eq!(
        h.query_all_by_label("Stop recording").count(),
        2,
        "title bar and status bar"
    );
    h.get_by_label("Pause").click();
    h.run_steps(2);
    h.get_by_label("Back to start").click();
    h.run_steps(2);
    let sent = h.state().link.take_sent();
    assert!(
        matches!(
            &sent[..],
            [
                Command::Playback(ruro_proto::PlaybackCommand::Pause),
                Command::Playback(ruro_proto::PlaybackCommand::Seek { position_s })
            ] if *position_s == 0.0
        ),
        "{sent:?}"
    );
    // Seeking by clicking the bar.
    let bar = h.get_by_label("Playback position").rect();
    click_at(&mut h, bar.center(), egui::Modifiers::NONE);
    let sent = h.state().link.take_sent();
    assert!(
        matches!(&sent[..], [Command::Playback(ruro_proto::PlaybackCommand::Seek { position_s })] if (*position_s - 45.0).abs() < 3.0),
        "{sent:?}"
    );
}

#[test]
fn fleet_lists_daemons_and_switches() {
    let mut h = harness(None);
    let ctx = h.ctx.clone();
    {
        let st = h.state_mut();
        st.handle_event(
            &ctx,
            Event::Connected {
                daemon: "laptop".into(),
                endpoint: "tcp/127.0.0.1:7447".into(),
            },
        );
        for (name, graph) in [("laptop", None), ("rover", Some("nav"))] {
            st.handle_event(&ctx, Event::Fleet(daemon_status(name, graph)));
        }
        st.link.take_sent();
        st.push(Cmd::Activity(Activity::Fleet));
    }
    h.run_steps(3);
    h.get_by_label("Connect").click();
    h.run_steps(3);
    let sent = h.state().link.take_sent();
    assert!(
        matches!(&sent[..], [Command::SwitchDaemon(n)] if n == "rover"),
        "{sent:?}"
    );
}

#[test]
fn point_clouds_and_frames_use_the_3d_view() {
    let mut h = harness(None);
    let ctx = h.ctx.clone();
    let st = h.state_mut();
    st.watches.push(crate::watch::Watch::new(
        "lidar".into(),
        "scan".into(),
        "std/LaserScan".into(),
        ("lidar".into(), "scan".into()),
    ));
    st.handle_event(
        &ctx,
        Event::Tap {
            node: "lidar".into(),
            port: "scan".into(),
            frame: TapFrame {
                seq: 1,
                preview: Preview::Points {
                    frame: "laser".into(),
                    points: vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.5]],
                },
            },
        },
    );
    assert_eq!(st.watches[0].visualizer(), Some("std/scene3d"));
    st.handle_event(
        &ctx,
        Event::Tap {
            node: "lidar".into(),
            port: "scan".into(),
            frame: TapFrame {
                seq: 2,
                preview: Preview::Values(vec![("x".into(), 1.0)]),
            },
        },
    );
    assert_eq!(st.watches[0].visualizer(), Some("std/plot"));
}

#[test]
fn every_builtin_visualizer_is_registered() {
    let ids: Vec<&str> = crate::plugin::visualizers().iter().map(|k| k.id).collect();
    for id in [
        "std/image",
        "std/plot",
        "std/scene3d",
        "std/text",
        "std/values",
    ] {
        assert!(ids.contains(&id), "{ids:?}");
    }
    assert!(crate::plugin::panels().iter().any(|p| p.id == "std/timing"));
}

// ----- simulation ----------------------------------------------------------------

fn daemon_status(name: &str, graph: Option<&str>) -> DaemonStatus {
    DaemonStatus {
        name: name.into(),
        version: "0.1.0".into(),
        pid: 4821,
        uptime_s: 7340.0,
        graph: graph.map(Into::into),
        groups: vec![],
        recording: None,
        playback: None,
    }
}

fn playback(paused: bool) -> PlaybackStatus {
    PlaybackStatus {
        file: "drive.mcap".into(),
        position_s: 31.4,
        duration_s: 90.0,
        paused,
        rate: 1.0,
    }
}

fn camera_status(recording: Option<&str>, playback: Option<PlaybackStatus>) -> DaemonStatus {
    DaemonStatus {
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
        recording: recording.map(Into::into),
        playback,
    }
}

fn set_status(h: &mut Harness<'static, Studio>, rec: Option<&str>, pb: Option<PlaybackStatus>) {
    let ctx = h.ctx.clone();
    h.state_mut()
        .handle_event(&ctx, Event::Status(camera_status(rec, pb)));
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
    ev(st, Event::Status(camera_status(None, None)));
    ev(st, Event::Fleet(daemon_status("lab-pc", None)));
    ev(
        st,
        Event::Fleet(daemon_status("arm-controller", Some("arm-demo"))),
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
        st.apply(
            &ctx,
            Cmd::Watch {
                node: node.into(),
                port: port.into(),
                ty: ty.into(),
            },
        );
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
    for (node, port, gray) in [("camera", "image", false), ("gray", "out", true)] {
        ev(
            st,
            Event::Tap {
                node: node.into(),
                port: port.into(),
                frame: TapFrame {
                    seq: 1,
                    preview: frame(gray, 2.0),
                },
            },
        );
    }
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
        crate::builtin::advance_clock_for_test(0.033);
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

/// A lidar point cloud with a tf tree, as a robot would stream it.
fn simulate_3d(h: &mut Harness<'static, Studio>) {
    let ctx = h.ctx.clone();
    let st = h.state_mut();
    st.apply(
        &ctx,
        Cmd::Watch {
            node: "lidar".into(),
            port: "points".into(),
            ty: "std/PointCloud".into(),
        },
    );
    let s = (0.6f32 / 2.0).sin();
    let c = (0.6f32 / 2.0).cos();
    let frames = vec![
        FramePreview {
            name: "odom".into(),
            parent: String::new(),
            translation: [0.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
        },
        FramePreview {
            name: "base_link".into(),
            parent: "odom".into(),
            translation: [1.2, 0.6, 0.0],
            rotation: [0.0, 0.0, s, c],
        },
        FramePreview {
            name: "laser".into(),
            parent: "base_link".into(),
            translation: [0.25, 0.0, 0.45],
            rotation: [0.0, 0.0, 0.0, 1.0],
        },
    ];
    // A room: walls, a floor patch and a box, seen from the laser.
    let mut points = Vec::new();
    for i in 0..1400 {
        let a = i as f32 / 1400.0 * std::f32::consts::TAU;
        let r = 4.0 + 0.6 * (a * 3.0).cos();
        for k in 0..12 {
            let z = -0.45 + k as f32 * 0.12;
            points.push([r * a.cos(), r * a.sin(), z]);
        }
    }
    for i in 0..30 {
        for j in 0..30 {
            for k in 0..4 {
                let (x, y) = (1.5 + i as f32 * 0.02, -1.0 + j as f32 * 0.02);
                if i == 0 || j == 0 || i == 29 || j == 29 {
                    points.push([x, y, -0.45 + k as f32 * 0.15]);
                }
            }
        }
    }
    for (seq, preview) in [
        (1, Preview::Frames(frames)),
        (
            2,
            Preview::Points {
                frame: "laser".into(),
                points,
            },
        ),
    ] {
        st.handle_event(
            &ctx,
            Event::Tap {
                node: "lidar".into(),
                port: "points".into(),
                frame: TapFrame { seq, preview },
            },
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
    let shot_with = |file: Option<PathBuf>, panel: f32| {
        ruro_nodes_std::link();
        let mut file = file;
        Harness::builder()
            .with_size(size)
            .wgpu()
            .build_eframe(move |cc| {
                let mut s = Studio::new(cc, Options::offline());
                s.registry = ruro_node::registry();
                s.settings.panel_height = Some(panel);
                s.settings.recent = ["camera.yaml", "arm.yaml"]
                    .iter()
                    .map(|f| examples().join(f).canonicalize().unwrap())
                    .collect();
                if let Some(f) = file.take() {
                    s.open(&f);
                }
                s
            })
    };
    let shot = |file: Option<PathBuf>| shot_with(file, 280.0);

    let mut h = shot(None);
    save(&mut h, "01-welcome");

    let mut h = shot(Some(examples().join("camera.yaml")));
    save(&mut h, "02-editing");

    simulate_live(&mut h);
    h.state_mut().push(Cmd::Focus(Some("blur".into())));
    save(&mut h, "03-live");

    h.state_mut().push(Cmd::Activity(Activity::Processes));
    h.state_mut().push(Cmd::ShowPanel(PanelTab::Output));
    save(&mut h, "04-processes-output");

    h.state_mut().push(Cmd::CommandPalette);
    h.run_steps(2);
    h.event(egui::Event::Text("watch".into()));
    save(&mut h, "05-command-palette");
    h.key_press(egui::Key::Escape);

    // Recording and replay.
    h.state_mut().push(Cmd::Activity(Activity::Library));
    h.state_mut().push(Cmd::ShowPanel(PanelTab::Watch));
    set_status(
        &mut h,
        Some("camera-demo-20261011-142210.mcap"),
        Some(playback(true)),
    );
    h.state_mut().recording_since =
        std::time::Instant::now().checked_sub(std::time::Duration::from_secs(83));
    save(&mut h, "06-recording-replay");
    h.state_mut().push(Cmd::Activity(Activity::Fleet));
    set_status(&mut h, None, None);
    save(&mut h, "07-fleet");

    // The 3D view.
    let mut h = shot_with(Some(examples().join("camera.yaml")), 520.0);
    simulate_live(&mut h);
    h.state_mut().watches.clear();
    simulate_3d(&mut h);
    h.state_mut().push(Cmd::Focus(Some("blur".into())));
    save(&mut h, "08-3d-view");

    // Light theme.
    let mut h = shot(Some(examples().join("camera.yaml")));
    simulate_live(&mut h);
    h.state_mut().push(Cmd::Focus(Some("blur".into())));
    h.state_mut().push(Cmd::SetTheme(ThemeKind::Light));
    save(&mut h, "09-light");
    let mut w = shot(None);
    w.state_mut().push(Cmd::SetTheme(ThemeKind::Light));
    save(&mut w, "10-welcome-light");
    crate::theme::apply(&w.ctx, ThemeKind::Dark);

    // Tabs, a subgraph and a comparison.
    let tmp = tempdir("shots");
    let path = copy_example("camera.yaml", &tmp);
    let mut h = shot(Some(path));
    h.state_mut().push(Cmd::Open(examples().join("arm.yaml")));
    h.run_steps(2);
    let first = h.state().tab_order[0];
    h.state_mut().push(Cmd::SwitchTab(first));
    h.run_steps(2);
    {
        let st = h.state_mut();
        st.marked = [st.doc.find("blur").unwrap(), st.doc.find("gray").unwrap()]
            .into_iter()
            .collect();
    }
    h.state_mut().push(Cmd::GroupSelection);
    h.run_steps(2);
    {
        let st = h.state_mut();
        let id = st.doc.find("group").unwrap();
        st.rename_node(id, "vision");
        st.doc.snarl.get_node_info_mut(id).unwrap().pos = egui::pos2(380.0, 120.0);
    }
    h.state_mut().push(Cmd::AddNode("std/sine".into()));
    h.run_steps(2);
    {
        let st = h.state_mut();
        let id = st.doc.find("sine").unwrap();
        st.doc.snarl.get_node_info_mut(id).unwrap().pos = egui::pos2(0.0, 300.0);
    }
    h.state_mut().push(Cmd::CompareWithSaved);
    h.state_mut().push(Cmd::FitView);
    save(&mut h, "11-compare");
    h.state_mut().push(Cmd::CloseCompare);
    h.state_mut().push(Cmd::Activity(Activity::Outline));
    h.state_mut().push(Cmd::Focus(Some("vision".into())));
    save(&mut h, "12-subgraph");
    h.state_mut().push(Cmd::OpenSubgraph("vision".into()));
    h.state_mut().push(Cmd::FitView);
    save(&mut h, "13-subgraph-tab");

    // Multi-selection and the close prompt.
    let mut h = shot(Some(examples().join("camera.yaml")));
    h.key_press_modifiers(CTRL, egui::Key::A);
    save(&mut h, "14-selection");
    h.state_mut().push(Cmd::AddNode("std/sine".into()));
    h.run_steps(2);
    let uid = h.state().doc.uid;
    h.state_mut().push(Cmd::CloseTab(uid));
    save(&mut h, "15-close-prompt");
}
