# Hardware drivers

The `ruro-nodes-hw` crate (`nodes/ruro-nodes-hw`) holds the hardware nodes. They are linked
into the standard `ruro-host`, so `ruro nodes` lists them and graphs can use them without a
custom host. By default they are plain Rust with no system libraries: serial ports through the
`serialport` crate (without libudev), everything else through Linux syscalls. Two cargo
features add more: `h264` builds the OpenH264 decoder from its bundled C++ source for
`hw/rtsp-camera`, and `realsense` adds `hw/realsense`, which loads librealsense2 at run time
(see [Optional node features](installation.md#optional-node-features)). Protocol code sits on a
small `Transport` trait, so packet encoding and parsing are unit tested against byte
fixtures, and the serial drivers also run end to end in an executor against simulated
devices on a pseudo-terminal (`nodes/ruro-nodes-hw/tests/graph.rs`).

> **Status: implemented to the protocol specifications, not yet tested on hardware.**
> "Tested" below means tested without the device: byte fixtures, mocks, pseudo-terminals and
> local sockets. Nothing in this crate has been run against the physical hardware it drives
> yet. The exception is server software: `hw/rtsp-camera` has been run against a real RTSP
> server (mediamtx), though not against a camera. Treat first contact with real devices
> accordingly (low speeds, torque limits, a physical emergency stop within reach).

| Node | Device | Messages | Tested without the device | Needs hardware for |
|---|---|---|---|---|
| `hw/v4l2-camera` | UVC/V4L2 cameras | → `std/Image` (RGB8), `std/CameraInfo` | YUYV→RGB against BT.601 colour bars, MJPEG decode (including UVC frames without Huffman tables), ioctl numbers and struct layouts against `<linux/videodev2.h>`, capability query on a real webcam (opt-in) | streaming (`REQBUFS`, `mmap`, `DQBUF`) |
| `hw/mjpeg-http-camera` | IP cameras, ESP32-CAM, mjpg-streamer | → `std/Image`, `std/CameraInfo` | multipart parsing against a local HTTP server, with and without `Content-Length` | real cameras |
| `hw/rtsp-camera` | RTSP network cameras (H.264, Motion JPEG) | → `std/Image`, `std/Bytes` (encoded), `std/CameraInfo` | the node against an in-process RTSP server: TCP and UDP, Basic and Digest auth, keep-alives, reconnection, H.264 from OpenH264's encoder (feature `h264`) and JPEG; MD5 and Digest against the RFC 1321 and RFC 2617 examples. Also run against mediamtx, see [below](#rtsp-cameras) | real cameras |
| `hw/realsense` | Intel RealSense (feature `realsense`) | → `std/Image` (colour, 16-bit depth, infrared), `std/CameraInfo`, `std/PointCloud`, `std/Imu` | frame conversion, depth units, intrinsics to `CameraInfo`, deprojection for each distortion model, IMU pairing, the error without librealsense2 | everything that touches librealsense2 |
| `hw/serial` | any serial port | `std/Bytes` ↔ `std/Bytes` | both directions over a pty | — |
| `hw/can` | SocketCAN (`can0`, `vcan0`) | `hw/CanFrame` ↔ `hw/CanFrame` | frame encoding (standard, extended, RTR, error frames) | a CAN interface (`vcan` loopback test is opt-in) |
| `hw/gpio-out`, `hw/gpio-in` | GPIO character devices (uAPI v2) | `std/Flag` | request/values encoding against `<linux/gpio.h>` | a GPIO chip you may drive |
| `hw/i2c` | `/dev/i2c-N` | `std/Bytes` ↔ `std/Bytes` | `I2C_RDWR` message layout | an I2C device |
| `hw/gamepad` | evdev joysticks and gamepads | → `std/Joy` | event decoding, normalisation, dead zones, ioctl numbers | a gamepad |
| `hw/joint-jog` | (teleoperation helper) | `std/Joy` + `std/JointState` → `std/JointCommand` | unit tests | — |
| `hw/dynamixel` | Robotis X-series, Protocol 2.0 | `std/JointCommand` → `std/JointState` | packets byte-identical to the Robotis SDK and e-Manual examples, CRC, byte stuffing, sync read/write, executor over a pty | servos |
| `hw/so100-arm` | LeRobot SO-100/101 (Feetech STS3215) | `std/JointCommand` → `std/JointState` | packets byte-identical to Feetech's SDK, sign-magnitude registers, half-duplex echo, calibration, executor over a pty | the arm |
| `hw/odrive` | ODrive 0.5/0.6, ASCII protocol | `std/JointCommand` → `std/JointState` | command formatting, checksums (documented example), feedback parsing, executor over a pty | an ODrive |
| `hw/witmotion-imu` | WitMotion WT901 and compatible | → `std/Imu` | frame decoding, checksums, resynchronisation, scale factors | the IMU |
| `hw/rplidar` | Slamtec RPLidar A1/A2/A3 (standard scan) | → `std/LaserScan` | requests, descriptors, sample bit layout, revolution assembly, angle convention | the lidar |
| `hw/livox` | Livox SDK2 lidars (Mid-360, HAP) | → `std/PointCloud` | point packet parsing for all three point formats, UDP reception on localhost | the lidar |

Protocol references are in the module docs. The Dynamixel and Feetech fixtures come
from the vendors' Python SDKs (`nodes/ruro-nodes-hw/tools/servo_fixtures.py`).

## Device access

When the daemon runs as a service, its user needs access to the devices: `dialout` for
serial ports, `video` for cameras, `input` for gamepads, and whatever group owns
`/dev/gpiochip*`, `/dev/i2c-*` on your system (often `gpio` and `i2c`). See
[Deploying on robots](deployment.md#system-service). Hardware nodes are good candidates
for their own process group: a crashing driver is then restarted without disturbing the
rest of the graph.

## RTSP cameras

`hw/rtsp-camera` is an RTSP 1.0 client in plain Rust. It sends `OPTIONS`, `DESCRIBE`,
`SETUP` and `PLAY`, keeps the session alive with `GET_PARAMETER` (or `OPTIONS` if the
server does not offer it) at half the session timeout, and sends `TEARDOWN` on stop. RTP
arrives interleaved on the RTSP connection (`transport: tcp`, the default, which works
through NAT and firewalls) or on a pair of UDP ports (`transport: udp`). Put the login in
the URL, `rtsp://user:password@camera/stream`; Basic and Digest (MD5, with and without
`qop=auth`) are supported. When the stream fails or the server closes it, the node
reconnects after a pause that doubles from 0.25 s up to 10 s. An example graph is
`examples/rtsp-camera.yaml`.

The first H.264 or Motion JPEG video track of the stream is used:

- **H.264** (RFC 6184, packetization modes 0 and 1: single NAL units, STAP-A, FU-A). SPS
  and PPS come from `sprop-parameter-sets` in the SDP or from the stream. Nothing is
  published before the first keyframe, and after packet loss frames are dropped until the
  next one.
- **Motion JPEG** (RFC 2435: types 0 and 1, with or without restart markers; quantization
  tables in band or scaled from Q). The JPEG headers are rebuilt with the standard Huffman
  tables, as the RFC requires. Senders that use optimised tables do not decode; ffmpeg's
  `mjpeg` encoder needs `-huffman default`.

Outputs:

- `encoded` (`std/Bytes`): every frame as received. H.264 frames are Annex B access units
  with SPS and PPS repeated before each keyframe, so a recording decodes from any keyframe
  (`ffmpeg -f h264 -i frames.h264`). JPEG frames are complete JPEG files. Record this
  rather than `image`: it is a small fraction of the size.
- `image` (`std/Image`, RGB8): the newest decoded frame at each step. Motion JPEG uses the
  decoder of the other camera nodes. H.264 needs the `h264` feature; without it the node
  logs a warning and publishes only `encoded`. `decode: false` turns decoding off.
- `camera_info` when `calibration` is set (see [Camera calibration](#camera-calibration)).

H.264 decoding uses [OpenH264](https://github.com/cisco/openh264) through the `openh264`
crate. Both are BSD-2-Clause, which is compatible with ruro's MIT/Apache-2.0 licence; a
binary built with the feature must include OpenH264's copyright notice. The build compiles
Cisco's bundled C++ source (about 15 s) and adds about 1.4 MB to the host. Without `nasm`
on the build machine there is no assembly, and decoding is up to three times slower.
Cisco's H.264 patent licence (through MPEG LA) covers only Cisco's own prebuilt binaries
([binary licence](http://www.openh264.org/BINARY_LICENSE.txt)), not builds from source, so
check what applies before shipping binaries built with `h264`. The `openh264` crate can
also load Cisco's binary at run time; that is not wired up yet.

Not supported yet: H.265 (a stream that offers only H.265 is refused with a message
saying so; it would need an RFC 7798 depacketizer and a decoder), audio, RTSPS and SRTP,
multicast, RTCP receiver reports (mediamtx kept UDP sessions alive on keep-alives alone), and
RTSP 2.0.

## Intel RealSense

`hw/realsense` (feature `realsense`) streams from a RealSense camera through librealsense2's
C API, loaded at run time with `libloading`, so building needs no librealsense. The library
is found through the `library` parameter, then `RURO_REALSENSE_LIB`, then `librealsense2.so`
and versioned copies in the usual library directories. Without it the node fails at start
with a message saying to install librealsense2 or set `RURO_REALSENSE_LIB`. An example
graph is `examples/realsense.yaml`.

> **Not tested with a camera or with a real librealsense2.** The C signatures and enum
> values were checked against the librealsense 2.59 headers, and everything between the
> library's frames and ruro messages is plain Rust with unit tests, but the binding itself
> has never run.

- `color` is RGB8, `infrared` the left imager as `mono8`.
- `depth` is `mono16` (ROS `16UC1`) in millimetres, 0 meaning no measurement, converted from
  the camera's depth unit (1 mm on the D400 series, 0.25 mm on the L515). With `align` on,
  the default, depth is registered to the colour image: it has the colour image's size, and
  `depth_info` carries the colour intrinsics and frame id.
- `color_info`, `depth_info` and `infrared_info` are filled from the stream intrinsics as
  realsense-ros does: `plumb_bob` with five coefficients (`equidistant` with four for
  Kannala-Brandt), R the identity, P = [K | 0]. Frame ids are
  `<frame_prefix>_color_optical_frame`, `_depth_`, `_infra1_` and `_imu_optical_frame`.
- `points` (when `point_step` is above 0) deprojects every n-th depth pixel to metres in the
  optical frame (x right, y down, z forward), with the distortion handling of librealsense's
  `rs2_deproject_pixel_to_point`.
- `imu` (with `enable_imu`) pairs each gyro sample with the latest accelerometer sample.
  Orientation is unknown (covariance -1).

Stamps come from the camera when librealsense maps its clock to host time, otherwise from the
node's clock. Frames are polled at the node rate, so set it to `fps`.

## Camera calibration

`hw/v4l2-camera`, `hw/mjpeg-http-camera` and `hw/rtsp-camera` publish `camera_info`
(`std/CameraInfo`, stamped, with `frame_id`) alongside every image when `calibration` names
a file in the ROS `camera_calibration` format, the YAML that `cameracalibrator.py` writes.
Without a projection matrix, P is [K | 0]. If the file is for a different image size, a
warning is logged once. Calibrate with existing tools (ROS `camera_calibration`, OpenCV);
ruro only loads the result. `hw/realsense` publishes the factory calibration instead.

## Opt-in device tests

Tests that touch real devices only run when their environment variable is set, and
they never configure, stream from or drive the device:

```sh
# Opens the camera only for VIDIOC_QUERYCAP and VIDIOC_ENUM_FMT. No frames are captured.
RURO_TEST_V4L2_QUERY=/dev/video0 cargo test -p ruro-nodes-hw v4l2_query -- --nocapture
# Reads the chip name and line count (read-only; usually needs root or the gpio group).
RURO_TEST_GPIOCHIP=/dev/gpiochip0 cargo test -p ruro-nodes-hw gpio_chip_info -- --nocapture
# Sends and receives one frame on a virtual CAN interface you created as root.
RURO_TEST_VCAN=vcan0 cargo test -p ruro-nodes-hw vcan_loopback
```

Two more read from real sources:

```sh
# Plays the stream over TCP, then UDP, and checks that decoded frames arrive.
RURO_TEST_RTSP_URL=rtsp://127.0.0.1:8554/stream \
  cargo test -p ruro-nodes-hw --features h264 --test rtsp real_server -- --nocapture
# Loads librealsense2 (RURO_REALSENSE_LIB or the system library); with
# RURO_TEST_REALSENSE=1 it also streams colour and aligned depth from the first camera.
cargo test -p ruro-nodes-hw --features realsense real_library -- --nocapture
```

The RTSP test has been run against [mediamtx](https://github.com/bluenviron/mediamtx)
1.21.2, fed by ffmpeg (`testsrc2` encoded with libx264) and by GStreamer
(`videotestsrc ! jpegenc ! rtpjpegpay` into a `udp+rtp` source), with Basic and with Digest
authentication. 30 frames arrived over each transport and decoded, and ffmpeg decoded the
recorded `encoded` output without errors. With `RURO_TEST_RTSP_SECONDS=75` sessions over
both transports outlived the server's 60 s session timeout on keep-alives. The RealSense
test has not been run with the library present.

## Driver notes

- **Units.** Joint positions are radians, velocities rad/s. Dynamixel and SO-100
  positions are relative to tick 2048 (Dynamixel) or the calibration `offsets`
  (SO-100). ODrive works in turns; the node converts.
- **Safety.** `hw/so100-arm` holds its current pose when it enables torque and
  rejects commands that move a joint more than `max_step` radians at once. Servo nodes
  disable torque on stop (`manage_torque`), ODrive axes go idle (`manage_state`).
  On a [software emergency stop](runtime.md#emergency-stop) (`ruro estop`) the servo
  nodes hold their present position with torque enabled, ODrive axes come to rest in
  closed-loop control, and `hw/joint-jog` stops following the sticks; commands are ignored
  until one is sent after the release. This is tested against simulated devices only, so
  keep a hardware e-stop as well.
- **Half-duplex adapters.** The Feetech bus code discards the echo of its own
  packets, which some single-wire adapters return.
- **RPLidar angles** are converted from the device's clockwise convention to the usual
  counter-clockwise one, binned over [-π, π).
- **Livox** needs the lidar configured (Livox Viewer or SDK JSON) to send point data
  to this host, port 56301 for the Mid-360. Packets are accumulated between steps, so
  the node rate sets the integration time.
- **Gamepads** are opened read-only and never grabbed. Your user needs read access to
  `/dev/input/event*` (usually the `input` group).

## Not implemented

- **GStreamer pipelines** and **H.265 over RTSP** (see [RTSP cameras](#rtsp-cameras)).
- **Calibrating** inside ruro: the camera nodes load calibration files but do not make them.
- **RPLidar express/boost scan modes** and **Dynamixel bulk read/write** are not
  implemented; the standard scan and sync read/write cover the common setups.
