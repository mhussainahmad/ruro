//! Machine learning and perception nodes (`ml/*`).
//!
//! Always available: the `ml/Tensor` and `ml/Detections` messages, image
//! preprocessing, YOLO decoding with non-maximum suppression, and the
//! `ml/yolo-decode` node. Behind cargo features:
//!
//! - `onnx`: `ml/onnx` (generic image model) and `ml/policy` (robot policy,
//!   e.g. ACT or diffusion exported to ONNX) on ONNX Runtime (CPU).
//! - `lerobot`: `ml/lerobot-recorder`, writing LeRobot v2.1 datasets.

#[cfg(feature = "lerobot")]
pub mod lerobot;
#[cfg(feature = "onnx")]
pub mod onnx;
pub mod preprocess;
pub mod yolo;

use ruro::msg::Preview;
use ruro::prelude::*;

/// A dense float tensor, row-major.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Tensor {
    pub shape: Vec<u32>,
    pub data: Vec<f32>,
}

ruro::msg::impl_postcard_message!(Tensor, "ml/Tensor", preview: |t: &Tensor| {
    let n = t.data.len();
    let (min, max) = t.data.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), &v| (a.min(v), b.max(v)));
    Preview::Text(format!("tensor {:?}, {n} values in [{min}, {max}]", t.shape))
});

impl Tensor {
    pub fn new(shape: Vec<u32>, data: Vec<f32>) -> Self {
        debug_assert_eq!(shape.iter().product::<u32>() as usize, data.len());
        Self { shape, data }
    }

    pub fn shape_usize(&self) -> Vec<usize> {
        self.shape.iter().map(|&d| d as usize).collect()
    }
}

/// One detected object. The box is in pixels of the source image, with
/// `(x, y)` its top-left corner.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Detection {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
    pub class_id: u32,
    pub label: String,
}

/// Detected objects in one image.
#[derive(Message, Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[message(name = "ml/Detections")]
pub struct Detections {
    pub header: Header,
    pub boxes: Vec<Detection>,
}

/// Parses `"a,b,c"` into floats.
#[cfg(feature = "onnx")]
pub(crate) fn parse_floats(s: &str) -> Result<Vec<f32>> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<f32>().map_err(|_| anyhow!("not a number: `{p}`")))
        .collect()
}

/// Class labels from a comma separated list or a file with one label per line.
pub fn load_labels(spec: &str) -> Result<Vec<String>> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(Vec::new());
    }
    let path = std::path::Path::new(spec);
    if !spec.contains(',') && path.is_file() {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {spec}"))?;
        return Ok(text.lines().map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()).collect());
    }
    Ok(spec.split(',').map(|s| s.trim().to_owned()).collect())
}

/// Turns raw YOLO output tensors into detections. Pair with `ml/onnx`
/// (`postprocess: none`) or any node producing YOLOv5/v8-style output.
#[derive(Node)]
#[node(kind = "ml/yolo-decode")]
pub struct YoloDecode {
    /// Raw model output: `[1, N, 5 + classes]` (v5) or `[1, 4 + classes, N]` (v8).
    #[input]
    tensor: Input<Tensor>,
    #[output]
    detections: Output<Detections>,
    /// `auto`, `v5` or `v8`.
    #[param(default = "auto")]
    layout: String,
    /// Minimum confidence.
    #[param(default = 0.25, min = 0, max = 1)]
    confidence: f32,
    /// IoU above which overlapping boxes of a class are suppressed.
    #[param(default = 0.45, min = 0, max = 1)]
    iou: f32,
    #[param(default = 100, min = 1, max = 10000)]
    max_detections: u32,
    /// Comma separated class names or a file with one per line.
    #[param(default = "")]
    labels: String,
    names: Option<Vec<String>>,
}

impl Node for YoloDecode {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let Some(t) = self.tensor.latest() else {
            return Ok(());
        };
        if self.names.is_none() {
            self.names = Some(load_labels(&self.labels)?);
        }
        let layout = yolo::Layout::parse(&self.layout)?;
        let cfg = yolo::Config {
            layout,
            confidence: self.confidence,
            iou: self.iou,
            max_detections: self.max_detections as usize,
        };
        let mut boxes = yolo::decode(&t.shape_usize(), &t.data, &cfg)?;
        yolo::label(&mut boxes, self.names.as_deref().unwrap_or(&[]));
        self.detections.send(Detections {
            header: Header::new("", t.stamp_ns),
            boxes,
        })
    }
}

/// Forces this crate to be linked. Hosts call it so the node registrations
/// survive dead-code elimination.
pub fn link() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_roundtrip() {
        let d = Detections {
            header: Header::new("cam", 5),
            boxes: vec![Detection {
                x: 1.0,
                y: 2.0,
                w: 3.0,
                h: 4.0,
                score: 0.5,
                class_id: 7,
                label: "cup".into(),
            }],
        };
        assert_eq!(Detections::decode(&d.to_bytes()).unwrap(), d);
        let t = Tensor::new(vec![2, 2], vec![0.0, -1.0, 2.0, 3.0]);
        assert_eq!(Tensor::decode(&t.to_bytes()).unwrap(), t);
        let Preview::Text(p) = t.preview() else { panic!() };
        assert!(p.contains("[-1, 3]"), "{p}");
    }

    #[test]
    fn labels_from_list_or_file() {
        assert_eq!(load_labels("cat, dog").unwrap(), ["cat", "dog"]);
        assert!(load_labels("").unwrap().is_empty());
        let path = std::env::temp_dir().join(format!("ruro-labels-{}.txt", std::process::id()));
        std::fs::write(&path, "person\nbicycle\n\ncar\n").unwrap();
        assert_eq!(load_labels(path.to_str().unwrap()).unwrap(), ["person", "bicycle", "car"]);
        std::fs::remove_file(path).unwrap();
    }
}
