//! The structs of `sdk/include/ruro/ruro.h`, field for field.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void};
use std::mem::size_of;

/// `RURO_ABI_VERSION` of this host.
pub const ABI_VERSION: u32 = 1;

macro_rules! slice_view {
    ($name:ident, $t:ty) => {
        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct $name {
            pub ptr: *const $t,
            pub len: usize,
        }
    };
}

slice_view!(ruro_str, c_char);

impl ruro_str {
    pub const EMPTY: Self = Self {
        ptr: std::ptr::null(),
        len: 0,
    };
}
slice_view!(ruro_f64s, f64);
slice_view!(ruro_f32s, f32);
slice_view!(ruro_i32s, i32);
slice_view!(ruro_u8s, u8);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_scalar {
    pub value: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_text {
    pub text: ruro_str,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_flag {
    pub value: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_bytes {
    pub data: ruro_u8s,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_quat {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub w: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_pose {
    pub position: ruro_vec3,
    pub orientation: ruro_quat,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_twist {
    pub linear: ruro_vec3,
    pub angular: ruro_vec3,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_joint_state {
    pub position: ruro_f64s,
    pub velocity: ruro_f64s,
    pub effort: ruro_f64s,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_header {
    pub stamp_ns: u64,
    pub frame_id: ruro_str,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_joint_command {
    pub header: ruro_header,
    pub position: ruro_f64s,
    pub velocity: ruro_f64s,
    pub effort: ruro_f64s,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_transform {
    pub header: ruro_header,
    pub child_frame_id: ruro_str,
    pub translation: ruro_vec3,
    pub rotation: ruro_quat,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_transforms {
    pub ptr: *const ruro_transform,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_imu {
    pub header: ruro_header,
    pub orientation: ruro_quat,
    pub orientation_covariance: [f64; 9],
    pub angular_velocity: ruro_vec3,
    pub angular_velocity_covariance: [f64; 9],
    pub linear_acceleration: ruro_vec3,
    pub linear_acceleration_covariance: [f64; 9],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_laser_scan {
    pub header: ruro_header,
    pub angle_min: f32,
    pub angle_max: f32,
    pub angle_increment: f32,
    pub time_increment: f32,
    pub scan_time: f32,
    pub range_min: f32,
    pub range_max: f32,
    pub ranges: ruro_f32s,
    pub intensities: ruro_f32s,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_odometry {
    pub header: ruro_header,
    pub child_frame_id: ruro_str,
    pub pose: ruro_pose,
    pub twist: ruro_twist,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_camera_info {
    pub header: ruro_header,
    pub width: u32,
    pub height: u32,
    pub distortion_model: ruro_str,
    pub d: ruro_f64s,
    pub k: [f64; 9],
    pub r: [f64; 9],
    pub p: [f64; 12],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_joy {
    pub header: ruro_header,
    pub axes: ruro_f32s,
    pub buttons: ruro_i32s,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_image {
    pub width: u32,
    pub height: u32,
    pub encoding: u32,
    pub data: ruro_u8s,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_point_cloud {
    pub xyz: *const f32,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_sample {
    pub seq: u64,
    pub stamp_ns: u64,
}

pub const RURO_INT: u32 = 0;
pub const RURO_FLOAT: u32 = 1;
pub const RURO_BOOL: u32 = 2;
pub const RURO_STRING: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_value {
    pub kind: u32,
    pub i: i64,
    pub f: f64,
    pub b: bool,
    pub s: ruro_str,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_param_desc {
    pub name: *const c_char,
    pub default_value: ruro_value,
    pub has_min: bool,
    pub min: f64,
    pub has_max: bool,
    pub max: f64,
    pub doc: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_port_desc {
    pub name: *const c_char,
    pub ty: *const c_char,
}

/// Opaque to plugins.
pub enum ruro_builder {}
pub enum ruro_ctx {}
pub enum ruro_input {}
pub enum ruro_output {}
pub enum ruro_registrar {}

pub type CreateFn =
    unsafe extern "C" fn(*const ruro_api, *mut ruro_builder, *mut c_void) -> *mut c_void;
pub type CallFn = unsafe extern "C" fn(*mut c_void, *mut ruro_ctx) -> c_int;
pub type VoidFn = unsafe extern "C" fn(*mut c_void, *mut ruro_ctx);
pub type SetParamFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const ruro_value) -> c_int;
pub type DestroyFn = unsafe extern "C" fn(*mut c_void);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ruro_node_type {
    pub kind: *const c_char,
    pub doc: *const c_char,
    pub rate: f64,
    pub inputs: *const ruro_port_desc,
    pub n_inputs: usize,
    pub outputs: *const ruro_port_desc,
    pub n_outputs: usize,
    pub params: *const ruro_param_desc,
    pub n_params: usize,
    pub create: Option<CreateFn>,
    pub start: Option<CallFn>,
    pub step: Option<CallFn>,
    pub stop: Option<VoidFn>,
    pub set_param: Option<SetParamFn>,
    pub on_estop: Option<VoidFn>,
    pub destroy: Option<DestroyFn>,
    pub type_data: *mut c_void,
}

#[repr(C)]
pub struct ruro_api {
    pub size: u32,
    pub abi_version: u32,
    pub set_error: unsafe extern "C" fn(*const c_char),
    pub log: unsafe extern "C" fn(c_int, *const c_char),
    pub node_id: unsafe extern "C" fn(*mut ruro_builder) -> *const c_char,
    pub input: unsafe extern "C" fn(*mut ruro_builder, *const c_char) -> *mut ruro_input,
    pub output: unsafe extern "C" fn(*mut ruro_builder, *const c_char) -> *mut ruro_output,
    pub param: unsafe extern "C" fn(*mut ruro_builder, *const c_char, *mut ruro_value) -> c_int,
    pub dt: unsafe extern "C" fn(*mut ruro_ctx) -> f64,
    pub time: unsafe extern "C" fn(*mut ruro_ctx) -> f64,
    pub steps: unsafe extern "C" fn(*mut ruro_ctx) -> u64,
    pub now_ns: unsafe extern "C" fn(*mut ruro_ctx) -> u64,
    pub estopped: unsafe extern "C" fn(*mut ruro_ctx) -> bool,
    pub queued: unsafe extern "C" fn(*mut ruro_input) -> usize,
    pub recv: unsafe extern "C" fn(*mut ruro_input, *mut c_void, *mut ruro_sample) -> c_int,
    pub latest: unsafe extern "C" fn(*mut ruro_input, *mut c_void, *mut ruro_sample) -> c_int,
    pub send: unsafe extern "C" fn(*mut ruro_output, *const c_void) -> c_int,
    pub register_node: unsafe extern "C" fn(
        *mut ruro_registrar,
        u32,
        *const u32,
        usize,
        *const ruro_node_type,
    ) -> c_int,
}

/// `RURO_LAYOUT`: the sizes of the structs, in the header's order.
pub const LAYOUT: [u32; 27] = [
    size_of::<ruro_api>() as u32,
    size_of::<ruro_str>() as u32,
    size_of::<ruro_scalar>() as u32,
    size_of::<ruro_text>() as u32,
    size_of::<ruro_flag>() as u32,
    size_of::<ruro_bytes>() as u32,
    size_of::<ruro_vec3>() as u32,
    size_of::<ruro_quat>() as u32,
    size_of::<ruro_pose>() as u32,
    size_of::<ruro_twist>() as u32,
    size_of::<ruro_joint_state>() as u32,
    size_of::<ruro_header>() as u32,
    size_of::<ruro_joint_command>() as u32,
    size_of::<ruro_transform>() as u32,
    size_of::<ruro_transforms>() as u32,
    size_of::<ruro_imu>() as u32,
    size_of::<ruro_laser_scan>() as u32,
    size_of::<ruro_odometry>() as u32,
    size_of::<ruro_camera_info>() as u32,
    size_of::<ruro_joy>() as u32,
    size_of::<ruro_image>() as u32,
    size_of::<ruro_point_cloud>() as u32,
    size_of::<ruro_sample>() as u32,
    size_of::<ruro_value>() as u32,
    size_of::<ruro_param_desc>() as u32,
    size_of::<ruro_port_desc>() as u32,
    size_of::<ruro_node_type>() as u32,
];

/// Names for `LAYOUT`'s entries, for error messages.
pub const LAYOUT_NAMES: [&str; 27] = [
    "ruro_api",
    "ruro_str",
    "ruro_scalar",
    "ruro_text",
    "ruro_flag",
    "ruro_bytes",
    "ruro_vec3",
    "ruro_quat",
    "ruro_pose",
    "ruro_twist",
    "ruro_joint_state",
    "ruro_header",
    "ruro_joint_command",
    "ruro_transform",
    "ruro_transforms",
    "ruro_imu",
    "ruro_laser_scan",
    "ruro_odometry",
    "ruro_camera_info",
    "ruro_joy",
    "ruro_image",
    "ruro_point_cloud",
    "ruro_sample",
    "ruro_value",
    "ruro_param_desc",
    "ruro_port_desc",
    "ruro_node_type",
];
