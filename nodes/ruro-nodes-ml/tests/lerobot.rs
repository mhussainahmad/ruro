//! LeRobot dataset writer tests. Reads the result back with the Rust Parquet
//! reader; if RURO_LEROBOT_PYTHON names a Python with `datasets<=3.6`,
//! `pandas` and `pillow`, also validates it with tools/check_lerobot_dataset.py.

#![cfg(feature = "lerobot")]

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use arrow_array::{Array, FixedSizeListArray, Float32Array, Int64Array, StructArray};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use ruro::prelude::*;
use ruro::serde_json::{self, Value};
use ruro_graph::Graph;
use ruro_host::{Executor, Remote};
use ruro_nodes_ml::lerobot::{DatasetWriter, Frame, Spec};

fn temp_root(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("ruro-lerobot-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

fn spec(camera: bool) -> Spec {
    Spec {
        fps: 10,
        robot_type: "so100".into(),
        state_names: vec!["a".into(), "b".into(), "c".into()],
        action_names: vec!["a".into(), "b".into(), "c".into()],
        camera: camera.then(|| ("front".to_owned(), 6, 8)),
    }
}

fn frame(i: usize, camera: bool) -> Frame {
    let v = i as f32;
    Frame {
        state: vec![v, 2.0 * v, -v],
        action: vec![v + 0.5, 0.0, 1.0],
        image: camera.then(|| {
            (0..6 * 8)
                .flat_map(|p| [(p * 5) as u8, i as u8, 200])
                .collect()
        }),
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn python_check(root: &Path) {
    let Ok(python) = std::env::var("RURO_LEROBOT_PYTHON") else {
        eprintln!("RURO_LEROBOT_PYTHON not set; skipping the Python check");
        return;
    };
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/check_lerobot_dataset.py");
    let out = std::process::Command::new(python)
        .arg("-I")
        .arg(script)
        .arg(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    println!("{stdout}");
    assert!(
        out.status.success(),
        "Python check failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn writes_episodes_and_metadata() {
    let root = temp_root("writer");
    let mut w = DatasetWriter::open(&root, spec(true)).unwrap();
    w.begin_episode("pick the cube");
    for i in 0..5 {
        w.add_frame(frame(i, true)).unwrap();
    }
    assert_eq!(w.end_episode().unwrap(), Some(0));
    w.begin_episode("stack the cubes");
    for i in 0..3 {
        w.add_frame(frame(i, true)).unwrap();
    }
    // Wrong sizes are rejected.
    assert!(
        w.add_frame(Frame {
            state: vec![1.0],
            ..frame(0, true)
        })
        .is_err()
    );
    assert!(
        w.add_frame(Frame {
            image: None,
            ..frame(0, true)
        })
        .is_err()
    );
    assert_eq!(w.end_episode().unwrap(), Some(1));
    // Empty episodes are not written.
    w.begin_episode("nothing");
    assert_eq!(w.end_episode().unwrap(), None);
    drop(w);

    // Reopen and append with an existing task.
    let mut w = DatasetWriter::open(&root, spec(true)).unwrap();
    assert_eq!((w.total_episodes(), w.total_frames()), (2, 8));
    w.begin_episode("pick the cube");
    for i in 0..2 {
        w.add_frame(frame(i, true)).unwrap();
    }
    w.end_episode().unwrap();
    // A different feature set cannot append.
    assert!(DatasetWriter::open(&root, spec(false)).is_err());

    let info = read_json(&root.join("meta/info.json"));
    assert_eq!(info["codebase_version"], "v2.1");
    assert_eq!(info["total_episodes"], 3);
    assert_eq!(info["total_frames"], 10);
    assert_eq!(info["total_tasks"], 2);
    assert_eq!(info["splits"]["train"], "0:3");
    assert_eq!(
        info["features"]["observation.images.front"]["shape"],
        serde_json::json!([6, 8, 3])
    );
    assert_eq!(
        info["features"]["observation.state"]["names"],
        serde_json::json!(["a", "b", "c"])
    );
    let tasks = read_jsonl(&root.join("meta/tasks.jsonl"));
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[1]["task"], "stack the cubes");
    let episodes = read_jsonl(&root.join("meta/episodes.jsonl"));
    assert_eq!(episodes[2]["length"], 2);
    assert_eq!(episodes[2]["tasks"], serde_json::json!(["pick the cube"]));
    let stats = read_jsonl(&root.join("meta/episodes_stats.jsonl"));
    // Episode 0 state column 1 is 0, 2, 4, 6, 8: mean 4, population std sqrt(8).
    let s = &stats[0]["stats"];
    assert_eq!(s["observation.state/mean"][1], 4.0);
    assert!((s["observation.state/std"][1].as_f64().unwrap() - 8f64.sqrt()).abs() < 1e-9);
    assert_eq!(s["observation.state/count"], serde_json::json!([5]));
    assert_eq!(s["index/max"], serde_json::json!([4.0]));
    let blue = s["observation.images.front/mean"][2][0][0]
        .as_f64()
        .unwrap();
    assert!((blue - 200.0 / 255.0).abs() < 1e-12);

    // Episode 1 parquet.
    let path = root.join("data/chunk-000/episode_000001.parquet");
    let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&path).unwrap()).unwrap();
    let hf = reader.schema().metadata()["huggingface"].clone();
    assert!(
        hf.contains(r#""observation.images.front":{"_type":"Image"}"#),
        "{hf}"
    );
    let batch = reader.build().unwrap().next().unwrap().unwrap();
    assert_eq!(batch.num_rows(), 3);
    let col = |name: &str| batch.column_by_name(name).unwrap().clone();
    let index = col("index");
    let index = index.as_any().downcast_ref::<Int64Array>().unwrap();
    assert_eq!(index.values(), &[5, 6, 7]);
    let ts = col("timestamp");
    assert_eq!(
        ts.as_any().downcast_ref::<Float32Array>().unwrap().values(),
        &[0.0, 0.1, 0.2]
    );
    let task = col("task_index");
    assert_eq!(
        task.as_any().downcast_ref::<Int64Array>().unwrap().values(),
        &[1, 1, 1]
    );
    let state = col("observation.state");
    let state = state.as_any().downcast_ref::<FixedSizeListArray>().unwrap();
    assert_eq!(state.value_length(), 3);
    let row2 = state.value(2);
    assert_eq!(
        row2.as_any()
            .downcast_ref::<Float32Array>()
            .unwrap()
            .values(),
        &[2.0, 4.0, -2.0]
    );
    let img = col("observation.images.front");
    let img = img.as_any().downcast_ref::<StructArray>().unwrap();
    let bytes = img
        .column(0)
        .as_any()
        .downcast_ref::<arrow_array::BinaryArray>()
        .unwrap();
    assert!(img.column(1).is_null(0));
    // The PNG decodes back to the original pixels.
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes.value(1).to_vec()));
    let mut r = decoder.read_info().unwrap();
    let mut px = vec![0u8; r.output_buffer_size().unwrap()];
    let info = r.next_frame(&mut px).unwrap();
    assert_eq!((info.width, info.height), (8, 6));
    assert_eq!(
        &px[..info.buffer_size()],
        frame(1, true).image.unwrap().as_slice()
    );

    python_check(&root);
    std::fs::remove_dir_all(&root).unwrap();
}

#[derive(Node)]
#[node(kind = "test/teleop", rate = 100)]
struct Teleop {
    #[output]
    state: Output<JointState>,
    #[output]
    action: Output<JointCommand>,
    #[output]
    image: Output<Image>,
}

impl Node for Teleop {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        let t = ctx.steps() as f64 * 0.01;
        self.state.send(JointState {
            position: vec![t, -t],
            ..Default::default()
        })?;
        self.action.send(JointCommand {
            position: vec![t + 0.1, -t - 0.1],
            ..Default::default()
        })?;
        let mut img = Image::new(4, 4, Encoding::Rgba8);
        img.data.fill(128);
        self.image.send(img)
    }
}

#[test]
fn recorder_node_saves_on_stop() {
    let root = temp_root("node");
    let graph = Graph::from_yaml(&format!(
        r#"
name: rec
nodes:
  robot: {{ type: test/teleop }}
  rec:
    type: ml/lerobot-recorder
    rate: 30
    params: {{ root: "{}", fps: 30, task: "wave", auto_start: true, joint_names: "shoulder,elbow", episode_seconds: 0.2 }}
wires:
  - robot.state -> rec.state
  - robot.action -> rec.action
  - robot.image -> rec.image
"#,
        root.display()
    ))
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    std::thread::sleep(Duration::from_millis(600));
    exec.stop();
    let info = read_json(&root.join("meta/info.json"));
    // 0.2 s episodes at 30 fps: 6 frames each, the last one possibly shorter.
    let episodes = read_jsonl(&root.join("meta/episodes.jsonl"));
    assert!(episodes.len() >= 2, "{episodes:?}");
    assert_eq!(episodes[0]["length"], 6);
    assert_eq!(
        info["features"]["action"]["names"],
        serde_json::json!(["shoulder", "elbow"])
    );
    assert_eq!(
        info["features"]["observation.images.front"]["shape"],
        serde_json::json!([4, 4, 3])
    );
    python_check(&root);
    std::fs::remove_dir_all(&root).unwrap();
}
