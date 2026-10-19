# Test interface definitions

`ruro_test_msgs` is written for ruro's tests (same license as ruro). It is a
buildable ROS 2 package: `../data/cdr_fixtures.jsonl` was produced by building it
in ROS 2 Jazzy and running `../gen_cdr_fixtures.py`.

The other directories hold `.msg` files copied unmodified from `share/<package>/msg/`
of the ROS 2 Jazzy Jalisco Debian packages, so that tests can decode types that are
not bundled with ruro without a ROS installation.

| Package | Upstream repository | License |
|---|---|---|
| `rcl_interfaces` | https://github.com/ros2/rcl_interfaces | Apache License 2.0 |
| `service_msgs` | https://github.com/ros2/rcl_interfaces | Apache License 2.0 |
| `example_interfaces` | https://github.com/ros2/example_interfaces | Apache License 2.0 |
| `diagnostic_msgs` | https://github.com/ros2/common_interfaces | Apache License 2.0 |
| `shape_msgs` | https://github.com/ros2/common_interfaces | Apache License 2.0 |
| `trajectory_msgs` | https://github.com/ros2/common_interfaces | Apache License 2.0 |
| `visualization_msgs` | https://github.com/ros2/common_interfaces | Apache License 2.0 |
| `unique_identifier_msgs` | https://github.com/ros2/unique_identifier_msgs | BSD 3-Clause |

Copyright of the definitions belongs to their authors (Open Source Robotics
Foundation, Inc. and contributors). The Apache License 2.0 text is available at
https://www.apache.org/licenses/LICENSE-2.0; the BSD 3-Clause text is in the
`LICENSE` file of the unique_identifier_msgs repository.
