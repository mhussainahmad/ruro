#!/bin/sh
# Runs "$*" in ROS 2 Jazzy (the image from scripts/ros2/Containerfile) with host
# networking, connected to the Zenoh router $ZENOH_ROUTER (default
# tcp/127.0.0.1:7447). This is what RURO_ROS2_CONTAINER expects:
#
#   RURO_ROS2_CONTAINER=$PWD/scripts/ros2/run.sh cargo test -p ruro-ros2 --test ros2_interop
#
# ROS_MOUNT=/dir mounts a host directory at the same path. PODMAN picks another
# command (docker, or podman with storage options) and RURO_ROS2_IMAGE another
# image.
set -e
ROUTER=${ZENOH_ROUTER:-tcp/127.0.0.1:7447}
MOUNT=""
[ -n "$ROS_MOUNT" ] && MOUNT="-v $ROS_MOUNT:$ROS_MOUNT:z"
# shellcheck disable=SC2086 # PODMAN and MOUNT are word lists.
exec ${PODMAN:-podman} run --rm --network host --ipc host $MOUNT \
  -e RMW_IMPLEMENTATION=rmw_zenoh_cpp -e ROS_DOMAIN_ID="${ROS_DOMAIN_ID:-0}" \
  -e ZENOH_CONFIG_OVERRIDE="connect/endpoints=[\"$ROUTER\"];scouting/multicast/enabled=false" \
  "${RURO_ROS2_IMAGE:-ruro-ros2-jazzy}" bash -c "source /opt/ros/jazzy/setup.bash; $*"
