use std::path::{Path, PathBuf};

use serde_json::json;

use super::*;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launch")
}

fn opts() -> ImportOptions {
    ImportOptions {
        ament_prefix_path: Some(fixtures().join("prefix").display().to_string()),
        params_files: vec![fixtures().join("params.yaml")],
        ..Default::default()
    }
}

/// The `ros2/node` parameters of a node, read back.
fn process(g: &Graph, id: &str) -> RosProcess {
    let n = g.nodes.get(id).unwrap_or_else(|| {
        panic!(
            "no node {id}; nodes: {:?}",
            g.nodes.keys().collect::<Vec<_>>()
        )
    });
    assert_eq!(n.kind, "ros2/node");
    assert_eq!(n.group.as_deref(), Some("ros2"));
    let s = |k: &str| n.params.get(k).and_then(Value::as_str).unwrap_or("");
    RosProcess::from_params(
        s("package"),
        s("executable"),
        s("command"),
        s("name"),
        s("namespace"),
        s("remaps"),
        s("parameters"),
        s("params_files"),
        s("arguments"),
        s("ros_args"),
        s("env"),
        s("prefix"),
    )
    .unwrap()
}

#[test]
fn xml_and_yaml_launch_files_become_ros2_nodes() {
    let imported = import(&fixtures().join("robot.launch.xml"), &opts()).unwrap();
    let g = &imported.graph;
    assert_eq!(g.name, "robot");
    let ids: Vec<&str> = g.nodes.keys().map(String::as_str).collect();
    assert_eq!(
        ids,
        [
            "r1_talker",
            "sensors_lidar",
            "sensors_sim_filter",
            "front_r2_driver",
            "listener",
            "vision_container",
            "load_rectify",
            "recorder",
            "late_listener",
            "tool"
        ]
    );

    let talker = process(g, "r1_talker");
    assert_eq!(
        (talker.package.as_str(), talker.executable.as_str()),
        ("demo_nodes_cpp", "talker")
    );
    assert_eq!(
        (talker.name.as_str(), talker.namespace.as_str()),
        ("talker", "/r1")
    );
    let params: serde_json::Map<String, Value> = talker.parameters.iter().cloned().collect();
    assert_eq!(
        Value::Object(params),
        json!({
            "use_sim_time": false, "rate": 5.0, "frame": "base_r1", "label": "42",
            "pid.kp": 1.5, "pid.gains": [1, 2, 3]
        })
    );
    let share = fixtures().join("prefix/share/robot_config");
    assert_eq!(
        talker.params_files,
        [
            fixtures().join("params.yaml").display().to_string(),
            share.join("config/talker.yaml").display().to_string()
        ]
    );
    assert_eq!(talker.remaps, [("chatter".into(), "/r1/out".into())]);
    assert_eq!(
        talker.env[0],
        ("RCUTILS_COLORIZED_OUTPUT".into(), "0".into())
    );
    assert_eq!(talker.env[1].0, "EXTRA");
    assert_eq!(g.nodes["r1_talker"].params["respawn"], "always");
    assert_eq!(g.nodes["r1_talker"].params["respawn_delay"], 2.0);

    let lidar = process(g, "sensors_lidar");
    assert_eq!(lidar.namespace, "/sensors");
    assert_eq!(lidar.remaps, [("scan".into(), "scan_filtered".into())]);
    assert_eq!(
        lidar.arguments,
        ["--port", "/dev/ttyUSB0", "--baud", "115200"]
    );
    assert_eq!(g.nodes["sensors_lidar"].params["respawn"], "never");

    // The include saw `robot` changed by the unscoped group.
    let cam = process(g, "front_r2_driver");
    assert_eq!(cam.namespace, "/front_r2");
    let params: serde_json::Map<String, Value> = cam.parameters.iter().cloned().collect();
    assert_eq!(params["framerate"], 30);
    assert_eq!(params["size"], json!([640, 480]));
    assert_eq!(
        cam.params_files[1],
        fixtures().join("camera_params.yaml").display().to_string()
    );
    assert_eq!(cam.remaps, [("image_raw".into(), "image".into())]);

    let container = process(g, "vision_container");
    assert_eq!(container.label(), "/vision/container");
    let load = process(g, "load_rectify");
    assert_eq!(
        load.command,
        [
            "ros2",
            "component",
            "load",
            "--node-name",
            "rectify",
            "-p",
            "use_sim_time:=false",
            "-p",
            "queue_size:=5",
            "-r",
            "image:=/camera/image_raw",
            "-e",
            "use_intra_process_comms:=true",
            "/vision/container",
            "image_proc",
            "image_proc::RectifyNode"
        ]
    );
    assert_eq!(g.nodes["load_rectify"].params["respawn"], "on-failure");
    assert_eq!(
        process(g, "recorder").command,
        ["rosbag2_record_helper", "--all"]
    );

    let warnings = imported.warnings.join("\n");
    for expected in [
        "<timer period=\"3.0\">",
        "<log> is not supported",
        "$(command date)",
        "composable node image_proc::RectifyNode",
    ] {
        assert!(
            warnings.contains(expected),
            "missing `{expected}` in:\n{warnings}"
        );
    }
    assert_eq!(imported.warnings.len(), 4, "{warnings}");

    // The rendered graph carries the warnings and loads back.
    let text = render(&imported, Path::new("robot.launch.xml")).unwrap();
    assert!(text.starts_with("# Imported from robot.launch.xml"));
    assert!(text.contains("# - "));
    assert_eq!(&Graph::from_yaml(&text).unwrap(), g);
}

#[test]
fn launch_arguments_and_unresolvable_substitutions() {
    let mut o = opts();
    o.args = vec![
        ("use_sim".into(), "true".into()),
        ("robot".into(), "bot".into()),
    ];
    o.ament_prefix_path = Some(String::new());
    let imported = import(&fixtures().join("robot.launch.xml"), &o).unwrap();
    let g = &imported.graph;
    assert!(g.nodes.contains_key("bot_talker"));
    assert!(g.nodes.contains_key("sensors_filter"));
    assert!(!g.nodes.contains_key("sensors_sim_filter"));
    // Without the package, the path is left for ros2/node to resolve.
    let talker = process(g, "bot_talker");
    assert_eq!(
        talker.params_files[1],
        "$(find-pkg-share robot_config)/config/talker.yaml"
    );
    let warnings = imported.warnings.join("\n");
    assert!(
        warnings.contains("package `robot_config` was not found"),
        "{warnings}"
    );
}

#[test]
fn bridges_python_files_and_broken_input() {
    let mut o = opts();
    o.subscribe = vec![("/odom".into(), "nav_msgs/msg/Odometry".into())];
    o.publish = vec![("/cmd_vel".into(), "geometry_msgs/msg/Twist".into())];
    let dir = std::env::temp_dir().join(format!("ruro-launch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("small.launch.yaml");
    std::fs::write(
        &file,
        "launch:\n- node:\n    pkg: p\n    exec: e\n    name: n\n    bogus: 1\n",
    )
    .unwrap();
    let imported = import(&file, &o).unwrap();
    let g = &imported.graph;
    assert_eq!(g.nodes["sub_odom"].kind, "ros2/sub");
    assert_eq!(g.nodes["sub_odom"].params["type"], "nav_msgs/msg/Odometry");
    assert_eq!(g.nodes["pub_cmd_vel"].kind, "ros2/pub");
    assert!(
        imported.warnings[0].contains("attribute `bogus`"),
        "{:?}",
        imported.warnings
    );

    let py = dir.join("robot.launch.py");
    std::fs::write(&py, "def generate_launch_description(): pass\n").unwrap();
    let err = format!("{:#}", import(&py, &o).unwrap_err());
    assert!(err.contains("Python launch file"), "{err}");
    assert!(err.contains("--eval"), "{err}");

    std::fs::write(dir.join("bad.launch.xml"), "<robot/>").unwrap();
    assert!(import(&dir.join("bad.launch.xml"), &o).is_err());
    std::fs::write(
        dir.join("nonode.launch.xml"),
        "<launch><node pkg=\"p\"/></launch>",
    )
    .unwrap();
    assert!(import(&dir.join("nonode.launch.xml"), &o).is_err());
}

#[test]
fn namespaces_join_like_ros() {
    assert_eq!(join_ns("", ""), "/");
    assert_eq!(join_ns("/", "a"), "/a");
    assert_eq!(join_ns("/a", "b/"), "/a/b");
    assert_eq!(join_ns("/a/", ""), "/a");
    assert_eq!(param_value("1"), json!(1));
    assert_eq!(param_value("1.0"), json!(1.0));
    assert_eq!(param_value("true"), json!(true));
    assert_eq!(param_value("[a, b]"), json!(["a", "b"]));
    assert_eq!(param_value("hello world"), json!("hello world"));
    assert_eq!(param_value(""), json!(""));
}

/// Imports `xml` (the inside of `<launch>`) from a file of its own.
fn import_xml(name: &str, xml: &str) -> Imported {
    let dir = std::env::temp_dir().join(format!("ruro-launch-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.launch.xml"));
    std::fs::write(&file, format!("<launch>{xml}</launch>")).unwrap();
    let mut o = opts();
    o.ament_prefix_path = Some(String::new());
    o.params_files.clear();
    import(&file, &o).unwrap()
}

#[test]
fn long_names_that_share_a_prefix_stay_apart() {
    let long = "a".repeat(70);
    let imported = import_xml(
        "long",
        &format!(
            r#"<node pkg="p" exec="e" name="{long}_left"/><node pkg="p" exec="e" name="{long}_right"/>"#
        ),
    );
    let g = &imported.graph;
    assert_eq!(g.nodes.len(), 2, "{:?}", g.nodes.keys().collect::<Vec<_>>());
    let names: Vec<String> = g.nodes.keys().map(|id| process(g, id).name).collect();
    assert!(names.contains(&format!("{long}_left")), "{names:?}");
    assert!(names.contains(&format!("{long}_right")), "{names:?}");
}

#[test]
fn env_substitutions_see_set_env() {
    let imported = import_xml(
        "setenv",
        r#"<set_env name="RURO_TEST_LAUNCH_VAR" value="x"/>
           <node pkg="p" exec="e" name="n" args="$(env RURO_TEST_LAUNCH_VAR) $(optenv RURO_TEST_LAUNCH_VAR none)"/>"#,
    );
    assert_eq!(process(&imported.graph, "n").arguments, ["x", "x"]);
}

#[test]
fn unresolved_substitutions_stay_one_word() {
    let imported = import_xml(
        "words",
        r#"<arg name="greeting" default="hello world"/>
           <node pkg="p" exec="e" name="n" args="--config $(find-pkg-share cfgpkg)/x.yaml '$(var greeting)'"/>"#,
    );
    assert_eq!(
        process(&imported.graph, "n").arguments,
        ["--config", "$(find-pkg-share cfgpkg)/x.yaml", "hello world"]
    );
}

#[test]
fn composable_nodes_obey_conditions_and_remap_like_nodes() {
    let imported = import_xml(
        "composable",
        r#"<set_remap from="image" to="/global"/>
           <node_container pkg="rclcpp_components" exec="component_container" name="c" namespace="/">
             <composable_node pkg="p" plugin="p::Keep" name="keep">
               <remap from="image" to="/local"/>
             </composable_node>
             <composable_node pkg="p" plugin="p::Skip" name="skipme" if="false"/>
           </node_container>"#,
    );
    let g = &imported.graph;
    assert!(
        !g.nodes.keys().any(|k| k.contains("skipme")),
        "{:?}",
        g.nodes.keys().collect::<Vec<_>>()
    );
    let load = g
        .nodes
        .keys()
        .find(|k| k.contains("keep"))
        .expect("keep loaded");
    let args = process(g, load).command.join(" ");
    let (global, local) = (
        args.find("image:=/global").expect(&args),
        args.find("image:=/local").expect(&args),
    );
    assert!(
        global < local,
        "scope remaps come first, as for nodes: {args}"
    );
}
