//! YOLO output decoding and non-maximum suppression.
//!
//! Two output layouts are common:
//!
//! - **v5** (YOLOv5/v7): `[1, N, 5 + C]`, rows of `cx, cy, w, h, objectness,
//!   class scores...`; confidence = objectness × class score.
//! - **v8** (YOLOv8/v11): `[1, 4 + C, N]`, columns of `cx, cy, w, h, class
//!   scores...` without objectness.
//!
//! Boxes come out in model-input pixels; [`to_source`] maps them back through
//! the letterbox.

use ruro::prelude::*;

use crate::Detection;
use crate::preprocess::Mapping;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Auto,
    V5,
    V8,
}

impl Layout {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "auto" => Ok(Layout::Auto),
            "v5" => Ok(Layout::V5),
            "v8" => Ok(Layout::V8),
            other => bail!("unknown YOLO layout `{other}` (expected auto, v5 or v8)"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub layout: Layout,
    pub confidence: f32,
    pub iou: f32,
    pub max_detections: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            layout: Layout::Auto,
            confidence: 0.25,
            iou: 0.45,
            max_detections: 100,
        }
    }
}

/// Decodes candidates above the confidence threshold and applies class-wise NMS.
pub fn decode(shape: &[usize], data: &[f32], cfg: &Config) -> Result<Vec<Detection>> {
    let dims: Vec<usize> = shape.iter().copied().skip_while(|&d| d == 1).collect();
    let (a, b) = match dims.as_slice() {
        [a, b] => (*a, *b),
        [] | [_] => return Ok(Vec::new()),
        other => bail!("expected a [1, A, B] output, got {other:?}"),
    };
    ensure!(data.len() == a * b, "tensor has {} values for shape {shape:?}", data.len());
    // v8 has far fewer rows (4 + classes) than anchors; v5 rows hold at least 6 values.
    let layout = match cfg.layout {
        Layout::Auto if a < b || b < 6 => Layout::V8,
        Layout::Auto => Layout::V5,
        l => l,
    };
    let mut candidates = Vec::new();
    match layout {
        Layout::V5 => {
            ensure!(b > 5, "v5 rows need at least 6 values, got {b}");
            for row in data.chunks_exact(b) {
                let obj = row[4];
                if obj < cfg.confidence {
                    continue;
                }
                let (class, score) = best(&row[5..]);
                let conf = obj * score;
                if conf >= cfg.confidence {
                    candidates.push(make(row[0], row[1], row[2], row[3], conf, class));
                }
            }
        }
        _ => {
            ensure!(a > 4, "v8 output needs at least 5 rows, got {a}");
            let n = b;
            let at = |r: usize, i: usize| data[r * n + i];
            for i in 0..n {
                let (mut class, mut score) = (0, f32::NEG_INFINITY);
                for c in 0..a - 4 {
                    let s = at(4 + c, i);
                    if s > score {
                        (class, score) = (c, s);
                    }
                }
                if score >= cfg.confidence {
                    candidates.push(make(at(0, i), at(1, i), at(2, i), at(3, i), score, class));
                }
            }
        }
    }
    Ok(nms(candidates, cfg.iou, cfg.max_detections))
}

fn best(scores: &[f32]) -> (usize, f32) {
    scores
        .iter()
        .copied()
        .enumerate()
        .fold((0, f32::NEG_INFINITY), |acc, (i, s)| if s > acc.1 { (i, s) } else { acc })
}

fn make(cx: f32, cy: f32, w: f32, h: f32, score: f32, class: usize) -> Detection {
    Detection {
        x: cx - w / 2.0,
        y: cy - h / 2.0,
        w,
        h,
        score,
        class_id: class as u32,
        label: String::new(),
    }
}

/// Intersection over union of two boxes.
pub fn iou(a: &Detection, b: &Detection) -> f32 {
    let ix = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let iy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    if ix <= 0.0 || iy <= 0.0 {
        return 0.0;
    }
    let inter = ix * iy;
    inter / (a.w * a.h + b.w * b.h - inter)
}

/// Greedy non-maximum suppression within each class.
pub fn nms(mut boxes: Vec<Detection>, iou_threshold: f32, max: usize) -> Vec<Detection> {
    boxes.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Detection> = Vec::new();
    for d in boxes {
        if kept.len() == max {
            break;
        }
        if kept
            .iter()
            .all(|k| k.class_id != d.class_id || iou(k, &d) <= iou_threshold)
        {
            kept.push(d);
        }
    }
    kept
}

/// Maps boxes from model-input pixels to source-image pixels, clipped to the image.
pub fn to_source(boxes: &mut [Detection], map: &Mapping, width: u32, height: u32) {
    for d in boxes {
        let (x0, y0) = map.to_source(d.x, d.y);
        let (x1, y1) = map.to_source(d.x + d.w, d.y + d.h);
        let (x0, x1) = (x0.clamp(0.0, width as f32), x1.clamp(0.0, width as f32));
        let (y0, y1) = (y0.clamp(0.0, height as f32), y1.clamp(0.0, height as f32));
        (d.x, d.y, d.w, d.h) = (x0, y0, x1 - x0, y1 - y0);
    }
}

/// Fills in labels from a class-name list (`class<N>` when out of range).
pub fn label(boxes: &mut [Detection], names: &[String]) {
    for d in boxes {
        d.label = names
            .get(d.class_id as usize)
            .cloned()
            .unwrap_or_else(|| format!("class{}", d.class_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(x: f32, y: f32, w: f32, h: f32, score: f32, class_id: u32) -> Detection {
        Detection {
            x,
            y,
            w,
            h,
            score,
            class_id,
            label: String::new(),
        }
    }

    #[test]
    fn iou_values() {
        let a = det(0.0, 0.0, 10.0, 10.0, 1.0, 0);
        assert_eq!(iou(&a, &a), 1.0);
        assert_eq!(iou(&a, &det(10.0, 0.0, 10.0, 10.0, 1.0, 0)), 0.0);
        let half = iou(&a, &det(5.0, 0.0, 10.0, 10.0, 1.0, 0));
        assert!((half - 50.0 / 150.0).abs() < 1e-6);
    }

    #[test]
    fn nms_is_per_class_and_greedy() {
        let boxes = vec![
            det(0.0, 0.0, 10.0, 10.0, 0.8, 0),
            det(1.0, 1.0, 10.0, 10.0, 0.9, 0), // overlaps the first, higher score
            det(1.0, 1.0, 10.0, 10.0, 0.7, 1), // same place, other class
            det(50.0, 50.0, 10.0, 10.0, 0.6, 0),
        ];
        let kept = nms(boxes.clone(), 0.5, 100);
        let scores: Vec<f32> = kept.iter().map(|d| d.score).collect();
        assert_eq!(scores, [0.9, 0.7, 0.6]);
        assert_eq!(nms(boxes, 0.5, 2).len(), 2);
    }

    /// Builds a v8 tensor `[1, 4 + C, N]` from (cx, cy, w, h, class scores).
    fn v8(anchors: &[([f32; 4], Vec<f32>)]) -> (Vec<usize>, Vec<f32>) {
        let c = anchors[0].1.len();
        let n = anchors.len();
        let mut data = vec![0f32; (4 + c) * n];
        for (i, (b, s)) in anchors.iter().enumerate() {
            for r in 0..4 {
                data[r * n + i] = b[r];
            }
            for (k, v) in s.iter().enumerate() {
                data[(4 + k) * n + i] = *v;
            }
        }
        (vec![1, 4 + c, n], data)
    }

    #[test]
    fn decodes_v8_layout() {
        let (shape, data) = v8(&[
            ([100.0, 100.0, 20.0, 40.0], vec![0.1, 0.95, 0.0]),
            ([102.0, 101.0, 20.0, 40.0], vec![0.1, 0.90, 0.0]), // duplicate, suppressed
            ([300.0, 200.0, 50.0, 50.0], vec![0.6, 0.0, 0.3]),
            ([10.0, 10.0, 5.0, 5.0], vec![0.1, 0.1, 0.2]), // below threshold
            ([0.0; 4], vec![0.0; 3]),
            ([0.0; 4], vec![0.0; 3]),
            ([0.0; 4], vec![0.0; 3]),
            ([0.0; 4], vec![0.0; 3]),
        ]);
        let out = decode(&shape, &data, &Config::default()).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], det(90.0, 80.0, 20.0, 40.0, 0.95, 1));
        assert_eq!((out[1].class_id, out[1].score), (0, 0.6));
        assert_eq!((out[1].x, out[1].y), (275.0, 175.0));
    }

    #[test]
    fn decodes_v5_layout_with_objectness() {
        // [1, N=8, 5 + 2]
        let mut rows = vec![
            [50.0, 60.0, 10.0, 20.0, 0.9, 0.2, 0.8],  // conf 0.72 class 1
            [50.0, 60.0, 10.0, 20.0, 0.2, 1.0, 0.0],  // objectness too low
            [10.0, 10.0, 4.0, 4.0, 0.5, 0.4, 0.1],    // conf 0.2 < 0.25
        ];
        rows.resize(8, [0.0; 7]);
        let data: Vec<f32> = rows.iter().flatten().copied().collect();
        let out = decode(&[1, 8, 7], &data, &Config::default()).unwrap();
        assert_eq!(out.len(), 1);
        assert!((out[0].score - 0.72).abs() < 1e-6);
        assert_eq!((out[0].class_id, out[0].x, out[0].y), (1, 45.0, 50.0));
        // Forcing the wrong layout does not panic.
        assert!(decode(&[1, 8, 7], &data, &Config { layout: Layout::V8, ..Default::default() }).is_ok());
    }

    #[test]
    fn maps_back_through_letterbox_and_labels() {
        let map = Mapping {
            scale_x: 0.5,
            scale_y: 0.5,
            pad_x: 0.0,
            pad_y: 140.0,
        };
        let mut boxes = vec![det(100.0, 150.0, 50.0, 30.0, 0.9, 2), det(600.0, 100.0, 100.0, 100.0, 0.5, 0)];
        to_source(&mut boxes, &map, 1280, 720);
        assert_eq!((boxes[0].x, boxes[0].y, boxes[0].w, boxes[0].h), (200.0, 20.0, 100.0, 60.0));
        // Second box starts in the top padding and runs off the right edge: clipped.
        assert_eq!((boxes[1].x, boxes[1].y, boxes[1].w, boxes[1].h), (1200.0, 0.0, 80.0, 120.0));
        label(&mut boxes, &["person".into(), "bike".into()]);
        assert_eq!((boxes[0].label.as_str(), boxes[1].label.as_str()), ("class2", "person"));
    }

    #[test]
    fn rejects_bad_shapes() {
        assert!(decode(&[1, 2, 3, 4], &[0.0; 24], &Config::default()).is_err());
        assert!(decode(&[1, 6, 4], &[0.0; 5], &Config::default()).is_err());
        assert!(decode(&[1, 0], &[], &Config::default()).unwrap().is_empty());
    }
}
