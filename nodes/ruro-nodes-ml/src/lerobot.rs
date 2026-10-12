//! LeRobot dataset recording (feature `lerobot`), format version v2.1:
//!
//! ```text
//! <root>/meta/info.json
//! <root>/meta/episodes.jsonl          {"episode_index", "tasks", "length"}
//! <root>/meta/tasks.jsonl             {"task_index", "task"}
//! <root>/meta/episodes_stats.jsonl    {"episode_index", "stats": {"<feature>/<min|max|mean|std|count>": ...}}
//! <root>/data/chunk-000/episode_000000.parquet
//! ```
//!
//! Parquet columns: `observation.state` and `action` (fixed-size float32
//! lists), `timestamp` (float32), `frame_index`, `episode_index`, `index`,
//! `task_index` (int64) and, when images are recorded,
//! `observation.images.<camera>` as `struct<bytes: binary, path: string>`
//! holding PNG frames (the Hugging Face `Image` feature). Camera streams are
//! stored as PNG frames inside the Parquet files rather than MP4 videos, so
//! `video_path` is null; LeRobot loads such datasets directly.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::builder::{BinaryBuilder, StringBuilder};
use arrow_array::{
    ArrayRef, FixedSizeListArray, Float32Array, Int64Array, RecordBatch, StructArray,
};
use arrow_schema::{DataType, Field, Fields, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use ruro::prelude::*;
use ruro::serde_json::{self, Value, json};

pub const CODEBASE_VERSION: &str = "v2.1";
pub const CHUNK_SIZE: usize = 1000;
pub const DATA_PATH: &str = "data/chunk-{episode_chunk:03d}/episode_{episode_index:06d}.parquet";

/// One recorded time step.
#[derive(Debug, Clone)]
pub struct Frame {
    pub state: Vec<f32>,
    pub action: Vec<f32>,
    /// RGB8 pixels (`height * width * 3`), if the dataset has a camera.
    pub image: Option<Vec<u8>>,
}

/// Static description of the dataset's features.
#[derive(Debug, Clone)]
pub struct Spec {
    pub fps: u32,
    pub robot_type: String,
    pub state_names: Vec<String>,
    pub action_names: Vec<String>,
    /// Camera name and `(height, width)`.
    pub camera: Option<(String, u32, u32)>,
}

impl Spec {
    fn image_key(&self) -> Option<String> {
        self.camera
            .as_ref()
            .map(|(c, _, _)| format!("observation.images.{c}"))
    }

    /// The `features` object of info.json.
    pub fn features(&self) -> Value {
        let vector =
            |names: &[String]| json!({"dtype": "float32", "shape": [names.len()], "names": names});
        let scalar = |dtype: &str| json!({"dtype": dtype, "shape": [1], "names": null});
        let mut f = serde_json::Map::new();
        f.insert("action".into(), vector(&self.action_names));
        f.insert("observation.state".into(), vector(&self.state_names));
        if let (Some(key), Some((_, h, w))) = (self.image_key(), &self.camera) {
            f.insert(
                key,
                json!({"dtype": "image", "shape": [h, w, 3], "names": ["height", "width", "channels"]}),
            );
        }
        f.insert("timestamp".into(), scalar("float32"));
        f.insert("frame_index".into(), scalar("int64"));
        f.insert("episode_index".into(), scalar("int64"));
        f.insert("index".into(), scalar("int64"));
        f.insert("task_index".into(), scalar("int64"));
        Value::Object(f)
    }

    fn schema(&self) -> Schema {
        let list = |n: usize| {
            DataType::FixedSizeList(
                Arc::new(Field::new("element", DataType::Float32, true)),
                n as i32,
            )
        };
        let mut fields = vec![
            Field::new("action", list(self.action_names.len()), true),
            Field::new("observation.state", list(self.state_names.len()), true),
        ];
        if let Some(key) = self.image_key() {
            fields.push(Field::new(key, image_type(), true));
        }
        fields.extend([
            Field::new("timestamp", DataType::Float32, true),
            Field::new("frame_index", DataType::Int64, true),
            Field::new("episode_index", DataType::Int64, true),
            Field::new("index", DataType::Int64, true),
            Field::new("task_index", DataType::Int64, true),
        ]);
        // Hugging Face `datasets` reads its feature types from this metadata.
        let seq = |n: usize| json!({"feature": {"dtype": "float32", "_type": "Value"}, "length": n, "_type": "Sequence"});
        let val = |d: &str| json!({"dtype": d, "_type": "Value"});
        let mut hf = serde_json::Map::new();
        hf.insert("action".into(), seq(self.action_names.len()));
        hf.insert("observation.state".into(), seq(self.state_names.len()));
        if let Some(key) = self.image_key() {
            hf.insert(key, json!({"_type": "Image"}));
        }
        hf.insert("timestamp".into(), val("float32"));
        for k in ["frame_index", "episode_index", "index", "task_index"] {
            hf.insert(k.into(), val("int64"));
        }
        let meta = json!({"info": {"features": hf}}).to_string();
        Schema::new(fields).with_metadata([("huggingface".to_owned(), meta)].into())
    }
}

fn image_type() -> DataType {
    DataType::Struct(Fields::from(vec![
        Field::new("bytes", DataType::Binary, true),
        Field::new("path", DataType::Utf8, true),
    ]))
}

/// Per-feature statistics as LeRobot computes them (population std).
fn vector_stats(rows: &[&[f32]]) -> Value {
    let n = rows.len();
    let d = rows.first().map_or(0, |r| r.len());
    let mut min = vec![f64::INFINITY; d];
    let mut max = vec![f64::NEG_INFINITY; d];
    let mut sum = vec![0f64; d];
    for r in rows {
        for (j, &v) in r.iter().enumerate() {
            let v = v as f64;
            min[j] = min[j].min(v);
            max[j] = max[j].max(v);
            sum[j] += v;
        }
    }
    let mean: Vec<f64> = sum.iter().map(|s| s / n as f64).collect();
    let mut var = vec![0f64; d];
    for r in rows {
        for (j, &v) in r.iter().enumerate() {
            var[j] += (v as f64 - mean[j]).powi(2);
        }
    }
    let std: Vec<f64> = var.iter().map(|v| (v / n as f64).sqrt()).collect();
    json!({"min": min, "max": max, "mean": mean, "std": std, "count": [n]})
}

/// Per-channel image statistics in 0..1, shaped `[3, 1, 1]`.
fn image_stats(images: &[&[u8]]) -> Value {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    let (mut sum, mut sq, mut count) = ([0f64; 3], [0f64; 3], 0f64);
    for img in images {
        for px in img.as_chunks::<3>().0 {
            for c in 0..3 {
                let v = px[c] as f64 / 255.0;
                min[c] = min[c].min(v);
                max[c] = max[c].max(v);
                sum[c] += v;
                sq[c] += v * v;
            }
            count += 1.0;
        }
    }
    let wrap = |a: [f64; 3]| json!([[[a[0]]], [[a[1]]], [[a[2]]]]);
    let mean = sum.map(|s| s / count);
    let std: [f64; 3] =
        std::array::from_fn(|c| (sq[c] / count - mean[c] * mean[c]).max(0.0).sqrt());
    json!({"min": wrap(min), "max": wrap(max), "mean": wrap(mean), "std": wrap(std), "count": [images.len()]})
}

fn flatten_stats(stats: &serde_json::Map<String, Value>) -> Value {
    let mut flat = serde_json::Map::new();
    for (feature, s) in stats {
        if let Value::Object(m) = s {
            for (k, v) in m {
                flat.insert(format!("{feature}/{k}"), v.clone());
            }
        }
    }
    Value::Object(flat)
}

fn encode_png(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut w = enc.write_header()?;
    w.write_image_data(rgb)?;
    w.finish()?;
    Ok(out)
}

fn append_jsonl(path: &Path, v: &Value) -> Result<()> {
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{v}")?;
    Ok(())
}

/// Writes a LeRobot v2.1 dataset, one episode at a time.
pub struct DatasetWriter {
    pub root: PathBuf,
    pub spec: Spec,
    info: Value,
    tasks: Vec<String>,
    frames: Vec<Frame>,
    task: String,
}

impl DatasetWriter {
    /// Creates a dataset at `root`, or appends to the one already there
    /// (its features must match `spec`).
    pub fn open(root: impl Into<PathBuf>, spec: Spec) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("meta"))?;
        let info_path = root.join("meta/info.json");
        let (info, tasks) = if info_path.exists() {
            let info: Value = serde_json::from_str(&fs::read_to_string(&info_path)?)?;
            ensure!(
                info["codebase_version"] == CODEBASE_VERSION,
                "{} is a {} dataset, expected {CODEBASE_VERSION}",
                root.display(),
                info["codebase_version"]
            );
            ensure!(
                info["features"] == spec.features(),
                "{} has different features than this recorder",
                root.display()
            );
            let mut tasks = Vec::new();
            if let Ok(text) = fs::read_to_string(root.join("meta/tasks.jsonl")) {
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    let v: Value = serde_json::from_str(line)?;
                    tasks.push(v["task"].as_str().unwrap_or_default().to_owned());
                }
            }
            (info, tasks)
        } else {
            let info = json!({
                "codebase_version": CODEBASE_VERSION,
                "robot_type": spec.robot_type,
                "total_episodes": 0,
                "total_frames": 0,
                "total_tasks": 0,
                "total_videos": 0,
                "total_chunks": 0,
                "chunks_size": CHUNK_SIZE,
                "fps": spec.fps,
                "splits": {},
                "data_path": DATA_PATH,
                "video_path": null,
                "features": spec.features(),
            });
            fs::write(&info_path, serde_json::to_string_pretty(&info)?)?;
            (info, Vec::new())
        };
        Ok(Self {
            root,
            spec,
            info,
            tasks,
            frames: Vec::new(),
            task: String::new(),
        })
    }

    pub fn total_episodes(&self) -> usize {
        self.info["total_episodes"].as_u64().unwrap_or(0) as usize
    }

    pub fn total_frames(&self) -> usize {
        self.info["total_frames"].as_u64().unwrap_or(0) as usize
    }

    /// Starts buffering a new episode for `task`.
    pub fn begin_episode(&mut self, task: &str) {
        self.frames.clear();
        self.task = task.to_owned();
    }

    pub fn is_recording(&self) -> bool {
        !self.task.is_empty()
    }

    pub fn frames_in_episode(&self) -> usize {
        self.frames.len()
    }

    pub fn add_frame(&mut self, frame: Frame) -> Result<()> {
        ensure!(self.is_recording(), "no episode in progress");
        ensure!(
            frame.state.len() == self.spec.state_names.len(),
            "state has {} values, dataset expects {}",
            frame.state.len(),
            self.spec.state_names.len()
        );
        ensure!(
            frame.action.len() == self.spec.action_names.len(),
            "action has {} values, dataset expects {}",
            frame.action.len(),
            self.spec.action_names.len()
        );
        if let Some((_, h, w)) = &self.spec.camera {
            let img = frame
                .image
                .as_ref()
                .context("dataset has a camera but the frame has no image")?;
            ensure!(
                img.len() == (*h * *w * 3) as usize,
                "image is not {w}x{h} RGB"
            );
        }
        self.frames.push(frame);
        Ok(())
    }

    /// Discards the episode in progress.
    pub fn discard_episode(&mut self) {
        self.frames.clear();
        self.task.clear();
    }

    /// Writes the buffered episode; returns its index, or `None` if it was empty.
    pub fn end_episode(&mut self) -> Result<Option<usize>> {
        let task = std::mem::take(&mut self.task);
        let frames = std::mem::take(&mut self.frames);
        if frames.is_empty() {
            return Ok(None);
        }
        let episode = self.total_episodes();
        let first_index = self.total_frames();
        let task_index = match self.tasks.iter().position(|t| *t == task) {
            Some(i) => i,
            None => {
                self.tasks.push(task.clone());
                append_jsonl(
                    &self.root.join("meta/tasks.jsonl"),
                    &json!({"task_index": self.tasks.len() - 1, "task": task}),
                )?;
                self.tasks.len() - 1
            }
        };
        let n = frames.len();
        let chunk = episode / CHUNK_SIZE;
        let rel = format!("data/chunk-{chunk:03}/episode_{episode:06}.parquet");
        let path = self.root.join(&rel);
        fs::create_dir_all(path.parent().unwrap())?;

        // Columns.
        let list = |rows: Vec<f32>, d: usize| -> Result<ArrayRef> {
            let field = Arc::new(Field::new("element", DataType::Float32, true));
            Ok(Arc::new(FixedSizeListArray::try_new(
                field,
                d as i32,
                Arc::new(Float32Array::from(rows)),
                None,
            )?))
        };
        let (ds, da) = (self.spec.state_names.len(), self.spec.action_names.len());
        let mut columns: Vec<ArrayRef> = vec![
            list(
                frames
                    .iter()
                    .flat_map(|f| f.action.iter().copied())
                    .collect(),
                da,
            )?,
            list(
                frames
                    .iter()
                    .flat_map(|f| f.state.iter().copied())
                    .collect(),
                ds,
            )?,
        ];
        if let Some((_, h, w)) = &self.spec.camera {
            let mut bytes = BinaryBuilder::new();
            let mut paths = StringBuilder::new();
            for f in &frames {
                bytes.append_value(encode_png(f.image.as_ref().unwrap(), *w, *h)?);
                paths.append_null();
            }
            let DataType::Struct(fields) = image_type() else {
                unreachable!()
            };
            columns.push(Arc::new(StructArray::try_new(
                fields,
                vec![Arc::new(bytes.finish()), Arc::new(paths.finish())],
                None,
            )?));
        }
        let fps = self.spec.fps as f32;
        columns.push(Arc::new(Float32Array::from_iter_values(
            (0..n).map(|i| i as f32 / fps),
        )));
        columns.push(Arc::new(Int64Array::from_iter_values(0..n as i64)));
        columns.push(Arc::new(Int64Array::from_iter_values(std::iter::repeat_n(
            episode as i64,
            n,
        ))));
        columns.push(Arc::new(Int64Array::from_iter_values(
            (first_index..first_index + n).map(|i| i as i64),
        )));
        columns.push(Arc::new(Int64Array::from_iter_values(std::iter::repeat_n(
            task_index as i64,
            n,
        ))));
        let schema = Arc::new(self.spec.schema());
        let batch = RecordBatch::try_new(schema.clone(), columns)?;
        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();
        let tmp = path.with_extension("parquet.partial");
        let mut w = ArrowWriter::try_new(File::create(&tmp)?, schema, Some(props))?;
        w.write(&batch)?;
        w.close()?;
        fs::rename(&tmp, &path)?;

        // Statistics, as LeRobot's compute_episode_stats.
        let mut stats = serde_json::Map::new();
        let states: Vec<&[f32]> = frames.iter().map(|f| f.state.as_slice()).collect();
        let actions: Vec<&[f32]> = frames.iter().map(|f| f.action.as_slice()).collect();
        stats.insert("action".into(), vector_stats(&actions));
        stats.insert("observation.state".into(), vector_stats(&states));
        if let Some(key) = self.spec.image_key() {
            let imgs: Vec<&[u8]> = frames.iter().map(|f| f.image.as_deref().unwrap()).collect();
            stats.insert(key, image_stats(&imgs));
        }
        let scalars = |v: Vec<f32>| -> Value {
            let rows: Vec<[f32; 1]> = v.into_iter().map(|x| [x]).collect();
            vector_stats(&rows.iter().map(|r| r.as_slice()).collect::<Vec<_>>())
        };
        stats.insert(
            "timestamp".into(),
            scalars((0..n).map(|i| i as f32 / fps).collect()),
        );
        stats.insert(
            "frame_index".into(),
            scalars((0..n).map(|i| i as f32).collect()),
        );
        stats.insert("episode_index".into(), scalars(vec![episode as f32; n]));
        stats.insert(
            "index".into(),
            scalars((first_index..first_index + n).map(|i| i as f32).collect()),
        );
        stats.insert("task_index".into(), scalars(vec![task_index as f32; n]));

        append_jsonl(
            &self.root.join("meta/episodes.jsonl"),
            &json!({"episode_index": episode, "tasks": [task], "length": n}),
        )?;
        append_jsonl(
            &self.root.join("meta/episodes_stats.jsonl"),
            &json!({"episode_index": episode, "stats": flatten_stats(&stats)}),
        )?;
        let total = episode + 1;
        self.info["total_episodes"] = json!(total);
        self.info["total_frames"] = json!(first_index + n);
        self.info["total_tasks"] = json!(self.tasks.len());
        self.info["total_chunks"] = json!(chunk + 1);
        self.info["splits"] = json!({"train": format!("0:{total}")});
        fs::write(
            self.root.join("meta/info.json"),
            serde_json::to_string_pretty(&self.info)?,
        )?;
        Ok(Some(episode))
    }
}

fn names(spec: &str, prefix: &str, n: usize) -> Vec<String> {
    let given: Vec<String> = spec
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    if given.len() == n {
        given
    } else {
        (0..n).map(|i| format!("{prefix}{i}")).collect()
    }
}

/// Records teleoperation or policy rollouts as a LeRobot v2.1 dataset.
/// Each step adds one frame with the latest joint state, action and camera
/// image. Episodes start when `recording` turns true and are written to disk
/// when it turns false (or when `episode_seconds` elapse). Run the node at
/// the dataset frame rate.
#[derive(Node)]
#[node(kind = "ml/lerobot-recorder", rate = 30)]
pub struct LeRobotRecorder {
    /// Robot state (`observation.state`): joint positions.
    #[input]
    state: Input<JointState>,
    /// Commanded action (`action`): joint position targets.
    #[input]
    action: Input<JointCommand>,
    /// Camera frame (`observation.images.<camera>`), stored as PNG.
    #[input]
    image: Input<Image>,
    /// Start (true) and stop (false) an episode.
    #[input]
    recording: Input<Flag>,
    /// Dataset directory.
    #[param(default = "datasets/ruro-recording")]
    root: String,
    /// Frame rate written to the dataset; set the node rate to match.
    #[param(default = 30, min = 1, max = 1000)]
    fps: u32,
    /// Natural-language task description stored with each episode.
    #[param(default = "")]
    task: String,
    #[param(default = "so100")]
    robot_type: String,
    /// Camera name; empty records no images.
    #[param(default = "front")]
    camera: String,
    /// Comma separated joint names (default: joint0, joint1, ...).
    #[param(default = "")]
    joint_names: String,
    /// Start recording immediately instead of waiting for `recording`.
    #[param(default = false)]
    auto_start: bool,
    /// Split episodes after this many seconds (0 = no limit).
    #[param(default = 0.0, min = 0, max = 86400)]
    episode_seconds: f64,
    writer: Option<DatasetWriter>,
    want: bool,
    last_state: Option<Vec<f32>>,
    last_action: Option<Vec<f32>>,
    last_image: Option<Arc<ruro::Sample<Image>>>,
}

impl LeRobotRecorder {
    fn open(&mut self, n: usize, image: Option<&Image>) -> Result<()> {
        let joints = names(&self.joint_names, "joint", n);
        let camera = match (self.camera.is_empty(), image) {
            (false, Some(img)) => Some((self.camera.clone(), img.height, img.width)),
            (false, None) => bail!("waiting for the first image"),
            (true, _) => None,
        };
        let spec = Spec {
            fps: self.fps,
            robot_type: self.robot_type.clone(),
            state_names: joints.clone(),
            action_names: joints,
            camera,
        };
        let w = DatasetWriter::open(&self.root, spec)?;
        info!(
            "LeRobot dataset {}: {} episodes, {} frames so far",
            self.root,
            w.total_episodes(),
            w.total_frames()
        );
        self.writer = Some(w);
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if let Some(w) = self.writer.as_mut().filter(|w| w.is_recording())
            && let Some(ep) = w.end_episode()?
        {
            info!("saved episode {ep} to {}", self.root);
        }
        Ok(())
    }
}

impl Node for LeRobotRecorder {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        self.want = self.auto_start;
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(f) = self.recording.recv() {
            self.want = f.0;
        }
        if let Some(s) = self.state.latest() {
            self.last_state = Some(s.position.iter().map(|&v| v as f32).collect());
        }
        if let Some(a) = self.action.latest() {
            self.last_action = Some(a.position.iter().map(|&v| v as f32).collect());
        }
        if let Some(i) = self.image.latest() {
            self.last_image = Some(i);
        }
        if !self.want {
            return self.finish();
        }
        let (Some(state), Some(action)) = (self.last_state.clone(), self.last_action.clone())
        else {
            return Ok(());
        };
        let use_camera = !self.camera.is_empty();
        if use_camera && self.last_image.is_none() {
            return Ok(());
        }
        if self.writer.is_none() {
            let img = self.last_image.clone();
            self.open(state.len(), img.as_deref().map(|s| &s.msg))?;
        }
        let max_frames = (self.episode_seconds * self.fps as f64) as usize;
        let w = self.writer.as_mut().unwrap();
        if w.is_recording() && max_frames > 0 && w.frames_in_episode() >= max_frames {
            self.finish()?;
        }
        let w = self.writer.as_mut().unwrap();
        if !w.is_recording() {
            let task = if self.task.is_empty() {
                "record"
            } else {
                &self.task
            };
            w.begin_episode(task);
        }
        let image = if use_camera {
            let img = &self.last_image.as_ref().unwrap().msg;
            let rgb = match img.encoding {
                Encoding::Rgb8 => img.data.clone(),
                Encoding::Rgba8 => img
                    .data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[0], p[1], p[2]])
                    .collect(),
                Encoding::Mono8 => img.data.iter().flat_map(|&v| [v, v, v]).collect(),
            };
            Some(rgb)
        } else {
            None
        };
        w.add_frame(Frame {
            state,
            action,
            image,
        })
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        if let Err(e) = self.finish() {
            error!("could not save the last episode: {e:#}");
        }
    }
}
