//! Admission budget for newly prepared CPU snap indices, separate from process RSS/VRAM.
use crate::{document::Document, mesh::Mesh};
use std::{collections::HashSet, sync::Arc};

const MIB: usize = 1024 * 1024;
const DEFAULT_MIB: usize = 512;
#[derive(Clone, Copy)]
pub struct IndexBudget {
    bytes: usize,
}
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct Reservation {
    pub limit_bytes: usize,
    pub estimated_working_bytes: usize,
    pub triangles: usize,
    pub instances: usize,
    pub shapes: usize,
}
impl IndexBudget {
    #[cfg(test)]
    pub fn for_test(bytes: usize) -> Self {
        Self { bytes }
    }
    pub fn configured() -> Result<Self, String> {
        let value = match std::env::var("U3D_SNAP_INDEX_BUDGET_MIB") {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(_) => return Err("SNAP_INDEX_INVALID_BUDGET: 1..4096 MiB".into()),
        };
        Self::parse(value.as_deref())
    }
    fn parse(value: Option<&str>) -> Result<Self, String> {
        let mib = match value {
            None => DEFAULT_MIB,
            Some(value) => value
                .parse::<usize>()
                .map_err(|_| "SNAP_INDEX_INVALID_BUDGET: 1..4096 MiB")?,
        };
        if !(1..=4096).contains(&mib) {
            return Err("SNAP_INDEX_INVALID_BUDGET: 1..4096 MiB".into());
        }
        Ok(Self {
            bytes: mib.checked_mul(MIB).ok_or("SNAP_INDEX_INVALID_BUDGET")?,
        })
    }
    fn exceeded(&self, estimated: usize) -> String {
        format!("SNAP_INDEX_BUDGET_EXCEEDED: Index memory budget exceeded / 索引のメモリ予算を超過 / 索引内存超出预算 ({} > {} MiB)", estimated.div_ceil(MIB), self.bytes / MIB)
    }
    pub fn check_capacity(self, bytes: usize) -> Result<(), String> {
        if bytes > self.bytes {
            Err(self.exceeded(bytes))
        } else {
            Ok(())
        }
    }
    pub fn reserve(
        self,
        triangles: usize,
        instances: usize,
        shapes: usize,
    ) -> Result<Reservation, String> {
        // Conservative admission estimate: disjoint vertices/edges, Vec slack, BVHs,
        // topology hash tables and construction scratch. This is not an allocator limit.
        let estimate = triangles
            .checked_mul(1024)
            .and_then(|n| instances.checked_mul(1024).and_then(|i| n.checked_add(i)))
            .and_then(|n| shapes.checked_mul(4096).and_then(|s| n.checked_add(s)))
            .ok_or_else(|| self.exceeded(usize::MAX))?;
        self.check_capacity(estimate)?;
        Ok(Reservation {
            limit_bytes: self.bytes,
            estimated_working_bytes: estimate,
            triangles,
            instances,
            shapes,
        })
    }
    pub fn meshes<'a>(
        self,
        assets: impl IntoIterator<Item = &'a Arc<Mesh>>,
    ) -> Result<Reservation, String> {
        let mut seen = HashSet::new();
        let mut triangles: usize = 0;
        for mesh in assets {
            if seen.insert(Arc::as_ptr(mesh) as usize) {
                triangles = triangles
                    .checked_add(mesh.triangles.len())
                    .ok_or_else(|| self.exceeded(usize::MAX))?;
            }
        }
        self.reserve(triangles, 0, seen.len())
    }
    pub fn document(self, document: &Document) -> Result<Reservation, String> {
        let ids: HashSet<_> = document
            .objects
            .iter()
            .filter_map(|o| o.mesh_id.as_ref())
            .collect();
        let meshes = self.meshes(ids.iter().filter_map(|id| document.meshes.get(*id)))?;
        let mut primitive_kinds = HashSet::new();
        let mut instances = 0usize;
        let mut triangles = meshes.triangles;
        for object in document.objects.iter().filter(|o| o.visible) {
            instances = instances
                .checked_add(if object.mirror { 2 } else { 1 })
                .ok_or_else(|| self.exceeded(usize::MAX))?;
            if object.mesh_id.is_none() && primitive_kinds.insert(object.kind.as_str()) {
                triangles = triangles
                    .checked_add(crate::renderer::geometry(&object.kind).len())
                    .ok_or_else(|| self.exceeded(usize::MAX))?;
            }
        }
        self.reserve(triangles, instances, meshes.shapes + primitive_kinds.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_and_checked_reservations_fail_closed() {
        assert_eq!(IndexBudget::parse(None).unwrap().bytes, 512 * MIB);
        for value in ["", "0", "4097", "-1", "junk", "99999999999999999999999"] {
            assert!(IndexBudget::parse(Some(value)).is_err());
        }
        let budget = IndexBudget::parse(Some("1")).unwrap();
        assert!(budget.reserve(100, 10, 2).is_ok());
        assert!(budget
            .reserve(1024, 1, 1)
            .unwrap_err()
            .contains("SNAP_INDEX_BUDGET_EXCEEDED"));
        assert!(budget.reserve(usize::MAX, 0, 0).is_err());
        assert!(budget.reserve(0, usize::MAX, 0).is_err());
        assert!(budget.check_capacity(MIB).is_ok());
        assert!(budget.check_capacity(MIB + 1).is_err());
    }
}
