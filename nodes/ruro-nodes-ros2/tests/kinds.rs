//! The ROS type to bridge kind table used by tools matches the registered nodes.

#[test]
fn every_bridge_kind_is_registered() {
    ruro_nodes_ros2::link();
    let registry = ruro::node::registry();
    for ty in [
        "geometry_msgs/msg/Twist",
        "geometry_msgs/msg/PoseStamped",
        "sensor_msgs/msg/Image",
        "sensor_msgs/msg/CameraInfo",
        "sensor_msgs/msg/LaserScan",
        "sensor_msgs/msg/Imu",
        "sensor_msgs/msg/JointState",
        "sensor_msgs/msg/Joy",
        "sensor_msgs/msg/PointCloud2",
        "nav_msgs/msg/Odometry",
        "tf2_msgs/msg/TFMessage",
        "std_msgs/msg/Float64",
        "std_msgs/msg/String",
        "std_msgs/msg/Bool",
    ] {
        let (sub, publ) = ruro_ros2::bridge_kinds(ty).unwrap_or_else(|| panic!("{ty} unmapped"));
        assert!(registry.get(sub).is_some(), "{sub} not registered");
        assert!(registry.get(publ).is_some(), "{publ} not registered");
    }
    assert!(ruro_ros2::bridge_kinds("std_msgs/msg/Int8").is_none());
}
