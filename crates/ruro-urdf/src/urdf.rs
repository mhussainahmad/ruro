//! URDF parsing (links, joints, geometry, limits, mimic).

use roxmltree::Node;

use crate::math::{self, Iso};
use crate::{
    Error, Geometry, Inertial, Joint, JointType, Limits, Link, Mimic, Result, Robot, Shape, invalid,
};

pub(crate) fn floats(s: &str) -> Result<Vec<f64>> {
    s.split_whitespace()
        .map(|v| {
            v.parse::<f64>()
                .map_err(|_| Error::Invalid(format!("not a number: `{v}`")))
        })
        .collect()
}

pub(crate) fn vec3(s: &str) -> Result<[f64; 3]> {
    let v = floats(s)?;
    v.try_into()
        .map_err(|v: Vec<f64>| Error::Invalid(format!("expected 3 numbers, got {}", v.len())))
}

fn attr_f64(n: Node, name: &str) -> Result<Option<f64>> {
    n.attribute(name)
        .map(|v| {
            v.trim().parse().map_err(|_| {
                Error::Invalid(format!(
                    "<{}> {name}=\"{v}\" is not a number",
                    n.tag_name().name()
                ))
            })
        })
        .transpose()
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.has_tag_name(name))
}

fn origin(n: Node) -> Result<Iso> {
    let Some(o) = child(n, "origin") else {
        return Ok(Iso::IDENTITY);
    };
    let xyz = o
        .attribute("xyz")
        .map(vec3)
        .transpose()?
        .unwrap_or([0.0; 3]);
    let rpy = o
        .attribute("rpy")
        .map(vec3)
        .transpose()?
        .unwrap_or([0.0; 3]);
    Ok(Iso::new(xyz, math::quat_from_rpy(rpy[0], rpy[1], rpy[2])))
}

fn geometry(n: Node) -> Result<Geometry> {
    let g = child(n, "geometry").ok_or_else(|| Error::Invalid("missing <geometry>".into()))?;
    let Some(shape) = g.children().find(|c| c.is_element()) else {
        return invalid("empty <geometry>");
    };
    let need = |a: &str| -> Result<f64> {
        attr_f64(shape, a)?
            .ok_or_else(|| Error::Invalid(format!("<{}> needs `{a}`", shape.tag_name().name())))
    };
    Ok(match shape.tag_name().name() {
        "box" => Geometry::Box {
            size: vec3(shape.attribute("size").unwrap_or("0 0 0"))?,
        },
        "cylinder" => Geometry::Cylinder {
            radius: need("radius")?,
            length: need("length")?,
        },
        "capsule" => Geometry::Capsule {
            radius: need("radius")?,
            length: need("length")?,
        },
        "sphere" => Geometry::Sphere {
            radius: need("radius")?,
        },
        "mesh" => Geometry::Mesh {
            filename: shape.attribute("filename").unwrap_or_default().to_owned(),
            scale: shape
                .attribute("scale")
                .map(vec3)
                .transpose()?
                .unwrap_or([1.0; 3]),
        },
        other => return invalid(format!("unsupported geometry <{other}>")),
    })
}

fn shapes(link: Node, tag: &str) -> Result<Vec<Shape>> {
    link.children()
        .filter(|c| c.has_tag_name(tag))
        .map(|v| {
            Ok(Shape {
                name: v.attribute("name").map(str::to_owned),
                origin: origin(v)?,
                geometry: geometry(v)?,
            })
        })
        .collect()
}

pub(crate) fn parse(xml: &str) -> Result<Robot> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    if !root.has_tag_name("robot") {
        return invalid("URDF root element must be <robot>");
    }
    let mut robot = Robot {
        name: root.attribute("name").unwrap_or_default().to_owned(),
        ..Default::default()
    };
    for l in root.children().filter(|c| c.has_tag_name("link")) {
        let name = l
            .attribute("name")
            .ok_or_else(|| Error::Invalid("<link> without a name".into()))?;
        let inertial = match child(l, "inertial") {
            Some(i) => Some(Inertial {
                origin: origin(i)?,
                mass: child(i, "mass")
                    .map(|m| attr_f64(m, "value"))
                    .transpose()?
                    .flatten()
                    .unwrap_or(0.0),
            }),
            None => None,
        };
        robot.links.push(Link {
            name: name.to_owned(),
            visuals: shapes(l, "visual")?,
            collisions: shapes(l, "collision")?,
            inertial,
        });
    }
    for j in root.children().filter(|c| c.has_tag_name("joint")) {
        let name = j
            .attribute("name")
            .ok_or_else(|| Error::Invalid("<joint> without a name".into()))?;
        let kind = match j.attribute("type").unwrap_or_default() {
            "revolute" => JointType::Revolute,
            "continuous" => JointType::Continuous,
            "prismatic" => JointType::Prismatic,
            "fixed" => JointType::Fixed,
            "floating" => JointType::Floating,
            "planar" => JointType::Planar,
            other => return invalid(format!("joint `{name}` has unknown type `{other}`")),
        };
        let link_of = |tag: &str| -> Result<String> {
            child(j, tag)
                .and_then(|p| p.attribute("link"))
                .map(str::to_owned)
                .ok_or_else(|| Error::Invalid(format!("joint `{name}` has no <{tag} link=...>")))
        };
        let axis = child(j, "axis")
            .and_then(|a| a.attribute("xyz"))
            .map(vec3)
            .transpose()?
            .unwrap_or([1.0, 0.0, 0.0]);
        let limits = match child(j, "limit") {
            Some(l) => Some(Limits {
                lower: attr_f64(l, "lower")?.unwrap_or(0.0),
                upper: attr_f64(l, "upper")?.unwrap_or(0.0),
                effort: attr_f64(l, "effort")?,
                velocity: attr_f64(l, "velocity")?,
            }),
            None if matches!(kind, JointType::Revolute | JointType::Prismatic) => {
                return invalid(format!("{kind:?} joint `{name}` needs <limit>"));
            }
            None => None,
        };
        let mimic = match child(j, "mimic") {
            Some(m) => Some(Mimic {
                joint: m
                    .attribute("joint")
                    .ok_or_else(|| Error::Invalid(format!("<mimic> in `{name}` needs `joint`")))?
                    .to_owned(),
                multiplier: attr_f64(m, "multiplier")?.unwrap_or(1.0),
                offset: attr_f64(m, "offset")?.unwrap_or(0.0),
            }),
            None => None,
        };
        robot.joints.push(Joint {
            name: name.to_owned(),
            kind,
            parent: link_of("parent")?,
            child: link_of("child")?,
            origin: origin(j)?,
            axis: math::normalize(axis),
            anchor: [0.0; 3],
            limits,
            reference: 0.0,
            mimic,
        });
    }
    robot.validate()?;
    Ok(robot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::FRAC_PI_2;

    const ARM: &str = r#"<?xml version="1.0"?>
<robot name="two_link_arm">
  <link name="base_link">
    <visual><geometry><cylinder radius="0.05" length="0.1"/></geometry></visual>
  </link>
  <link name="upper_arm">
    <inertial><origin xyz="0 0 0.25"/><mass value="1.5"/><inertia ixx="1" ixy="0" ixz="0" iyy="1" iyz="0" izz="1"/></inertial>
    <visual>
      <origin xyz="0 0 0.25" rpy="0 0 0"/>
      <geometry><box size="0.04 0.04 0.5"/></geometry>
    </visual>
    <collision><origin xyz="0 0 0.25"/><geometry><capsule radius="0.02" length="0.5"/></geometry></collision>
  </link>
  <link name="forearm">
    <visual><geometry><mesh filename="package://arm/meshes/forearm.stl" scale="0.001 0.001 0.001"/></geometry></visual>
  </link>
  <link name="tool"/>
  <link name="finger"/>
  <joint name="shoulder" type="revolute">
    <parent link="base_link"/><child link="upper_arm"/>
    <origin xyz="0 0 0.1" rpy="0 0 0"/>
    <axis xyz="0 1 0"/>
    <limit lower="-1.57" upper="1.57" effort="10" velocity="2"/>
  </joint>
  <joint name="elbow" type="revolute">
    <parent link="upper_arm"/><child link="forearm"/>
    <origin xyz="0 0 0.5"/>
    <axis xyz="0 1 0"/>
    <limit lower="-2.5" upper="2.5" effort="5" velocity="3"/>
  </joint>
  <joint name="tool_mount" type="fixed">
    <parent link="forearm"/><child link="tool"/>
    <origin xyz="0 0 0.4" rpy="0 0 1.5707963267948966"/>
  </joint>
  <joint name="finger_joint" type="prismatic">
    <parent link="tool"/><child link="finger"/>
    <axis xyz="1 0 0"/>
    <limit lower="0" upper="0.04" effort="1" velocity="0.1"/>
    <mimic joint="elbow" multiplier="0.01" offset="0.005"/>
  </joint>
</robot>"#;

    #[test]
    fn parses_links_joints_and_geometry() {
        let r = parse(ARM).unwrap();
        assert_eq!(r.name, "two_link_arm");
        assert_eq!(r.links.len(), 5);
        assert_eq!(r.root(), Some("base_link"));
        let shoulder = r.joint("shoulder").unwrap();
        assert_eq!(shoulder.kind, JointType::Revolute);
        assert_eq!(shoulder.axis, [0.0, 1.0, 0.0]);
        assert_eq!(
            shoulder.limits,
            Some(Limits {
                lower: -1.57,
                upper: 1.57,
                effort: Some(10.0),
                velocity: Some(2.0)
            })
        );
        let upper = r.link("upper_arm").unwrap();
        assert_eq!(
            upper.visuals[0].geometry,
            Geometry::Box {
                size: [0.04, 0.04, 0.5]
            }
        );
        assert_eq!(
            upper.collisions[0].geometry,
            Geometry::Capsule {
                radius: 0.02,
                length: 0.5
            }
        );
        assert_eq!(upper.inertial.unwrap().mass, 1.5);
        assert!(matches!(
            &r.link("forearm").unwrap().visuals[0].geometry,
            Geometry::Mesh { filename, scale } if filename.ends_with("forearm.stl") && scale[0] == 0.001
        ));
        // Mimic and fixed joints take no position.
        assert_eq!(r.nq(), 2);
        assert_eq!(
            r.active_joints()
                .map(|j| j.name.as_str())
                .collect::<Vec<_>>(),
            ["shoulder", "elbow"]
        );
    }

    #[test]
    fn forward_kinematics_of_the_arm() {
        let r = parse(ARM).unwrap();
        // Straight up.
        let fk = r.forward_kinematics(&[0.0, 0.0]).unwrap();
        let tool = fk.link_pose("tool").unwrap();
        assert!(tool.approx_eq(
            &Iso::new([0.0, 0.0, 1.0], math::quat_from_rpy(0.0, 0.0, FRAC_PI_2)),
            1e-12
        ));

        // Shoulder +90° about Y tips the arm towards +X.
        let fk = r.forward_kinematics(&[FRAC_PI_2, 0.0]).unwrap();
        let t = fk.link_pose("tool").unwrap().translation;
        assert!(
            (t[0] - 0.9).abs() < 1e-12 && (t[2] - 0.1).abs() < 1e-12,
            "{t:?}"
        );

        // Elbow -90°: forearm points back up.
        let fk = r.forward_kinematics(&[FRAC_PI_2, -FRAC_PI_2]).unwrap();
        let t = fk.link_pose("tool").unwrap().translation;
        assert!(
            (t[0] - 0.5).abs() < 1e-12 && (t[2] - 0.5).abs() < 1e-12,
            "{t:?}"
        );

        // Mimic: finger slides 0.01 * elbow + 0.005 along the tool's X.
        let f = fk.link_pose("finger").unwrap();
        let tool = fk.link_pose("tool").unwrap();
        let d = (-FRAC_PI_2) * 0.01 + 0.005;
        let expect = tool.transform_point([d, 0.0, 0.0]);
        assert!((0..3).all(|i| (f.translation[i] - expect[i]).abs() < 1e-12));
        assert_eq!(fk.joints.len(), 4);
        assert!(r.forward_kinematics(&[0.0]).is_err());
    }

    #[test]
    fn diff_drive_base() {
        let xml = r#"<robot name="diffbot">
          <link name="base_footprint"/>
          <link name="base_link"><visual><geometry><box size="0.4 0.3 0.1"/></geometry></visual></link>
          <link name="left_wheel"><visual><origin rpy="1.5707963 0 0"/><geometry><cylinder radius="0.05" length="0.03"/></geometry></visual></link>
          <link name="right_wheel"/>
          <link name="caster"><collision><geometry><sphere radius="0.02"/></geometry></collision></link>
          <joint name="footprint" type="fixed"><parent link="base_footprint"/><child link="base_link"/><origin xyz="0 0 0.05"/></joint>
          <joint name="left_wheel_joint" type="continuous"><parent link="base_link"/><child link="left_wheel"/><origin xyz="0 0.17 0"/><axis xyz="0 1 0"/></joint>
          <joint name="right_wheel_joint" type="continuous"><parent link="base_link"/><child link="right_wheel"/><origin xyz="0 -0.17 0"/><axis xyz="0 1 0"/></joint>
          <joint name="caster_joint" type="fixed"><parent link="base_link"/><child link="caster"/><origin xyz="-0.15 0 -0.03"/></joint>
        </robot>"#;
        let r = parse(xml).unwrap();
        assert_eq!(r.root(), Some("base_footprint"));
        assert_eq!(r.nq(), 2);
        assert!(r.joint("left_wheel_joint").unwrap().limits.is_none());
        let fk = r.forward_kinematics(&[1.0, -1.0]).unwrap();
        let lw = fk.link_pose("left_wheel").unwrap();
        assert_eq!(lw.translation, [0.0, 0.17, 0.05]);
        assert!(lw.approx_eq(
            &Iso::new(
                [0.0, 0.17, 0.05],
                math::quat_from_axis_angle([0.0, 1.0, 0.0], 1.0)
            ),
            1e-12
        ));
        let caster = fk.link_pose("caster").unwrap();
        assert!(caster.approx_eq(&Iso::translation([-0.15, 0.0, 0.02]), 1e-12));
    }

    #[test]
    fn invalid_models_are_rejected() {
        let missing_link = r#"<robot name="x"><link name="a"/>
          <joint name="j" type="fixed"><parent link="a"/><child link="b"/></joint></robot>"#;
        assert!(
            parse(missing_link)
                .unwrap_err()
                .to_string()
                .contains("unknown link `b`")
        );
        let two_roots = r#"<robot name="x"><link name="a"/><link name="b"/></robot>"#;
        assert!(parse(two_roots).is_err());
        let no_limit = r#"<robot name="x"><link name="a"/><link name="b"/>
          <joint name="j" type="revolute"><parent link="a"/><child link="b"/></joint></robot>"#;
        assert!(parse(no_limit).is_err());
        assert!(parse("<robot><link name='a'/><link name='a'/></robot>").is_err());
        assert!(parse("<robot").is_err());
    }
}
