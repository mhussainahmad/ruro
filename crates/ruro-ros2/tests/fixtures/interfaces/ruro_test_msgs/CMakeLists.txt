cmake_minimum_required(VERSION 3.8)
project(ruro_test_msgs)
find_package(ament_cmake REQUIRED)
find_package(rosidl_default_generators REQUIRED)
find_package(std_msgs REQUIRED)
find_package(geometry_msgs REQUIRED)
find_package(action_msgs REQUIRED)
rosidl_generate_interfaces(${PROJECT_NAME}
  "msg/Primitives.msg" "msg/Arrays.msg" "msg/Nested.msg" "msg/Deep.msg"
  "srv/Lookup.srv" "action/Count.action"
  DEPENDENCIES std_msgs geometry_msgs action_msgs)
ament_package()
