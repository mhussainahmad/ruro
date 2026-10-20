//! Run-time binding to the librealsense2 C API.
//!
//! Signatures follow `librealsense2/rs.h` and `h/rs_context.h`,
//! `rs_device.h`, `rs_pipeline.h`, `rs_config.h`, `rs_frame.h`,
//! `rs_sensor.h`, `rs_processing.h` and `rs_types.h` (librealsense 2.59);
//! opaque handles are `*mut c_void`, C enums `c_int`. This layer only moves
//! frames out of the library: each one is copied into a [`RawFrame`] and
//! converted to messages by the plain code in the parent module.
//!
//! Run against librealsense 2.56.3 with a D455 and two D455Fs
//! (`tests/realsense.rs`).

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::ptr::null_mut;
use std::sync::Arc;

use libloading::Library;

use super::{Intrinsics, RawFrame, RawIntrinsics, format, stream};

/// Environment variable naming the librealsense2 shared library to load.
pub const LIB_ENV: &str = "RURO_REALSENSE_LIB";

type H = *mut c_void;
type E = *mut *mut c_void;
/// `rs2_frame_callback_ptr`: takes ownership of the frame.
type FrameCallback = unsafe extern "C" fn(H, *mut c_void);

const EXTENSION_DEPTH_SENSOR: c_int = 7;
const EXTENSION_VIDEO_FRAME: c_int = 8;
const EXTENSION_MOTION_FRAME: c_int = 9;
const CAMERA_INFO_NAME: c_int = 0;
const CAMERA_INFO_SERIAL_NUMBER: c_int = 1;
const TIMESTAMP_DOMAIN_HARDWARE_CLOCK: c_int = 0;

/// The resolved functions.
struct Fns {
    get_api_version: unsafe extern "C" fn(E) -> c_int,
    create_context: unsafe extern "C" fn(c_int, E) -> H,
    delete_context: unsafe extern "C" fn(H),
    query_devices: unsafe extern "C" fn(H, E) -> H,
    get_device_count: unsafe extern "C" fn(H, E) -> c_int,
    delete_device_list: unsafe extern "C" fn(H),
    create_device: unsafe extern "C" fn(H, c_int, E) -> H,
    delete_device: unsafe extern "C" fn(H),
    get_device_info: unsafe extern "C" fn(H, c_int, E) -> *const c_char,
    create_pipeline: unsafe extern "C" fn(H, E) -> H,
    delete_pipeline: unsafe extern "C" fn(H),
    create_config: unsafe extern "C" fn(E) -> H,
    delete_config: unsafe extern "C" fn(H),
    config_enable_stream: unsafe extern "C" fn(H, c_int, c_int, c_int, c_int, c_int, c_int, E),
    config_enable_device: unsafe extern "C" fn(H, *const c_char, E),
    pipeline_start_with_config_and_callback:
        unsafe extern "C" fn(H, H, FrameCallback, *mut c_void, E) -> H,
    pipeline_stop: unsafe extern "C" fn(H, E),
    delete_pipeline_profile: unsafe extern "C" fn(H),
    pipeline_profile_get_device: unsafe extern "C" fn(H, E) -> H,
    query_sensors: unsafe extern "C" fn(H, E) -> H,
    get_sensors_count: unsafe extern "C" fn(H, E) -> c_int,
    create_sensor: unsafe extern "C" fn(H, c_int, E) -> H,
    delete_sensor: unsafe extern "C" fn(H),
    delete_sensor_list: unsafe extern "C" fn(H),
    is_sensor_extendable_to: unsafe extern "C" fn(H, c_int, E) -> c_int,
    get_depth_scale: unsafe extern "C" fn(H, E) -> f32,
    get_stream_profiles: unsafe extern "C" fn(H, E) -> H,
    get_stream_profiles_count: unsafe extern "C" fn(H, E) -> c_int,
    get_stream_profile: unsafe extern "C" fn(H, c_int, E) -> H,
    is_stream_profile_default: unsafe extern "C" fn(H, E) -> c_int,
    delete_stream_profiles_list: unsafe extern "C" fn(H),
    open_multiple: unsafe extern "C" fn(H, *const H, c_int, E),
    start_queue: unsafe extern "C" fn(H, H, E),
    sensor_stop: unsafe extern "C" fn(H, E),
    close: unsafe extern "C" fn(H, E),
    poll_for_frame: unsafe extern "C" fn(H, *mut H, E) -> c_int,
    enqueue_frame: FrameCallback,
    embedded_frames_count: unsafe extern "C" fn(H, E) -> c_int,
    extract_frame: unsafe extern "C" fn(H, c_int, E) -> H,
    release_frame: unsafe extern "C" fn(H),
    is_frame_extendable_to: unsafe extern "C" fn(H, c_int, E) -> c_int,
    get_frame_data: unsafe extern "C" fn(H, E) -> *const c_void,
    get_frame_data_size: unsafe extern "C" fn(H, E) -> c_int,
    get_frame_width: unsafe extern "C" fn(H, E) -> c_int,
    get_frame_height: unsafe extern "C" fn(H, E) -> c_int,
    get_frame_stride_in_bytes: unsafe extern "C" fn(H, E) -> c_int,
    get_frame_timestamp: unsafe extern "C" fn(H, E) -> f64,
    get_frame_timestamp_domain: unsafe extern "C" fn(H, E) -> c_int,
    get_frame_stream_profile: unsafe extern "C" fn(H, E) -> H,
    get_stream_profile_data:
        unsafe extern "C" fn(H, *mut c_int, *mut c_int, *mut c_int, *mut c_int, *mut c_int, E),
    get_video_stream_intrinsics: unsafe extern "C" fn(H, *mut RawIntrinsics, E),
    create_align: unsafe extern "C" fn(c_int, E) -> H,
    create_frame_queue: unsafe extern "C" fn(c_int, E) -> H,
    delete_frame_queue: unsafe extern "C" fn(H),
    start_processing_queue: unsafe extern "C" fn(H, H, E),
    process_frame: unsafe extern "C" fn(H, H, E),
    wait_for_frame: unsafe extern "C" fn(H, c_uint, E) -> H,
    delete_processing_block: unsafe extern "C" fn(H),
    get_error_message: unsafe extern "C" fn(H) -> *const c_char,
    get_failed_function: unsafe extern "C" fn(H) -> *const c_char,
    free_error: unsafe extern "C" fn(H),
}

/// A loaded librealsense2.
pub struct Api {
    f: Fns,
    /// `major.minor.patch` of the loaded library.
    pub version: String,
    _lib: Library,
}

/// Library names tried when none is given: the development symlink, then
/// versioned names in the usual directories.
fn candidates() -> Vec<String> {
    let mut out = vec!["librealsense2.so".to_owned()];
    let dirs = [
        "/usr/lib",
        "/usr/local/lib",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
    ];
    let mut versioned: Vec<String> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.starts_with("librealsense2.so.")
                .then(|| e.path().to_string_lossy().into_owned())
        })
        .collect();
    versioned.sort();
    out.extend(versioned.into_iter().rev());
    out
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: librealsense returns NUL-terminated strings it owns.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

impl Api {
    /// Loads `path`, or `RURO_REALSENSE_LIB`, or librealsense2 from the
    /// system library paths.
    pub fn load(path: Option<&str>) -> Result<Api, String> {
        let explicit = path
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .or_else(|| std::env::var(LIB_ENV).ok().filter(|p| !p.is_empty()));
        let names = match &explicit {
            Some(p) => vec![p.clone()],
            None => candidates(),
        };
        let mut errors = Vec::new();
        let lib = names
            .iter()
            .find_map(|n| {
                // SAFETY: loading librealsense2 runs its static initialisers only.
                unsafe { Library::new(n) }
                    .map_err(|e| errors.push(format!("{n}: {e}")))
                    .ok()
            })
            .ok_or_else(|| {
                format!(
                    "cannot load librealsense2 ({}). Install librealsense2 \
                     (https://github.com/IntelRealSense/librealsense) or set {LIB_ENV} \
                     to the path of librealsense2.so",
                    errors.join("; ")
                )
            })?;
        // SAFETY: the symbol types match the C declarations listed above.
        let f = unsafe {
            macro_rules! sym {
                ($name:literal) => {
                    *lib.get(concat!($name, "\0").as_bytes())
                        .map_err(|e| format!("librealsense2 lacks {}: {e}", $name))?
                };
            }
            Fns {
                get_api_version: sym!("rs2_get_api_version"),
                create_context: sym!("rs2_create_context"),
                delete_context: sym!("rs2_delete_context"),
                query_devices: sym!("rs2_query_devices"),
                get_device_count: sym!("rs2_get_device_count"),
                delete_device_list: sym!("rs2_delete_device_list"),
                create_device: sym!("rs2_create_device"),
                delete_device: sym!("rs2_delete_device"),
                get_device_info: sym!("rs2_get_device_info"),
                create_pipeline: sym!("rs2_create_pipeline"),
                delete_pipeline: sym!("rs2_delete_pipeline"),
                create_config: sym!("rs2_create_config"),
                delete_config: sym!("rs2_delete_config"),
                config_enable_stream: sym!("rs2_config_enable_stream"),
                config_enable_device: sym!("rs2_config_enable_device"),
                pipeline_start_with_config_and_callback: sym!(
                    "rs2_pipeline_start_with_config_and_callback"
                ),
                pipeline_stop: sym!("rs2_pipeline_stop"),
                delete_pipeline_profile: sym!("rs2_delete_pipeline_profile"),
                pipeline_profile_get_device: sym!("rs2_pipeline_profile_get_device"),
                query_sensors: sym!("rs2_query_sensors"),
                get_sensors_count: sym!("rs2_get_sensors_count"),
                create_sensor: sym!("rs2_create_sensor"),
                delete_sensor: sym!("rs2_delete_sensor"),
                delete_sensor_list: sym!("rs2_delete_sensor_list"),
                is_sensor_extendable_to: sym!("rs2_is_sensor_extendable_to"),
                get_depth_scale: sym!("rs2_get_depth_scale"),
                get_stream_profiles: sym!("rs2_get_stream_profiles"),
                get_stream_profiles_count: sym!("rs2_get_stream_profiles_count"),
                get_stream_profile: sym!("rs2_get_stream_profile"),
                is_stream_profile_default: sym!("rs2_is_stream_profile_default"),
                delete_stream_profiles_list: sym!("rs2_delete_stream_profiles_list"),
                open_multiple: sym!("rs2_open_multiple"),
                start_queue: sym!("rs2_start_queue"),
                sensor_stop: sym!("rs2_stop"),
                close: sym!("rs2_close"),
                poll_for_frame: sym!("rs2_poll_for_frame"),
                enqueue_frame: sym!("rs2_enqueue_frame"),
                embedded_frames_count: sym!("rs2_embedded_frames_count"),
                extract_frame: sym!("rs2_extract_frame"),
                release_frame: sym!("rs2_release_frame"),
                is_frame_extendable_to: sym!("rs2_is_frame_extendable_to"),
                get_frame_data: sym!("rs2_get_frame_data"),
                get_frame_data_size: sym!("rs2_get_frame_data_size"),
                get_frame_width: sym!("rs2_get_frame_width"),
                get_frame_height: sym!("rs2_get_frame_height"),
                get_frame_stride_in_bytes: sym!("rs2_get_frame_stride_in_bytes"),
                get_frame_timestamp: sym!("rs2_get_frame_timestamp"),
                get_frame_timestamp_domain: sym!("rs2_get_frame_timestamp_domain"),
                get_frame_stream_profile: sym!("rs2_get_frame_stream_profile"),
                get_stream_profile_data: sym!("rs2_get_stream_profile_data"),
                get_video_stream_intrinsics: sym!("rs2_get_video_stream_intrinsics"),
                create_align: sym!("rs2_create_align"),
                create_frame_queue: sym!("rs2_create_frame_queue"),
                delete_frame_queue: sym!("rs2_delete_frame_queue"),
                start_processing_queue: sym!("rs2_start_processing_queue"),
                process_frame: sym!("rs2_process_frame"),
                wait_for_frame: sym!("rs2_wait_for_frame"),
                delete_processing_block: sym!("rs2_delete_processing_block"),
                get_error_message: sym!("rs2_get_error_message"),
                get_failed_function: sym!("rs2_get_failed_function"),
                free_error: sym!("rs2_free_error"),
            }
        };
        let mut api = Api {
            f,
            version: String::new(),
            _lib: lib,
        };
        let v = api.call(|e| unsafe { (api.f.get_api_version)(e) })?;
        api.version = format!("{}.{}.{}", v / 10000, v / 100 % 100, v % 100);
        Ok(api)
    }

    /// Name and serial number of each connected camera.
    pub fn devices(&self) -> Result<Vec<(String, String)>, String> {
        let f = &self.f;
        // SAFETY: each handle is live from its creation to its deletion
        // below; device info strings are copied before the device is deleted.
        unsafe {
            let version = self.call(|e| (f.get_api_version)(e))?;
            let ctx = self.call(|e| (f.create_context)(version, e))?;
            let r = (|| {
                let list = self.call(|e| (f.query_devices)(ctx, e))?;
                let r = (|| {
                    let n = self.call(|e| (f.get_device_count)(list, e))?;
                    let mut out = Vec::new();
                    for i in 0..n {
                        let dev = self.call(|e| (f.create_device)(list, i, e))?;
                        let info = |k| {
                            cstr(
                                self.call(|e| (f.get_device_info)(dev, k, e))
                                    .unwrap_or(std::ptr::null()),
                            )
                        };
                        out.push((info(CAMERA_INFO_NAME), info(CAMERA_INFO_SERIAL_NUMBER)));
                        (f.delete_device)(dev);
                    }
                    Ok(out)
                })();
                (f.delete_device_list)(list);
                r
            })();
            (f.delete_context)(ctx);
            r
        }
    }

    /// The camera with `serial`, or the first one if it is empty.
    ///
    /// # Safety
    /// `ctx` must be a live context.
    unsafe fn find_device(&self, ctx: H, serial: &str) -> Result<H, String> {
        let f = &self.f;
        // SAFETY: the list and each device not returned are deleted once.
        unsafe {
            let list = self.call(|e| (f.query_devices)(ctx, e))?;
            let r = (|| {
                let n = self.call(|e| (f.get_device_count)(list, e))?;
                for i in 0..n {
                    let dev = self.call(|e| (f.create_device)(list, i, e))?;
                    let sn = cstr(
                        self.call(|e| (f.get_device_info)(dev, CAMERA_INFO_SERIAL_NUMBER, e))
                            .unwrap_or(std::ptr::null()),
                    );
                    if serial.is_empty() || sn == serial {
                        return Ok(dev);
                    }
                    (f.delete_device)(dev);
                }
                Err(if n == 0 {
                    "no RealSense device connected".to_owned()
                } else {
                    format!("no RealSense device with serial number {serial}")
                })
            })();
            (f.delete_device_list)(list);
            r
        }
    }

    /// Runs one C call with an error slot and turns a set error into `Err`.
    fn call<T>(&self, f: impl FnOnce(E) -> T) -> Result<T, String> {
        let mut err: H = null_mut();
        let r = f(&mut err);
        if err.is_null() {
            return Ok(r);
        }
        // SAFETY: `err` is a live rs2_error, freed exactly once here.
        unsafe {
            let msg = format!(
                "{}: {}",
                cstr((self.f.get_failed_function)(err)),
                cstr((self.f.get_error_message)(err))
            );
            (self.f.free_error)(err);
            Err(msg)
        }
    }
}

/// What to stream.
#[derive(Debug, Clone)]
pub struct Config {
    /// Serial number, or empty for the first device.
    pub serial: String,
    pub width: i32,
    pub height: i32,
    pub fps: i32,
    pub color: bool,
    pub depth: bool,
    pub infrared: bool,
    pub imu: bool,
    /// Align depth to the colour image.
    pub align: bool,
}

/// A started pipeline.
pub struct Camera {
    api: Arc<Api>,
    ctx: H,
    pipe: H,
    profile: H,
    /// Every frameset the pipeline produces, in order. The pipeline's own
    /// queue, read by `rs2_pipeline_poll_for_frames`, keeps only the newest.
    sets: H,
    /// Align processing block and its output queue.
    align: Option<(H, H)>,
    /// The motion sensor, started apart from the pipeline, and its queue.
    motion: Option<(H, H)>,
    /// A failure in [`Camera::poll`] after some frames were taken, for the next call.
    error: Option<String>,
    pub depth_scale: f32,
    pub name: String,
    pub serial: String,
}

// SAFETY: the handles are owned by this struct alone; librealsense objects
// may be used from any thread.
unsafe impl Send for Camera {}

impl Camera {
    pub fn open(api: Arc<Api>, cfg: &Config) -> Result<Camera, String> {
        let shared = api;
        let (api, f) = (&*shared, &shared.f);
        // SAFETY (whole block): each handle passed to a call is live (created
        // above it and not yet deleted); out-parameters point to locals.
        unsafe {
            let version = api.call(|e| (f.get_api_version)(e))?;
            let ctx = api.call(|e| (f.create_context)(version, e))?;
            let mut cam = Camera {
                api: shared.clone(),
                ctx,
                pipe: null_mut(),
                profile: null_mut(),
                sets: null_mut(),
                align: None,
                motion: None,
                error: None,
                depth_scale: 0.001,
                name: String::new(),
                serial: String::new(),
            };
            let list = api.call(|e| (f.query_devices)(ctx, e))?;
            let count = api.call(|e| (f.get_device_count)(list, e));
            (f.delete_device_list)(list);
            if count? == 0 {
                return Err("no RealSense device connected".into());
            }
            let video = cfg.color || cfg.depth || cfg.infrared;
            if !video && !cfg.imu {
                return Err("no stream enabled".into());
            }
            let dev = if video {
                cam.pipe = api.call(|e| (f.create_pipeline)(ctx, e))?;
                // Half a second of framesets between polls; older ones are dropped.
                cam.sets = api.call(|e| (f.create_frame_queue)((cfg.fps / 2).max(4), e))?;
                let config = api.call(|e| (f.create_config)(e))?;
                let r = (|| {
                    if !cfg.serial.is_empty() {
                        let s = CString::new(cfg.serial.as_str()).map_err(|e| e.to_string())?;
                        api.call(|e| (f.config_enable_device)(config, s.as_ptr(), e))?;
                    }
                    let (w, h, fps) = (cfg.width, cfg.height, cfg.fps);
                    let enable = |s, index, w, h, fmt, fps| {
                        api.call(|e| (f.config_enable_stream)(config, s, index, w, h, fmt, fps, e))
                    };
                    if cfg.color {
                        enable(stream::COLOR, -1, w, h, format::RGB8, fps)?;
                    }
                    if cfg.depth {
                        enable(stream::DEPTH, -1, w, h, format::Z16, fps)?;
                    }
                    if cfg.infrared {
                        enable(stream::INFRARED, 1, w, h, format::Y8, fps)?;
                    }
                    // rs2_enqueue_frame is a frame callback whose user pointer is
                    // the queue: framesets go straight into `sets`.
                    api.call(|e| {
                        (f.pipeline_start_with_config_and_callback)(
                            cam.pipe,
                            config,
                            f.enqueue_frame,
                            cam.sets,
                            e,
                        )
                    })
                })();
                (f.delete_config)(config);
                cam.profile = r?;
                api.call(|e| (f.pipeline_profile_get_device)(cam.profile, e))?
            } else {
                // A pipeline without streams starts its default ones, the
                // motion sensor among them: open the device directly.
                api.find_device(ctx, &cfg.serial)?
            };
            cam.name = cstr(
                api.call(|e| (f.get_device_info)(dev, CAMERA_INFO_NAME, e))
                    .unwrap_or(std::ptr::null()),
            );
            cam.serial = cstr(
                api.call(|e| (f.get_device_info)(dev, CAMERA_INFO_SERIAL_NUMBER, e))
                    .unwrap_or(std::ptr::null()),
            );
            if let Ok(sensors) = api.call(|e| (f.query_sensors)(dev, e)) {
                let n = api.call(|e| (f.get_sensors_count)(sensors, e)).unwrap_or(0);
                for i in 0..n {
                    let Ok(s) = api.call(|e| (f.create_sensor)(sensors, i, e)) else {
                        continue;
                    };
                    if api.call(|e| (f.is_sensor_extendable_to)(s, EXTENSION_DEPTH_SENSOR, e))
                        == Ok(1)
                        && let Ok(scale) = api.call(|e| (f.get_depth_scale)(s, e))
                    {
                        cam.depth_scale = scale;
                    }
                    if cfg.imu && cam.motion.is_none() {
                        match cam.start_motion(s) {
                            Ok(true) => continue, // `cam` owns the sensor now
                            Ok(false) => {}
                            Err(err) => {
                                (f.delete_sensor)(s);
                                (f.delete_sensor_list)(sensors);
                                (f.delete_device)(dev);
                                return Err(err);
                            }
                        }
                    }
                    (f.delete_sensor)(s);
                }
                (f.delete_sensor_list)(sensors);
            }
            (f.delete_device)(dev);
            if cfg.imu && cam.motion.is_none() {
                return Err(format!("{} has no IMU", cam.name));
            }
            if cfg.align && cfg.color && cfg.depth {
                let block = api.call(|e| (f.create_align)(stream::COLOR, e))?;
                let queue = match api.call(|e| (f.create_frame_queue)(1, e)) {
                    Ok(q) => q,
                    Err(err) => {
                        (f.delete_processing_block)(block);
                        return Err(err);
                    }
                };
                cam.align = Some((block, queue));
                api.call(|e| (f.start_processing_queue)(block, queue, e))?;
            }
            Ok(cam)
        }
    }

    /// Starts gyro and accelerometer on `sensor` if it has them, into a
    /// queue of their own. The pipeline delivers motion samples bundled into
    /// video framesets, one per set, and keeps one set: most samples would be
    /// lost. Returns whether `sensor` was started (and is then owned by `self`).
    ///
    /// # Safety
    /// `sensor` must be a live sensor handle.
    unsafe fn start_motion(&mut self, sensor: H) -> Result<bool, String> {
        let (api, f) = (&self.api, &self.api.f);
        unsafe {
            let list = api.call(|e| (f.get_stream_profiles)(sensor, e))?;
            let n = api
                .call(|e| (f.get_stream_profiles_count)(list, e))
                .unwrap_or(0);
            // Per stream: the default profile, else the first one listed;
            // with whether it is the default.
            let (mut gyro, mut accel) = (None::<(H, bool)>, None::<(H, bool)>);
            for i in 0..n {
                let Ok(p) = api.call(|e| (f.get_stream_profile)(list, i, e)) else {
                    continue;
                };
                let (mut s, mut fmt, mut index, mut uid, mut fps) = (0, 0, 0, 0, 0);
                if api
                    .call(|e| {
                        (f.get_stream_profile_data)(
                            p, &mut s, &mut fmt, &mut index, &mut uid, &mut fps, e,
                        )
                    })
                    .is_err()
                    || fmt != format::MOTION_XYZ32F
                {
                    continue;
                }
                let default = api.call(|e| (f.is_stream_profile_default)(p, e)) == Ok(1);
                let slot = match s {
                    stream::GYRO => &mut gyro,
                    stream::ACCEL => &mut accel,
                    _ => continue,
                };
                if slot.is_none_or(|(_, d)| default && !d) {
                    *slot = Some((p, default));
                }
            }
            let (Some((g, _)), Some((a, _))) = (gyro, accel) else {
                (f.delete_stream_profiles_list)(list);
                return Ok(false);
            };
            let profiles = [g, a];
            let opened = api.call(|e| (f.open_multiple)(sensor, profiles.as_ptr(), 2, e));
            (f.delete_stream_profiles_list)(list);
            opened?;
            // Room for well over a second of samples between polls.
            let queue = match api.call(|e| (f.create_frame_queue)(1024, e)) {
                Ok(q) => q,
                Err(err) => {
                    let _ = api.call(|e| (f.close)(sensor, e));
                    return Err(err);
                }
            };
            if let Err(err) = api.call(|e| (f.start_queue)(sensor, queue, e)) {
                let _ = api.call(|e| (f.close)(sensor, e));
                (f.delete_frame_queue)(queue);
                return Err(err);
            }
            self.motion = Some((sensor, queue));
            Ok(true)
        }
    }

    /// Copies one frame out of the library.
    ///
    /// # Safety
    /// `frame` must be a live frame handle.
    unsafe fn copy(&self, frame: H) -> Result<Option<RawFrame>, String> {
        let (api, f) = (&self.api, &self.api.f);
        unsafe {
            let profile = api.call(|e| (f.get_frame_stream_profile)(frame, e))?;
            let (mut s, mut fmt, mut index, mut uid, mut fps) = (0, 0, 0, 0, 0);
            api.call(|e| {
                (f.get_stream_profile_data)(
                    profile, &mut s, &mut fmt, &mut index, &mut uid, &mut fps, e,
                )
            })?;
            let size = api.call(|e| (f.get_frame_data_size)(frame, e))?.max(0) as usize;
            let ptr = api.call(|e| (f.get_frame_data)(frame, e))? as *const u8;
            if ptr.is_null() {
                return Ok(None);
            }
            let data = std::slice::from_raw_parts(ptr, size).to_vec();
            let ts = api.call(|e| (f.get_frame_timestamp)(frame, e))?;
            let domain = api.call(|e| (f.get_frame_timestamp_domain)(frame, e))?;
            // System and global time are milliseconds since the Unix epoch.
            let stamp_ns = (domain != TIMESTAMP_DOMAIN_HARDWARE_CLOCK && ts > 0.0)
                .then_some((ts * 1e6) as u64);
            let mut raw = RawFrame {
                stream: s,
                index,
                format: fmt,
                width: 0,
                height: 0,
                stride: 0,
                stamp_ns,
                intrinsics: None,
                data,
            };
            if api.call(|e| (f.is_frame_extendable_to)(frame, EXTENSION_VIDEO_FRAME, e))? == 1 {
                raw.width = api.call(|e| (f.get_frame_width)(frame, e))?.max(0) as u32;
                raw.height = api.call(|e| (f.get_frame_height)(frame, e))?.max(0) as u32;
                raw.stride = api
                    .call(|e| (f.get_frame_stride_in_bytes)(frame, e))?
                    .max(0) as usize;
                let mut intr = RawIntrinsics::default();
                if api
                    .call(|e| (f.get_video_stream_intrinsics)(profile, &mut intr, e))
                    .is_ok()
                {
                    raw.intrinsics = Some(Intrinsics::from(intr));
                }
            } else if api.call(|e| (f.is_frame_extendable_to)(frame, EXTENSION_MOTION_FRAME, e))?
                != 1
            {
                return Ok(None);
            }
            Ok(Some(raw))
        }
    }

    /// Copies the frames of a frameset (or a single frame) and releases it.
    ///
    /// # Safety
    /// `set` must be a live frame handle owned by the caller.
    unsafe fn take_set(&self, set: H, out: &mut Vec<RawFrame>) -> Result<(), String> {
        let (api, f) = (&self.api, &self.api.f);
        unsafe {
            let r = (|| {
                let n = api.call(|e| (f.embedded_frames_count)(set, e)).unwrap_or(0);
                if n == 0 {
                    // Motion frames can arrive alone, not in a frameset.
                    out.extend(self.copy(set)?);
                }
                for i in 0..n {
                    let fr = api.call(|e| (f.extract_frame)(set, i, e))?;
                    let r = self.copy(fr);
                    (f.release_frame)(fr);
                    out.extend(r?);
                }
                Ok(())
            })();
            (f.release_frame)(set);
            r
        }
    }

    /// Frames that arrived since the last call, one group per frameset
    /// (aligned if configured), then one group of motion samples. Does not
    /// block. Should a frame fail, those taken off the queues before it are
    /// still returned and the error comes with the next call.
    pub fn poll(&mut self) -> Result<Vec<Vec<RawFrame>>, String> {
        if let Some(err) = self.error.take() {
            return Err(err);
        }
        let mut out = Vec::new();
        let r = self
            .poll_sets(&mut out)
            .and_then(|()| self.poll_motion(&mut out));
        match r {
            Err(err) if !out.is_empty() => {
                self.error = Some(err);
                Ok(out)
            }
            r => r.map(|()| out),
        }
    }

    fn poll_sets(&self, out: &mut Vec<Vec<RawFrame>>) -> Result<(), String> {
        let (api, f) = (&self.api, &self.api.f);
        if self.sets.is_null() {
            return Ok(());
        }
        loop {
            let mut set: H = null_mut();
            // SAFETY: live queue; `set` receives a frame we then own.
            let got = api.call(|e| unsafe { (f.poll_for_frame)(self.sets, &mut set, e) })?;
            if got == 0 || set.is_null() {
                return Ok(());
            }
            let mut frames = Vec::new();
            // SAFETY: `set` is ours; align takes a reference of its own.
            unsafe {
                match self.align {
                    Some((block, queue))
                        if api.call(|e| (f.embedded_frames_count)(set, e)).unwrap_or(0) > 1 =>
                    {
                        // rs2_process_frame takes ownership of the frame it is given.
                        api.call(|e| (f.process_frame)(block, set, e))?;
                        let aligned = api.call(|e| (f.wait_for_frame)(queue, 1000, e))?;
                        self.take_set(aligned, &mut frames)?;
                    }
                    _ => self.take_set(set, &mut frames)?,
                }
            }
            out.push(frames);
        }
    }

    fn poll_motion(&self, out: &mut Vec<Vec<RawFrame>>) -> Result<(), String> {
        let (api, f) = (&self.api, &self.api.f);
        let Some((_, queue)) = self.motion else {
            return Ok(());
        };
        let mut samples = Vec::new();
        let r = loop {
            let mut frame: H = null_mut();
            // SAFETY: live queue; `frame` receives a frame we then own.
            let got = match api.call(|e| unsafe { (f.poll_for_frame)(queue, &mut frame, e) }) {
                Ok(got) => got,
                Err(err) => break Err(err),
            };
            if got == 0 || frame.is_null() {
                break Ok(());
            }
            // SAFETY: `frame` is ours and released once.
            let r = unsafe {
                let r = self.copy(frame);
                (f.release_frame)(frame);
                r
            };
            match r {
                Ok(raw) => samples.extend(raw),
                Err(err) => break Err(err),
            }
        };
        if !samples.is_empty() {
            out.push(samples);
        }
        r
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        let f = &self.api.f;
        // SAFETY: every non-null handle is live and deleted once.
        unsafe {
            if let Some((sensor, queue)) = self.motion.take() {
                let _ = self.api.call(|e| (f.sensor_stop)(sensor, e));
                let _ = self.api.call(|e| (f.close)(sensor, e));
                (f.delete_frame_queue)(queue);
                (f.delete_sensor)(sensor);
            }
            if !self.pipe.is_null() && !self.profile.is_null() {
                let _ = self.api.call(|e| (f.pipeline_stop)(self.pipe, e));
            }
            if let Some((block, queue)) = self.align.take() {
                (f.delete_processing_block)(block);
                (f.delete_frame_queue)(queue);
            }
            // After the pipeline stopped: it no longer delivers into `sets`.
            if !self.sets.is_null() {
                (f.delete_frame_queue)(self.sets);
            }
            if !self.profile.is_null() {
                (f.delete_pipeline_profile)(self.profile);
            }
            if !self.pipe.is_null() {
                (f.delete_pipeline)(self.pipe);
            }
            (f.delete_context)(self.ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_library_is_reported_clearly() {
        let err = Api::load(Some("/nonexistent/librealsense2.so"))
            .err()
            .unwrap();
        assert!(err.contains("cannot load librealsense2"), "{err}");
        assert!(err.contains(LIB_ENV), "{err}");
        assert!(err.contains("/nonexistent/librealsense2.so"), "{err}");
    }

    /// Opt-in: loads the real library (RURO_REALSENSE_LIB or the system
    /// one) and, with RURO_TEST_REALSENSE=1, streams from the first camera.
    #[test]
    fn real_library() {
        let api = match Api::load(None) {
            Ok(api) => api,
            Err(e) => {
                eprintln!("skipped: {e}");
                return;
            }
        };
        eprintln!("librealsense2 {}", api.version);
        if std::env::var("RURO_TEST_REALSENSE").is_err() {
            eprintln!("skipped streaming: set RURO_TEST_REALSENSE=1 with a camera attached");
            return;
        }
        let cfg = Config {
            serial: String::new(),
            width: 640,
            height: 480,
            fps: 30,
            color: true,
            depth: true,
            infrared: false,
            imu: false,
            align: true,
        };
        let mut cam = Camera::open(Arc::new(api), &cfg).unwrap();
        eprintln!(
            "{} {} depth scale {}",
            cam.name, cam.serial, cam.depth_scale
        );
        let start = std::time::Instant::now();
        let mut frames = Vec::new();
        while frames.len() < 10 && start.elapsed().as_secs() < 10 {
            frames.extend(cam.poll().unwrap().into_iter().flatten());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!frames.is_empty(), "no frames in 10 s");
    }
}
