# Bundled ROS 2 interface definitions

The `.msg`, `.srv` and `.action` files under this directory are copied
unmodified from the ROS 2 Jazzy Jalisco packages listed below (Debian packages
built 2026-09, taken from `share/<package>/{msg,srv,action}/` of each package).
They are included so that ruro can generate message, service and action types
and compute type hashes without a ROS installation.

| Package | Version | Upstream repository | License |
|---|---|---|---|
| `builtin_interfaces` | 2.0.4 | https://github.com/ros2/rcl_interfaces | Apache License 2.0 |
| `service_msgs` | 2.0.4 | https://github.com/ros2/rcl_interfaces | Apache License 2.0 |
| `action_msgs` | 2.0.4 | https://github.com/ros2/rcl_interfaces | Apache License 2.0 |
| `unique_identifier_msgs` | 2.5.1 | https://github.com/ros2/unique_identifier_msgs | BSD 3-Clause |
| `std_msgs` | 5.3.8 | https://github.com/ros2/common_interfaces | Apache License 2.0 |
| `std_srvs` | 5.3.8 | https://github.com/ros2/common_interfaces | Apache License 2.0 |
| `geometry_msgs` | 5.3.8 | https://github.com/ros2/common_interfaces | Apache License 2.0 |
| `sensor_msgs` | 5.3.8 | https://github.com/ros2/common_interfaces | Apache License 2.0 |
| `nav_msgs` | 5.3.8 | https://github.com/ros2/common_interfaces | Apache License 2.0 |
| `example_interfaces` | 0.12.1 | https://github.com/ros2/example_interfaces | Apache License 2.0 |
| `tf2_msgs` | 0.36.23 | https://github.com/ros2/geometry2 | BSD 3-Clause |
| `rosgraph_msgs` | 2.0.4 | https://github.com/ros2/rcl_interfaces | Apache License 2.0 |
| `rcl_interfaces` | 2.0.4 | https://github.com/ros2/rcl_interfaces | Apache License 2.0 |
| `type_description_interfaces` | 2.0.4 | https://github.com/ros2/rcl_interfaces | Apache License 2.0 |

Copyright of the definitions belongs to their authors (Open Source Robotics
Foundation, Inc. and contributors). The Apache License 2.0 text is available at
https://www.apache.org/licenses/LICENSE-2.0 and the BSD 3-Clause texts in the
`LICENSE` files of the geometry2 and unique_identifier_msgs repositories.

`tests/data/jazzy_type_hashes.txt` lists the RIHS01 type hashes published in
the same packages (`share/<package>/{msg,srv,action}/<Type>.json`); the test
suite checks that ruro computes exactly these values, including the hashes of
service and action types.
