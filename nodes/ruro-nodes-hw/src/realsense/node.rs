use std::sync::Arc;

use ruro::prelude::*;

use super::Converter;
use super::ffi::{Api, Camera, Config};

/// Intel RealSense depth camera (D400 series and others librealsense2
/// supports), through librealsense2 loaded at run time.
///
/// `depth` is 16-bit millimetres (`mono16`, ROS `16UC1`; 0 means no
/// measurement), aligned to `color` unless `align` is off. `points` is in
/// metres in the optical frame of the depth image. `imu` pairs each gyro
/// sample with the latest accelerometer sample. Every frame that arrived
/// since the last step is published, whatever `fps` is.
#[derive(Node)]
#[node(kind = "hw/realsense", rate = 30)]
pub struct RealSense {
    #[output]
    color: Output<Image>,
    #[output]
    color_info: Output<CameraInfo>,
    #[output]
    depth: Output<Image>,
    #[output]
    depth_info: Output<CameraInfo>,
    #[output]
    infrared: Output<Image>,
    #[output]
    infrared_info: Output<CameraInfo>,
    #[output]
    points: Output<PointCloud>,
    #[output]
    imu: Output<Imu>,
    /// Serial number of the camera; empty for the first one found.
    #[param(default = "")]
    serial: String,
    /// Resolution and frame rate of the colour, depth and infrared streams.
    #[param(default = 640, min = 0, max = 4096)]
    width: u32,
    #[param(default = 480, min = 0, max = 4096)]
    height: u32,
    #[param(default = 30, min = 0, max = 300)]
    fps: u32,
    #[param(default = true)]
    enable_color: bool,
    #[param(default = true)]
    enable_depth: bool,
    /// The left infrared imager (`infra1`).
    #[param(default = false)]
    enable_infrared: bool,
    /// Gyro and accelerometer (D435i, D455).
    #[param(default = false)]
    enable_imu: bool,
    /// Align depth to the colour image.
    #[param(default = true)]
    align: bool,
    /// Publish `points` from every n-th depth pixel; 0 turns it off.
    #[param(default = 0, min = 0, max = 64)]
    point_step: u32,
    /// Points farther than this many metres are dropped.
    #[param(default = 10.0, min = 0.1, max = 100)]
    max_range: f64,
    /// Prefix of the frame ids, e.g. `camera` for `camera_color_optical_frame`.
    #[param(default = "camera")]
    frame_prefix: String,
    /// Path to librealsense2.so; empty for `RURO_REALSENSE_LIB` or the system library.
    #[param(default = "")]
    library: String,
    camera: Option<Camera>,
    converter: Option<Converter>,
}

impl Node for RealSense {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        let api = Api::load(Some(&self.library)).map_err(|e| anyhow!(e))?;
        let version = api.version.clone();
        let cfg = Config {
            serial: self.serial.clone(),
            width: self.width as i32,
            height: self.height as i32,
            fps: self.fps as i32,
            color: self.enable_color,
            depth: self.enable_depth,
            infrared: self.enable_infrared,
            imu: self.enable_imu,
            align: self.align,
        };
        let cam = Camera::open(Arc::new(api), &cfg).map_err(|e| anyhow!("RealSense: {e}"))?;
        info!(
            "RealSense {} ({}), librealsense {version}, depth scale {} m",
            cam.name, cam.serial, cam.depth_scale
        );
        self.converter = Some(Converter::new(
            cam.depth_scale,
            &self.frame_prefix,
            self.align && self.enable_color,
            self.point_step as usize,
            self.max_range as f32,
        ));
        self.camera = Some(cam);
        Ok(())
    }

    fn step(&mut self, ctx: &mut Ctx) -> Result {
        let (Some(cam), Some(conv)) = (self.camera.as_mut(), self.converter.as_mut()) else {
            bail!("not started");
        };
        // Every frameset since the last step: the camera's rate need not
        // match the node's.
        for set in cam.poll().map_err(|e| anyhow!("RealSense: {e}"))? {
            let m = conv.convert(&set, ctx.now_ns())?;
            if let Some((img, info)) = m.color {
                self.color_info.send(info)?;
                self.color.send(img)?;
            }
            if let Some((img, info)) = m.depth {
                self.depth_info.send(info)?;
                self.depth.send(img)?;
            }
            if let Some((img, info)) = m.infrared {
                self.infrared_info.send(info)?;
                self.infrared.send(img)?;
            }
            if let Some(pc) = m.points {
                self.points.send(pc)?;
            }
            for i in m.imu {
                self.imu.send(i)?;
            }
        }
        Ok(())
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        // Dropping the camera stops the pipeline.
        self.camera = None;
        self.converter = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruro::node::testing::Harness;
    use serde_json::json;

    #[test]
    fn fails_clearly_without_librealsense() {
        let mut h = Harness::with_node::<RealSense>(
            json!({ "library": "/nonexistent/librealsense2.so" }),
            |_| {},
        )
        .unwrap();
        let err = h.step().unwrap_err().to_string();
        assert!(err.contains("cannot load librealsense2"), "{err}");
    }
}
