//! Camera calibration files in the ROS `camera_calibration` YAML format
//! (written by `cameracalibrator.py` and `camera_calibration_parsers`), read
//! into `std/CameraInfo` for the camera nodes' `calibration` parameter.

use std::path::Path;

use ruro::prelude::*;

#[derive(Deserialize)]
struct Matrix {
    rows: usize,
    cols: usize,
    data: Vec<f64>,
}

#[derive(Deserialize)]
struct File {
    image_width: u32,
    image_height: u32,
    #[serde(default)]
    distortion_model: Option<String>,
    camera_matrix: Matrix,
    #[serde(default)]
    distortion_coefficients: Option<Matrix>,
    #[serde(default)]
    rectification_matrix: Option<Matrix>,
    #[serde(default)]
    projection_matrix: Option<Matrix>,
}

fn fixed<const N: usize>(m: &Matrix, name: &str, rows: usize, cols: usize) -> Result<[f64; N]> {
    ensure!(
        m.rows == rows && m.cols == cols && m.data.len() == N,
        "{name} must be {rows}x{cols} with {N} values, got {}x{} with {}",
        m.rows,
        m.cols,
        m.data.len()
    );
    Ok(m.data.as_slice().try_into().unwrap())
}

/// Parses a calibration file's text.
pub fn parse(text: &str) -> Result<CameraInfo> {
    let f: File = serde_saphyr::from_str(text).map_err(|e| anyhow!("{e}"))?;
    let k: [f64; 9] = fixed(&f.camera_matrix, "camera_matrix", 3, 3)?;
    let r = match &f.rectification_matrix {
        Some(m) => fixed(m, "rectification_matrix", 3, 3)?,
        None => [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
    };
    // Without a projection matrix, P = [K | 0] (a monocular, unrectified camera).
    let p = match &f.projection_matrix {
        Some(m) => fixed(m, "projection_matrix", 3, 4)?,
        None => [
            k[0], k[1], k[2], 0.0, k[3], k[4], k[5], 0.0, k[6], k[7], k[8], 0.0,
        ],
    };
    let d = f
        .distortion_coefficients
        .map(|m| m.data)
        .unwrap_or_default();
    let distortion_model = f.distortion_model.unwrap_or_else(|| {
        if d.len() > 5 {
            "rational_polynomial".into()
        } else {
            "plumb_bob".into()
        }
    });
    Ok(CameraInfo {
        width: f.image_width,
        height: f.image_height,
        distortion_model,
        d,
        k,
        r,
        p,
        ..Default::default()
    })
}

pub fn load(path: &Path) -> Result<CameraInfo> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading calibration {}", path.display()))?;
    parse(&text).with_context(|| format!("parsing calibration {}", path.display()))
}

/// The calibration a camera node publishes with its frames.
#[derive(Default)]
pub struct Calibration {
    info: Option<CameraInfo>,
    warned: bool,
}

impl Calibration {
    /// Loads `path`, or nothing for an empty path.
    pub fn open(path: &str, frame_id: &str) -> Result<Self> {
        if path.is_empty() {
            return Ok(Self::default());
        }
        let mut info = load(Path::new(path))?;
        info.header.frame_id = frame_id.to_owned();
        Ok(Self {
            info: Some(info),
            warned: false,
        })
    }

    /// Publishes the calibration for a `width`x`height` frame taken at `stamp_ns`.
    pub fn publish(
        &mut self,
        out: &Output<CameraInfo>,
        width: u32,
        height: u32,
        stamp_ns: u64,
    ) -> Result {
        let Some(info) = self.info.as_mut() else {
            return Ok(());
        };
        if (info.width, info.height) != (width, height) && !self.warned {
            self.warned = true;
            warn!(
                "calibration is for {}x{} but frames are {width}x{height}",
                info.width, info.height
            );
        }
        info.header.stamp_ns = stamp_ns;
        out.send(info.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // As written by ROS `cameracalibrator.py`.
    const ROS_YAML: &str = "image_width: 640
image_height: 480
camera_name: narrow_stereo
camera_matrix:
  rows: 3
  cols: 3
  data: [615.1, 0, 320.5, 0, 615.9, 240.2, 0, 0, 1]
distortion_model: plumb_bob
distortion_coefficients:
  rows: 1
  cols: 5
  data: [0.1, -0.2, 0.001, 0.002, 0]
rectification_matrix:
  rows: 3
  cols: 3
  data: [1, 0, 0, 0, 1, 0, 0, 0, 1]
projection_matrix:
  rows: 3
  cols: 4
  data: [620, 0, 321, 0, 0, 621, 241, 0, 0, 0, 1, 0]
";

    #[test]
    fn parses_ros_calibration_files() {
        let c = parse(ROS_YAML).unwrap();
        assert_eq!(
            (c.width, c.height, c.distortion_model.as_str()),
            (640, 480, "plumb_bob")
        );
        assert_eq!(c.k[0], 615.1);
        assert_eq!(c.k[5], 240.2);
        assert_eq!(c.d, [0.1, -0.2, 0.001, 0.002, 0.0]);
        assert_eq!(c.p[2], 321.0);
        assert_eq!(c.r[4], 1.0);
    }

    #[test]
    fn minimal_files_and_errors() {
        let c = parse(
            "image_width: 4\nimage_height: 3\ncamera_matrix: {rows: 3, cols: 3, data: [2, 0, 1, 0, 2, 1, 0, 0, 1]}\n",
        )
        .unwrap();
        assert_eq!(
            c.p,
            [2.0, 0.0, 1.0, 0.0, 0.0, 2.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
        );
        assert_eq!(c.distortion_model, "plumb_bob");
        let err = parse("image_width: 4\nimage_height: 3\ncamera_matrix: {rows: 2, cols: 2, data: [1, 2, 3, 4]}\n")
            .unwrap_err();
        assert!(
            err.to_string().contains("camera_matrix must be 3x3"),
            "{err}"
        );
        assert!(Calibration::open("/nonexistent.yaml", "cam").is_err());
        assert!(Calibration::open("", "cam").unwrap().info.is_none());
    }
}
