//! Affine transforms retain the shear created by non-uniform world scaling.
use crate::document::Object;
use crate::gizmo::{euler_near, rotation_basis};

pub fn linear_basis(object: &Object) -> [[f32; 3]; 3] {
    let r = rotation_basis(object.rotation);
    let [xy, xz, yz] = object.shear;
    [
        r[0].map(|v| v * object.scale[0]),
        std::array::from_fn(|i| (r[0][i] * xy + r[1][i]) * object.scale[1]),
        std::array::from_fn(|i| (r[0][i] * xz + r[1][i] * yz + r[2][i]) * object.scale[2]),
    ]
}
fn world_scaled(before: &Object, factors: [f32; 3]) -> Option<Object> {
    if factors == [1.; 3] {
        return Some(before.clone());
    }
    let mut m = linear_basis(before).map(|column| column.map(f64::from));
    for column in &mut m {
        for axis in 0..3 {
            column[axis] *= f64::from(factors[axis]);
        }
    }
    // Modified Gram-Schmidt QR, with original signed scale diagonals. Positive
    // world multipliers preserve handedness, so Q remains a proper rotation.
    let dot = |a: [f64; 3], b: [f64; 3]| (0..3).map(|i| a[i] * b[i]).sum::<f64>();
    let mut q = [[0.; 3]; 3];
    let mut upper = [[0.; 3]; 3];
    for j in 0..3 {
        let mut v = m[j];
        for i in 0..j {
            let projection = dot(q[i], v);
            upper[j][i] = projection;
            v = std::array::from_fn(|k| v[k] - q[i][k] * projection);
        }
        let length = dot(v, v).sqrt();
        if !length.is_finite() || length < 1e-12 {
            return None;
        }
        upper[j][j] = length * f64::from(before.scale[j].signum());
        q[j] = v.map(|v| v / upper[j][j]);
    }
    let mut result = before.clone();
    result.rotation = euler_near(q.map(|c| c.map(|v| v as f32)), before.rotation);
    result.scale = std::array::from_fn(|i| upper[i][i] as f32);
    result.shear = [
        (upper[1][0] / upper[1][1]) as f32,
        (upper[2][0] / upper[2][2]) as f32,
        (upper[2][1] / upper[2][2]) as f32,
    ];
    result.validate().ok()?;
    Some(result)
}
pub fn world_scaled_bounded(before: &Object, axis: usize, factor: f32) -> Object {
    world_scaled_axes_bounded(
        before,
        std::array::from_fn(|i| if i == axis { factor } else { 1. }),
    )
}
pub fn world_scaled_axes_bounded(before: &Object, factors: [f32; 3]) -> Object {
    if let Some(result) = world_scaled(before, factors) {
        return result;
    }
    // Approach the requested multi-axis transform as one operation, preserving its ratio.
    let mut valid = 0.;
    let mut invalid = 1.;
    let logs = factors.map(f32::ln);
    let mut result = before.clone();
    for _ in 0..24 {
        let middle = (valid + invalid) * 0.5;
        if let Some(candidate) = world_scaled(before, logs.map(|v| (v * middle).exp())) {
            valid = middle;
            result = candidate;
        } else {
            invalid = middle;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    #[test]
    fn world_scaling_reconstructs_affine_matrix_and_preserves_reflections() {
        for signs in [[1., 1., 1.], [-1., 1., 1.], [1., -1., 1.], [-1., -1., -1.]] {
            for axis in 0..3 {
                for factor in [0.4, 1., 2.7] {
                    let mut before = Document::default().objects[0].clone();
                    before.rotation = [28., -37., 53.];
                    before.shear = [0.2, -0.4, 0.3];
                    before.scale = std::array::from_fn(|i| signs[i] * (i + 1) as f32);
                    let expected = linear_basis(&before).map(|mut c| {
                        c[axis] *= factor;
                        c
                    });
                    let result = world_scaled_bounded(&before, axis, factor);
                    let actual = linear_basis(&result);
                    for i in 0..3 {
                        for j in 0..3 {
                            assert!(
                                (actual[i][j] - expected[i][j]).abs() < 0.0001,
                                "{actual:?} != {expected:?}"
                            );
                        }
                    }
                    assert_eq!(result.scale.map(f32::signum), signs);
                    assert_eq!(result.position, before.position);
                    result.validate().unwrap();
                    if factor == 1. {
                        assert_eq!(result, before);
                    }
                }
            }
        }
    }
    #[test]
    fn affine_limits_remain_finite_and_legacy_documents_migrate_without_losing_shear() {
        let mut doc = Document::default();
        doc.objects[0].rotation = [20., 40., 60.];
        doc.objects[0].scale = [-0.001, 3., 10000.];
        for axis in 0..3 {
            for factor in [0.000001, 1000000.] {
                world_scaled_bounded(&doc.objects[0], axis, factor)
                    .validate()
                    .unwrap();
            }
        }
        let legacy=br#"{"version":1,"objects":[{"id":1,"name":"Old","kind":"cube","position":[0,0,0],"rotation":[0,0,0],"scale":[1,1,1],"visible":true,"mirror":false}],"selected":1}"#;
        let mut loaded: Document = serde_json::from_slice(legacy).unwrap();
        loaded.validate().unwrap();
        assert_eq!(loaded.objects[0].shear, [0.; 3]);
        loaded.objects[0].shear = [0.5, -0.3, 0.8];
        let saved: Document = serde_json::from_slice(&loaded.file_bytes().unwrap()).unwrap();
        saved.validate().unwrap();
        assert_eq!(saved.version, 2);
        assert_eq!(saved.objects, loaded.objects);
        loaded.objects[0].shear[0] = f32::NAN;
        assert!(loaded.validate().is_err());
    }
    #[test]
    fn plane_world_scale_limits_are_bounded_as_one_transform() {
        let mut before = Document::default().objects[0].clone();
        before.rotation = [28., -37., 53.];
        before.scale = [-0.001, 3., 10000.];
        before.shear = [0.2, -0.4, 0.3];
        for normal in 0..3 {
            for factor in [0.000001, 0.4, 1., 2.7, 1000000.] {
                let factors = std::array::from_fn(|i| if i == normal { 1. } else { factor });
                let result = world_scaled_axes_bounded(&before, factors);
                result.validate().unwrap();
                let a = linear_basis(&before);
                let b = linear_basis(&result);
                for i in 0..3 {
                    assert!((a[i][normal] - b[i][normal]).abs() < 0.01);
                }
                assert_eq!(result.scale.map(f32::signum), before.scale.map(f32::signum));
                assert_eq!(result.position, before.position);
            }
        }
    }
}
