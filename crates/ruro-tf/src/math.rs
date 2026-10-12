//! Rigid transforms: rotation as a unit quaternion plus a translation.

use ruro::msg::{Quat, Transform, Vec3};

/// A rigid transform. As a pose it is the child frame expressed in the parent
/// frame; applied to a point it maps child coordinates into parent coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Isometry {
    /// Translation `[x, y, z]`.
    pub translation: [f64; 3],
    /// Unit quaternion `[x, y, z, w]`.
    pub rotation: [f64; 4],
}

impl Default for Isometry {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Isometry {
    /// The identity transform.
    pub const IDENTITY: Isometry = Isometry {
        translation: [0.0; 3],
        rotation: [0.0, 0.0, 0.0, 1.0],
    };

    /// A transform from a translation and a (not necessarily normalised) quaternion.
    pub fn new(translation: [f64; 3], rotation: [f64; 4]) -> Self {
        Self {
            translation,
            rotation: normalize(rotation),
        }
    }

    /// A pure translation.
    pub fn from_translation(x: f64, y: f64, z: f64) -> Self {
        Self {
            translation: [x, y, z],
            ..Self::IDENTITY
        }
    }

    /// A rotation from roll, pitch and yaw in radians (extrinsic X, Y, Z, as in ROS).
    pub fn from_rpy(roll: f64, pitch: f64, yaw: f64) -> Self {
        let (sr, cr) = (roll * 0.5).sin_cos();
        let (sp, cp) = (pitch * 0.5).sin_cos();
        let (sy, cy) = (yaw * 0.5).sin_cos();
        Self {
            translation: [0.0; 3],
            rotation: [
                sr * cp * cy - cr * sp * sy,
                cr * sp * cy + sr * cp * sy,
                cr * cp * sy - sr * sp * cy,
                cr * cp * cy + sr * sp * sy,
            ],
        }
    }

    /// Roll, pitch and yaw in radians.
    pub fn rpy(&self) -> [f64; 3] {
        let [x, y, z, w] = self.rotation;
        let roll = (2.0 * (w * x + y * z)).atan2(1.0 - 2.0 * (x * x + y * y));
        let pitch = (2.0 * (w * y - z * x)).clamp(-1.0, 1.0).asin();
        let yaw = (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z));
        [roll, pitch, yaw]
    }

    /// Sets the translation.
    pub fn with_translation(mut self, x: f64, y: f64, z: f64) -> Self {
        self.translation = [x, y, z];
        self
    }

    /// `self * other`: apply `other` first, then `self`.
    pub fn compose(&self, other: &Isometry) -> Isometry {
        let t = rotate(self.rotation, other.translation);
        Isometry {
            translation: [
                self.translation[0] + t[0],
                self.translation[1] + t[1],
                self.translation[2] + t[2],
            ],
            rotation: normalize(quat_mul(self.rotation, other.rotation)),
        }
    }

    /// The inverse transform.
    pub fn inverse(&self) -> Isometry {
        let q = conjugate(self.rotation);
        let t = rotate(q, self.translation);
        Isometry {
            translation: [-t[0], -t[1], -t[2]],
            rotation: q,
        }
    }

    /// Maps a point from child into parent coordinates.
    pub fn transform_point(&self, p: [f64; 3]) -> [f64; 3] {
        let r = rotate(self.rotation, p);
        [
            r[0] + self.translation[0],
            r[1] + self.translation[1],
            r[2] + self.translation[2],
        ]
    }

    /// Rotates a vector (no translation).
    pub fn transform_vector(&self, v: [f64; 3]) -> [f64; 3] {
        rotate(self.rotation, v)
    }

    /// Interpolates between `self` (at `s = 0`) and `other` (at `s = 1`):
    /// linear in translation, spherical in rotation.
    pub fn interpolate(&self, other: &Isometry, s: f64) -> Isometry {
        let lerp = |a: f64, b: f64| a + (b - a) * s;
        Isometry {
            translation: [
                lerp(self.translation[0], other.translation[0]),
                lerp(self.translation[1], other.translation[1]),
                lerp(self.translation[2], other.translation[2]),
            ],
            rotation: slerp(self.rotation, other.rotation, s),
        }
    }

    /// Whether two transforms are equal within `eps` (rotations compared up to sign).
    pub fn approx_eq(&self, other: &Isometry, eps: f64) -> bool {
        let t = self
            .translation
            .iter()
            .zip(other.translation)
            .all(|(a, b)| (a - b).abs() <= eps);
        let dot: f64 = self
            .rotation
            .iter()
            .zip(other.rotation)
            .map(|(a, b)| a * b)
            .sum();
        t && (1.0 - dot.abs()) <= eps
    }

    /// From a transform message (header and frame names are ignored).
    pub fn from_msg(t: &Transform) -> Isometry {
        let (v, q) = (t.translation, t.rotation);
        Isometry::new([v.x, v.y, v.z], [q.x, q.y, q.z, q.w])
    }

    /// The translation as a message vector.
    pub fn vec3(&self) -> Vec3 {
        let [x, y, z] = self.translation;
        Vec3 { x, y, z }
    }

    /// The rotation as a message quaternion.
    pub fn quat(&self) -> Quat {
        let [x, y, z, w] = self.rotation;
        Quat { x, y, z, w }
    }
}

fn normalize(q: [f64; 4]) -> [f64; 4] {
    let n = q.iter().map(|c| c * c).sum::<f64>().sqrt();
    if n < 1e-12 {
        [0.0, 0.0, 0.0, 1.0]
    } else {
        q.map(|c| c / n)
    }
}

fn conjugate([x, y, z, w]: [f64; 4]) -> [f64; 4] {
    [-x, -y, -z, w]
}

fn quat_mul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    // v' = v + 2w(u x v) + 2u x (u x v), with u the vector part of q.
    let [x, y, z, w] = q;
    let cross = |a: [f64; 3], b: [f64; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let u = [x, y, z];
    let uv = cross(u, v);
    let uuv = cross(u, uv);
    [
        v[0] + 2.0 * (w * uv[0] + uuv[0]),
        v[1] + 2.0 * (w * uv[1] + uuv[1]),
        v[2] + 2.0 * (w * uv[2] + uuv[2]),
    ]
}

fn slerp(a: [f64; 4], b: [f64; 4], s: f64) -> [f64; 4] {
    let mut dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    // Take the short way around.
    let b = if dot < 0.0 {
        dot = -dot;
        b.map(|c| -c)
    } else {
        b
    };
    if dot > 0.9995 {
        let mut q = [0.0; 4];
        for i in 0..4 {
            q[i] = a[i] + (b[i] - a[i]) * s;
        }
        return normalize(q);
    }
    let theta = dot.clamp(-1.0, 1.0).acos();
    let sin = theta.sin();
    let wa = ((1.0 - s) * theta).sin() / sin;
    let wb = (s * theta).sin() / sin;
    normalize([
        wa * a[0] + wb * b[0],
        wa * a[1] + wb * b[1],
        wa * a[2] + wb * b[2],
        wa * a[3] + wb * b[3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, PI};

    const EPS: f64 = 1e-9;

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < EPS)
    }

    #[test]
    fn yaw_rotates_x_into_y() {
        let r = Isometry::from_rpy(0.0, 0.0, FRAC_PI_2);
        assert!(close(r.transform_point([1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]));
    }

    #[test]
    fn rpy_roundtrip() {
        let r = Isometry::from_rpy(0.1, -0.4, 2.0);
        let [roll, pitch, yaw] = r.rpy();
        assert!(close([roll, pitch, yaw], [0.1, -0.4, 2.0]));
    }

    #[test]
    fn compose_then_inverse_is_identity() {
        let a = Isometry::from_rpy(0.3, 0.2, -1.0).with_translation(1.0, 2.0, 3.0);
        let b = Isometry::from_rpy(-0.7, 0.0, 0.4).with_translation(-4.0, 0.5, 0.0);
        let ab = a.compose(&b);
        assert!(
            ab.compose(&ab.inverse())
                .approx_eq(&Isometry::IDENTITY, EPS)
        );
        let p = [0.3, -0.2, 5.0];
        assert!(close(
            ab.transform_point(p),
            a.transform_point(b.transform_point(p))
        ));
    }

    #[test]
    fn slerp_halfway_and_short_path() {
        let a = Isometry::IDENTITY;
        let b = Isometry::from_rpy(0.0, 0.0, PI / 2.0);
        let mid = a.interpolate(&b, 0.5);
        assert!(mid.approx_eq(&Isometry::from_rpy(0.0, 0.0, PI / 4.0), EPS));
        // Same rotation with flipped sign must interpolate the short way.
        let neg = Isometry::new([0.0; 3], b.rotation.map(|c| -c));
        assert!(
            a.interpolate(&neg, 0.5)
                .approx_eq(&Isometry::from_rpy(0.0, 0.0, PI / 4.0), EPS)
        );
    }
}
