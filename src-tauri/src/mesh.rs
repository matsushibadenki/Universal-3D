use serde::{Deserialize, Serialize};
pub type Face = [[f32; 3]; 3];
pub const MAX_TRIANGLES: usize = 500_000;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mesh {
    pub triangles: Vec<Face>,
    /// ASCII masks 0..7 for triangle sides 01, 12, 20; absent in legacy snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_mask: Option<String>,
    pub color: [f32; 3],
    pub source: Source,
    pub prim_path: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub origin: String,
    pub revision: String,
    pub license: String,
}
impl Mesh {
    pub fn validate(&self) -> Result<(), String> {
        self.validate_cancellable(None)
    }
    pub fn validate_cancellable(
        &self,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        self.validate_with_check(|| crate::snapping::check_cancel(cancel))
    }
    fn validate_with_check(
        &self,
        mut check: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        if self.triangles.is_empty()
            || self.triangles.len() > MAX_TRIANGLES
            || self
                .color
                .iter()
                .any(|x| !x.is_finite() || !(0. ..=1.).contains(x))
            || self.prim_path.len() > 4096
            || self.source.origin.len() > 8192
            || self.source.revision.len() > 128
            || self.source.license.len() > 4096
            || self
                .edge_mask
                .as_ref()
                .is_some_and(|mask| mask.len() != self.triangles.len())
        {
            return Err("Invalid imported mesh".into());
        }
        if let Some(mask) = &self.edge_mask {
            for chunk in mask.as_bytes().chunks(4096) {
                check()?;
                if !chunk.iter().all(|byte| (b'0'..=b'7').contains(byte)) {
                    return Err("Invalid imported mesh".into());
                }
            }
        }
        for chunk in self.triangles.chunks(1024) {
            check()?;
            if chunk
                .iter()
                .flatten()
                .flatten()
                .any(|x| !x.is_finite() || x.abs() > 10000.)
            {
                return Err("Invalid imported mesh".into());
            }
        }
        check()
    }
}

// Ear clipping preserves concave polygon boundaries. Degenerate/non-simple polygons fail
// instead of silently filling an incorrect fan. Hole faces are omitted by the importer.
#[cfg(test)]
pub fn triangulate(points: &[[f32; 3]], indices: &[usize]) -> Result<Vec<Face>, String> {
    triangulate_cancellable(points, indices, None)
}
pub fn triangulate_cancellable(
    points: &[[f32; 3]],
    indices: &[usize],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Vec<Face>, String> {
    crate::snapping::check_cancel(cancel)?;
    if indices.len() < 3 || indices.len() > 4096 || indices.iter().any(|&i| i >= points.len()) {
        return Err("Invalid USD face topology".into());
    }
    let mut normal = [0f64; 3];
    for i in 0..indices.len() {
        let a = points[indices[i]].map(f64::from);
        let b = points[indices[(i + 1) % indices.len()]].map(f64::from);
        for j in 0..3 {
            normal[j] += (a[(j + 1) % 3] - b[(j + 1) % 3]) * (a[(j + 2) % 3] + b[(j + 2) % 3]);
        }
    }
    let drop = (0..3)
        .max_by(|&a, &b| normal[a].abs().total_cmp(&normal[b].abs()))
        .unwrap();
    let uv: Vec<[f64; 2]> = indices
        .iter()
        .map(|&i| {
            [
                points[i][(drop + 1) % 3] as f64,
                points[i][(drop + 2) % 3] as f64,
            ]
        })
        .collect();
    let cross = |a: usize, b: usize, c: usize| {
        (uv[b][0] - uv[a][0]) * (uv[c][1] - uv[a][1])
            - (uv[b][1] - uv[a][1]) * (uv[c][0] - uv[a][0])
    };
    let area: f64 = (0..uv.len())
        .map(|i| {
            let j = (i + 1) % uv.len();
            uv[i][0] * uv[j][1] - uv[j][0] * uv[i][1]
        })
        .sum();
    if area.abs() < 1e-16 {
        return Ok(Vec::new());
    }
    let sign = area.signum();
    let eps = area.abs() * 1e-12;
    let mut remaining: Vec<usize> = (0..indices.len()).collect();
    let mut faces = Vec::new();
    while remaining.len() > 3 {
        crate::snapping::check_cancel(cancel)?;
        let mut found = false;
        for j in 0..remaining.len() {
            if j.is_multiple_of(32) {
                crate::snapping::check_cancel(cancel)?;
            }
            let a = remaining[(j + remaining.len() - 1) % remaining.len()];
            let b = remaining[j];
            let c = remaining[(j + 1) % remaining.len()];
            if cross(a, b, c) * sign <= eps {
                continue;
            }
            if remaining.iter().any(|&p| {
                p != a
                    && p != b
                    && p != c
                    && cross(a, b, p) * sign >= -eps
                    && cross(b, c, p) * sign >= -eps
                    && cross(c, a, p) * sign >= -eps
            }) {
                continue;
            }
            faces.push([points[indices[a]], points[indices[b]], points[indices[c]]]);
            remaining.remove(j);
            found = true;
            break;
        }
        if !found {
            return Err("USD polygon cannot be triangulated safely".into());
        }
    }
    faces.push(std::array::from_fn(|i| points[indices[remaining[i]]]));
    Ok(faces)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mesh_validation_checks_mid_buffer_and_retains_all_rejection_rules() {
        let mut mesh = Mesh {
            triangles: vec![[[0.; 3]; 3]; 2500],
            edge_mask: Some("7".repeat(2500)),
            color: [0.5; 3],
            source: Source {
                origin: "test".into(),
                revision: String::new(),
                license: String::new(),
            },
            prim_path: "/Mesh".into(),
        };
        let mut checks = 0;
        let error = mesh
            .validate_with_check(|| {
                checks += 1;
                // Metadata and the mask pass; cancel between geometry chunks.
                if checks == 4 {
                    Err("LOAD_CANCELLED".into())
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(error, "LOAD_CANCELLED");
        assert_eq!(checks, 4);
        assert!(mesh.validate().is_ok());
        mesh.triangles[2400][2][2] = f32::NAN;
        assert!(mesh.validate().is_err());
        mesh.triangles[2400][2][2] = 0.;
        mesh.edge_mask = Some(format!("{}8", "7".repeat(2499)));
        assert!(mesh.validate().is_err());
        mesh.edge_mask = None;
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        assert_eq!(
            mesh.validate_cancellable(Some(&cancelled)).unwrap_err(),
            "LOAD_CANCELLED"
        );
        assert!(mesh.validate().is_ok());
    }
    #[test]
    fn concave_polygon_preserves_area_and_winding() {
        let points = [
            [0., 0., 0.],
            [2., 0., 0.],
            [2., 2., 0.],
            [1., 1., 0.],
            [0., 2., 0.],
        ];
        for indices in [vec![0, 1, 2, 3, 4], vec![4, 3, 2, 1, 0]] {
            let faces = triangulate(&points, &indices).unwrap();
            assert_eq!(faces.len(), 3);
            let area: f32 = faces
                .iter()
                .map(|[a, b, c]| {
                    ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])) / 2.
                })
                .sum();
            assert_eq!(area.abs(), 3.);
            assert_eq!(area.signum(), if indices[0] == 0 { 1. } else { -1. });
        }
    }
    #[test]
    fn invalid_indices_are_rejected() {
        assert!(triangulate(&[[0.; 3]], &[0, 1, 2]).is_err());
    }
}
