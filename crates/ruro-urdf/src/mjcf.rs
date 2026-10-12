//! MJCF (MuJoCo XML) parsing: the kinematic tree, joints, geoms and defaults.
//!
//! Supported: `<compiler angle eulerseq meshdir>`, nested `<default>` classes
//! with `class`/`childclass`, `<body>`, `<frame>`, `<joint>` (hinge, slide,
//! ball, free), `<freejoint>`, `<geom>` (all primitive types, `fromto`,
//! meshes from `<asset>`), `<inertial>`, and every orientation form (`quat`,
//! `axisangle`, `euler`, `xyaxes`, `zaxis`). `<include>`, `<replicate>`,
//! attachments and composites are not expanded.

use std::collections::HashMap;

use roxmltree::Node;

use crate::math::{self, Iso};
use crate::urdf::floats;
use crate::{
    Error, Geometry, Inertial, Joint, JointType, Limits, Link, Result, Robot, Shape, invalid,
};

type Attrs = HashMap<String, String>;

struct Compiler {
    degrees: bool,
    eulerseq: Vec<char>,
    meshdir: String,
}

struct Parser {
    compiler: Compiler,
    /// class -> element tag -> attributes
    defaults: HashMap<String, HashMap<String, Attrs>>,
    meshes: HashMap<String, (String, [f64; 3])>,
    robot: Robot,
    unnamed: usize,
}

fn collect_defaults(
    n: Node,
    parent: Option<&HashMap<String, Attrs>>,
    out: &mut HashMap<String, HashMap<String, Attrs>>,
) {
    let class = n.attribute("class").unwrap_or("main").to_owned();
    let mut mine = parent.cloned().unwrap_or_default();
    for c in n
        .children()
        .filter(|c| c.is_element() && !c.has_tag_name("default"))
    {
        let e = mine.entry(c.tag_name().name().to_owned()).or_default();
        for a in c.attributes() {
            e.insert(a.name().to_owned(), a.value().to_owned());
        }
    }
    for c in n.children().filter(|c| c.has_tag_name("default")) {
        collect_defaults(c, Some(&mine), out);
    }
    out.insert(class, mine);
}

impl Parser {
    /// Attribute of `n`, falling back to its default class.
    fn attr(&self, n: Node, class: &str, name: &str) -> Option<String> {
        if let Some(v) = n.attribute(name) {
            return Some(v.to_owned());
        }
        let class = n.attribute("class").unwrap_or(class);
        self.defaults
            .get(class)
            .and_then(|d| d.get(n.tag_name().name()))
            .and_then(|a| a.get(name))
            .cloned()
    }

    fn nums(&self, n: Node, class: &str, name: &str) -> Result<Option<Vec<f64>>> {
        self.attr(n, class, name)
            .map(|s| {
                floats(&s)
                    .map_err(|e| Error::Invalid(format!("<{}> {name}: {e}", n.tag_name().name())))
            })
            .transpose()
    }

    fn angle(&self, v: f64) -> f64 {
        if self.compiler.degrees {
            v.to_radians()
        } else {
            v
        }
    }

    fn orientation(&self, n: Node, class: &str) -> Result<[f64; 4]> {
        if let Some(q) = self.nums(n, class, "quat")? {
            let [w, x, y, z]: [f64; 4] = q
                .try_into()
                .map_err(|_| Error::Invalid("quat needs 4 numbers".into()))?;
            return Ok(math::quat_normalize([x, y, z, w]));
        }
        if let Some(a) = self.nums(n, class, "axisangle")? {
            if a.len() != 4 {
                return invalid("axisangle needs 4 numbers");
            }
            return Ok(math::quat_from_axis_angle(
                [a[0], a[1], a[2]],
                self.angle(a[3]),
            ));
        }
        if let Some(e) = self.nums(n, class, "euler")? {
            if e.len() != 3 || self.compiler.eulerseq.len() != 3 {
                return invalid("euler needs 3 angles and a 3-letter eulerseq");
            }
            let mut q = [0.0, 0.0, 0.0, 1.0];
            for (c, &v) in self.compiler.eulerseq.iter().zip(&e) {
                let axis = match c.to_ascii_lowercase() {
                    'x' => [1.0, 0.0, 0.0],
                    'y' => [0.0, 1.0, 0.0],
                    'z' => [0.0, 0.0, 1.0],
                    other => return invalid(format!("bad eulerseq axis `{other}`")),
                };
                let r = math::quat_from_axis_angle(axis, self.angle(v));
                // Lowercase: axes move with the frame (intrinsic); uppercase: fixed.
                q = if c.is_ascii_lowercase() {
                    math::quat_mul(q, r)
                } else {
                    math::quat_mul(r, q)
                };
            }
            return Ok(math::quat_normalize(q));
        }
        if let Some(v) = self.nums(n, class, "xyaxes")? {
            if v.len() != 6 {
                return invalid("xyaxes needs 6 numbers");
            }
            let x = math::normalize([v[0], v[1], v[2]]);
            let y0 = [v[3], v[4], v[5]];
            let d = math::dot(x, y0);
            let y = math::normalize([y0[0] - d * x[0], y0[1] - d * x[1], y0[2] - d * x[2]]);
            let z = math::cross(x, y);
            return Ok(math::quat_from_matrix([
                [x[0], y[0], z[0]],
                [x[1], y[1], z[1]],
                [x[2], y[2], z[2]],
            ]));
        }
        if let Some(z) = self.nums(n, class, "zaxis")? {
            if z.len() != 3 {
                return invalid("zaxis needs 3 numbers");
            }
            return Ok(math::quat_z_to([z[0], z[1], z[2]]));
        }
        Ok([0.0, 0.0, 0.0, 1.0])
    }

    fn frame(&self, n: Node, class: &str) -> Result<Iso> {
        let pos = match self.nums(n, class, "pos")? {
            Some(p) if p.len() == 3 => [p[0], p[1], p[2]],
            Some(_) => return invalid("pos needs 3 numbers"),
            None => [0.0; 3],
        };
        Ok(Iso::new(pos, self.orientation(n, class)?))
    }

    fn geom(&self, n: Node, class: &str, outer: &Iso) -> Result<Shape> {
        let ty = self
            .attr(n, class, "type")
            .unwrap_or_else(|| "sphere".into());
        let size = self.nums(n, class, "size")?.unwrap_or_default();
        let s = |i: usize| size.get(i).copied().unwrap_or(0.0);
        let mut origin = self.frame(n, class)?;
        let mut half_len = s(1);
        if let Some(ft) = self.nums(n, class, "fromto")? {
            if ft.len() != 6 {
                return invalid("fromto needs 6 numbers");
            }
            let (a, b) = ([ft[0], ft[1], ft[2]], [ft[3], ft[4], ft[5]]);
            let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            half_len = math::norm(d) / 2.0;
            origin = Iso::new(
                [
                    (a[0] + b[0]) / 2.0,
                    (a[1] + b[1]) / 2.0,
                    (a[2] + b[2]) / 2.0,
                ],
                math::quat_z_to(d),
            );
        }
        let geometry = match ty.as_str() {
            "sphere" => Geometry::Sphere { radius: s(0) },
            "capsule" => Geometry::Capsule {
                radius: s(0),
                length: 2.0 * half_len,
            },
            "cylinder" => Geometry::Cylinder {
                radius: s(0),
                length: 2.0 * half_len,
            },
            "ellipsoid" => Geometry::Ellipsoid {
                radii: [s(0), s(1), s(2)],
            },
            "box" => Geometry::Box {
                size: [2.0 * s(0), 2.0 * s(1), 2.0 * s(2)],
            },
            "plane" => Geometry::Plane,
            "mesh" => {
                let name = self.attr(n, class, "mesh").unwrap_or_default();
                let (file, scale) = self
                    .meshes
                    .get(&name)
                    .cloned()
                    .unwrap_or((name.clone(), [1.0; 3]));
                let filename = if self.compiler.meshdir.is_empty() {
                    file
                } else {
                    format!("{}/{file}", self.compiler.meshdir.trim_end_matches('/'))
                };
                Geometry::Mesh { filename, scale }
            }
            other => return invalid(format!("unsupported geom type `{other}`")),
        };
        Ok(Shape {
            name: n.attribute("name").map(str::to_owned),
            origin: outer.mul(&origin),
            geometry,
        })
    }

    fn joint(
        &mut self,
        n: Node,
        class: &str,
        body: &str,
        parent: &str,
        child: &str,
        origin: Iso,
    ) -> Result<()> {
        let free = n.has_tag_name("freejoint");
        let ty = if free {
            "free".to_owned()
        } else {
            self.attr(n, class, "type")
                .unwrap_or_else(|| "hinge".into())
        };
        let kind = match ty.as_str() {
            "hinge" => JointType::Revolute,
            "slide" => JointType::Prismatic,
            "ball" => JointType::Ball,
            "free" => JointType::Floating,
            other => return invalid(format!("unknown joint type `{other}`")),
        };
        let axis = match self.nums(n, class, "axis")? {
            Some(a) if a.len() == 3 => math::normalize([a[0], a[1], a[2]]),
            Some(_) => return invalid("axis needs 3 numbers"),
            None => [0.0, 0.0, 1.0],
        };
        let anchor = match self.nums(n, class, "pos")? {
            Some(p) if p.len() == 3 && !free => [p[0], p[1], p[2]],
            _ => [0.0; 3],
        };
        let angular = kind == JointType::Revolute;
        let conv = |v: f64| if angular { self.angle(v) } else { v };
        let range = self.nums(n, class, "range")?;
        let limited = self
            .attr(n, class, "limited")
            .unwrap_or_else(|| "auto".into());
        let limits = match (range, limited.as_str()) {
            (Some(r), "true" | "auto") if r.len() == 2 && r[0] < r[1] => Some(Limits {
                lower: conv(r[0]),
                upper: conv(r[1]),
                effort: None,
                velocity: None,
            }),
            _ => None,
        };
        let kind = if kind == JointType::Revolute && limits.is_none() {
            JointType::Continuous
        } else {
            kind
        };
        // A free joint's position is the body pose in the world frame; the
        // body's own pos/quat only set the initial value.
        let origin = if kind == JointType::Floating {
            Iso::IDENTITY
        } else {
            origin
        };
        let reference = self
            .nums(n, class, "ref")?
            .and_then(|r| r.first().copied())
            .map_or(0.0, conv);
        let name = match n.attribute("name") {
            Some(s) => s.to_owned(),
            None => {
                self.unnamed += 1;
                format!("{body}/joint{}", self.unnamed)
            }
        };
        self.robot.joints.push(Joint {
            name,
            kind,
            parent: parent.to_owned(),
            child: child.to_owned(),
            origin,
            axis,
            anchor,
            limits,
            reference,
            mimic: None,
        });
        Ok(())
    }

    /// Visits the children of a body (or of the worldbody / a frame).
    fn children(&mut self, n: Node, link: &str, class: &str, outer: Iso) -> Result<()> {
        for c in n.children().filter(|c| c.is_element()) {
            match c.tag_name().name() {
                "body" => self.body(c, link, class, &outer)?,
                "frame" => {
                    let fc = c.attribute("childclass").unwrap_or(class).to_owned();
                    let f = outer.mul(&self.frame(c, class)?);
                    self.children(c, link, &fc, f)?;
                }
                "geom" => {
                    let g = self.geom(c, class, &outer)?;
                    let group = self
                        .attr(c, class, "group")
                        .and_then(|g| g.parse::<i32>().ok());
                    let contype = self
                        .attr(c, class, "contype")
                        .and_then(|g| g.parse::<i32>().ok());
                    let l = self
                        .robot
                        .links
                        .iter_mut()
                        .find(|l| l.name == link)
                        .unwrap();
                    // Geoms render and collide; groups 3+ are conventionally collision-only.
                    if group.unwrap_or(0) < 3 {
                        l.visuals.push(g.clone());
                    }
                    if contype != Some(0) {
                        l.collisions.push(g);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn body(&mut self, n: Node, parent: &str, class: &str, outer: &Iso) -> Result<()> {
        let class = n.attribute("childclass").unwrap_or(class).to_owned();
        let name = match n.attribute("name") {
            Some(s) => s.to_owned(),
            None => {
                self.unnamed += 1;
                format!("body{}", self.unnamed)
            }
        };
        let origin = outer.mul(&self.frame(n, &class)?);
        let joints: Vec<Node> = n
            .children()
            .filter(|c| c.has_tag_name("joint") || c.has_tag_name("freejoint"))
            .collect();
        let inertial = match n.children().find(|c| c.has_tag_name("inertial")) {
            Some(i) => Some(Inertial {
                origin: self.frame(i, &class)?,
                mass: self
                    .nums(i, &class, "mass")?
                    .and_then(|m| m.first().copied())
                    .unwrap_or(0.0),
            }),
            None => None,
        };

        if joints.is_empty() {
            self.robot.links.push(Link {
                name: name.clone(),
                inertial,
                ..Default::default()
            });
            self.robot.joints.push(Joint {
                name: format!("{name}/weld"),
                kind: JointType::Fixed,
                parent: parent.to_owned(),
                child: name.clone(),
                origin,
                axis: [0.0, 0.0, 1.0],
                anchor: [0.0; 3],
                limits: None,
                reference: 0.0,
                mimic: None,
            });
        } else {
            // Chain the joints through intermediate links coincident with the body frame.
            let mut prev = parent.to_owned();
            for (k, j) in joints.iter().enumerate() {
                let last = k + 1 == joints.len();
                let child = if last {
                    name.clone()
                } else {
                    format!("{name}/{}", j.attribute("name").unwrap_or("joint"))
                };
                self.robot.links.push(Link {
                    name: child.clone(),
                    inertial: if last { inertial } else { None },
                    ..Default::default()
                });
                let o = if k == 0 { origin } else { Iso::IDENTITY };
                self.joint(*j, &class, &name, &prev, &child, o)?;
                prev = child;
            }
        }
        self.children(n, &name, &class, Iso::IDENTITY)
    }
}

pub(crate) fn parse(xml: &str) -> Result<Robot> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    if !root.has_tag_name("mujoco") {
        return invalid("MJCF root element must be <mujoco>");
    }
    let compiler = root.children().find(|c| c.has_tag_name("compiler"));
    let get = |a: &str| compiler.and_then(|c| c.attribute(a));
    let mut p = Parser {
        compiler: Compiler {
            degrees: get("angle").unwrap_or("degree") != "radian",
            eulerseq: get("eulerseq").unwrap_or("xyz").chars().collect(),
            meshdir: get("meshdir").unwrap_or_default().to_owned(),
        },
        defaults: HashMap::new(),
        meshes: HashMap::new(),
        robot: Robot {
            name: root.attribute("model").unwrap_or_default().to_owned(),
            ..Default::default()
        },
        unnamed: 0,
    };
    for d in root.children().filter(|c| c.has_tag_name("default")) {
        collect_defaults(d, None, &mut p.defaults);
    }
    for a in root.children().filter(|c| c.has_tag_name("asset")) {
        for m in a.children().filter(|c| c.has_tag_name("mesh")) {
            let file = m.attribute("file").unwrap_or_default().to_owned();
            let name = m.attribute("name").map(str::to_owned).unwrap_or_else(|| {
                // MuJoCo names unnamed meshes after the file stem.
                std::path::Path::new(&file)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            let scale = match m.attribute("scale").map(floats).transpose()? {
                Some(s) if s.len() == 3 => [s[0], s[1], s[2]],
                _ => [1.0; 3],
            };
            p.meshes.insert(name, (file, scale));
        }
    }
    p.robot.links.push(Link {
        name: "world".into(),
        ..Default::default()
    });
    for wb in root.children().filter(|c| c.has_tag_name("worldbody")) {
        p.children(wb, "world", "main", Iso::IDENTITY)?;
    }
    p.robot.validate()?;
    Ok(p.robot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, PI};

    const PENDULUM: &str = r#"
<mujoco model="pendulum">
  <compiler angle="degree"/>
  <option timestep="0.001" integrator="RK4"/>
  <default>
    <joint damping="0"/>
    <default class="arm">
      <geom type="capsule" size="0.02" rgba="0.8 0.3 0.3 1"/>
    </default>
  </default>
  <worldbody>
    <geom name="floor" type="plane" size="1 1 0.1"/>
    <body name="pole" pos="0 0 1" childclass="arm">
      <joint name="hinge" type="hinge" axis="0 1 0" range="-90 90"/>
      <geom fromto="0 0 0 0 0 -0.5"/>
      <body name="bob" pos="0 0 -0.5" euler="0 0 90">
        <geom type="sphere" size="0.05"/>
      </body>
    </body>
  </worldbody>
  <actuator><motor joint="hinge" gear="1"/></actuator>
</mujoco>"#;

    #[test]
    fn parses_bodies_joints_and_defaults() {
        let r = parse(PENDULUM).unwrap();
        assert_eq!(r.name, "pendulum");
        assert_eq!(r.root(), Some("world"));
        let hinge = r.joint("hinge").unwrap();
        assert_eq!(hinge.kind, JointType::Revolute);
        assert_eq!(hinge.axis, [0.0, 1.0, 0.0]);
        let lim = hinge.limits.unwrap();
        assert!((lim.lower + FRAC_PI_2).abs() < 1e-12 && (lim.upper - FRAC_PI_2).abs() < 1e-12);
        assert_eq!(hinge.origin.translation, [0.0, 0.0, 1.0]);
        // Default class `arm` makes the fromto geom a capsule.
        let pole = r.link("pole").unwrap();
        let g = &pole.visuals[0];
        assert_eq!(
            g.geometry,
            Geometry::Capsule {
                radius: 0.02,
                length: 0.5
            }
        );
        assert!((g.origin.translation[2] + 0.25).abs() < 1e-12);
        assert_eq!(
            r.link("world").unwrap().visuals[0].geometry,
            Geometry::Plane
        );
        assert_eq!(r.nq(), 1);
    }

    #[test]
    fn pendulum_kinematics() {
        let r = parse(PENDULUM).unwrap();
        let fk = r.forward_kinematics(&[FRAC_PI_2]).unwrap();
        // Rotating +90° about Y swings the bob from -Z to -X.
        let bob = fk.link_pose("bob").unwrap();
        assert!((bob.translation[0] + 0.5).abs() < 1e-12, "{bob:?}");
        assert!((bob.translation[2] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn orientation_forms_agree() {
        let xml = r#"<mujoco><compiler angle="radian"/><worldbody>
            <body name="q" quat="0.7071067811865476 0 0 0.7071067811865476"/>
            <body name="aa" axisangle="0 0 1 1.5707963267948966"/>
            <body name="eu" euler="0 0 1.5707963267948966"/>
            <body name="xy" xyaxes="0 1 0 -1 0 0"/>
            <body name="za" zaxis="0 0 1"/>
          </worldbody></mujoco>"#;
        let r = parse(xml).unwrap();
        let fk = r.forward_kinematics(&[]).unwrap();
        let yaw90 = Iso::rotation(math::quat_from_axis_angle([0.0, 0.0, 1.0], FRAC_PI_2));
        for b in ["q", "aa", "eu", "xy"] {
            assert!(fk.link_pose(b).unwrap().approx_eq(&yaw90, 1e-9), "{b}");
        }
        assert!(fk.link_pose("za").unwrap().approx_eq(&Iso::IDENTITY, 1e-12));
    }

    #[test]
    fn euler_sequences() {
        // Intrinsic xyz: R = Rx(a) Ry(b) Rz(c). Extrinsic XYZ: R = Rz(c) Ry(b) Rx(a).
        let make = |seq: &str| {
            let xml = format!(
                r#"<mujoco><compiler angle="radian" eulerseq="{seq}"/><worldbody>
                <body name="b" euler="0.3 -0.4 1.2"/></worldbody></mujoco>"#
            );
            *parse(&xml)
                .unwrap()
                .forward_kinematics(&[])
                .unwrap()
                .link_pose("b")
                .unwrap()
        };
        let rx = math::quat_from_axis_angle([1.0, 0.0, 0.0], 0.3);
        let ry = math::quat_from_axis_angle([0.0, 1.0, 0.0], -0.4);
        let rz = math::quat_from_axis_angle([0.0, 0.0, 1.0], 1.2);
        let intrinsic = Iso::rotation(math::quat_mul(math::quat_mul(rx, ry), rz));
        let extrinsic = Iso::rotation(math::quat_mul(math::quat_mul(rz, ry), rx));
        assert!(make("xyz").approx_eq(&intrinsic, 1e-12));
        assert!(make("XYZ").approx_eq(&extrinsic, 1e-12));
        // Fixed-axis XYZ equals URDF rpy.
        assert!(make("XYZ").approx_eq(&Iso::rotation(math::quat_from_rpy(0.3, -0.4, 1.2)), 1e-12));
    }

    #[test]
    fn multiple_joints_free_ball_and_anchors() {
        let xml = r#"<mujoco><compiler angle="radian"/><worldbody>
            <body name="floating" pos="0 0 2"><freejoint name="root"/>
              <body name="gimbal" pos="1 0 0">
                <joint name="yaw" axis="0 0 1"/>
                <joint name="pitch" axis="0 1 0" range="-1 1"/>
                <body name="ballend" pos="0 0 -1"><joint name="sph" type="ball"/></body>
              </body>
              <body name="door" pos="0 1 0"><joint name="swing" axis="0 0 1" pos="0.5 0 0"/></body>
              <body name="slider"><joint name="rail" type="slide" axis="1 0 0" range="0 1"/></body>
            </body></worldbody></mujoco>"#;
        let r = parse(xml).unwrap();
        assert_eq!(
            r.active_joints()
                .map(|j| j.name.as_str())
                .collect::<Vec<_>>(),
            ["root", "yaw", "pitch", "sph", "swing", "rail"]
        );
        assert_eq!(r.nq(), 7 + 1 + 1 + 4 + 1 + 1);
        assert_eq!(r.joint("yaw").unwrap().kind, JointType::Continuous);
        assert_eq!(r.joint("yaw").unwrap().child, "gimbal/yaw");
        assert_eq!(r.joint("pitch").unwrap().parent, "gimbal/yaw");

        // qpos in MuJoCo order: free (pos, wxyz quat), yaw, pitch, ball (wxyz), swing, rail.
        let s = (PI / 4.0).sin();
        let c = (PI / 4.0).cos();
        let q = [
            0.0, 0.0, 1.0, c, 0.0, 0.0, s, // root: lifted 1 m, yawed 90°
            0.0, 0.0, 1.0, 0.0, 0.0, 0.0, // yaw, pitch, ball identity
            PI, 0.25, // door swung 180° about its hinge line, slider out 0.25
        ];
        let fk = r.forward_kinematics(&q).unwrap();
        let root = fk.link_pose("floating").unwrap();
        assert!(root.approx_eq(&Iso::new([0.0, 0.0, 1.0], [0.0, 0.0, s, c]), 1e-12));
        // gimbal at body offset (1,0,0) rotated by the root's 90° yaw -> (0,1,1).
        let g = fk.link_pose("gimbal").unwrap().translation;
        assert!((g[0]).abs() < 1e-12 && (g[1] - 1.0).abs() < 1e-12 && (g[2] - 1.0).abs() < 1e-12);
        // Door hinge 0.5 m along the door's x: swinging 180° moves its origin by 1 m.
        let door_local = root.inverse().mul(fk.link_pose("door").unwrap());
        assert!(
            (door_local.translation[0] - 1.0).abs() < 1e-12,
            "{door_local:?}"
        );
        assert!((door_local.translation[1] - 1.0).abs() < 1e-12);
        let slider_local = root.inverse().mul(fk.link_pose("slider").unwrap());
        assert!((slider_local.translation[0] - 0.25).abs() < 1e-12);
    }

    #[test]
    fn meshes_and_frames() {
        let xml = r#"<mujoco><compiler meshdir="assets"/>
          <asset><mesh file="link1.stl" scale="0.001 0.001 0.001"/><mesh name="hand" file="hand.obj"/></asset>
          <worldbody>
            <frame pos="0 0 1"><body name="b"><geom type="mesh" mesh="link1"/><geom type="mesh" mesh="hand" pos="0 0 0.1"/></body></frame>
          </worldbody></mujoco>"#;
        let r = parse(xml).unwrap();
        let b = r.link("b").unwrap();
        assert_eq!(
            b.visuals[0].geometry,
            Geometry::Mesh {
                filename: "assets/link1.stl".into(),
                scale: [0.001; 3]
            }
        );
        assert!(
            matches!(&b.visuals[1].geometry, Geometry::Mesh { filename, .. } if filename == "assets/hand.obj")
        );
        let fk = r.forward_kinematics(&[]).unwrap();
        assert_eq!(fk.link_pose("b").unwrap().translation, [0.0, 0.0, 1.0]);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse("<robot/>").is_err());
        assert!(
            parse(r#"<mujoco><worldbody><body><joint type="screw"/></body></worldbody></mujoco>"#)
                .is_err()
        );
        assert!(parse(r#"<mujoco><worldbody><body quat="1 0"/></worldbody></mujoco>"#).is_err());
    }
}
