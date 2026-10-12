# Hardware drivers and simulators

> **Status:** in development. This page describes the planned interface and will be verified
> before release.

The standard library currently contains synthetic sources (`std/fake-camera`, `std/sine`,
`std/fake-arm`) that need no hardware. Node libraries for real devices and for simulators are
being added.

## Planned scope

- **Cameras:** V4L2 (USB/CSI cameras) producing `Image` and `CameraInfo`.
- **Serial and CAN devices:** generic byte-stream nodes plus drivers for common motor
  controllers and IMUs.
- **Input devices:** gamepads producing `Joy`, for teleoperation.
- **Simulators:** bridges to MuJoCo and Gazebo, exposing simulated sensors and actuators with
  the same message types as the real hardware, so a graph can switch between simulation and
  the robot by swapping a few nodes.

Each library will be a separate crate. To use one, link it into a host as described in
[Writing nodes](../writing-nodes.md#building-a-host).

Hardware nodes are good candidates for their own process group: a crashing driver is then
restarted by the daemon without disturbing the rest of the graph. When the daemon runs as a
system service, grant its user access to the devices (see
[Deploying on robots](../deployment.md)).
