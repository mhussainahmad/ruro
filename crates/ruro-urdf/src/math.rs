//! Minimal rigid-body math: unit quaternions and rigid transforms.

/// A rigid transform: rotation followed by translation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Iso {
    pub translation: [f64; 3],
    /// Unit quaternion `[x, y, z, w]`.
    pub rotation: [f64; 4],
}

impl Default for Iso {
    fn default() -> Self {
        Self::IDENTITY
    }
}

pub fn quat_mul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

pub fn quat_rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    // v' = v + 2w (u x v) + 2 u x (u x v)
    let u = [q[0], q[1], q[2]];
    let w = q[3];
    let c = cross(u, v);
    let cc = cross(u, c);
    [
        v[0] + 2.0 * (w * c[0] + cc[0]),
        v[1] + 2.0 * (w * c[1] + cc[1]),
        v[2] + 2.0 * (w * c[2] + cc[2]),
    ]
}

pub fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

pub fn normalize(a: [f64; 3]) -> [f64; 3] {
    let n = norm(a);
    if n > 0.0 {
        [a[0] / n, a[1] / n, a[2] / n]
    } else {
        a
    }
}

pub fn quat_normalize(q: [f64; 4]) -> [f64; 4] {
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if n > 0.0 {
        q.map(|c| c / n)
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// Rotation of `angle` radians about `axis` (need not be normalised).
pub fn quat_from_axis_angle(axis: [f64; 3], angle: f64) -> [f64; 4] {
    let a = normalize(axis);
    let (s, c) = (angle / 2.0).sin_cos();
    [a[0] * s, a[1] * s, a[2] * s, c]
}

/// URDF `rpy`: fixed-axis roll about X, then pitch about Y, then yaw about Z
/// (R = Rz(yaw) · Ry(pitch) · Rx(roll)).
pub fn quat_from_rpy(roll: f64, pitch: f64, yaw: f64) -> [f64; 4] {
    let qx = quat_from_axis_angle([1.0, 0.0, 0.0], roll);
    let qy = quat_from_axis_angle([0.0, 1.0, 0.0], pitch);
    let qz = quat_from_axis_angle([0.0, 0.0, 1.0], yaw);
    quat_mul(qz, quat_mul(qy, qx))
}

/// Rotation matrix (row-major) to quaternion.
pub fn quat_from_matrix(m: [[f64; 3]; 3]) -> [f64; 4] {
    let tr = m[0][0] + m[1][1] + m[2][2];
    let q = if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        [
            (m[2][1] - m[1][2]) / s,
            (m[0][2] - m[2][0]) / s,
            (m[1][0] - m[0][1]) / s,
            0.25 * s,
        ]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [
            0.25 * s,
            (m[0][1] + m[1][0]) / s,
            (m[0][2] + m[2][0]) / s,
            (m[2][1] - m[1][2]) / s,
        ]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [
            (m[0][1] + m[1][0]) / s,
            0.25 * s,
            (m[1][2] + m[2][1]) / s,
            (m[0][2] - m[2][0]) / s,
        ]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [
            (m[0][2] + m[2][0]) / s,
            (m[1][2] + m[2][1]) / s,
            0.25 * s,
            (m[1][0] - m[0][1]) / s,
        ]
    };
    quat_normalize(q)
}

/// The shortest rotation taking +Z onto `dir`.
pub fn quat_z_to(dir: [f64; 3]) -> [f64; 4] {
    let d = normalize(dir);
    let z = [0.0, 0.0, 1.0];
    let c = dot(z, d);
    if c < -1.0 + 1e-12 {
        return [1.0, 0.0, 0.0, 0.0]; // 180° about X
    }
    let axis = cross(z, d);
    quat_normalize([axis[0], axis[1], axis[2], 1.0 + c])
}

impl Iso {
    pub const IDENTITY: Iso = Iso {
        translation: [0.0; 3],
        rotation: [0.0, 0.0, 0.0, 1.0],
    };

    pub fn new(translation: [f64; 3], rotation: [f64; 4]) -> Self {
        Self {
            translation,
            rotation,
        }
    }

    pub fn translation(t: [f64; 3]) -> Self {
        Self::new(t, [0.0, 0.0, 0.0, 1.0])
    }

    pub fn rotation(q: [f64; 4]) -> Self {
        Self::new([0.0; 3], q)
    }

    /// `self * other`: apply `other` first, then `self`.
    pub fn mul(&self, other: &Iso) -> Iso {
        let t = self.transform_point(other.translation);
        Iso::new(t, quat_normalize(quat_mul(self.rotation, other.rotation)))
    }

    pub fn transform_point(&self, p: [f64; 3]) -> [f64; 3] {
        let r = quat_rotate(self.rotation, p);
        [
            r[0] + self.translation[0],
            r[1] + self.translation[1],
            r[2] + self.translation[2],
        ]
    }

    pub fn inverse(&self) -> Iso {
        let [x, y, z, w] = self.rotation;
        let qi = [-x, -y, -z, w];
        let t = quat_rotate(qi, self.translation);
        Iso::new([-t[0], -t[1], -t[2]], qi)
    }

    /// True if both transforms agree within `eps` (quaternion sign ignored).
    pub fn approx_eq(&self, other: &Iso, eps: f64) -> bool {
        let dt = (0..3).all(|i| (self.translation[i] - other.translation[i]).abs() < eps);
        let d = (0..4)
            .map(|i| self.rotation[i] * other.rotation[i])
            .sum::<f64>();
        dt && (1.0 - d.abs()) < eps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::FRAC_PI_2;

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-12)
    }

    #[test]
    fn rpy_is_fixed_axis_xyz() {
        // Roll 90° then yaw 90°, both about fixed axes.
        let q = quat_from_rpy(FRAC_PI_2, 0.0, FRAC_PI_2);
        // Rx(90) maps y -> z; then Rz(90) leaves z alone.
        assert!(close(quat_rotate(q, [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]));
        // Rx(90) leaves x; Rz(90) maps x -> y.
        assert!(close(quat_rotate(q, [1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]));
    }

    #[test]
    fn iso_compose_and_invert() {
        let a = Iso::new([1.0, 2.0, 3.0], quat_from_rpy(0.3, -0.2, 1.1));
        let b = Iso::new(
            [-0.5, 0.0, 0.25],
            quat_from_axis_angle([1.0, 1.0, 0.0], 0.7),
        );
        let ab = a.mul(&b);
        let p = [0.3, -0.7, 2.0];
        assert!(close(
            ab.transform_point(p),
            a.transform_point(b.transform_point(p))
        ));
        assert!(a.mul(&a.inverse()).approx_eq(&Iso::IDENTITY, 1e-12));
    }

    #[test]
    fn matrix_and_direction_conversions() {
        for q in [
            quat_from_rpy(0.1, 0.2, 0.3),
            quat_from_rpy(3.0, -1.0, 0.5),
            quat_from_axis_angle([0.0, 1.0, 0.0], 3.1),
        ] {
            let cols =
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].map(|e| quat_rotate(q, e));
            let m = [
                [cols[0][0], cols[1][0], cols[2][0]],
                [cols[0][1], cols[1][1], cols[2][1]],
                [cols[0][2], cols[1][2], cols[2][2]],
            ];
            assert!(Iso::rotation(quat_from_matrix(m)).approx_eq(&Iso::rotation(q), 1e-9));
        }
        for d in [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.3, -0.4, 0.5]] {
            assert!(close(
                quat_rotate(quat_z_to(d), [0.0, 0.0, 1.0]),
                normalize(d)
            ));
        }
    }
}
