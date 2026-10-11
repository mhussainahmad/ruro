//! The derives as a node author uses them, from outside the workspace's internals.

use ruro::msg::{Message as _, Preview};
use ruro::prelude::*;

#[derive(Message, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[message(name = "test/Detection")]
pub struct Detection {
    pub x: f32,
    pub y: f32,
    pub score: f32,
}

/// Keeps detections above a threshold.
#[derive(Node)]
#[node(kind = "test/threshold")]
pub struct Threshold {
    #[input]
    input: Input<Detection>,
    #[output]
    kept: Output<Detection>,
    /// Minimum score to keep.
    #[param(default = 0.5, min = 0, max = 1)]
    min_score: f32,
    #[param(default = "front")]
    label: String,
    #[param]
    enabled: bool,
    seen: u64,
}

impl Node for Threshold {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(d) = self.input.recv() {
            self.seen += 1;
            if self.enabled && d.score >= self.min_score {
                self.kept.send(d.msg.clone())?;
            }
        }
        Ok(())
    }
}

#[test]
fn custom_message_roundtrips_and_previews() {
    let d = Detection { x: 1.5, y: -2.0, score: 0.9 };
    assert_eq!(Detection::TYPE, "test/Detection");
    assert_eq!(Detection::decode(&d.to_bytes()).unwrap(), d);
    let Preview::Values(v) = d.preview() else { panic!() };
    assert_eq!(v.len(), 3);
}

#[test]
fn node_descriptor_is_generated() {
    use ruro::NodeSpec;
    let desc = Threshold::descriptor();
    assert_eq!(desc.kind, "test/threshold");
    assert_eq!(desc.doc, "Keeps detections above a threshold.");
    assert_eq!(desc.inputs[0].ty, "test/Detection");
    let names: Vec<&str> = desc.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["min_score", "label", "enabled"]);
    assert_eq!(desc.params[1].default, serde_json::json!("front"));
    assert_eq!(desc.params[2].default, serde_json::json!(false));
    assert!(ruro::node::registry().get("test/threshold").is_some());
}

#[test]
fn parameters_are_set_by_name() {
    use ruro::NodeSpec;
    let params = Default::default();
    let mut b = ruro::node::Builder::new("t", &params, Default::default());
    let mut node = Threshold::build(&mut b).unwrap();
    assert_eq!(node.min_score, 0.5);
    node.set_param("min_score", &serde_json::json!(0.25)).unwrap();
    assert_eq!(node.min_score, 0.25);
    assert!(node.set_param("min_score", &serde_json::json!("high")).is_err());
    assert!(node.set_param("nope", &serde_json::json!(1)).is_err());
}
