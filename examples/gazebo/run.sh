#!/bin/bash
# Starts Gazebo Harmonic headless with rover.sdf and ros_gz_bridge for
# /cmd_vel, /odom, /scan and /clock, with rmw_zenoh connected to ROUTER (a
# ruro daemon or rmw_zenohd). Stops both when it is stopped.
#
#   run.sh [ROUTER]            # default tcp/127.0.0.1:7447
#
# Needs ROS 2 Jazzy with ros-jazzy-ros-gz and ros-jazzy-rmw-zenoh-cpp, natively
# or in a container (see Containerfile and README.md). The lidar is rendered,
# so the server uses EGL (--headless-rendering); Mesa's software renderer works.
set -euo pipefail
router="${1:-tcp/127.0.0.1:7447}"
here="$(cd "$(dirname "$0")" && pwd)"

set +u
# shellcheck disable=SC1091
source /opt/ros/jazzy/setup.bash
set -u
export RMW_IMPLEMENTATION=rmw_zenoh_cpp
export ZENOH_CONFIG_OVERRIDE="connect/endpoints=[\"$router\"];scouting/multicast/enabled=false"
# Keep Gazebo's own transport on the loopback interface: with host networking
# its discovery can otherwise pick an interface where the bridge cannot see it.
export GZ_IP="${GZ_IP:-127.0.0.1}"

trap 'kill $(jobs -p) 2>/dev/null; wait' EXIT INT TERM
gz sim -s -r --headless-rendering -v 2 "$here/rover.sdf" &
ros2 run ros_gz_bridge parameter_bridge \
    /cmd_vel@geometry_msgs/msg/Twist]gz.msgs.Twist \
    /odom@nav_msgs/msg/Odometry[gz.msgs.Odometry \
    /scan@sensor_msgs/msg/LaserScan[gz.msgs.LaserScan \
    /clock@rosgraph_msgs/msg/Clock[gz.msgs.Clock &
wait -n
