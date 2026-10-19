#!/bin/bash
# End-to-end check of the ROS 2 import path against a real ROS 2 Jazzy, run
# inside it: a ruro daemon runs demo_nodes_cpp's talker/listener from their
# launch file through ros2/node, and a ROS 2 bag imported with `ruro bag
# import` is replayed into ros2/sub outputs and published back to ROS.
#
#   cargo build -p ruro-cli -p ruro-host
#   RURO_ROS2_CONTAINER=path/to/run-in-ros.sh scripts/ros2-e2e.sh
#
# RURO_ROS2_CONTAINER runs a shell command in a ROS 2 Jazzy environment with
# rmw_zenoh, host networking and the directory ROS_MOUNT mounted at the same
# path (the binaries built here must run there). Ports 17320-17321 are used.
set -euo pipefail
W=$(cd "$(dirname "$0")/.." && pwd)
if [ "${1:-}" != "--inside" ]; then
  if [ -z "${RURO_ROS2_CONTAINER:-}" ]; then
    echo "RURO_ROS2_CONTAINER is not set" >&2
    exit 2
  fi
  ROS_MOUNT=$W ZENOH_ROUTER=tcp/127.0.0.1:17320 exec "$RURO_ROS2_CONTAINER" "$W/scripts/ros2-e2e.sh --inside"
fi

R=$W/target/debug/ruro
OUT=$W/target/ros2-e2e
mkdir -p "$OUT"
cd "$OUT"
export XDG_RUNTIME_DIR=/tmp/ruro-e2e
mkdir -p $XDG_RUNTIME_DIR
chmod 700 $XDG_RUNTIME_DIR
fail() { echo "FAIL: $*" >&2; exit 1; }

echo "== launch file through ros2/node"
EP=tcp/127.0.0.1:17320
$R daemon --listen $EP --connect $EP --name e2e --host "$W/target/debug/ruro-host" > daemon1.log 2>&1 &
D1=$!
sleep 3
$R ros2 import /opt/ros/jazzy/share/demo_nodes_cpp/launch/topics/talker_listener_launch.xml \
  --router $EP --sub /chatter=std_msgs/msg/String -o graph.yaml
$R --connect $EP --daemon e2e deploy graph.yaml
sleep 8
timeout 5 $R --connect $EP --daemon e2e echo sub_chatter.out > echo1.txt 2>&1 || true
grep -q "data: 'Hello World" echo1.txt || fail "no /chatter in ruro: $(cat echo1.txt)"
timeout 4 $R --connect $EP --daemon e2e logs > logs1.txt 2>&1 || true
grep -q "I heard" logs1.txt || fail "no listener output in the ruro log"
pgrep -f lib/demo_nodes_cpp/talker > /dev/null || fail "talker not running"
$R --connect $EP --daemon e2e stop
sleep 4
if pgrep -f lib/demo_nodes_cpp/talker > /dev/null; then fail "talker still running after stop"; fi
kill $D1
echo "ok"

echo "== bag replay into ros2/sub and back to ROS"
EP=tcp/127.0.0.1:17321
rm -f replay.ruro.mcap
$R bag import "$W/crates/ruro-record/tests/data/jazzy_bag" -o replay.ruro.mcap \
  --map /chatter=chatter.out --map /ints=ints.out
cat > replay.yaml <<YAML
name: replay
nodes:
  chatter:
    type: ros2/sub
    params: { topic: /chatter, type: std_msgs/msg/String, router: "$EP" }
  ints:
    type: ros2/sub
    params:
      topic: /ints
      type: example_interfaces/msg/Int64MultiArray
      interfaces: $W/crates/ruro-ros2/tests/fixtures/interfaces
      router: "$EP"
  decode:
    type: ros2/decode
  republish:
    type: ros2/pub
    params: { topic: /replayed_ints, router: "$EP" }
wires:
  - chatter.out -> decode.input
  - ints.out -> republish.input
YAML
$R daemon --listen $EP --connect $EP --name replay --host "$W/target/debug/ruro-host" > daemon2.log 2>&1 &
D2=$!
sleep 3
$R --connect $EP --daemon replay deploy replay.yaml
sleep 3
(ZENOH_CONFIG_OVERRIDE="connect/endpoints=[\"$EP\"];scouting/multicast/enabled=false" \
  timeout 20 ros2 topic echo --once /replayed_ints example_interfaces/msg/Int64MultiArray > ros_echo.txt 2>&1 &)
(timeout 15 $R --connect $EP --daemon replay echo decode.out > echo2.txt 2>&1 &)
sleep 3
$R --connect $EP --daemon replay play replay.ruro.mcap > /dev/null
sleep 2
grep -q "Hello World: 1" echo2.txt || fail "replayed /chatter not decoded: $(cat echo2.txt)"
grep -q -- "- -2" ros_echo.txt || fail "ROS did not receive the replayed /ints: $(cat ros_echo.txt)"
$R --connect $EP --daemon replay stop
kill $D2
echo "ok"
