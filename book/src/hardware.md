# Hardware drivers

The `ruro-nodes-hw` crate (`nodes/ruro-nodes-hw`) holds the hardware nodes. They are
plain Rust with no system libraries: serial ports through the `serialport` crate
(without libudev), everything else through Linux syscalls. Protocol code sits on a
small `Transport` trait, so packet encoding and parsing are unit tested against byte
fixtures, and the serial drivers also run end to end in an executor against simulated
devices on a pseudo-terminal (`nodes/ruro-nodes-hw/tests/graph.rs`).

"Tested here" below means tested without the device: fixtures, mocks, pseudo-terminals,
local sockets. Nothing in this crate has been run against the physical hardware it
drives yet. Treat first contact with real devices accordingly (low speeds, torque
limits, an e-stop within reach).

| Node | Device | Messages | Tested here | Needs hardware for |
|---|---|---|---|---|
| `hw/v4l2-camera` | UVC/V4L2 cameras | → `std/Image` (RGB8) | YUYV→RGB against BT.601 colour bars, MJPEG decode (including UVC frames without Huffman tables), ioctl numbers and struct layouts against `<linux/videodev2.h>`, capability query on a real webcam (opt-in) | streaming (`REQBUFS`, `mmap`, `DQBUF`) |
| `hw/mjpeg-http-camera` | IP cameras, ESP32-CAM, mjpg-streamer | → `std/Image` | multipart parsing against a local HTTP server, with and without `Content-Length` | real cameras |
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

## Driver notes

- **Units.** Joint positions are radians, velocities rad/s. Dynamixel and SO-100
  positions are relative to tick 2048 (Dynamixel) or the calibration `offsets`
  (SO-100). ODrive works in turns; the node converts.
- **Safety.** `hw/so100-arm` holds its current pose when it enables torque and
  rejects commands that move a joint more than `max_step` radians at once. Servo nodes
  disable torque on stop (`manage_torque`), ODrive axes go idle (`manage_state`).
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

- **RTSP / GStreamer cameras.** GStreamer is not available on the build machine and a
  pure-Rust RTSP + H.264 stack is out of scope for now. Many IP cameras also offer an
  MJPEG HTTP stream, which `hw/mjpeg-http-camera` handles.
- **Intel RealSense.** librealsense is not installed here, so a binding could not be
  compiled or tested. Planned approach: load `librealsense2.so` at run time (as the
  MuJoCo node does) behind a cargo feature, publishing `std/Image` colour and depth.
- **RPLidar express/boost scan modes** and **Dynamixel bulk read/write** are not
  implemented; the standard scan and sync read/write cover the common setups.
