# ROS 2 bridge

> **Status:** in development. This page describes the planned interface and will be verified
> before release.

The ROS 2 bridge connects a ruro graph to an existing ROS 2 system, in both directions, so you
can adopt ruro one subsystem at a time.

## Planned design

- Bridge nodes subscribe to ROS 2 topics and publish them on ruro outputs, and publish ruro
  inputs as ROS 2 topics.
- Standard ruro messages map to their common ROS 2 counterparts: for example `Image` to
  `sensor_msgs/Image`, `Imu` to `sensor_msgs/Imu`, `LaserScan` to `sensor_msgs/LaserScan`,
  `PointCloud` to `sensor_msgs/PointCloud2`, `JointState` to `sensor_msgs/JointState`,
  `Twist` to `geometry_msgs/Twist`, `Odometry` to `nav_msgs/Odometry` and `Transforms` to
  `tf2_msgs/TFMessage`.
- Bridge nodes are ordinary nodes: they are placed in a graph, given a process group, watched
  and tuned in the studio like any other.
- The bridge talks to ROS 2 over its standard middleware, so ROS 2 nodes need no changes.

The message names, node types and parameters will be documented here once the bridge is
released.
