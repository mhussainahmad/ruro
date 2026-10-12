//! Talk to ROS 2 nodes that use rmw_zenoh.
//!
//! ```sh
//! # print std_msgs/String messages from a ROS 2 talker
//! cargo run -p ruro-ros2 --example ros2_chat -- listen /chatter
//! # print geometry_msgs/Twist messages, e.g. from teleop
//! cargo run -p ruro-ros2 --example ros2_chat -- twist /cmd_vel
//! # publish std_msgs/String messages for `ros2 topic echo`
//! cargo run -p ruro-ros2 --example ros2_chat -- talk /ruro_chatter
//! ```
//!
//! Set `ROUTER` (default `tcp/127.0.0.1:7447`) and `ROS_DOMAIN_ID` as needed.

use std::time::Duration;

use ruro_ros2::msgs::geometry_msgs::msg::Twist;
use ruro_ros2::msgs::std_msgs::msg::String as RosString;
use ruro_ros2::rmw::{Endpoint, EndpointConfig, Qos};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, topic) = match args.as_slice() {
        [m, t] => (m.as_str(), t.as_str()),
        _ => anyhow::bail!("usage: ros2_chat listen|twist|talk <topic>"),
    };
    let mut cfg = EndpointConfig {
        node_name: "ruro_chat".into(),
        ..Default::default()
    };
    if let Ok(r) = std::env::var("ROUTER") {
        cfg.connect = vec![r];
    }
    let endpoint = Endpoint::open(cfg)?;
    match mode {
        "listen" => {
            let _sub = endpoint.subscriber::<RosString>(topic, Qos::default(), |m, info| {
                println!(
                    "received {:?} {:?}",
                    m.map(|m| m.data),
                    info.map(|i| i.sequence)
                );
            })?;
            std::thread::sleep(Duration::from_secs(3600));
        }
        "twist" => {
            let _sub = endpoint.subscriber::<Twist>(topic, Qos::default(), |m, _| {
                println!("received {m:?}");
            })?;
            std::thread::sleep(Duration::from_secs(3600));
        }
        "talk" => {
            let publisher = endpoint.publisher::<RosString>(topic, Qos::default())?;
            for i in 0.. {
                publisher.publish(&RosString {
                    data: format!("hello from ruro {i}"),
                })?;
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        other => anyhow::bail!("unknown mode {other}"),
    }
    Ok(())
}
