//! Admission budget for newly prepared CPU snap indices, separate from process RSS/VRAM.
#[cfg(test)]
use crate::document::Document;
use crate::mesh::Mesh;
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

const MIB: usize = 1024 * 1024;
const DEFAULT_MIB: usize = 512;
#[derive(Default)]
pub struct Usage {
    pub resident: AtomicUsize,
    pub working: AtomicUsize,
}
#[derive(Clone)]
pub struct IndexBudget {
    bytes: usize,
    usage: Arc<Usage>,
    covered: Option<Arc<Admission>>,
}
pub struct Admission {
    usage: Arc<Usage>,
    bytes: Mutex<usize>,
}
impl Admission {
    fn bytes(&self) -> usize {
        *self.bytes.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn ensure_capacity(&self, budget: &IndexBudget, bytes: usize) -> Result<(), String> {
        let mut current = self.bytes.lock().unwrap_or_else(|e| e.into_inner());
        let extra = bytes.saturating_sub(*current);
        if extra == 0 {
            return budget.check_total(
                budget
                    .usage
                    .resident
                    .load(Ordering::SeqCst)
                    .saturating_add(budget.usage.working.load(Ordering::SeqCst)),
            );
        }
        self.usage
            .working
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |working| {
                let next = working.checked_add(extra)?;
                (self
                    .usage
                    .resident
                    .load(Ordering::SeqCst)
                    .checked_add(next)?
                    <= budget.bytes)
                    .then_some(next)
            })
            .map_err(|working| {
                budget.exceeded(
                    self.usage
                        .resident
                        .load(Ordering::SeqCst)
                        .saturating_add(working)
                        .saturating_add(extra),
                )
            })?;
        *current = bytes;
        Ok(())
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        self.usage.working.fetch_sub(
            *self.bytes.get_mut().unwrap_or_else(|e| e.into_inner()),
            Ordering::SeqCst,
        );
    }
}
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct Reservation {
    pub limit_bytes: usize,
    pub estimated_working_bytes: usize,
    pub resident_bytes: usize,
    pub concurrent_working_bytes: usize,
    pub estimated_combined_bytes: usize,
    pub triangles: usize,
    pub instances: usize,
    pub shapes: usize,
}
impl IndexBudget {
    #[cfg(test)]
    pub fn for_test(bytes: usize) -> Self {
        Self {
            bytes,
            usage: Arc::default(),
            covered: None,
        }
    }
    pub fn with_usage(mut self, usage: Arc<Usage>) -> Self {
        self.usage = usage;
        self
    }
    /// Atomically reserve construction workspace; the result owns this lease until adoption.
    pub fn admission(&self, bytes: usize) -> Result<(Self, Arc<Admission>), String> {
        if let Some(lease) = &self.covered {
            self.check_capacity(bytes)?;
            return Ok((self.clone(), lease.clone()));
        }
        self.usage
            .working
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |working| {
                let next = working.checked_add(bytes)?;
                (self
                    .usage
                    .resident
                    .load(Ordering::SeqCst)
                    .checked_add(next)?
                    <= self.bytes)
                    .then_some(next)
            })
            .map_err(|working| {
                self.exceeded(
                    self.usage
                        .resident
                        .load(Ordering::SeqCst)
                        .saturating_add(working)
                        .saturating_add(bytes),
                )
            })?;
        let lease = Arc::new(Admission {
            usage: self.usage.clone(),
            bytes: Mutex::new(bytes),
        });
        let mut covered = self.clone();
        covered.covered = Some(lease.clone());
        Ok((covered, lease))
    }
    pub fn with_admission(mut self, admission: Option<Arc<Admission>>) -> Self {
        self.covered = admission;
        self
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
            usage: Arc::default(),
            covered: None,
        })
    }
    fn exceeded(&self, estimated: usize) -> String {
        format!("SNAP_INDEX_BUDGET_EXCEEDED: Index memory budget exceeded / 索引のメモリ予算を超過 / 索引内存超出预算 ({} > {} MiB)", estimated.div_ceil(MIB), self.bytes / MIB)
    }
    fn combined(&self, bytes: usize) -> Result<usize, String> {
        let own = self.covered.as_ref().map_or(0, |lease| lease.bytes());
        self.usage
            .resident
            .load(Ordering::SeqCst)
            .checked_add(
                self.usage
                    .working
                    .load(Ordering::SeqCst)
                    .saturating_sub(own)
                    .checked_add(own.max(bytes))
                    .ok_or_else(|| self.exceeded(usize::MAX))?,
            )
            .ok_or_else(|| self.exceeded(usize::MAX))
    }
    fn check_total(&self, total: usize) -> Result<(), String> {
        if total > self.bytes {
            Err(self.exceeded(total))
        } else {
            Ok(())
        }
    }
    pub fn check_capacity(&self, bytes: usize) -> Result<(), String> {
        self.check_total(self.combined(bytes)?)
    }
    pub fn reserve(
        &self,
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
            resident_bytes: self.usage.resident.load(Ordering::SeqCst),
            concurrent_working_bytes: self.usage.working.load(Ordering::SeqCst),
            estimated_combined_bytes: self.combined(estimate)?,
            triangles,
            instances,
            shapes,
        })
    }
    pub fn meshes<'a>(
        &self,
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
    #[cfg(test)]
    pub fn document(&self, document: &Document) -> Result<Reservation, String> {
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
    fn combined_admission_tracks_resident_growth_nested_work_and_release() {
        let usage = Arc::new(Usage::default());
        usage.resident.store(400, Ordering::SeqCst);
        let budget = IndexBudget::for_test(1000).with_usage(usage.clone());
        assert!(budget.check_capacity(600).is_ok());
        assert!(budget.check_capacity(601).is_err());
        let (covered, lease) = budget.admission(500).unwrap();
        let (nested, child) = covered.admission(300).unwrap();
        assert_eq!(usage.working.load(Ordering::SeqCst), 500);
        assert!(nested.check_capacity(500).is_ok());
        assert!(budget.admission(101).is_err());
        let (_, other) = budget.admission(100).unwrap();
        assert_eq!(usage.working.load(Ordering::SeqCst), 600);
        usage.resident.store(401, Ordering::SeqCst);
        assert!(covered.check_capacity(300).is_err()); // live usage, not captured baseline
        drop(child);
        drop(nested);
        drop(other);
        assert_eq!(usage.working.load(Ordering::SeqCst), 500);
        let pin = lease.clone();
        drop(lease);
        assert_eq!(usage.working.load(Ordering::SeqCst), 500);
        drop(pin);
        drop(covered);
        assert_eq!(usage.working.load(Ordering::SeqCst), 0);
        usage.resident.store(usize::MAX, Ordering::SeqCst);
        assert!(budget.admission(1).is_err());
    }
    #[test]
    fn retained_growth_adds_to_other_work_and_keeps_the_lease_alive() {
        let usage = Arc::new(Usage::default());
        usage.resident.store(400, Ordering::SeqCst);
        let budget = IndexBudget::for_test(1000).with_usage(usage.clone());
        let (covered, lease) = budget.admission(500).unwrap();
        let (_, other) = budget.admission(100).unwrap();
        assert!(covered.check_capacity(501).is_err());
        assert!(lease.ensure_capacity(&covered, 501).is_err());
        assert_eq!(usage.working.load(Ordering::SeqCst), 600);
        drop(other);
        lease.ensure_capacity(&covered, 600).unwrap();
        assert_eq!(usage.working.load(Ordering::SeqCst), 600);
        drop(lease);
        assert_eq!(usage.working.load(Ordering::SeqCst), 600);
        drop(covered);
        assert_eq!(usage.working.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn concurrent_reservations_cannot_over_admit_and_unwind_releases_them() {
        let usage = Arc::new(Usage::default());
        let budget = IndexBudget::for_test(1000).with_usage(usage.clone());
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let budget = budget.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let lease = budget.admission(600).ok();
                    barrier.wait();
                    let admitted = lease.is_some();
                    drop(lease);
                    admitted
                })
            })
            .collect();
        assert_eq!(
            threads
                .into_iter()
                .map(|t| usize::from(t.join().unwrap()))
                .sum::<usize>(),
            1
        );
        assert_eq!(usage.working.load(Ordering::SeqCst), 0);
        let _ = std::panic::catch_unwind(|| {
            let _lease = budget.admission(600).unwrap();
            panic!("synthetic worker unwind");
        });
        assert_eq!(usage.working.load(Ordering::SeqCst), 0);
    }
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
