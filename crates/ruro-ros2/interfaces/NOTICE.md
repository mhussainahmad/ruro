# Bundled ROS 2 interface definitions

The `.msg` files under this directory are copied unmodified from the ROS 2
Jazzy Jalisco packages listed below (Debian packages built 2026-09, taken from
`share/<package>/msg/` of each package). They are included so that ruro can
generate message types and compute type hashes without a ROS installation.

| Package | Upstream repository | License |
|---|---|---|
| `builtin_interfaces` | https://github.com/ros2/rcl_interfaces | Apache License 2.0 |
| `std_msgs` | https://github.com/ros2/common_interfaces | Apache License 2.0 |
| `geometry_msgs` | https://github.com/ros2/common_interfaces | Apache License 2.0 |
| `sensor_msgs` | https://github.com/ros2/common_interfaces | Apache License 2.0 |
| `nav_msgs` | https://github.com/ros2/common_interfaces | Apache License 2.0 |
| `tf2_msgs` | https://github.com/ros2/geometry2 | BSD 3-Clause |

Copyright of the definitions belongs to their authors (Open Source Robotics
Foundation, Inc. and contributors). The Apache License 2.0 text is available at
https://www.apache.org/licenses/LICENSE-2.0 and the BSD 3-Clause text in the
geometry2 repository's `LICENSE` file.

`tests/data/jazzy_type_hashes.txt` lists the RIHS01 type hashes published in
the same packages (`share/<package>/msg/<Type>.json`); the test suite checks
that ruro computes exactly these values.
