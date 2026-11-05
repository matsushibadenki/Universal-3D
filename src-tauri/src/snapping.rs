//! Revision-cached, two-level vertex search. No geometry crosses the IPC boundary.
use crate::document::{Document, Editor, Object, RenderChange};
use crate::renderer::{camera_basis, model_matrix, object_geometry, world_point};
use crate::spatial::{Bounds, BoundsTree};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Weak,
};

const RADIUS: f32 = 14.;
#[derive(Clone, Copy)]
pub struct Projection {
    eye: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    forward: [f32; 3],
    near: f32,
    far: f32,
    size: [f32; 2],
}
impl Projection {
    pub fn new(editor: &Editor, size: [f32; 2]) -> Self {
        let (eye, right, up, forward) = camera_basis(editor);
        Self {
            eye,
            right,
            up,
            forward,
            size,
            near: (editor.distance * 0.1).min(0.1),
            far: (editor.distance * 4.).max(100.),
        }
    }
    pub fn project(&self, world: [f32; 3]) -> Option<([f32; 2], f32)> {
        let v: [f32; 3] = std::array::from_fn(|i| world[i] - self.eye[i]);
        let dot = |b: [f32; 3]| (0..3).map(|i| v[i] * b[i]).sum::<f32>();
        let depth = dot(self.forward);
        if !depth.is_finite() || depth <= self.near || depth >= self.far {
            return None;
        }
        let unit = 0.9 * self.size[1] / depth;
        let p = [
            self.size[0] * 0.5 + dot(self.right) * unit,
            self.size[1] * 0.5 - dot(self.up) * unit,
        ];
        p.iter().all(|v| v.is_finite()).then_some((p, depth))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Candidate {
    pub world: [f32; 3],
    pub screen: [f32; 2],
    depth: f32,
}
#[derive(Clone, Copy, Hash, Eq, PartialEq, Ord, PartialOrd)]
enum GeometryKey {
    Primitive(u8),
    Mesh(usize),
}
impl GeometryKey {
    fn in_document(document: &Document, object: &Object) -> Option<Self> {
        if let Some(id) = &object.mesh_id {
            document
                .meshes
                .get(id)
                .map(|mesh| Self::Mesh(Arc::as_ptr(mesh) as usize))
        } else {
            Some(Self::Primitive(match object.kind.as_str() {
                "sphere" => 1,
                "plane" => 2,
                _ => 0,
            }))
        }
    }
}
struct VertexNode {
    bounds: Bounds,
    range: std::ops::Range<usize>,
    children: Option<[usize; 2]>,
}
struct Geometry {
    points: Vec<[f32; 3]>,
    nodes: Vec<VertexNode>,
    // Own the asset so a pointer key cannot be reused for a different mesh (ABA).
    _asset: Option<Arc<crate::mesh::Mesh>>,
    primitive_faces: Option<Box<[crate::mesh::Face]>>,
    surfaces: std::sync::OnceLock<Arc<crate::surface_snap::SurfaceIndex>>,
}
impl Geometry {
    fn build(editor: &Editor, object: &Object) -> Self {
        Self::from_faces(
            object_geometry(editor, object),
            object
                .mesh_id
                .as_ref()
                .and_then(|id| editor.document.meshes.get(id).cloned()),
            None,
        )
        .expect("Uncancelled vertex construction cannot fail")
    }
    fn from_faces(
        faces: &[crate::mesh::Face],
        asset: Option<Arc<crate::mesh::Mesh>>,
        cancel: Option<&AtomicBool>,
    ) -> Result<Self, String> {
        let mut unique = HashSet::new();
        let mut points = Vec::new();
        for chunk in faces.chunks(1024) {
            check_cancel(cancel)?;
            for point in chunk.iter().flatten() {
                if unique.insert(point.map(|v| if v == 0. { 0 } else { v.to_bits() })) {
                    points.push(*point);
                }
            }
        }
        drop(unique);
        check_cancel(cancel)?;
        Self::order_points(&mut points, cancel)?;
        let mut result = Self {
            points,
            nodes: Vec::new(),
            primitive_faces: asset.is_none().then(|| faces.to_vec().into_boxed_slice()),
            _asset: asset,
            surfaces: std::sync::OnceLock::new(),
        };
        if !result.points.is_empty() {
            result.partition(0..result.points.len(), cancel)?;
        }
        check_cancel(cancel)?;
        Ok(result)
    }
    fn faces(&self) -> &[crate::mesh::Face] {
        self._asset.as_ref().map_or_else(
            || self.primitive_faces.as_deref().unwrap_or(&[]),
            |asset| asset.triangles.as_slice(),
        )
    }
    fn prepare_surfaces(&self, cancel: Option<&AtomicBool>) -> Result<(), String> {
        check_cancel(cancel)?;
        if self.surfaces.get().is_none() {
            let index = crate::surface_snap::SurfaceIndex::prepare(
                &self.points,
                self.faces(),
                self._asset
                    .as_ref()
                    .and_then(|asset| asset.edge_mask.as_deref()),
                cancel,
            )?;
            let _ = self.surfaces.set(Arc::new(index));
        }
        check_cancel(cancel)
    }
    fn buffer_bytes(&self) -> usize {
        let mut memory = IndexMemory::default();
        memory.add(self);
        memory.buffer_bytes()
    }
    fn order_points(points: &mut Vec<[f32; 3]>, cancel: Option<&AtomicBool>) -> Result<(), String> {
        check_cancel(cancel)?;
        if points.len() <= 16 {
            return check_cancel(cancel);
        }
        let bounds = points
            .iter()
            .copied()
            .map(Bounds::point)
            .reduce(Bounds::union)
            .unwrap();
        let spread = |mut x: u32| {
            x &= 0x3ff;
            x = (x | (x << 16)) & 0x030000ff;
            x = (x | (x << 8)) & 0x0300f00f;
            x = (x | (x << 4)) & 0x030c30c3;
            (x | (x << 2)) & 0x09249249
        };
        let key = |point: [f32; 3]| {
            let coordinate: [u32; 3] = std::array::from_fn(|i| {
                let span = f64::from(bounds.max[i]) - f64::from(bounds.min[i]);
                if span == 0. {
                    0
                } else {
                    (((f64::from(point[i]) - f64::from(bounds.min[i])) / span) * 1023.)
                        .clamp(0., 1023.) as u32
                }
            });
            spread(coordinate[0]) | (spread(coordinate[1]) << 1) | (spread(coordinate[2]) << 2)
        };
        let mut current: Vec<_> = std::mem::take(points)
            .into_iter()
            .map(|p| (key(p), p))
            .collect();
        check_cancel(cancel)?;
        let mut scratch = vec![(0, [0.; 3]); current.len()];
        // Three stable radix passes make ordering linear in unique vertex count.
        for shift in [0, 10, 20] {
            check_cancel(cancel)?;
            let mut offsets = [0usize; 1024];
            for (key, _) in &current {
                offsets[((key >> shift) & 1023) as usize] += 1;
            }
            let mut start = 0;
            for offset in &mut offsets {
                let count = *offset;
                *offset = start;
                start += count;
            }
            for item in &current {
                let slot = &mut offsets[((item.0 >> shift) & 1023) as usize];
                scratch[*slot] = *item;
                *slot += 1;
            }
            std::mem::swap(&mut current, &mut scratch);
        }
        *points = current.into_iter().map(|(_, p)| p).collect();
        check_cancel(cancel)
    }
    fn partition(
        &mut self,
        range: std::ops::Range<usize>,
        cancel: Option<&AtomicBool>,
    ) -> Result<usize, String> {
        let index = self.nodes.len();
        if index.is_multiple_of(1024) {
            check_cancel(cancel)?;
        }
        self.nodes.push(VertexNode {
            bounds: Bounds::point(self.points[range.start]),
            range: range.clone(),
            children: None,
        });
        if range.len() > 16 {
            let middle = range.start + (range.len() / 2 / 16).max(1) * 16;
            let left = self.partition(range.start..middle, cancel)?;
            let right = self.partition(middle..range.end, cancel)?;
            self.nodes[index].children = Some([left, right]);
            self.nodes[index].bounds = self.nodes[left].bounds.union(self.nodes[right].bounds);
        } else {
            self.nodes[index].bounds = self.points[range]
                .iter()
                .copied()
                .map(Bounds::point)
                .reduce(Bounds::union)
                .unwrap();
        }
        Ok(index)
    }
}

pub(crate) fn check_cancel(cancel: Option<&AtomicBool>) -> Result<(), String> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        Err("LOAD_CANCELLED".into())
    } else {
        Ok(())
    }
}

/// Buffer capacities and logical map entries are reported separately from process RSS.
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct IndexMemory {
    pub instances: usize,
    pub shapes: usize,
    pub ready_surface_shapes: usize,
    pub vertices: usize,
    pub vertex_nodes: usize,
    pub edge_nodes: usize,
    pub face_nodes: usize,
    pub edges: usize,
    pub triangles: usize,
    pub vertex_vec_bytes: usize,
    pub vertex_tree_vec_bytes: usize,
    pub edge_vec_bytes: usize,
    pub edge_tree_vec_bytes: usize,
    pub face_tree_vec_bytes: usize,
    pub primitive_face_bytes: usize,
    pub scene_tree_vec_bytes: usize,
    /// Live key/value payload only; excludes HashMap buckets/control bytes and allocator overhead.
    pub scene_entry_payload_bytes: usize,
}
impl IndexMemory {
    fn add(&mut self, geometry: &Geometry) {
        self.shapes += 1;
        self.vertices += geometry.points.len();
        self.vertex_nodes += geometry.nodes.len();
        self.vertex_vec_bytes += geometry.points.capacity() * std::mem::size_of::<[f32; 3]>();
        self.vertex_tree_vec_bytes += geometry.nodes.capacity() * std::mem::size_of::<VertexNode>();
        self.primitive_face_bytes += geometry
            .primitive_faces
            .as_ref()
            .map_or(0, |faces| std::mem::size_of_val(faces.as_ref()));
        if let Some(surface) = geometry.surfaces.get() {
            let memory = surface.memory();
            self.ready_surface_shapes += 1;
            self.edge_nodes += memory.edge_nodes;
            self.face_nodes += memory.face_nodes;
            self.edges += memory.edges;
            self.triangles += memory.triangles;
            self.edge_vec_bytes += memory.edge_vec_bytes;
            self.edge_tree_vec_bytes += memory.edge_tree_vec_bytes;
            self.face_tree_vec_bytes += memory.face_tree_vec_bytes;
        }
    }
    pub fn buffer_bytes(&self) -> usize {
        self.vertex_vec_bytes
            + self.vertex_tree_vec_bytes
            + self.edge_vec_bytes
            + self.edge_tree_vec_bytes
            + self.face_tree_vec_bytes
            + self.primitive_face_bytes
            + self.scene_tree_vec_bytes
    }
}

/// Worker-built geometry only. This owns immutable assets, never a document or editor lock.
#[derive(Default, Clone)]
pub struct PreparedVertices {
    geometries: HashMap<GeometryKey, Arc<Geometry>>,
    metrics: PreparationMetrics,
    _admission: Option<Arc<crate::snap_budget::Admission>>,
}
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct PreparationMetrics {
    pub elapsed_us: u64,
    pub reused_shapes: usize,
    pub reused_buffer_bytes: usize,
    pub budget: crate::snap_budget::Reservation,
    pub memory: IndexMemory,
    pub buffer_bytes: usize,
}
#[derive(serde::Serialize)]
pub struct SnapDiagnostics {
    pub document_epoch: u64,
    pub cache_epoch: u64,
    pub render_revision: u64,
    pub indexed_revision: Option<u64>,
    pub memory: IndexMemory,
    pub buffer_bytes: usize,
    pub last_preparation: Option<PreparationMetrics>,
    pub retention: RetentionDiagnostics,
    pub admission: AdmissionDiagnostics,
    pub surface_build: crate::surface_snap::SurfaceBuild,
}
pub fn diagnostics(editor: &Editor) -> SnapDiagnostics {
    let mut cache = editor.vertex_cache.borrow_mut();
    cache.publish_usage();
    let memory = cache.memory();
    let mut surface_build = crate::surface_snap::SurfaceBuild::default();
    for geometry in cache.geometries.values() {
        if let Some(surface) = geometry.surfaces.get() {
            surface_build.add(surface.build_profile());
        }
    }
    SnapDiagnostics {
        document_epoch: editor.document_epoch,
        cache_epoch: cache.epoch,
        render_revision: editor.render_revision,
        indexed_revision: cache.revision,
        buffer_bytes: memory.buffer_bytes(),
        retention: cache.retention_diagnostics(memory.buffer_bytes()),
        admission: cache.admission_diagnostics(),
        surface_build,
        memory,
        last_preparation: (cache.epoch == editor.document_epoch)
            .then_some(cache.last_preparation)
            .flatten(),
    }
}
impl PreparedVertices {
    pub fn retained_capacity(&self) -> usize {
        self.memory().buffer_bytes()
    }
    pub fn check_publication(&self, editor: &Editor) -> Result<(), String> {
        editor
            .snap_build_budget()?
            .with_admission(self._admission.clone())
            .check_capacity(self.retained_capacity())
    }
    pub fn memory(&self) -> IndexMemory {
        let mut memory = IndexMemory::default();
        for geometry in self.geometries.values() {
            memory.add(geometry);
        }
        memory
    }
    #[cfg(test)]
    pub fn meshes<'a>(
        assets: impl IntoIterator<Item = &'a Arc<crate::mesh::Mesh>>,
        cancel: Option<&AtomicBool>,
    ) -> Result<Self, String> {
        Self::meshes_with_budget(
            assets,
            cancel,
            crate::snap_budget::IndexBudget::configured()?,
        )
    }
    #[cfg(test)]
    fn meshes_with_budget<'a>(
        assets: impl IntoIterator<Item = &'a Arc<crate::mesh::Mesh>>,
        cancel: Option<&AtomicBool>,
        budget: crate::snap_budget::IndexBudget,
    ) -> Result<Self, String> {
        Self::meshes_context(assets, cancel, budget, None)
    }
    fn meshes_context<'a>(
        assets: impl IntoIterator<Item = &'a Arc<crate::mesh::Mesh>>,
        cancel: Option<&AtomicBool>,
        budget: crate::snap_budget::IndexBudget,
        task: Option<&crate::load_task::Task>,
    ) -> Result<Self, String> {
        let started = std::time::Instant::now();
        check_cancel(cancel)?;
        let mut collected = Vec::new();
        for asset in assets {
            check_cancel(cancel)?;
            collected.push(asset);
        }
        let reservation = budget.meshes(collected.iter().copied())?;
        let (budget, admission) = budget.admission(reservation.estimated_working_bytes)?;
        let mut prepared = Self {
            _admission: Some(admission),
            ..Self::default()
        };
        let mut retained_bytes = 0usize;
        let total = reservation.shapes;
        for asset in collected {
            check_cancel(cancel)?;
            let key = GeometryKey::Mesh(Arc::as_ptr(asset) as usize);
            if prepared.geometries.contains_key(&key) {
                continue;
            }
            if let Some(task) = task {
                task.progress(
                    crate::load_task::Phase::Vertices,
                    prepared.geometries.len(),
                    total,
                )?;
            }
            asset.validate_cancellable(cancel)?;
            let geometry = Geometry::from_faces(&asset.triangles, Some(asset.clone()), cancel)?;
            if let Some(task) = task {
                task.progress(
                    crate::load_task::Phase::Surfaces,
                    prepared.geometries.len(),
                    total,
                )?;
            }
            geometry.prepare_surfaces(cancel)?;
            let mut memory = IndexMemory::default();
            memory.add(&geometry);
            retained_bytes = retained_bytes
                .checked_add(memory.buffer_bytes())
                .ok_or("SNAP_INDEX_BUDGET_EXCEEDED")?;
            budget.check_capacity(retained_bytes)?;
            prepared.geometries.insert(key, Arc::new(geometry));
        }
        if let Some(task) = task {
            task.progress(crate::load_task::Phase::Surfaces, total, total)?;
        }
        check_cancel(cancel)?;
        let memory = prepared.memory();
        prepared
            ._admission
            .as_ref()
            .unwrap()
            .ensure_capacity(&budget, memory.buffer_bytes())?;
        prepared.metrics = PreparationMetrics {
            budget: reservation,
            reused_shapes: 0,
            reused_buffer_bytes: 0,
            elapsed_us: started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
            buffer_bytes: memory.buffer_bytes(),
            memory,
        };
        Ok(prepared)
    }
    #[cfg(test)]
    pub fn meshes_with_task<'a>(
        assets: impl IntoIterator<Item = &'a Arc<crate::mesh::Mesh>>,
        task: &crate::load_task::Task,
    ) -> Result<Self, String> {
        Self::meshes_with_task_budget(assets, task, crate::snap_budget::IndexBudget::configured()?)
    }
    pub fn meshes_with_task_budget<'a>(
        assets: impl IntoIterator<Item = &'a Arc<crate::mesh::Mesh>>,
        task: &crate::load_task::Task,
        budget: crate::snap_budget::IndexBudget,
    ) -> Result<Self, String> {
        Self::meshes_context(assets, Some(task.cancel_flag()), budget, Some(task))
    }
    #[cfg(test)]
    pub fn document(document: &Document) -> Result<Self, String> {
        Self::document_with_budget(document, crate::snap_budget::IndexBudget::configured()?)
    }
    #[cfg(test)]
    fn document_with_budget(
        document: &Document,
        budget: crate::snap_budget::IndexBudget,
    ) -> Result<Self, String> {
        // Hidden referenced assets are warm too; orphan assets retained for undo need no work.
        let ids: BTreeSet<_> = document
            .objects
            .iter()
            .filter_map(|o| o.mesh_id.as_ref())
            .collect();
        Self::meshes_with_budget(
            ids.into_iter().filter_map(|id| document.meshes.get(id)),
            None,
            budget,
        )
    }
}
/// Own the exact document that produced the scene. It cannot be adopted into another document.
pub struct PreparedDocument {
    document: Document,
    geometries: HashMap<GeometryKey, Arc<Geometry>>,
    scene: Arc<SceneIndex>,
    metrics: PreparationMetrics,
    _admission: Arc<crate::snap_budget::Admission>,
    reused_buffer_bytes: usize,
}
impl PreparedDocument {
    #[cfg(test)]
    pub fn new(document: Document) -> Result<Self, String> {
        Self::with_budget(document, crate::snap_budget::IndexBudget::configured()?)
    }
    #[cfg(test)]
    fn with_budget(
        document: Document,
        budget: crate::snap_budget::IndexBudget,
    ) -> Result<Self, String> {
        Self::context(document, budget, None)
    }
    #[cfg(test)]
    pub fn with_task(document: Document, task: &crate::load_task::Task) -> Result<Self, String> {
        Self::with_task_budget(
            document,
            task,
            crate::snap_budget::IndexBudget::configured()?,
        )
    }
    pub fn with_task_budget(
        document: Document,
        task: &crate::load_task::Task,
        budget: crate::snap_budget::IndexBudget,
    ) -> Result<Self, String> {
        Self::context(document, budget, Some(task))
    }
    fn context(
        document: Document,
        budget: crate::snap_budget::IndexBudget,
        task: Option<&crate::load_task::Task>,
    ) -> Result<Self, String> {
        Self::context_seed(document, budget, task, HashMap::new())
    }
    fn context_seed(
        document: Document,
        budget: crate::snap_budget::IndexBudget,
        task: Option<&crate::load_task::Task>,
        mut seed: HashMap<GeometryKey, Arc<Geometry>>,
    ) -> Result<Self, String> {
        let started = std::time::Instant::now();
        let cancel = task.map(|t| t.cancel_flag());
        if let Some(task) = task {
            task.progress(crate::load_task::Phase::Validate, 0, 0)?;
        }
        if let Some(task) = task {
            document.validate_with_task(task)?;
        } else {
            document.validate()?;
        }
        check_cancel(cancel)?;
        let mut required = HashSet::new();
        let mut triangles = 0usize;
        let mut instances = 0usize;
        for object in &document.objects {
            check_cancel(cancel)?;
            if object.visible {
                instances += if object.mirror { 2 } else { 1 };
            }
            if object.mesh_id.is_none() && !object.visible {
                continue;
            }
            if let Some(key) = GeometryKey::in_document(&document, object) {
                required.insert(key);
            }
        }
        seed.retain(|key, g| required.contains(key) && g.surfaces.get().is_some());
        let mut missing = HashSet::new();
        for object in &document.objects {
            let Some(key) = GeometryKey::in_document(&document, object) else {
                continue;
            };
            if required.contains(&key) && !seed.contains_key(&key) && missing.insert(key) {
                triangles = triangles
                    .checked_add(
                        object
                            .mesh_id
                            .as_ref()
                            .and_then(|id| document.meshes.get(id))
                            .map_or_else(
                                || crate::renderer::geometry(&object.kind).len(),
                                |m| m.triangles.len(),
                            ),
                    )
                    .ok_or("SNAP_INDEX_BUDGET_EXCEEDED")?;
            }
        }
        let reservation = budget.reserve(triangles, instances, missing.len())?;
        let (budget, admission) = budget.admission(reservation.estimated_working_bytes)?;
        let reused_shapes = seed.len();
        let reused_buffer_bytes = seed.values().map(|g| g.buffer_bytes()).sum::<usize>();
        let ids: BTreeSet<_> = document
            .objects
            .iter()
            .filter_map(|o| o.mesh_id.as_ref())
            .collect();
        let mut prepared = PreparedVertices::meshes_context(
            ids.into_iter()
                .filter_map(|id| document.meshes.get(id))
                .filter(|asset| {
                    !seed.contains_key(&GeometryKey::Mesh(Arc::as_ptr(asset) as usize))
                }),
            cancel,
            budget.clone(),
            task,
        )?;
        prepared.geometries.extend(seed);
        let mut scene = SceneIndex::default();
        let mut leaves = Vec::new();
        if let Some(task) = task {
            task.progress(
                crate::load_task::Phase::Instances,
                0,
                document.objects.len(),
            )?;
        }
        for (position, object) in document.objects.iter().enumerate() {
            if position.is_multiple_of(128) {
                budget.check_capacity(reservation.estimated_working_bytes)?;
                if let Some(task) = task {
                    task.progress(
                        crate::load_task::Phase::Instances,
                        position,
                        document.objects.len(),
                    )?;
                }
            }
            if !object.visible {
                continue;
            }
            let Some(revision) = InstanceRevision::in_document(&document, object) else {
                continue;
            };
            let geometry = match prepared.geometries.entry(revision.geometry) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.get().clone(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    // All referenced mesh assets are prepared above. Only built-in shapes remain.
                    let geometry = Geometry::from_faces(
                        crate::renderer::geometry(&object.kind),
                        None,
                        cancel,
                    )?;
                    geometry.prepare_surfaces(cancel)?;
                    entry.insert(Arc::new(geometry)).clone()
                }
            };
            let Some(root) = geometry.nodes.first() else {
                continue;
            };
            for mirrored in [false, true] {
                if mirrored && !object.mirror {
                    continue;
                }
                let matrix = model_matrix(object, mirrored);
                let bounds = root.bounds.transformed(matrix);
                let key = (object.id, mirrored);
                scene.instances.insert(
                    key,
                    Instance {
                        revision,
                        geometry: geometry.clone(),
                        matrix,
                        bounds,
                    },
                );
                leaves.push((key, bounds));
            }
        }
        scene.tree = BoundsTree::from_leaves_cancellable(leaves, cancel)?;
        if let Some(task) = task {
            task.progress(
                crate::load_task::Phase::Instances,
                document.objects.len(),
                document.objects.len(),
            )?;
        }
        let mut memory = prepared.memory();
        scene.add_memory(&mut memory);
        admission.ensure_capacity(
            &budget,
            memory
                .buffer_bytes()
                .saturating_sub(reused_buffer_bytes)
                .saturating_add(scene.capacity_payload_bytes()),
        )?;
        let metrics = PreparationMetrics {
            budget: reservation,
            reused_shapes,
            reused_buffer_bytes,
            elapsed_us: started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
            buffer_bytes: memory.buffer_bytes(),
            memory,
        };
        Ok(Self {
            document,
            geometries: prepared.geometries,
            scene: Arc::new(scene),
            metrics,
            _admission: admission,
            reused_buffer_bytes,
        })
    }
    pub fn check_publication(&self, editor: &Editor) -> Result<(), String> {
        editor
            .snap_build_budget()?
            .with_admission(Some(self._admission.clone()))
            .check_capacity(self.retained_capacity())
    }
    pub fn retained_capacity(&self) -> usize {
        self.metrics
            .buffer_bytes
            .saturating_sub(self.reused_buffer_bytes)
            .saturating_add(self.scene.capacity_payload_bytes())
    }
    pub fn document(&self) -> &Document {
        &self.document
    }
}
/// Snapshot captured after conversion, never a lock carried into the worker.
pub struct UsdAppendInput {
    document: Document,
    first: u64,
    stamp: crate::persistence::Stamp,
    seed: HashMap<GeometryKey, Arc<Geometry>>,
    budget: crate::snap_budget::IndexBudget,
}
pub struct PreparedUsdAppend {
    prepared: PreparedDocument,
    publication: crate::usd::AppendPublication,
    stamp: crate::persistence::Stamp,
}
impl PreparedUsdAppend {
    pub fn build(
        input: UsdAppendInput,
        imported: crate::usd::Imported,
        task: &crate::load_task::Task,
    ) -> Result<Self, String> {
        task.progress(crate::load_task::Phase::Validate, 0, 0)?;
        let plan = crate::usd::AppendPlan::prepare(
            input.document,
            input.first,
            imported,
            Some(task.cancel_flag()),
        )?;
        let (document, publication) = plan.into_parts();
        let prepared =
            PreparedDocument::context_seed(document, input.budget, Some(task), input.seed)?;
        Ok(Self {
            prepared,
            publication,
            stamp: input.stamp,
        })
    }
    pub fn commit(
        self,
        editor: &mut Editor,
        task: &crate::load_task::Task,
    ) -> Result<crate::usd::Report, String> {
        editor.ensure_unchanged(self.stamp)?;
        self.prepared.check_publication(editor)?;
        task.commit()?;
        let PreparedDocument {
            document,
            geometries,
            scene,
            metrics,
            _admission,
            ..
        } = self.prepared;
        let report = self.publication.publish(editor, document)?;
        let mut cache = editor.vertex_cache.borrow_mut();
        cache.publish_usage();
        if cache.epoch != editor.document_epoch {
            cache.geometries.clear();
            cache.access_order.clear();
            cache.retention.last_trim = None;
            cache.retention.last_check = None;
        }
        for (key, geometry) in geometries {
            let reused = cache
                .geometries
                .get(&key)
                .is_some_and(|old| Arc::ptr_eq(old, &geometry));
            if !reused {
                cache.work.prepared_geometries += 1;
                cache.work.prepared_vertices += geometry.points.len();
                cache.work.prepared_surface_indices +=
                    usize::from(geometry.surfaces.get().is_some());
            }
            cache.touch(key);
            cache.geometries.insert(key, geometry);
        }
        cache.work.prepared_instances += scene.instances.len();
        cache.scene = scene;
        cache.epoch = editor.document_epoch;
        cache.revision = Some(editor.render_revision);
        cache.last_preparation = Some(metrics);
        cache.trim(false);
        drop(_admission);
        Ok(report)
    }
}
impl Editor {
    pub fn capture_usd_append(&mut self) -> Result<UsdAppendInput, String> {
        self.ensure_not_dragging()?;
        let budget = self.snap_build_budget()?;
        let first = self.next_id()?;
        let cache = self.vertex_cache.borrow();
        let seed = if cache.epoch == self.document_epoch {
            cache.geometries.clone()
        } else {
            HashMap::new()
        };
        Ok(UsdAppendInput {
            document: self.document.clone(),
            first,
            stamp: self.stamp(),
            seed,
            budget,
        })
    }
}
impl Editor {
    /// Replacement and indices are inseparable; callers still fence file transactions by Stamp.
    pub fn replace_prepared_document(
        &mut self,
        prepared: PreparedDocument,
        path: Option<std::path::PathBuf>,
        recovered: bool,
    ) {
        self.replace_document(prepared.document, path, recovered);
        let mut cache = self.vertex_cache.borrow_mut();
        let mut work = cache.work;
        work.prepared_geometries += prepared.geometries.len();
        work.prepared_vertices += prepared.metrics.memory.vertices;
        work.prepared_surface_indices += prepared.metrics.memory.ready_surface_shapes;
        work.prepared_instances += prepared.scene.instances.len();
        cache.publish_usage();
        let usage = cache.usage.clone();
        let tracked_scenes = std::mem::take(&mut cache.tracked_scenes);
        let tracked_geometries = std::mem::take(&mut cache.tracked_geometries);
        let retention = std::mem::take(&mut cache.retention);
        *cache = VertexCache {
            usage,
            tracked_scenes,
            tracked_geometries,
            retention,
            revision: Some(self.render_revision),
            epoch: self.document_epoch,
            geometries: prepared.geometries,
            scene: prepared.scene,
            work,
            last_preparation: Some(prepared.metrics),
            ..VertexCache::default()
        };
        cache.trim(false);
    }
}
impl Editor {
    pub fn snap_build_budget(&self) -> Result<crate::snap_budget::IndexBudget, String> {
        let mut cache = self.vertex_cache.borrow_mut();
        cache.publish_usage();
        Ok(crate::snap_budget::IndexBudget::configured()?.with_usage(cache.usage.clone()))
    }
    /// Copy only immutable asset handles; construction happens after releasing Editor.
    pub fn missing_snap_meshes(&self) -> Vec<Arc<crate::mesh::Mesh>> {
        let cache = self.vertex_cache.borrow();
        let mut seen = HashSet::new();
        self.document
            .objects
            .iter()
            .filter(|o| o.visible)
            .filter_map(|o| o.mesh_id.as_ref())
            .filter_map(|id| self.document.meshes.get(id))
            .filter(|asset| {
                let key = GeometryKey::Mesh(Arc::as_ptr(asset) as usize);
                seen.insert(key)
                    && (cache.epoch != self.document_epoch
                        || cache
                            .geometries
                            .get(&key)
                            .is_none_or(|g| g.surfaces.get().is_none()))
            })
            .cloned()
            .collect()
    }
    pub fn trim_snap_cache(&self, purge_idle: bool) -> TrimResult {
        self.vertex_cache.borrow_mut().trim(purge_idle)
    }
    pub fn set_snap_cache_budget(&self, mib: u32) -> Result<TrimResult, String> {
        if !(1..=4096).contains(&mib) {
            return Err("SNAP_CACHE_INVALID_BUDGET: 1..4096 MiB".into());
        }
        let bytes = (mib as usize)
            .checked_mul(1024 * 1024)
            .ok_or("SNAP_CACHE_INVALID_BUDGET")?;
        let mut cache = self.vertex_cache.borrow_mut();
        cache.retention.limit_bytes = bytes;
        Ok(cache.trim(false))
    }
    #[cfg(test)]
    pub fn install_prepared_vertices(&self, prepared: PreparedVertices) {
        self.install_prepared_vertices_context(prepared, true);
    }
    pub fn install_snap_gesture_vertices(&self, prepared: PreparedVertices) {
        // Existing visible cache entries must survive until the target snapshot pins them.
        self.install_prepared_vertices_context(prepared, false);
    }
    fn install_prepared_vertices_context(&self, prepared: PreparedVertices, trim: bool) {
        let mut cache = self.vertex_cache.borrow_mut();
        if cache.epoch != self.document_epoch {
            cache.geometries.clear();
            cache.access_order.clear();
            cache.retention.last_trim = None;
            cache.retention.last_check = None;
            cache.scene = Arc::new(SceneIndex::default());
            cache.revision = None;
            cache.epoch = self.document_epoch;
            cache.last_preparation = None;
        }
        let live: HashSet<_> = self
            .document
            .meshes
            .values()
            .map(|a| GeometryKey::Mesh(Arc::as_ptr(a) as usize))
            .collect();
        if prepared.geometries.keys().all(|key| live.contains(key)) {
            cache.last_preparation = Some(prepared.metrics);
        }
        for (key, geometry) in prepared.geometries {
            if !live.contains(&key) {
                continue;
            }
            cache.touch(key);
            if let Some(existing) = cache.geometries.get(&key) {
                // Same immutable asset and deterministic vertex ordering: share the ready
                // surface index into existing snapshots without copying vertex arrays.
                let installed = geometry
                    .surfaces
                    .get()
                    .is_some_and(|surface| existing.surfaces.set(surface.clone()).is_ok());
                if installed {
                    cache.work.prepared_surface_indices += 1;
                }
            } else {
                cache.work.prepared_geometries += 1;
                cache.work.prepared_vertices += geometry.points.len();
                cache.work.prepared_surface_indices +=
                    usize::from(geometry.surfaces.get().is_some());
                cache.geometries.insert(key, geometry);
            }
        }
        if trim {
            cache.trim(false);
        } else {
            cache.publish_usage();
        }
    }
}
#[derive(Clone, Copy, PartialEq)]
struct InstanceRevision {
    geometry: GeometryKey,
    position: [f32; 3],
    rotation: [f32; 3],
    scale: [f32; 3],
    shear: [f32; 3],
}
impl InstanceRevision {
    fn new(editor: &Editor, object: &Object) -> Option<Self> {
        Self::in_document(&editor.document, object)
    }
    fn in_document(document: &Document, object: &Object) -> Option<Self> {
        Some(Self {
            geometry: GeometryKey::in_document(document, object)?,
            position: object.position,
            rotation: object.rotation,
            scale: object.scale,
            shear: object.shear,
        })
    }
}
#[derive(Clone)]
struct Instance {
    revision: InstanceRevision,
    geometry: Arc<Geometry>,
    matrix: [[f32; 4]; 4],
    bounds: Bounds,
}
#[derive(Clone, Default)]
struct SceneIndex {
    instances: HashMap<(u64, bool), Instance>,
    tree: BoundsTree<(u64, bool)>,
}
impl SceneIndex {
    fn capacity_payload_bytes(&self) -> usize {
        self.instances.capacity() * std::mem::size_of::<((u64, bool), Instance)>()
            + self.tree.capacity_payload_bytes()
    }
    fn managed_bytes(&self) -> usize {
        self.tree
            .vec_bytes()
            .saturating_add(self.capacity_payload_bytes())
    }
    fn compact(&mut self) {
        self.tree.compact();
        self.instances.shrink_to_fit();
    }
    fn add_memory(&self, memory: &mut IndexMemory) {
        memory.instances = self.instances.len();
        memory.scene_tree_vec_bytes = self.tree.vec_bytes();
        memory.scene_entry_payload_bytes = self.instances.len()
            * std::mem::size_of::<((u64, bool), Instance)>()
            + self.tree.entry_payload_bytes();
    }
}
#[derive(Default, Clone, Copy, Debug)]
pub struct CacheWork {
    pub full_syncs: usize,
    pub objects_visited: usize,
    pub instances_updated: usize,
    pub geometries_built: usize,
    pub unique_vertices: usize,
    pub snapshot_copies: usize,
    pub prepared_geometries: usize,
    pub prepared_vertices: usize,
    pub prepared_surface_indices: usize,
    pub prepared_instances: usize,
}
const DEFAULT_CACHE_BYTES: usize = 128 * 1024 * 1024;
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct TrimResult {
    pub before_buffer_bytes: usize,
    pub after_buffer_bytes: usize,
    pub evicted_shapes: usize,
    pub evicted_buffer_bytes: usize,
    pub scene_reclaimed_buffer_bytes: usize,
    pub scene_reclaimed_payload_bytes: usize,
    pub scene_compaction_deferred: bool,
}
#[derive(serde::Serialize)]
pub struct RetentionDiagnostics {
    pub limit_bytes: usize,
    pub current_buffer_bytes: usize,
    pub referenced_buffer_bytes: usize,
    pub idle_buffer_bytes: usize,
    pub over_budget_bytes: usize,
    pub evictions_total: usize,
    pub evicted_buffer_bytes_total: usize,
    pub last_trim: Option<TrimResult>,
}
#[derive(serde::Serialize)]
pub struct AdmissionDiagnostics {
    pub resident_index_bytes: usize,
    pub reserved_working_bytes: usize,
    pub combined_bytes: usize,
    pub tracked_scenes: usize,
}
struct Retention {
    limit_bytes: usize,
    evictions_total: usize,
    evicted_buffer_bytes_total: usize,
    last_trim: Option<TrimResult>,
    last_check: Option<std::time::Instant>,
}
impl Default for Retention {
    fn default() -> Self {
        Self {
            limit_bytes: DEFAULT_CACHE_BYTES,
            evictions_total: 0,
            evicted_buffer_bytes_total: 0,
            last_trim: None,
            last_check: None,
        }
    }
}
#[derive(Default)]
pub struct VertexCache {
    revision: Option<u64>,
    epoch: u64,
    geometries: HashMap<GeometryKey, Arc<Geometry>>,
    scene: Arc<SceneIndex>,
    pub work: CacheWork,
    last_preparation: Option<PreparationMetrics>,
    retention: Retention,
    access_order: HashMap<GeometryKey, u64>,
    access_clock: u64,
    usage: Arc<crate::snap_budget::Usage>,
    tracked_scenes: Vec<Weak<SceneIndex>>,
    tracked_geometries: Vec<Weak<Geometry>>,
}
impl VertexCache {
    fn publish_usage(&mut self) {
        self.tracked_scenes.push(Arc::downgrade(&self.scene));
        self.tracked_geometries
            .extend(self.geometries.values().map(Arc::downgrade));
        let mut scenes = HashSet::new();
        let mut bytes = 0usize;
        self.tracked_scenes.retain(|weak| {
            let Some(scene) = weak.upgrade() else {
                return false;
            };
            if !scenes.insert(Arc::as_ptr(&scene) as usize) {
                return false;
            }
            bytes = bytes.saturating_add(scene.managed_bytes());
            true
        });
        // Shape handles are registered on adoption/synchronization, before they can retire.
        // Accounting never walks scene objects, vertices or topology.
        let mut seen = HashSet::new();
        self.tracked_geometries.retain(|weak| {
            let Some(g) = weak.upgrade() else {
                return false;
            };
            let ptr = Arc::as_ptr(&g) as usize;
            if !seen.insert(ptr) {
                return false;
            }
            bytes = bytes.saturating_add(g.buffer_bytes());
            true
        });
        self.usage.resident.store(bytes, Ordering::SeqCst);
    }
    fn admission_diagnostics(&self) -> AdmissionDiagnostics {
        let resident = self.usage.resident.load(Ordering::SeqCst);
        let working = self.usage.working.load(Ordering::SeqCst);
        AdmissionDiagnostics {
            resident_index_bytes: resident,
            reserved_working_bytes: working,
            combined_bytes: resident.saturating_add(working),
            tracked_scenes: self.tracked_scenes.len(),
        }
    }
    fn touch(&mut self, key: GeometryKey) {
        self.access_clock = self.access_clock.saturating_add(1);
        self.access_order.insert(key, self.access_clock);
    }
    fn retention_diagnostics(&self, current: usize) -> RetentionDiagnostics {
        let referenced = self.scene.tree.vec_bytes()
            + self
                .geometries
                .values()
                .filter(|g| Arc::strong_count(g) > 1)
                .map(|g| g.buffer_bytes())
                .sum::<usize>();
        RetentionDiagnostics {
            limit_bytes: self.retention.limit_bytes,
            current_buffer_bytes: current,
            referenced_buffer_bytes: referenced,
            idle_buffer_bytes: current.saturating_sub(referenced),
            over_budget_bytes: current.saturating_sub(self.retention.limit_bytes),
            evictions_total: self.retention.evictions_total,
            evicted_buffer_bytes_total: self.retention.evicted_buffer_bytes_total,
            last_trim: self.retention.last_trim,
        }
    }
    fn trim(&mut self, purge_idle: bool) -> TrimResult {
        let before = self.memory().buffer_bytes();
        let target = if purge_idle {
            0
        } else {
            self.retention.limit_bytes
        };
        let mut result = TrimResult {
            before_buffer_bytes: before,
            after_buffer_bytes: before,
            ..TrimResult::default()
        };
        let scene_bytes = self.scene.tree.vec_bytes();
        let payload = self.scene.capacity_payload_bytes();
        let live_payload = self.scene.instances.len().saturating_mul(
            std::mem::size_of::<((u64, bool), Instance)>()
                + std::mem::size_of::<((u64, bool), usize)>(),
        );
        let sparse = self.scene.tree.has_excess_capacity()
            || payload > live_payload.saturating_mul(4).saturating_add(64 * 1024);
        if purge_idle || sparse {
            if Arc::strong_count(&self.scene) == 1 {
                let payload = self.scene.capacity_payload_bytes();
                Arc::make_mut(&mut self.scene).compact();
                result.scene_reclaimed_payload_bytes =
                    payload.saturating_sub(self.scene.capacity_payload_bytes());
                result.scene_reclaimed_buffer_bytes =
                    scene_bytes.saturating_sub(self.scene.tree.vec_bytes());
                result.after_buffer_bytes = result
                    .after_buffer_bytes
                    .saturating_sub(result.scene_reclaimed_buffer_bytes);
            } else {
                result.scene_compaction_deferred = true;
            }
        }
        if purge_idle || before > target {
            let mut idle: Vec<_> = self
                .geometries
                .iter()
                .filter(|(_, g)| Arc::strong_count(g) == 1)
                .map(|(key, g)| {
                    (
                        self.access_order.get(key).copied().unwrap_or(0),
                        *key,
                        g.buffer_bytes(),
                    )
                })
                .collect();
            idle.sort_unstable_by_key(|(order, key, _)| (*order, *key));
            for (_, key, bytes) in idle {
                if !purge_idle && result.after_buffer_bytes <= target {
                    break;
                }
                self.geometries.remove(&key);
                self.access_order.remove(&key);
                result.evicted_shapes += 1;
                result.evicted_buffer_bytes += bytes;
                result.after_buffer_bytes = result.after_buffer_bytes.saturating_sub(bytes);
            }
        }
        self.access_order
            .retain(|key, _| self.geometries.contains_key(key));
        self.retention.evictions_total += result.evicted_shapes;
        self.retention.evicted_buffer_bytes_total += result.evicted_buffer_bytes;
        self.retention.last_trim = Some(result);
        self.retention.last_check = Some(std::time::Instant::now());
        self.publish_usage();
        result
    }
    fn retry_trim(&mut self) {
        if self
            .retention
            .last_check
            .is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(500))
        {
            self.trim(false);
        }
    }
    pub fn memory(&self) -> IndexMemory {
        let mut memory = IndexMemory::default();
        for geometry in self.geometries.values() {
            memory.add(geometry);
        }
        self.scene.add_memory(&mut memory);
        memory
    }
    fn geometry(&mut self, editor: &Editor, object: &Object, key: GeometryKey) -> Arc<Geometry> {
        self.touch(key);
        if let Some(geometry) = self.geometries.get(&key) {
            return geometry.clone();
        }
        let geometry = Arc::new(Geometry::build(editor, object));
        self.work.geometries_built += 1;
        self.work.unique_vertices += geometry.points.len();
        self.geometries.insert(key, geometry.clone());
        geometry
    }
    fn source(&mut self, editor: &Editor) -> Option<Instance> {
        if self.epoch != editor.document_epoch {
            self.geometries.clear();
            self.access_order.clear();
            self.retention.last_trim = None;
            self.retention.last_check = None;
            self.last_preparation = None;
            self.scene = Arc::new(SceneIndex::default());
            self.revision = None;
            self.epoch = editor.document_epoch;
        }
        let (_, object) = editor.indexed_object(editor.document.selected?)?;
        if !object.visible {
            return None;
        }
        let revision = InstanceRevision::new(editor, object)?;
        self.touch(revision.geometry);
        // Source lookup is O(1) in object count and never synchronizes the target scene.
        if let Some(instance) = self
            .scene
            .instances
            .get(&(object.id, false))
            .filter(|i| i.revision == revision)
        {
            return Some(instance.clone());
        }
        let geometry = self.geometry(editor, object, revision.geometry);
        let matrix = model_matrix(object, false);
        let bounds = geometry.nodes.first()?.bounds.transformed(matrix);
        let instance = Instance {
            revision,
            geometry,
            matrix,
            bounds,
        };
        self.retry_trim();
        self.publish_usage();
        Some(instance)
    }
    fn scene_mut(&mut self) -> &mut SceneIndex {
        if Arc::strong_count(&self.scene) > 1 {
            self.work.snapshot_copies += 1;
        }
        Arc::make_mut(&mut self.scene)
    }
    fn instance(
        &mut self,
        editor: &Editor,
        object: &Object,
        revision: InstanceRevision,
        mirrored: bool,
    ) -> Option<Instance> {
        let geometry = self.geometry(editor, object, revision.geometry);
        let matrix = model_matrix(object, mirrored);
        let bounds = geometry.nodes.first()?.bounds.transformed(matrix);
        Some(Instance {
            revision,
            geometry,
            matrix,
            bounds,
        })
    }
    fn rebuild_scene(&mut self, editor: &Editor) {
        // Drop the old snapshot before allocating the replacement, unless a gesture owns it.
        self.scene = Arc::new(SceneIndex::default());
        let mut scene = SceneIndex::default();
        let mut leaves = Vec::new();
        for object in &editor.document.objects {
            self.work.objects_visited += 1;
            if !object.visible {
                continue;
            }
            let Some(revision) = InstanceRevision::new(editor, object) else {
                continue;
            };
            for mirrored in [false, true] {
                if mirrored && !object.mirror {
                    continue;
                }
                let Some(instance) = self.instance(editor, object, revision, mirrored) else {
                    continue;
                };
                let key = (object.id, mirrored);
                leaves.push((key, instance.bounds));
                scene.instances.insert(key, instance);
                self.work.instances_updated += 1;
            }
        }
        scene.tree = BoundsTree::from_leaves(leaves);
        self.scene = Arc::new(scene);
    }
    fn update(&mut self, editor: &Editor, id: u64, object: Option<&Object>) {
        self.work.objects_visited += 1;
        let desired = object
            .filter(|o| o.visible)
            .and_then(|o| InstanceRevision::new(editor, o).map(|revision| (o, revision)));
        for mirrored in [false, true] {
            let key = (id, mirrored);
            let desired = desired.filter(|(o, _)| !mirrored || o.mirror);
            if let Some((object, revision)) = desired {
                if self
                    .scene
                    .instances
                    .get(&key)
                    .is_some_and(|i| i.revision == revision)
                {
                    continue;
                }
                let Some(instance) = self.instance(editor, object, revision, mirrored) else {
                    continue;
                };
                let bounds = instance.bounds;
                let scene = self.scene_mut();
                scene.tree.insert(key, bounds);
                scene.instances.insert(key, instance);
                self.work.instances_updated += 1;
            } else if self.scene.instances.contains_key(&key) {
                let geometry_key = self.scene.instances[&key].revision.geometry;
                self.touch(geometry_key);
                let scene = self.scene_mut();
                scene.tree.remove(key);
                scene.instances.remove(&key);
                self.work.instances_updated += 1;
            }
        }
    }
    fn snapshot(&mut self, editor: &Editor) -> Arc<SceneIndex> {
        if self.revision == Some(editor.render_revision) && self.epoch == editor.document_epoch {
            if self.retention.last_trim.is_some_and(|r| {
                r.after_buffer_bytes > self.retention.limit_bytes || r.scene_compaction_deferred
            }) {
                self.retry_trim();
            }
            return self.scene.clone();
        }
        let reset = self.epoch != editor.document_epoch
            || self.revision.is_some_and(|r| {
                r > editor.render_revision
                    || editor
                        .render_changes
                        .iter()
                        .any(|(next, c)| *next > r && matches!(c, RenderChange::Reset))
            });
        let full = reset
            || self.revision.is_none()
            || self.revision.is_some_and(|r| {
                editor
                    .render_changes
                    .front()
                    .is_none_or(|(first, _)| r + 1 < *first)
            });
        if full {
            self.work.full_syncs += 1;
            if self.epoch != editor.document_epoch {
                self.geometries.clear();
                self.access_order.clear();
                self.retention.last_trim = None;
                self.retention.last_check = None;
                self.last_preparation = None;
            }
            self.rebuild_scene(editor);
        } else {
            let mut ids = BTreeSet::new();
            for (_, change) in editor
                .render_changes
                .iter()
                .filter(|(r, _)| *r > self.revision.unwrap())
            {
                if let RenderChange::Objects(changed) = change {
                    ids.extend(changed.iter().copied());
                }
            }
            for id in ids {
                self.update(editor, id, editor.indexed_object(id).map(|(_, o)| o));
            }
        }
        // Match immutable asset identity, not the import-time name or object ID.
        let live: HashSet<_> = editor
            .document
            .meshes
            .values()
            .map(|a| GeometryKey::Mesh(Arc::as_ptr(a) as usize))
            .collect();
        self.geometries
            .retain(|key, _| matches!(key, GeometryKey::Primitive(_)) || live.contains(key));
        self.revision = Some(editor.render_revision);
        self.epoch = editor.document_epoch;
        self.trim(false);
        self.scene.clone()
    }
}
#[derive(Default, Clone, Copy, Debug)]
pub struct QueryWork {
    pub object_nodes: usize,
    pub instances: usize,
    pub vertex_nodes: usize,
    pub vertices: usize,
}
enum IndexData {
    Source(Option<Instance>),
    Targets(Arc<SceneIndex>),
}
pub struct VertexIndex {
    data: IndexData,
    projection: Projection,
    selected: Option<u64>,
}
type RankedCandidate = (f32, (u64, bool, [u32; 3]), Candidate);
struct Query<'a> {
    projection: &'a Projection,
    point: [f32; 2],
    planes: [[f64; 4]; 6],
    best: Option<RankedCandidate>,
    work: QueryWork,
}
impl Projection {
    fn planes(&self, point: [f32; 2]) -> [[f64; 4]; 6] {
        self.planes_with_radius(point, RADIUS)
    }
    pub(crate) fn planes_with_radius(&self, point: [f32; 2], radius: f32) -> [[f64; 4]; 6] {
        let scale = 0.9 * f64::from(self.size[1]);
        let f = self.forward.map(f64::from);
        let r = self.right.map(f64::from);
        let u = self.up.map(f64::from);
        let x = f64::from(point[0]) - f64::from(self.size[0]) * 0.5;
        let y = f64::from(self.size[1]) * 0.5 - f64::from(point[1]);
        let radius = f64::from(radius);
        let normals: [([f64; 3], f64); 6] = [
            (
                std::array::from_fn(|i| scale * r[i] - (x - radius) * f[i]),
                0.,
            ),
            (
                std::array::from_fn(|i| (x + radius) * f[i] - scale * r[i]),
                0.,
            ),
            (
                std::array::from_fn(|i| scale * u[i] - (y - radius) * f[i]),
                0.,
            ),
            (
                std::array::from_fn(|i| (y + radius) * f[i] - scale * u[i]),
                0.,
            ),
            (f, -f64::from(self.near)),
            (f.map(|v| -v), f64::from(self.far)),
        ];
        normals.map(|(n, offset)| {
            [
                n[0],
                n[1],
                n[2],
                offset - (0..3).map(|i| n[i] * f64::from(self.eye[i])).sum::<f64>(),
            ]
        })
    }
}
impl Projection {
    pub(crate) fn on_screen(&self, screen: [f32; 2], margin: f32) -> bool {
        screen
            .iter()
            .enumerate()
            .all(|(i, v)| *v >= -margin && *v <= self.size[i] + margin)
    }
    pub(crate) fn surface_planes(&self, point: [f32; 2], face: bool) -> [[f64; 4]; 6] {
        self.planes_with_radius(point, if face { 0.001 } else { RADIUS })
    }
    pub(crate) fn ray(&self, point: [f32; 2]) -> ([f64; 3], [f64; 3]) {
        let x =
            (f64::from(point[0]) - f64::from(self.size[0]) * 0.5) / (0.9 * f64::from(self.size[1]));
        let y =
            (f64::from(self.size[1]) * 0.5 - f64::from(point[1])) / (0.9 * f64::from(self.size[1]));
        (
            self.eye.map(f64::from),
            std::array::from_fn(|i| {
                f64::from(self.forward[i])
                    + f64::from(self.right[i]) * x
                    + f64::from(self.up[i]) * y
            }),
        )
    }
    pub(crate) fn depth(&self, point: [f32; 3]) -> f64 {
        (0..3)
            .map(|i| f64::from(point[i] - self.eye[i]) * f64::from(self.forward[i]))
            .sum()
    }
    pub(crate) fn clip_depth(&self) -> (f64, f64) {
        (f64::from(self.near), f64::from(self.far))
    }
}
impl Query<'_> {
    fn consider(&mut self, world: [f32; 3], owner: (u64, bool)) {
        self.work.vertices += 1;
        let Some((screen, depth)) = self.projection.project(world) else {
            return;
        };
        if screen
            .iter()
            .enumerate()
            .any(|(i, v)| *v < -RADIUS || *v > self.projection.size[i] + RADIUS)
        {
            return;
        }
        let distance = (screen[0] - self.point[0]).powi(2) + (screen[1] - self.point[1]).powi(2);
        let rank = (owner.0, owner.1, world.map(f32::to_bits));
        if distance <= RADIUS * RADIUS
            && self.best.is_none_or(|(d, old, c)| {
                distance < d
                    || (distance == d && (depth < c.depth || (depth == c.depth && rank < old)))
            })
        {
            self.best = Some((
                distance,
                rank,
                Candidate {
                    world,
                    screen,
                    depth,
                },
            ));
        }
    }
    fn instance(&mut self, owner: (u64, bool), instance: &Instance) {
        if !instance.bounds.intersects(&self.planes) {
            return;
        }
        self.work.instances += 1;
        // Transform the cursor cone into local coordinates without inverting the affine matrix.
        // Reflections, shear and tiny scales therefore use the same conservative test.
        let to_local = |planes: [[f64; 4]; 6]| {
            planes.map(|p| {
                std::array::from_fn(|j| {
                    let value = (0..3)
                        .map(|i| p[i] * f64::from(instance.matrix[j][i]))
                        .sum::<f64>();
                    value + if j == 3 { p[3] } else { 0. }
                })
            })
        };
        let mut radius_squared = RADIUS * RADIUS;
        let mut local = to_local(self.planes);
        let geometry = &instance.geometry;
        let mut stack = Vec::new();
        if !geometry.nodes.is_empty() {
            stack.push(0);
        }
        while let Some(i) = stack.pop() {
            // Tighten the search after finding a closer vertex. Keep a small pixel margin
            // so f32 projection roundoff cannot drop an equal-distance/depth tie.
            if let Some((distance, _, _)) = self.best {
                if distance < radius_squared {
                    radius_squared = distance;
                    local = to_local(
                        self.projection
                            .planes_with_radius(self.point, distance.sqrt() + 0.001),
                    );
                }
            }
            self.work.vertex_nodes += 1;
            let node = &geometry.nodes[i];
            if !node.bounds.intersects(&local) {
                continue;
            }
            if let Some([a, b]) = node.children {
                // Visit the child closest to the cursor first, so a good candidate tightens
                // the cone before visiting distant leaves. This only orders work; never culls.
                let priority = |i: usize| {
                    let bounds = geometry.nodes[i].bounds;
                    let center = std::array::from_fn(|j| bounds.min[j] * 0.5 + bounds.max[j] * 0.5);
                    self.projection
                        .project(world_point(instance.matrix, center))
                        .map_or(f32::INFINITY, |(p, _)| {
                            (p[0] - self.point[0]).powi(2) + (p[1] - self.point[1]).powi(2)
                        })
                };
                if priority(a) <= priority(b) {
                    stack.extend([b, a]);
                } else {
                    stack.extend([a, b]);
                }
            } else {
                for point in &geometry.points[node.range.clone()] {
                    self.consider(world_point(instance.matrix, *point), owner);
                }
            }
        }
    }
}
impl VertexIndex {
    pub fn new(editor: &Editor, size: [f32; 2], source: bool) -> Self {
        let mut cache = editor.vertex_cache.borrow_mut();
        let data = if source {
            IndexData::Source(cache.source(editor))
        } else {
            IndexData::Targets(cache.snapshot(editor))
        };
        Self {
            data,
            projection: Projection::new(editor, size),
            selected: editor.document.selected,
        }
    }
    pub fn nearest_mode(&self, point: [f32; 2], mode: crate::gizmo::MoveSnap) -> Option<Candidate> {
        use crate::gizmo::MoveSnap;
        if !matches!(mode, MoveSnap::Edge | MoveSnap::Face) {
            return self.nearest(point);
        }
        let IndexData::Targets(scene) = &self.data else {
            return None;
        };
        let face = mode == MoveSnap::Face;
        if face && !self.projection.on_screen(point, 0.) {
            return None;
        }
        let planes = self.projection.surface_planes(point, face);
        let mut best: Option<RankedCandidate> = None;
        scene.tree.query(&planes, |owner| {
            if Some(owner.0) == self.selected {
                return;
            }
            let instance = &scene.instances[&owner];
            let geometry = &instance.geometry;
            let index = geometry.surfaces.get_or_init(|| {
                Arc::new(crate::surface_snap::SurfaceIndex::new(
                    &geometry.points,
                    geometry.faces(),
                    geometry
                        ._asset
                        .as_ref()
                        .and_then(|asset| asset.edge_mask.as_deref()),
                ))
            });
            if let Some(hit) = index.query(
                &geometry.points,
                geometry.faces(),
                instance.matrix,
                &self.projection,
                point,
                face,
            ) {
                let distance = if face {
                    0.
                } else {
                    (hit.screen[0] - point[0]).powi(2) + (hit.screen[1] - point[1]).powi(2)
                };
                let rank = (owner.0, owner.1, hit.world.map(f32::to_bits));
                if best.is_none_or(|(d, old, c)| {
                    distance < d
                        || (distance == d
                            && (hit.depth < c.depth || (hit.depth == c.depth && rank < old)))
                }) {
                    best = Some((
                        distance,
                        rank,
                        Candidate {
                            world: hit.world,
                            screen: hit.screen,
                            depth: hit.depth,
                        },
                    ));
                }
            }
        });
        best.map(|(_, _, candidate)| candidate)
    }
    pub fn nearest(&self, point: [f32; 2]) -> Option<Candidate> {
        self.query(point).0
    }
    fn query(&self, point: [f32; 2]) -> (Option<Candidate>, QueryWork) {
        let mut query = Query {
            projection: &self.projection,
            point,
            planes: self.projection.planes(point),
            best: None,
            work: QueryWork::default(),
        };
        match &self.data {
            IndexData::Source(Some(instance)) => {
                query.instance((self.selected.unwrap(), false), instance)
            }
            IndexData::Source(None) => {}
            IndexData::Targets(scene) => {
                let planes = query.planes;
                query.work.object_nodes = scene.tree.query(&planes, |owner| {
                    if Some(owner.0) != self.selected {
                        query.instance(owner, &scene.instances[&owner]);
                    }
                });
            }
        }
        (query.best.map(|(_, _, candidate)| candidate), query.work)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::{Mesh, Source};
    use std::sync::Arc;
    #[test]
    fn imported_vertices_follow_affine_mirror_visibility_and_exclude_the_selected_object() {
        let mut e = Editor::new();
        let mut o = e.document.objects[0].clone();
        o.id = 2;
        o.kind = "mesh".into();
        o.mesh_id = Some("test".into());
        o.position = [1.5, 0.2, 0.1];
        o.rotation = [13., 23., -8.];
        o.scale = [-0.8, 1.2, 0.6];
        o.shear = [0.2, -0.3, 0.1];
        o.mirror = true;
        let local = [0.2, 0.3, 0.4];
        e.document.meshes.insert(
            "test".into(),
            Arc::new(Mesh {
                edge_mask: None,
                triangles: vec![[local, [0.5, -0.2, 0.1], [-0.4, 0.1, -0.3]]; 10],
                color: [0.5; 3],
                source: Source {
                    origin: "test".into(),
                    revision: "test".into(),
                    license: "test".into(),
                },
                prim_path: "/test".into(),
            }),
        );
        e.document.objects.push(o.clone());
        let projection = Projection::new(&e, [800., 600.]);
        let index = VertexIndex::new(&e, [800., 600.], false);
        let IndexData::Targets(scene) = &index.data else {
            unreachable!()
        };
        assert_eq!(
            scene
                .instances
                .iter()
                .filter(|(owner, _)| Some(owner.0) != index.selected)
                .map(|(_, i)| i.geometry.points.len())
                .sum::<usize>(),
            6
        ); // shared triangle corners deduplicated
        for mirrored in [false, true] {
            let world = world_point(model_matrix(&o, mirrored), local);
            let screen = projection.project(world).unwrap().0;
            assert_eq!(index.nearest(screen).unwrap().world, world);
        }
        let screen = projection
            .project(world_point(model_matrix(&o, false), local))
            .unwrap()
            .0;
        let mut hidden = o.clone();
        hidden.visible = false;
        e.execute(crate::editor::Command::Update { object: hidden })
            .unwrap();
        assert!(VertexIndex::new(&e, [800., 600.], false)
            .nearest(screen)
            .is_none());
        o.position = [0., 0., 1000.];
        e.execute(crate::editor::Command::Update { object: o })
            .unwrap();
        assert!(VertexIndex::new(&e, [800., 600.], false)
            .nearest(screen)
            .is_none());
    }
    #[test]
    fn radius_and_depth_ties_are_deterministic() {
        let mut e = Editor::new();
        e.yaw = 0.;
        e.pitch = 0.;
        let projection = Projection::new(&e, [800., 600.]);
        let query = |point| Query {
            projection: &projection,
            point,
            planes: projection.planes(point),
            best: None,
            work: QueryWork::default(),
        };
        let near = [0., 0., 4.];
        let far = [0., 0., 2.];
        let mut q = query([400., 300.]);
        q.consider(far, (2, false));
        q.consider(near, (3, false));
        assert_eq!(q.best.unwrap().2.world, near);
        let mut q = query([413.9, 300.]);
        q.consider(near, (3, false));
        assert!(q.best.is_some());
        let mut q = query([414.1, 300.]);
        q.consider(near, (3, false));
        assert!(q.best.is_none());
    }
    fn legacy_candidates(
        editor: &Editor,
        size: [f32; 2],
        source: bool,
    ) -> Vec<((u64, bool), Candidate)> {
        let projection = Projection::new(editor, size);
        let mut candidates = Vec::new();
        for object in editor
            .document
            .objects
            .iter()
            .filter(|o| o.visible && (Some(o.id) == editor.document.selected) == source)
        {
            let mut unique = HashSet::new();
            for mirrored in
                [false, true]
                    .into_iter()
                    .take(if !source && object.mirror { 2 } else { 1 })
            {
                let matrix = model_matrix(object, mirrored);
                for local in object_geometry(editor, object).iter().flatten() {
                    if !unique.insert((
                        mirrored,
                        local.map(|v| if v == 0. { 0 } else { v.to_bits() }),
                    )) {
                        continue;
                    }
                    let world = world_point(matrix, *local);
                    let Some((screen, depth)) = projection.project(world) else {
                        continue;
                    };
                    if screen
                        .iter()
                        .enumerate()
                        .any(|(i, v)| *v < -RADIUS || *v > size[i] + RADIUS)
                    {
                        continue;
                    }
                    candidates.push((
                        (object.id, mirrored),
                        Candidate {
                            world,
                            screen,
                            depth,
                        },
                    ));
                }
            }
        }
        candidates
    }
    fn legacy_nearest(
        candidates: &[((u64, bool), Candidate)],
        point: [f32; 2],
    ) -> Option<Candidate> {
        candidates
            .iter()
            .filter_map(|(owner, c)| {
                let d = (c.screen[0] - point[0]).powi(2) + (c.screen[1] - point[1]).powi(2);
                (d <= RADIUS * RADIUS).then_some((
                    d,
                    (owner.0, owner.1, c.world.map(f32::to_bits)),
                    *c,
                ))
            })
            .min_by(|a, b| {
                a.0.total_cmp(&b.0)
                    .then(a.2.depth.total_cmp(&b.2.depth))
                    .then(a.1.cmp(&b.1))
            })
            .map(|(_, _, c)| c)
    }
    fn compare(editor: &Editor, size: [f32; 2], source: bool) {
        let oracle = legacy_candidates(editor, size, source);
        let index = VertexIndex::new(editor, size, source);
        let mut points = vec![
            [0., 0.],
            [size[0] * 0.5, size[1] * 0.5],
            [size[0] - 1., size[1] - 1.],
        ];
        let probes = if oracle.len() > 100_000 { 12 } else { 80 };
        for (_, candidate) in oracle.iter().step_by((oracle.len() / probes).max(1)) {
            points.extend([
                candidate.screen,
                [candidate.screen[0] + 13.9, candidate.screen[1]],
                [candidate.screen[0] - 14.1, candidate.screen[1] + 0.2],
            ]);
        }
        for point in points {
            let expected = legacy_nearest(&oracle, point);
            let actual = index.nearest(point);
            assert_eq!(
                actual.map(|c| c.world),
                expected.map(|c| c.world),
                "point={point:?}, size={size:?}, source={source}"
            );
        }
    }
    fn objects(count: usize) -> Editor {
        let mut e = Editor::new();
        let template = e.document.objects[0].clone();
        e.document.objects = (0..count)
            .map(|i| {
                let mut o = template.clone();
                o.id = i as u64 + 1;
                o.position = [
                    (i % 100) as f32 * 4. - 200.,
                    0.,
                    (i / 100) as f32 * 4. - 200.,
                ];
                o
            })
            .collect();
        e.distance = 400.;
        e.invalidate_document();
        e
    }
    #[test]
    fn ten_thousand_objects_reuse_revision_cache_and_update_only_changed_instances() {
        use crate::editor::Command;
        let mut e = objects(10_000);
        let size = [1280., 860.];
        {
            let index = VertexIndex::new(&e, size, false);
            let (_, work) = index.query([640., 430.]);
            assert!(work.instances < 200, "{work:?}");
            assert!(work.vertices < 1000, "{work:?}");
            assert!(work.object_nodes < 2500, "{work:?}");
            let next = VertexIndex::new(&e, size, false);
            let (IndexData::Targets(a), IndexData::Targets(b)) = (&index.data, &next.data) else {
                unreachable!()
            };
            assert!(Arc::ptr_eq(a, b));
        }
        let before = e.vertex_cache.borrow().work;
        assert_eq!(before.full_syncs, 1);
        assert_eq!(before.objects_visited, 10_000);
        assert_eq!(before.geometries_built, 1);
        assert_eq!(before.unique_vertices, 8);
        e.yaw += 0.2;
        e.pitch -= 0.1;
        e.camera_target = [1., 2., 3.];
        {
            let _index = VertexIndex::new(&e, [375., 600.], false);
        }
        assert_eq!(
            e.vertex_cache.borrow().work.objects_visited,
            before.objects_visited
        );
        let mut object = e.document.objects[5000].clone();
        object.position[1] += 1.;
        object.shear = [0.1, 0.2, 0.3];
        e.execute(Command::Update { object }).unwrap();
        {
            let _index = VertexIndex::new(&e, size, false);
        }
        let edited = e.vertex_cache.borrow().work;
        assert_eq!(edited.objects_visited - before.objects_visited, 1);
        assert_eq!(edited.instances_updated - before.instances_updated, 1);
        assert_eq!(edited.geometries_built, 1);
        assert_eq!(edited.snapshot_copies, 0);
        let mut object = e.document.objects[5000].clone();
        object.name = "Renamed".into();
        e.execute(Command::Update { object }).unwrap();
        {
            let _index = VertexIndex::new(&e, size, false);
        }
        assert_eq!(
            e.vertex_cache.borrow().work.instances_updated,
            edited.instances_updated
        );
        e.select(Some(5001));
        {
            let _index = VertexIndex::new(&e, size, false);
        }
        assert_eq!(
            e.vertex_cache.borrow().work.instances_updated,
            edited.instances_updated
        );
        assert_eq!(e.vertex_cache.borrow().work.full_syncs, 1);
    }
    #[test]
    fn bvh_matches_full_projection_for_camera_clip_shear_reflections_and_shared_geometry() {
        let mut e = Editor::new();
        let base = e.document.objects[0].clone();
        for id in 2..=38 {
            let mut o = base.clone();
            o.id = id;
            o.kind = ["cube", "sphere", "plane"][(id % 3) as usize].into();
            o.position = [
                (id % 7) as f32 - 3.,
                (id % 5) as f32 - 2.,
                (id % 11) as f32 - 5.,
            ];
            o.rotation = [id as f32 * 17., id as f32 * 23., id as f32 * -11.];
            o.scale = [if id % 2 == 0 { -1.2 } else { 0.7 }, 0.3, 1.7];
            o.shear = [0.5, -0.3, 0.2];
            o.mirror = id % 3 == 0;
            o.visible = id % 9 != 0;
            e.document.objects.push(o);
        }
        e.invalidate_document();
        for (yaw, pitch, distance, target) in [
            (0.7, 0.45, 7., [0.; 3]),
            (-1.2, -0.3, 3., [1., 0.5, -1.]),
            (0., 0., 0.05, [0.; 3]),
            (2., 1.3, 70., [0.; 3]),
        ] {
            e.yaw = yaw;
            e.pitch = pitch;
            e.distance = distance;
            e.camera_target = target;
            for size in [[1280., 860.], [375., 600.]] {
                compare(&e, size, false);
                compare(&e, size, true);
            }
        }
        assert_eq!(e.vertex_cache.borrow().work.geometries_built, 3);
        assert_eq!(e.vertex_cache.borrow().work.full_syncs, 1);
    }
    #[test]
    fn structural_changes_history_preview_and_old_snapshots_never_return_stale_vertices() {
        use crate::editor::Command;
        let mut e = Editor::new();
        e.execute(Command::Add {
            kind: "cube".into(),
        })
        .unwrap();
        e.select(Some(1));
        let mut o = e.document.objects[1].clone();
        o.position = [1.5, 0.2, -0.4];
        e.execute(Command::Update { object: o }).unwrap();
        compare(&e, [800., 600.], false);
        let old = VertexIndex::new(&e, [800., 600.], false);
        let old_points = legacy_candidates(&e, [800., 600.], false);
        let mut o = e.document.objects[1].clone();
        o.position = [2.2, -0.3, 0.8];
        o.mirror = true;
        e.execute(Command::Update { object: o }).unwrap();
        compare(&e, [800., 600.], false);
        for (_, p) in &old_points {
            assert_eq!(
                old.nearest(p.screen).map(|c| c.world),
                legacy_nearest(&old_points, p.screen).map(|c| c.world)
            );
        }
        drop(old);
        for field in 0..4 {
            let mut o = e.document.objects[1].clone();
            match field {
                0 => o.kind = "sphere".into(),
                1 => o.visible = false,
                2 => o.visible = true,
                _ => {
                    o.scale = [-0.6, 1.4, 0.2];
                    o.shear = [0.4, -0.7, 0.3];
                }
            }
            e.execute(Command::Update { object: o }).unwrap();
            compare(&e, [800., 600.], false);
            e.undo();
            compare(&e, [800., 600.], false);
            e.redo();
            compare(&e, [800., 600.], false);
        }
        e.select(Some(2));
        e.execute(Command::Delete).unwrap();
        compare(&e, [800., 600.], false);
        e.undo();
        compare(&e, [800., 600.], false);
        // A moved selected object is excluded dynamically; cancel restores the next gesture's source.
        e.tool = crate::gizmo::Tool::Move;
        e.select(Some(2));
        let h = crate::gizmo::handles(&e, [800., 600.]).unwrap();
        e.execute(Command::TransformBegin {
            token: "cache".into(),
            x: h.origin[0],
            y: h.origin[1],
            width: 800.,
            height: 600.,
            vertex_snap: true,
        })
        .unwrap();
        e.execute(Command::TransformUpdate {
            token: "cache".into(),
            points: vec![[h.origin[0] + 40., h.origin[1] - 30.]],
            snap_toggle: false,
            vertex_snap: false,
        })
        .unwrap();
        e.execute(Command::TransformEnd {
            token: "cache".into(),
            cancel: true,
        })
        .unwrap();
        compare(&e, [800., 600.], true);
        compare(&e, [800., 600.], false);
        assert_eq!(e.vertex_cache.borrow().work.full_syncs, 1);
    }
    #[test]
    fn reset_reused_asset_ids_and_missed_revisions_rebuild_safely() {
        use crate::editor::Command;
        let mut e = Editor::new();
        e.execute(Command::Add {
            kind: "cube".into(),
        })
        .unwrap();
        e.select(Some(1));
        compare(&e, [800., 600.], false);
        for i in 0..140 {
            let mut o = e.document.objects[1].clone();
            o.position[0] = i as f32 * 0.001;
            e.execute(Command::Update { object: o }).unwrap();
        }
        compare(&e, [800., 600.], false);
        assert_eq!(e.vertex_cache.borrow().work.full_syncs, 2);
        let mut doc = e.document.clone();
        doc.objects[1].kind = "plane".into();
        doc.objects[1].position = [2., 0., 0.];
        e.replace_document(doc, None, false);
        compare(&e, [800., 600.], false);
        assert_eq!(e.vertex_cache.borrow().work.full_syncs, 3);
        // Explicit invalidation also handles same IDs and in-place asset replacement.
        e.document.objects[1].kind = "sphere".into();
        e.invalidate_document();
        compare(&e, [800., 600.], false);
        assert_eq!(e.vertex_cache.borrow().work.full_syncs, 4);
    }
    #[test]
    fn source_lookup_does_not_walk_other_objects_or_build_the_target_scene() {
        let e = objects(10_000);
        let _source = VertexIndex::new(&e, [800., 600.], true);
        let work = e.vertex_cache.borrow().work;
        assert_eq!(work.full_syncs, 0);
        assert_eq!(work.objects_visited, 0);
        assert_eq!(work.geometries_built, 1);
        assert_eq!(work.unique_vertices, 8);
    }
    fn mesh_scene(subdivisions: usize, instances: usize) -> Editor {
        let mut e = Editor::new();
        let mut triangles = Vec::new();
        let p = |x: usize, y: usize| {
            [
                x as f32 / subdivisions as f32 * 2. - 1.,
                y as f32 / subdivisions as f32 * 2. - 1.,
                0.,
            ]
        };
        for y in 0..subdivisions {
            for x in 0..subdivisions {
                let [a, b, c, d] = [p(x, y), p(x + 1, y), p(x + 1, y + 1), p(x, y + 1)];
                triangles.extend([[a, b, c], [a, c, d]]);
            }
        }
        e.document.meshes.insert(
            "shared".into(),
            Arc::new(Mesh {
                edge_mask: None,
                triangles,
                color: [0.5; 3],
                source: Source {
                    origin: "synthetic benchmark".into(),
                    revision: "1".into(),
                    license: "test".into(),
                },
                prim_path: "/grid".into(),
            }),
        );
        let template = e.document.objects[0].clone();
        for i in 0..instances {
            let mut o = template.clone();
            o.id = i as u64 + 2;
            o.kind = "mesh".into();
            o.mesh_id = Some("shared".into());
            o.position = [
                (i % 12) as f32 * 2.5 - 14.,
                (i / 12) as f32 * 2.5 - 14.,
                -0.5,
            ];
            e.document.objects.push(o);
        }
        e.distance = 45.;
        e.yaw = 0.;
        e.pitch = 0.;
        e.invalidate_document();
        e
    }
    #[test]
    fn shared_dense_mesh_is_built_once_and_only_nearby_vertices_are_projected() {
        let e = mesh_scene(80, 144);
        let index = VertexIndex::new(&e, [1280., 860.], false);
        let (_, work) = index.query([640., 430.]);
        let cache = e.vertex_cache.borrow().work;
        assert_eq!(cache.geometries_built, 2);
        assert_eq!(cache.unique_vertices, 8 + 81 * 81);
        assert!(work.instances < 10, "{work:?}");
        assert!(work.vertices < 1000, "{work:?}");
        // 944,784 target vertices would be projected by the old per-gesture path.
        assert!(work.vertices < 144 * 81 * 81 / 100);
        compare(&e, [1280., 860.], false);
    }
    #[test]
    fn mesh_pointer_identity_survives_reused_asset_names_and_old_snapshots() {
        use crate::editor::Command;
        let mut e = mesh_scene(4, 1);
        let size = [800., 600.];
        e.document.objects[1].position = [0.; 3];
        e.distance = 7.;
        e.invalidate_document();
        let old = VertexIndex::new(&e, size, false);
        let oracle = legacy_candidates(&e, size, false);
        let mut replacement = (*e.document.meshes["shared"]).clone();
        for p in replacement.triangles.iter_mut().flatten() {
            p[0] += 0.3;
            p[1] += 0.2;
        }
        e.document
            .meshes
            .insert("shared".into(), Arc::new(replacement));
        let mut object = e.document.objects[1].clone();
        object.name = "new asset revision".into();
        e.execute(Command::Update { object }).unwrap();
        compare(&e, size, false);
        assert_eq!(
            e.vertex_cache
                .borrow()
                .geometries
                .keys()
                .filter(|k| matches!(k, GeometryKey::Mesh(_)))
                .count(),
            1
        );
        for (_, p) in &oracle {
            assert_eq!(
                old.nearest(p.screen).map(|c| c.world),
                legacy_nearest(&oracle, p.screen).map(|c| c.world)
            );
        }
        e.render_revision = 0; // A rollback cannot be mistaken for a cache hit.
        compare(&e, size, false);
        assert_eq!(e.vertex_cache.borrow().work.full_syncs, 2);
    }
    // The former gesture-local implementation, retained only as a CPU benchmark baseline.
    struct LegacyIndex {
        cells: HashMap<(i32, i32), Vec<Candidate>>,
    }
    impl LegacyIndex {
        fn new(editor: &Editor, size: [f32; 2]) -> Self {
            let projection = Projection::new(editor, size);
            let mut cells: HashMap<_, Vec<_>> = HashMap::new();
            for object in editor
                .document
                .objects
                .iter()
                .filter(|o| o.visible && Some(o.id) != editor.document.selected)
            {
                let mut unique = HashSet::new();
                for mirrored in [false, true]
                    .into_iter()
                    .take(if object.mirror { 2 } else { 1 })
                {
                    let matrix = model_matrix(object, mirrored);
                    for local in object_geometry(editor, object).iter().flatten() {
                        if !unique.insert((
                            mirrored,
                            local.map(|v| if v == 0. { 0 } else { v.to_bits() }),
                        )) {
                            continue;
                        }
                        let world = world_point(matrix, *local);
                        let Some((screen, depth)) = projection.project(world) else {
                            continue;
                        };
                        if screen
                            .iter()
                            .enumerate()
                            .any(|(i, v)| *v < -RADIUS || *v > size[i] + RADIUS)
                        {
                            continue;
                        }
                        cells
                            .entry((
                                (screen[0] / RADIUS).floor() as i32,
                                (screen[1] / RADIUS).floor() as i32,
                            ))
                            .or_default()
                            .push(Candidate {
                                world,
                                screen,
                                depth,
                            });
                    }
                }
            }
            Self { cells }
        }
        fn nearest(&self, p: [f32; 2]) -> Option<Candidate> {
            let (x, y) = (
                (p[0] / RADIUS).floor() as i32,
                (p[1] / RADIUS).floor() as i32,
            );
            let mut best: Option<(f32, Candidate)> = None;
            for a in x - 1..=x + 1 {
                for b in y - 1..=y + 1 {
                    for c in self.cells.get(&(a, b)).into_iter().flatten() {
                        let d = (c.screen[0] - p[0]).powi(2) + (c.screen[1] - p[1]).powi(2);
                        if d <= RADIUS * RADIUS
                            && best.is_none_or(|(distance, old)| {
                                d < distance || (d == distance && c.depth < old.depth)
                            })
                        {
                            best = Some((d, *c));
                        }
                    }
                }
            }
            best.map(|(_, c)| c)
        }
    }
    #[test]
    #[ignore = "CPU timing benchmark; run explicitly with --ignored --nocapture"]
    fn vertex_search_cpu_benchmark() {
        use std::hint::black_box;
        use std::time::Instant;
        let size = [1280., 860.];
        let point = [640., 430.];
        for (label, grid, instances) in [("shared_144x6561", 80, 144), ("dense_1x231361", 480, 1)] {
            let mut legacy_times = Vec::new();
            let mut cold_times = Vec::new();
            let mut warm_times = Vec::new();
            let mut edit_times = Vec::new();
            let mut measured = None;
            for run in 0..3 {
                let mut e = mesh_scene(grid, instances);
                if instances == 1 {
                    e.document.objects[1].position = [0.; 3];
                    e.invalidate_document();
                }
                let t = Instant::now();
                let legacy = LegacyIndex::new(&e, size);
                let expected = legacy.nearest(point);
                legacy_times.push(t.elapsed().as_micros());
                drop(legacy);
                let t = Instant::now();
                {
                    let index = VertexIndex::new(&e, size, false);
                    let (actual, work) = index.query(point);
                    assert_eq!(actual.map(|c| c.world), expected.map(|c| c.world));
                    measured = Some(work);
                    black_box(actual);
                }
                cold_times.push(t.elapsed().as_micros());
                let t = Instant::now();
                for _ in 0..50 {
                    black_box(VertexIndex::new(&e, size, false).nearest(point));
                }
                warm_times.push(t.elapsed().as_nanos() / 50);
                let mut o = e.document.objects[1].clone();
                o.position[0] += 0.1;
                let before = e.vertex_cache.borrow().work;
                e.execute(crate::editor::Command::Update { object: o })
                    .unwrap();
                let t = Instant::now();
                black_box(VertexIndex::new(&e, size, false).nearest(point));
                edit_times.push(t.elapsed().as_micros());
                assert_eq!(
                    e.vertex_cache.borrow().work.objects_visited - before.objects_visited,
                    1
                );
                assert_eq!(
                    e.vertex_cache.borrow().work.geometries_built,
                    before.geometries_built
                );
                assert_eq!(e.vertex_cache.borrow().work.snapshot_copies, 0);
                black_box(run);
            }
            legacy_times.sort();
            cold_times.sort();
            warm_times.sort();
            edit_times.sort();
            println!("{label}: baseline_build_query_us={}, cold_build_query_us={}, warm_build_query_ns={}, one_edit_sync_query_us={}, query_work={:?}",legacy_times[1],cold_times[1],warm_times[1],edit_times[1],measured.unwrap());
        }
    }
    #[test]
    fn committed_source_becomes_a_fresh_target_after_selection_and_history_changes() {
        use crate::editor::Command;
        let mut e = Editor::new();
        e.tool = crate::gizmo::Tool::Move;
        e.execute(Command::Add {
            kind: "cube".into(),
        })
        .unwrap();
        let mut target = e.document.objects[1].clone();
        target.position = [2., 0., 0.];
        e.execute(Command::Update { object: target }).unwrap();
        e.select(Some(1));
        let size = [800., 600.];
        let h = crate::gizmo::handles(&e, size).unwrap();
        let target = world_point(model_matrix(&e.document.objects[1], false), [0.5, 0.5, 0.5]);
        let point = Projection::new(&e, size).project(target).unwrap().0;
        e.execute(Command::TransformBegin {
            token: "commit-cache".into(),
            x: h.origin[0],
            y: h.origin[1],
            width: size[0],
            height: size[1],
            vertex_snap: true,
        })
        .unwrap();
        e.execute(Command::TransformUpdate {
            token: "commit-cache".into(),
            points: vec![point],
            snap_toggle: false,
            vertex_snap: true,
        })
        .unwrap();
        e.execute(Command::TransformEnd {
            token: "commit-cache".into(),
            cancel: false,
        })
        .unwrap();
        e.select(Some(2));
        compare(&e, size, false);
        e.undo();
        compare(&e, size, false);
        e.redo();
        compare(&e, size, false);
        let work = e.vertex_cache.borrow().work;
        assert_eq!(work.full_syncs, 1);
        assert_eq!(work.geometries_built, 1);
        assert_eq!(work.snapshot_copies, 0);
    }
    #[test]
    #[ignore = "Requires usd-core and U3D_USD_SOURCE; tests the real converter and prepared append"]
    fn usd_append_sdk_pipeline() {
        let source = std::env::var("U3D_USD_SOURCE").unwrap();
        let path = std::env::temp_dir().join(format!("u3d-append-sdk-{}", std::process::id()));
        let mut e = Editor::new();
        let tasks = crate::load_task::Tasks::default();
        let guard = tasks.begin(crate::load_task::Kind::Usd).unwrap();
        let imported =
            crate::usd::load_with_task(source, path.clone(), guard.task.cancel_arc(), &guard.task)
                .unwrap();
        let input = e.capture_usd_append().unwrap();
        let prepared = PreparedUsdAppend::build(input, imported, &guard.task).unwrap();
        let count = prepared.prepared.scene.instances.len();
        let report = prepared.commit(&mut e, &guard.task).unwrap();
        let before = e.vertex_cache.borrow().work;
        VertexIndex::new(&e, [800., 600.], false);
        assert_eq!(
            e.vertex_cache.borrow().work.objects_visited,
            before.objects_visited
        );
        assert_eq!(
            e.vertex_cache.borrow().work.geometries_built,
            before.geometries_built
        );
        assert_eq!(e.document.objects.len(), count);
        let saved = e.document.file_bytes().unwrap();
        e.undo();
        e.redo();
        assert_eq!(e.document.file_bytes().unwrap(), saved);
        println!(
            "USD_APPEND_SDK_METRICS {}",
            serde_json::json!({"objects":report.objects,
            "triangles":report.triangles,"prepared_instances":count,"first_snap_objects_visited":0,
            "first_snap_geometries_built":0})
        );
        if path.exists() {
            std::fs::remove_dir_all(path).unwrap();
        }
    }
    #[test]
    fn usd_append_prebuilds_cold_scene_and_preserves_history() {
        let mut e = Editor::new();
        let original = e.document.objects.clone();
        let selected = e.document.selected;
        let tasks = crate::load_task::Tasks::default();
        let guard = tasks.begin(crate::load_task::Kind::Usd).unwrap();
        let input = e.capture_usd_append().unwrap();
        let task = guard.task.clone();
        let prepared = std::thread::spawn(move || {
            PreparedUsdAppend::build(input, crate::usd::tests::fixture(), &task)
        })
        .join()
        .unwrap()
        .unwrap();
        let progress = tasks.snapshot().unwrap();
        assert_eq!(progress.phase, crate::load_task::Phase::Instances);
        assert_eq!(progress.completed, 2);
        assert_eq!(progress.total, 2);
        let scene = prepared.prepared.scene.clone();
        let report = prepared.commit(&mut e, &guard.task).unwrap();
        assert_eq!(report.objects, 1);
        assert_eq!(e.undo.len(), 1);
        assert_eq!(e.document.objects[..1], original);
        assert!(e.dirty);
        assert!(Arc::ptr_eq(&scene, &e.vertex_cache.borrow().scene));
        let work = e.vertex_cache.borrow().work;
        VertexIndex::new(&e, [1280., 860.], false);
        assert_eq!(
            e.vertex_cache.borrow().work.objects_visited,
            work.objects_visited
        );
        e.select(Some(1)); // selection changes legitimately synchronize just two IDs
        let index = VertexIndex::new(&e, [1280., 860.], false);
        for (mode, world) in [
            (crate::gizmo::MoveSnap::Vertex, [2., 0., 0.]),
            (crate::gizmo::MoveSnap::Edge, [2., 0.5, 0.]),
            (crate::gizmo::MoveSnap::Face, [1.5, 0.5, 0.]),
        ] {
            let point = Projection::new(&e, [1280., 860.]).project(world).unwrap().0;
            assert!(
                index.nearest_mode(point, mode).is_some(),
                "mode {mode:?} at {world:?}"
            );
        }
        assert_eq!(e.vertex_cache.borrow().work.full_syncs, work.full_syncs);
        assert_eq!(
            e.vertex_cache.borrow().work.objects_visited,
            work.objects_visited + 2
        );
        assert_eq!(
            e.vertex_cache.borrow().work.geometries_built,
            work.geometries_built
        );
        e.undo();
        assert_eq!(e.document.objects, original);
        assert_eq!(e.document.selected, selected);
        VertexIndex::new(&e, [1280., 860.], false);
        e.redo();
        VertexIndex::new(&e, [1280., 860.], false);
        assert_eq!(
            e.vertex_cache.borrow().work.geometries_built,
            work.geometries_built
        );
        assert_eq!(
            e.vertex_cache.borrow().usage.working.load(Ordering::SeqCst),
            0
        );
    }
    #[test]
    fn usd_append_reuses_warm_shapes_and_prepares_large_placements() {
        let mut e = mesh_scene(8, 9998);
        e.document.objects[1].visible = false;
        e.document.objects[2].mirror = true;
        e.document.objects[2].rotation = [20., 35., 0.];
        e.document.objects[2].shear = [0.2, 0.1, 0.];
        let prepared = PreparedDocument::new(e.document.clone()).unwrap();
        e.replace_prepared_document(prepared, None, false);
        let old = VertexIndex::new(&e, [1280., 860.], false);
        let shared = e.document.meshes["shared"].clone();
        let key = GeometryKey::Mesh(Arc::as_ptr(&shared) as usize);
        let geometry = e.vertex_cache.borrow().geometries[&key].clone();
        let tasks = crate::load_task::Tasks::default();
        let guard = tasks.begin(crate::load_task::Kind::Usd).unwrap();
        let input = e.capture_usd_append().unwrap();
        let task = guard.task.clone();
        let prepared = std::thread::spawn(move || {
            PreparedUsdAppend::build(input, crate::usd::tests::fixture(), &task)
        })
        .join()
        .unwrap()
        .unwrap();
        assert!(Arc::ptr_eq(&geometry, &prepared.prepared.geometries[&key]));
        assert_eq!(prepared.prepared.metrics.reused_shapes, 2);
        assert_eq!(prepared.prepared.metrics.budget.triangles, 3);
        assert_eq!(prepared.prepared.metrics.budget.shapes, 1);
        assert_eq!(prepared.prepared.scene.instances.len(), 10000);
        prepared.commit(&mut e, &guard.task).unwrap();
        assert!(Arc::ptr_eq(&shared, &e.document.meshes["shared"]));
        let before = e.vertex_cache.borrow().work;
        let target = VertexIndex::new(&e, [1280., 860.], false);
        assert_eq!(
            e.vertex_cache.borrow().work.objects_visited,
            before.objects_visited
        );
        assert_eq!(e.vertex_cache.borrow().work.full_syncs, before.full_syncs);
        assert_eq!(
            e.vertex_cache.borrow().work.geometries_built,
            before.geometries_built
        );
        assert!(matches!(&old.data, IndexData::Targets(s) if s.instances.len() == 9999));
        drop(target);
    }
    #[test]
    fn usd_append_reserves_new_shapes_and_complete_placement_tree_once() {
        let mut e = mesh_scene(32, 50);
        let warm = PreparedDocument::new(e.document.clone()).unwrap();
        e.replace_prepared_document(warm, None, false);
        let tasks = crate::load_task::Tasks::default();
        let guard = tasks.begin(crate::load_task::Kind::Usd).unwrap();
        let usage = e.vertex_cache.borrow().usage.clone();
        let live = e.snap_build_budget().unwrap();
        let estimate = live.reserve(3, 52, 1).unwrap().estimated_working_bytes;
        let resident = usage.resident.load(Ordering::SeqCst);
        let mut input = e.capture_usd_append().unwrap();
        input.budget = crate::snap_budget::IndexBudget::for_test(resident + estimate - 1)
            .with_usage(usage.clone());
        assert!(
            PreparedUsdAppend::build(input, crate::usd::tests::fixture(), &guard.task).is_err()
        );
        assert_eq!(usage.working.load(Ordering::SeqCst), 0);
        let mut input = e.capture_usd_append().unwrap();
        input.budget = crate::snap_budget::IndexBudget::for_test(resident + estimate)
            .with_usage(usage.clone());
        let prepared =
            PreparedUsdAppend::build(input, crate::usd::tests::fixture(), &guard.task).unwrap();
        assert_eq!(
            prepared.prepared.metrics.budget.estimated_working_bytes,
            estimate
        );
        assert_eq!(usage.working.load(Ordering::SeqCst), estimate);
        prepared.commit(&mut e, &guard.task).unwrap();
        assert_eq!(usage.working.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn usd_append_allows_view_changes_and_keeps_live_selection_in_undo() {
        let mut e = Editor::new();
        e.execute(crate::editor::Command::Add {
            kind: "plane".into(),
        })
        .unwrap();
        let tasks = crate::load_task::Tasks::default();
        let guard = tasks.begin(crate::load_task::Kind::Usd).unwrap();
        let input = e.capture_usd_append().unwrap();
        let prepared =
            PreparedUsdAppend::build(input, crate::usd::tests::fixture(), &guard.task).unwrap();
        e.select(Some(1));
        e.frame = 80;
        e.tool = crate::gizmo::Tool::Rotate;
        e.yaw += 0.5;
        let history = e.undo.len();
        prepared.commit(&mut e, &guard.task).unwrap();
        assert_eq!(e.frame, 80);
        assert_eq!(e.tool, crate::gizmo::Tool::Rotate);
        assert_eq!(e.undo.len(), history + 1);
        let before = e.vertex_cache.borrow().work;
        VertexIndex::new(&e, [1280., 860.], false);
        assert_eq!(
            e.vertex_cache.borrow().work.objects_visited,
            before.objects_visited
        );
        e.undo();
        assert_eq!(e.document.selected, Some(1));
        assert_eq!(e.undo.len(), history);
    }
    #[test]
    fn usd_append_rejection_cancellation_and_conflicts_are_atomic() {
        for scenario in 0..5 {
            let mut e = Editor::new();
            e.execute(crate::editor::Command::Add {
                kind: "sphere".into(),
            })
            .unwrap();
            e.undo(); // preserve existing redo until a successful import
            let tasks = crate::load_task::Tasks::default();
            let guard = tasks.begin(crate::load_task::Kind::Usd).unwrap();
            let mut input = e.capture_usd_append().unwrap();
            if scenario == 0 {
                input.budget = crate::snap_budget::IndexBudget::for_test(1)
                    .with_usage(e.vertex_cache.borrow().usage.clone());
            }
            if scenario == 1 {
                tasks.cancel(&tasks.snapshot().unwrap().id);
            }
            let prepared =
                PreparedUsdAppend::build(input, crate::usd::tests::fixture(), &guard.task);
            if scenario < 2 {
                assert!(prepared.is_err());
            } else {
                let prepared = prepared.unwrap();
                if scenario == 2 {
                    tasks.cancel(&tasks.snapshot().unwrap().id);
                }
                if scenario == 3 {
                    e.execute(crate::editor::Command::Add {
                        kind: "plane".into(),
                    })
                    .unwrap();
                }
                if scenario == 4 {
                    e.replace_document(Document::default(), None, true);
                }
                let bytes = e.document.file_bytes().unwrap();
                let stamp = e.stamp();
                let history = (e.undo.len(), e.redo.len(), e.dirty);
                let scene = e.vertex_cache.borrow().scene.clone();
                assert!(prepared.commit(&mut e, &guard.task).is_err());
                assert_eq!(e.document.file_bytes().unwrap(), bytes);
                assert_eq!(e.stamp(), stamp);
                assert_eq!((e.undo.len(), e.redo.len(), e.dirty), history);
                assert!(Arc::ptr_eq(&scene, &e.vertex_cache.borrow().scene));
            }
            assert_eq!(
                e.vertex_cache.borrow().usage.working.load(Ordering::SeqCst),
                0
            );
        }
    }
    #[test]
    fn scene_capacity_compaction_and_retired_snapshot_accounting() {
        let e = mesh_scene(2, 2048);
        let mut cache = VertexCache::default();
        let old = cache.snapshot(&e);
        let before = cache.admission_diagnostics().resident_index_bytes;
        for object in e.document.objects.iter().skip(12) {
            cache.update(&e, object.id, None);
        }
        let sparse_bytes = cache.scene.tree.vec_bytes();
        let current = cache.scene.clone();
        let deferred = cache.trim(true);
        assert!(deferred.scene_compaction_deferred);
        assert_eq!(cache.scene.tree.vec_bytes(), sparse_bytes);
        drop(current);
        let trimmed = cache.trim(false);
        assert!(trimmed.scene_reclaimed_buffer_bytes > 0);
        assert!(trimmed.scene_reclaimed_payload_bytes > 0);
        assert_eq!(old.instances.len(), 2049);
        assert_eq!(cache.scene.instances.len(), 12);
        let live = cache.admission_diagnostics();
        assert_eq!(live.tracked_scenes, 2);
        assert!(live.resident_index_bytes >= before); // old snapshot still allocated
        drop(old);
        cache.publish_usage();
        assert_eq!(cache.admission_diagnostics().tracked_scenes, 1);
        assert!(cache.admission_diagnostics().resident_index_bytes < before / 4);
        for object in e.document.objects.iter().skip(12).take(50) {
            cache.update(&e, object.id, Some(object));
        }
        assert_eq!(cache.scene.instances.len(), 62);
    }
    #[test]
    fn combined_accounting_survives_replacement_with_an_old_snapshot() {
        let mut e = mesh_scene(8, 20);
        let old = VertexIndex::new(&e, [1280., 860.], false);
        let usage = e.vertex_cache.borrow().usage.clone();
        let baseline = e.snap_build_budget().unwrap();
        let before = usage.resident.load(Ordering::SeqCst);
        let tasks = crate::load_task::Tasks::default();
        let task = tasks.begin(crate::load_task::Kind::Open).unwrap();
        let prepared =
            PreparedDocument::with_task_budget(Document::default(), &task.task, baseline).unwrap();
        assert!(usage.working.load(Ordering::SeqCst) > 0);
        prepared.check_publication(&e).unwrap();
        task.task.commit().unwrap();
        e.replace_prepared_document(prepared, None, false);
        assert!(Arc::ptr_eq(&usage, &e.vertex_cache.borrow().usage));
        assert_eq!(usage.working.load(Ordering::SeqCst), 0);
        assert!(usage.resident.load(Ordering::SeqCst) >= before);
        drop(old);
        let after = diagnostics(&e).admission.resident_index_bytes;
        assert!(after < before);
        assert_eq!(diagnostics(&e).admission.tracked_scenes, 1);
    }
    #[test]
    #[ignore = "Synthetic placement capacity measurement; run explicitly with --ignored --nocapture"]
    fn scene_capacity_memory_measurement() {
        let e = mesh_scene(2, 50_000);
        let mut cache = VertexCache::default();
        let old = cache.snapshot(&e);
        let before = cache.admission_diagnostics();
        for object in e.document.objects.iter().skip(32) {
            cache.update(&e, object.id, None);
        }
        let before_trim_tree = cache.scene.tree.vec_bytes();
        let started = std::time::Instant::now();
        let result = cache.trim(true);
        let elapsed_us = started.elapsed().as_micros();
        let pinned = cache.admission_diagnostics();
        drop(old);
        cache.publish_usage();
        let after = cache.admission_diagnostics();
        assert!(result.scene_reclaimed_buffer_bytes > 0);
        assert!(after.resident_index_bytes < before.resident_index_bytes / 100);
        println!(
            "SCENE_CAPACITY_METRICS {}",
            serde_json::json!({
                "date":"2026-11-04", "initial_instances":50001, "remaining_instances":32,
                "before":before, "sparse_tree_buffer_bytes":before_trim_tree,
                "trim":result, "trim_elapsed_us":elapsed_us, "old_snapshot_pinned":pinned,
                "after_old_snapshot_release":after,
                "note":"Single synthetic debug run; capacity accounting, not RSS or allocator peak"
            })
        );
    }
    #[test]
    fn combined_worker_rejection_and_cancellation_preserve_state_and_release_reservations() {
        use crate::snap_budget::IndexBudget;
        let e = mesh_scene(20, 2);
        let stamp = e.stamp();
        let _targets = VertexIndex::new(&e, [1280., 860.], false);
        let budget = e.snap_build_budget().unwrap();
        let usage = e.vertex_cache.borrow().usage.clone();
        let live = usage.resident.load(Ordering::SeqCst);
        let assets = e.document.meshes.values().cloned().collect::<Vec<_>>();
        let work = budget
            .meshes(assets.iter())
            .unwrap()
            .estimated_working_bytes;
        let limited = IndexBudget::for_test(work + live - 1).with_usage(usage.clone());
        let tasks = crate::load_task::Tasks::default();
        let guard = tasks.begin(crate::load_task::Kind::Snap).unwrap();
        assert!(
            PreparedVertices::meshes_with_task_budget(assets.iter(), &guard.task, limited).is_err()
        );
        assert_eq!(usage.working.load(Ordering::SeqCst), 0);
        let allowed = IndexBudget::for_test(work + live).with_usage(usage.clone());
        let prepared =
            PreparedVertices::meshes_with_task_budget(assets.iter(), &guard.task, allowed).unwrap();
        assert_eq!(usage.working.load(Ordering::SeqCst), work);
        assert_eq!(e.stamp(), stamp);
        let pinned = prepared.clone();
        drop(prepared);
        assert_eq!(usage.working.load(Ordering::SeqCst), work);
        drop(pinned);
        assert_eq!(usage.working.load(Ordering::SeqCst), 0);
        tasks.cancel(&tasks.snapshot().unwrap().id);
        assert!(
            PreparedVertices::meshes_with_task_budget(assets.iter(), &guard.task, budget).is_err()
        );
        assert_eq!(usage.working.load(Ordering::SeqCst), 0);
        assert_eq!(e.stamp(), stamp);
    }
    #[test]
    fn worker_rehydration_pins_assets_and_preserves_document() {
        let e = mesh_scene(24, 3);
        let stamp = e.stamp();
        let assets = e.missing_snap_meshes();
        assert_eq!(assets.len(), 1); // shared shape, not three copies
        let tasks = crate::load_task::Tasks::default();
        let guard = tasks.begin(crate::load_task::Kind::Snap).unwrap();
        let task = guard.task.clone();
        let prepared =
            std::thread::spawn(move || PreparedVertices::meshes_with_task(assets.iter(), &task))
                .join()
                .unwrap()
                .unwrap();
        guard.task.commit().unwrap();
        e.install_snap_gesture_vertices(prepared.clone());
        assert!(e.missing_snap_meshes().is_empty());
        assert_eq!(e.trim_snap_cache(true).evicted_shapes, 0); // worker result pins
        let before = e.vertex_cache.borrow().work.geometries_built;
        let targets = VertexIndex::new(&e, [1280., 860.], false);
        assert_eq!(e.vertex_cache.borrow().work.geometries_built, before + 1); // cube only
        assert_eq!(e.stamp(), stamp);
        drop(targets);
        drop(prepared);
        // Cancellation before publication cannot install a partial result.
        drop(guard);
        let cancelled = tasks.begin(crate::load_task::Kind::Snap).unwrap();
        assert!(tasks.cancel(&tasks.snapshot().unwrap().id));
        let assets = e.document.meshes.values().cloned().collect::<Vec<_>>();
        assert!(PreparedVertices::meshes_with_task(assets.iter(), &cancelled.task).is_err());
        assert_eq!(e.stamp(), stamp);
    }
    #[test]
    fn cache_retention_evicts_oldest_idle_shape_to_the_exact_target() {
        let e = Editor::new();
        let mut cache = VertexCache::default();
        let cube = e.document.objects[0].clone();
        let mut sphere = cube.clone();
        sphere.kind = "sphere".into();
        let mut plane = cube.clone();
        plane.kind = "plane".into();
        let cube_key = GeometryKey::in_document(&e.document, &cube).unwrap();
        let sphere_key = GeometryKey::in_document(&e.document, &sphere).unwrap();
        let plane_key = GeometryKey::in_document(&e.document, &plane).unwrap();
        drop(cache.geometry(&e, &cube, cube_key));
        let sphere_bytes = cache.geometry(&e, &sphere, sphere_key).buffer_bytes();
        drop(cache.geometry(&e, &plane, plane_key));
        drop(cache.geometry(&e, &cube, cube_key)); // cube used more recently than sphere/plane
        let before = cache.memory().buffer_bytes();
        cache.retention.limit_bytes = before - sphere_bytes;
        let result = cache.trim(false);
        assert_eq!(result.evicted_shapes, 1);
        assert_eq!(result.evicted_buffer_bytes, sphere_bytes);
        assert_eq!(result.after_buffer_bytes, before - sphere_bytes);
        assert!(!cache.geometries.contains_key(&sphere_key));
        assert!(
            cache.geometries.contains_key(&cube_key) && cache.geometries.contains_key(&plane_key)
        );
        assert_eq!(cache.trim(false).evicted_shapes, 0);
        assert_eq!(cache.trim(true).evicted_shapes, 2);
        assert_eq!(cache.memory().buffer_bytes(), 0);
        assert!(cache.access_order.is_empty());
    }
    #[test]
    fn retention_protects_old_gestures_and_rebuilds_evicted_assets_after_undo() {
        use crate::gizmo::MoveSnap;
        let mut e = mesh_scene(160, 1);
        e.document.objects[1].position = [3., 0., 0.];
        let prepared = PreparedDocument::new(e.document.clone()).unwrap();
        e.replace_prepared_document(prepared, None, false);
        e.set_snap_cache_budget(1).unwrap();
        assert!(diagnostics(&e).retention.over_budget_bytes > 0);
        let old = VertexIndex::new(&e, [1280., 860.], false);
        let object = e.document.objects[1].clone();
        let key = GeometryKey::in_document(&e.document, &object).unwrap();
        let weak = Arc::downgrade(&e.vertex_cache.borrow().geometries[&key]);
        let projection = Projection::new(&e, [1280., 860.]);
        let point = projection.project([4., 0., 0.]).unwrap().0;
        e.select(Some(object.id));
        e.execute(crate::editor::Command::Delete).unwrap();
        let current = VertexIndex::new(&e, [1280., 860.], false);
        assert!(current.nearest_mode(point, MoveSnap::Edge).is_none());
        drop(current);
        let stamp = e.stamp();
        let redo = e.redo.len();
        let dirty = e.dirty;
        assert_eq!(e.trim_snap_cache(true).evicted_shapes, 0);
        for mode in [MoveSnap::Vertex, MoveSnap::Edge, MoveSnap::Face] {
            assert!(old.nearest_mode(point, mode).is_some());
        }
        assert!(weak.upgrade().is_some());
        drop(old);
        let result = e.trim_snap_cache(true);
        assert_eq!(result.evicted_shapes, 1);
        assert!(result.evicted_buffer_bytes > 1024 * 1024);
        assert!(weak.upgrade().is_none()); // index really lost its last owner
        assert!(e.document.meshes.contains_key("shared"));
        assert_eq!(e.stamp(), stamp);
        assert_eq!(e.redo.len(), redo);
        assert_eq!(e.dirty, dirty);
        assert!(e.set_snap_cache_budget(0).is_err());
        assert!(e.set_snap_cache_budget(4097).is_err());
        assert_eq!(diagnostics(&e).retention.limit_bytes, 1024 * 1024);
        let builds = e.vertex_cache.borrow().work.geometries_built;
        e.undo();
        e.select(Some(1));
        let restored = VertexIndex::new(&e, [1280., 860.], false);
        assert!(restored.nearest_mode(point, MoveSnap::Edge).is_some());
        assert_eq!(e.vertex_cache.borrow().work.geometries_built, builds + 1);
        drop(restored);
        e.redo();
        VertexIndex::new(&e, [1280., 860.], false);
        assert!(!e.vertex_cache.borrow().geometries.contains_key(&key));
        assert!(diagnostics(&e).retention.evictions_total >= 2);
    }
    #[test]
    fn retention_budget_survives_document_replacement_and_trims_hidden_prewarm() {
        let mut e = mesh_scene(160, 2);
        e.document.objects[1].visible = false;
        e.document.objects[2].visible = false;
        e.set_snap_cache_budget(1).unwrap();
        let prepared = PreparedDocument::new(e.document.clone()).unwrap();
        assert_eq!(prepared.geometries.len(), 2);
        e.replace_prepared_document(prepared, None, true);
        let d = diagnostics(&e);
        assert_eq!(d.retention.limit_bytes, 1024 * 1024);
        assert_eq!(d.retention.evictions_total, 1);
        assert_eq!(d.memory.shapes, 1);
        assert!(d.last_preparation.unwrap().buffer_bytes > d.buffer_bytes);
        assert!(e.dirty && e.path.is_none());
        let before = e.vertex_cache.borrow().work;
        for _ in 0..3 {
            VertexIndex::new(&e, [800., 600.], false);
        }
        assert_eq!(
            e.vertex_cache.borrow().work.objects_visited,
            before.objects_visited
        );
        assert_eq!(e.vertex_cache.borrow().work.full_syncs, before.full_syncs);
        let encoded = serde_json::to_value(diagnostics(&e)).unwrap();
        assert_eq!(encoded["retention"]["over_budget_bytes"], 0);
    }
    #[test]
    fn cancelled_completed_load_cannot_publish_or_replace_current_cache() {
        let mut e = mesh_scene(4, 2);
        let mut object = e.document.objects[1].clone();
        object.position[0] += 0.1;
        e.execute(crate::editor::Command::Update { object })
            .unwrap();
        e.undo();
        VertexIndex::new(&e, [800., 600.], false);
        let scene = e.vertex_cache.borrow().scene.clone();
        let stamp = e.stamp();
        let dirty = e.dirty;
        let redo = e.redo.len();
        let tasks = crate::load_task::Tasks::default();
        let guard = tasks.begin(crate::load_task::Kind::Recovery).unwrap();
        let prepared = PreparedDocument::with_task(e.document.clone(), &guard.task).unwrap();
        let progress = tasks.snapshot().unwrap();
        assert_eq!(progress.phase, crate::load_task::Phase::Instances);
        assert_eq!(progress.completed, e.document.objects.len());
        assert_eq!(progress.completed, progress.total);
        assert!(tasks.cancel(&progress.id));
        assert!(guard.task.commit().is_err());
        drop(prepared);
        drop(guard);
        assert!(tasks.snapshot().is_none());
        assert_eq!(e.stamp(), stamp);
        assert_eq!(e.dirty, dirty);
        assert_eq!(e.redo.len(), redo);
        assert!(Arc::ptr_eq(&scene, &e.vertex_cache.borrow().scene));
        let guard = tasks.begin(crate::load_task::Kind::Open).unwrap();
        assert!(tasks.cancel(&tasks.snapshot().unwrap().id));
        assert!(PreparedDocument::with_task(e.document.clone(), &guard.task).is_err());
    }
    #[test]
    fn budget_rejection_precedes_geometry_build_and_preserves_editor_state() {
        use crate::snap_budget::IndexBudget;
        let mut e = mesh_scene(40, 2);
        let mut object = e.document.objects[1].clone();
        object.position[0] += 0.25;
        e.execute(crate::editor::Command::Update { object })
            .unwrap();
        e.undo();
        assert!(!e.redo.is_empty());
        VertexIndex::new(&e, [800., 600.], false);
        let scene = e.vertex_cache.borrow().scene.clone();
        let stamp = e.stamp();
        let before = e.document.file_bytes().unwrap();
        let redo = e.redo.len();
        let dirty = e.dirty;
        let asset = e.document.meshes["shared"].clone();
        let strong = Arc::strong_count(&asset);
        let budget = IndexBudget::for_test(1024 * 1024);
        assert!(
            PreparedDocument::with_budget(e.document.clone(), budget.clone())
                .err()
                .unwrap()
                .contains("SNAP_INDEX_BUDGET_EXCEEDED")
        );
        let mut invalid = (*asset).clone();
        invalid.color[0] = f32::NAN;
        let invalid = Arc::new(invalid);
        // Budget rejection comes before validation/index work on this oversized mesh.
        let error = PreparedVertices::meshes_with_budget([&invalid], None, budget.clone())
            .err()
            .unwrap();
        assert!(error.contains("SNAP_INDEX_BUDGET_EXCEEDED"));
        assert_eq!(Arc::strong_count(&asset), strong);
        assert_eq!(e.stamp(), stamp);
        assert_eq!(e.document.file_bytes().unwrap(), before);
        assert_eq!(e.redo.len(), redo);
        assert_eq!(e.dirty, dirty);
        assert!(Arc::ptr_eq(&scene, &e.vertex_cache.borrow().scene));
        assert!(e.vertex_cache.borrow().last_preparation.is_none());
        let cancel = AtomicBool::new(true);
        assert!(
            PreparedVertices::meshes_with_budget([&asset], Some(&cancel), budget)
                .err()
                .unwrap()
                .contains("CANCELLED")
        );
    }
    #[test]
    fn budget_counts_unique_assets_visible_mirrors_and_excludes_orphans() {
        use crate::snap_budget::IndexBudget;
        let mut e = mesh_scene(4, 144);
        let shared = e.document.meshes["shared"].clone();
        e.document.meshes.insert("alias".into(), shared.clone());
        e.document.objects[2].mesh_id = Some("alias".into());
        e.document.objects[1].mirror = true;
        e.document.objects[2].visible = false;
        e.document
            .meshes
            .insert("orphan".into(), Arc::new((*shared).clone()));
        let budget = IndexBudget::for_test(1024 * 1024);
        let reservation = budget.document(&e.document).unwrap();
        assert_eq!(reservation.shapes, 2); // mesh Arc once, visible cube once
        assert_eq!(reservation.triangles, 32 + 12);
        assert_eq!(reservation.instances, 145);
        let exact = IndexBudget::for_test(reservation.estimated_working_bytes);
        let prepared = PreparedDocument::with_budget(e.document.clone(), exact).unwrap();
        assert_eq!(
            prepared.metrics.budget.limit_bytes,
            reservation.estimated_working_bytes
        );
        assert!(prepared.metrics.buffer_bytes <= prepared.metrics.budget.estimated_working_bytes);
        assert!(PreparedDocument::with_budget(
            e.document.clone(),
            IndexBudget::for_test(reservation.estimated_working_bytes - 1)
        )
        .is_err());
        let empty = Document {
            objects: Vec::new(),
            selected: None,
            ..Document::default()
        };
        assert_eq!(budget.document(&empty).unwrap().estimated_working_bytes, 0);
        let once = budget.meshes([&shared]).unwrap();
        let duplicate = budget.meshes([&shared, &shared]).unwrap();
        assert_eq!(
            once.estimated_working_bytes,
            duplicate.estimated_working_bytes
        );
        assert_eq!(once.shapes, 1);
    }
    #[test]
    fn prepared_document_keeps_first_gesture_off_the_full_scene_scan() {
        use crate::gizmo::MoveSnap;
        let mut e = mesh_scene(4, 9999);
        e.document.objects[1].mirror = true;
        e.document.objects[1].position = [3., 0., 0.];
        e.document.objects[1].scale = [-1.2, 0.6, 1.4];
        e.document.objects[1].shear = [0.2, -0.4, 0.3];
        e.document.objects[2].visible = false;
        e.invalidate_document();
        let old = VertexIndex::new(&e, [1280., 860.], false);
        let old_scene = e.vertex_cache.borrow().scene.clone();
        let loaded: Document = serde_json::from_slice(&e.document.file_bytes().unwrap()).unwrap();
        let prepared = std::thread::spawn(move || PreparedDocument::new(loaded).unwrap())
            .join()
            .unwrap();
        assert_eq!(prepared.metrics.memory.instances, 10000);
        assert_eq!(prepared.scene.instances.len(), 10000); // hidden excluded, mirror added
        let scene = prepared.scene.clone();
        assert_eq!(prepared.metrics.memory.ready_surface_shapes, 2);
        e.replace_prepared_document(prepared, Some("scene.u3d".into()), false);
        let stamp = e.stamp();
        let before = e.vertex_cache.borrow().work;
        assert_eq!(before.prepared_instances, 10000);
        let index = VertexIndex::new(&e, [1280., 860.], false);
        assert!(Arc::ptr_eq(&scene, &e.vertex_cache.borrow().scene));
        assert!(!Arc::ptr_eq(&scene, &old_scene));
        let object = e.document.objects[1].clone();
        let projection = Projection::new(&e, [1280., 860.]);
        for mirrored in [false, true] {
            let instance = &scene.instances[&(object.id, mirrored)];
            assert_eq!(instance.matrix, model_matrix(&object, mirrored));
            for (mode, local) in [
                (MoveSnap::Edge, [1., 0., 0.]),
                (MoveSnap::Face, [0.2, 0.3, 0.]),
            ] {
                let point = projection
                    .project(world_point(instance.matrix, local))
                    .unwrap()
                    .0;
                assert!(index.nearest_mode(point, mode).is_some());
            }
        }
        assert!(index.nearest([640., 430.]).is_some());
        e.yaw += 0.1;
        VertexIndex::new(&e, [800., 600.], false);
        assert!(Arc::ptr_eq(&scene, &e.vertex_cache.borrow().scene));
        let after = e.vertex_cache.borrow().work;
        assert_eq!(after.objects_visited, before.objects_visited);
        assert_eq!(after.full_syncs, before.full_syncs);
        assert_eq!(after.geometries_built, before.geometries_built);
        assert_eq!(e.stamp(), stamp);
        assert!(!e.dirty && e.undo.is_empty() && e.redo.is_empty());
        assert_eq!(e.path.as_deref(), Some(std::path::Path::new("scene.u3d")));
        assert!(old.nearest([640., 430.]).is_some());
        drop(index);
        drop(scene);
        let mut object = e.document.objects[1].clone();
        object.position[0] += 0.25;
        e.execute(crate::editor::Command::Update { object })
            .unwrap();
        VertexIndex::new(&e, [1280., 860.], false);
        let changed = e.vertex_cache.borrow().work;
        assert_eq!(changed.objects_visited, after.objects_visited + 1);
        assert_eq!(changed.full_syncs, after.full_syncs);
        e.undo();
        VertexIndex::new(&e, [1280., 860.], false);
        e.redo();
        VertexIndex::new(&e, [1280., 860.], false);
        assert_eq!(e.vertex_cache.borrow().work.full_syncs, after.full_syncs);
    }
    #[test]
    fn prepared_document_owns_validated_input_and_recovery_remains_unsaved() {
        let mut e = mesh_scene(4, 1);
        e.document.objects[1].visible = false;
        let orphan = Arc::new((*e.document.meshes["shared"]).clone());
        e.document.meshes.insert("orphan".into(), orphan.clone());
        let prepared = PreparedDocument::new(e.document.clone()).unwrap();
        assert_eq!(prepared.geometries.len(), 2); // hidden referenced mesh + visible cube
        assert_eq!(prepared.scene.instances.len(), 1);
        assert!(prepared
            .geometries
            .values()
            .all(|g| g._asset.as_ref().is_none_or(|a| !Arc::ptr_eq(a, &orphan))));
        e.replace_prepared_document(prepared, None, true);
        assert!(e.dirty && e.path.is_none() && e.undo.is_empty());
        let stamp = e.stamp();
        let mut invalid = e.document.clone();
        invalid.objects[1].id = invalid.objects[0].id;
        assert!(PreparedDocument::new(invalid).is_err());
        assert_eq!(e.stamp(), stamp);
        let empty = PreparedDocument::new(Document {
            objects: Vec::new(),
            selected: None,
            ..Document::default()
        })
        .unwrap();
        e.replace_prepared_document(empty, None, false);
        assert!(VertexIndex::new(&e, [800., 600.], false)
            .nearest([400., 300.])
            .is_none());
        assert_eq!(e.vertex_cache.borrow().work.full_syncs, 0);
    }
    #[test]
    fn worker_prepares_shared_referenced_assets_and_first_gesture_reuses_them() {
        let mut e = mesh_scene(80, 144);
        e.document.objects[1].visible = false;
        let orphan = Arc::new((*e.document.meshes["shared"]).clone());
        e.document.meshes.insert("orphan".into(), orphan);
        // This is the file-read path: deserialization creates fresh immutable asset identities.
        let loaded: Document = serde_json::from_slice(&e.document.file_bytes().unwrap()).unwrap();
        loaded.validate().unwrap();
        let snapshot = loaded.clone();
        let prepared = std::thread::spawn(move || PreparedVertices::document(&snapshot).unwrap())
            .join()
            .unwrap();
        assert_eq!(prepared.geometries.len(), 1); // shared, hidden, referenced; no orphan work
        e.replace_document(loaded, None, false);
        let stamp = e.stamp();
        let revision = e.render_revision;
        let dirty = e.dirty;
        e.install_prepared_vertices(prepared);
        assert_eq!(e.stamp(), stamp);
        assert_eq!(e.render_revision, revision);
        assert_eq!(e.dirty, dirty);
        assert!(e.undo.is_empty());
        let before = e.vertex_cache.borrow().work;
        assert_eq!(before.prepared_geometries, 1);
        assert_eq!(before.prepared_vertices, 81 * 81);
        assert_eq!(before.geometries_built, 0);
        compare(&e, [1280., 860.], false);
        // Only the tiny built-in cube is lazy; the imported shape is not rebuilt.
        assert_eq!(e.vertex_cache.borrow().work.geometries_built, 1);
        let mut object = e.document.objects[1].clone();
        object.visible = true;
        e.execute(crate::editor::Command::Update { object })
            .unwrap();
        compare(&e, [1280., 860.], false);
        assert_eq!(e.vertex_cache.borrow().work.geometries_built, 1);
    }
    #[test]
    fn prepared_import_installs_after_commit_and_survives_undo_redo_without_new_mesh_work() {
        let mut e = Editor::new();
        let imported = crate::usd::tests::fixture();
        let prepared =
            PreparedVertices::meshes(imported.meshes.iter().map(|(_, _, mesh)| mesh), None)
                .unwrap();
        assert_eq!(prepared.geometries.len(), 1);
        e.import_usd(imported).unwrap();
        let before = e.stamp();
        let undo = e.undo.len();
        e.install_prepared_vertices(prepared);
        assert_eq!(e.stamp(), before);
        assert_eq!(e.undo.len(), undo);
        compare(&e, [800., 600.], true);
        assert_eq!(e.vertex_cache.borrow().work.geometries_built, 0);
        compare(&e, [800., 600.], false);
        let builds = e.vertex_cache.borrow().work.geometries_built;
        e.undo();
        compare(&e, [800., 600.], false);
        e.redo();
        compare(&e, [800., 600.], true);
        assert_eq!(e.vertex_cache.borrow().work.geometries_built, builds);
        assert_eq!(e.vertex_cache.borrow().work.prepared_geometries, 1);
    }
    #[test]
    fn cancelled_preparation_drops_partial_assets_and_never_changes_editor_state() {
        let e = mesh_scene(40, 1);
        let asset = e.document.meshes["shared"].clone();
        let second = Arc::new((*asset).clone());
        let cancel = AtomicBool::new(false);
        let count = Arc::strong_count(&asset);
        let mut seen = 0;
        let assets = [&asset, &second].into_iter().inspect(|_| {
            if seen == 1 {
                cancel.store(true, Ordering::Relaxed);
            }
            seen += 1;
        });
        assert!(PreparedVertices::meshes(assets, Some(&cancel)).is_err());
        assert_eq!(Arc::strong_count(&asset), count);
        assert_eq!(e.vertex_cache.borrow().work.prepared_geometries, 0);
        assert_eq!(e.vertex_cache.borrow().work.geometries_built, 0);
        assert!(PreparedVertices::meshes(std::iter::empty(), Some(&cancel)).is_err());
        assert!(
            Geometry::from_faces(&asset.triangles, Some(asset.clone()), Some(&cancel)).is_err()
        );
        assert!(Geometry::order_points(&mut vec![[0.; 3]; 32], Some(&cancel)).is_err());
        let mut invalid = (*asset).clone();
        invalid.triangles[0][0][0] = f32::NAN;
        assert!(PreparedVertices::meshes([&Arc::new(invalid)], None).is_err());
    }
    #[test]
    fn stale_prepared_assets_and_stale_file_transactions_cannot_poison_a_replacement() {
        let mut e = mesh_scene(4, 1);
        let stamp = e.stamp();
        let before = e.document.clone();
        let stale = PreparedVertices::document(&before).unwrap();
        let mut replacement = before.clone();
        let mut asset = (*replacement.meshes["shared"]).clone();
        for p in asset.triangles.iter_mut().flatten() {
            p[0] += 0.4;
        }
        replacement.meshes.insert("shared".into(), Arc::new(asset));
        e.replace_document(replacement, None, false);
        assert!(e.ensure_unchanged(stamp).is_err());
        e.install_prepared_vertices(stale);
        assert_eq!(e.vertex_cache.borrow().work.prepared_geometries, 0);
        assert!(e.vertex_cache.borrow().geometries.is_empty());
        let prepared = PreparedVertices::document(&e.document).unwrap();
        e.install_prepared_vertices(prepared);
        compare(&e, [800., 600.], false);
        assert_eq!(e.vertex_cache.borrow().work.prepared_geometries, 1);
        // Restoring the exact immutable asset is safe even when object IDs/names differ.
        let prepared = PreparedVertices::document(&before).unwrap();
        e.replace_document(before, None, true);
        e.install_prepared_vertices(prepared);
        compare(&e, [800., 600.], false);
        assert_eq!(e.vertex_cache.borrow().work.prepared_geometries, 2);
        assert!(e.dirty);
    }
    #[test]
    fn prepared_cache_merge_is_idempotent_and_preserves_existing_shape_indices() {
        let mut e = mesh_scene(4, 1);
        compare(&e, [800., 600.], false);
        let existing = e.vertex_cache.borrow().scene.clone();
        let work = e.vertex_cache.borrow().work;
        let prepared = PreparedVertices::document(&e.document).unwrap();
        e.install_prepared_vertices(prepared);
        assert_eq!(e.vertex_cache.borrow().work.prepared_geometries, 0);
        assert!(Arc::ptr_eq(&existing, &e.vertex_cache.borrow().scene));
        let imported = crate::usd::tests::fixture();
        let prepared =
            PreparedVertices::meshes(imported.meshes.iter().map(|(_, _, mesh)| mesh), None)
                .unwrap();
        e.import_usd(imported).unwrap();
        e.install_prepared_vertices(prepared);
        drop(existing);
        compare(&e, [800., 600.], false);
        assert_eq!(e.vertex_cache.borrow().work.prepared_geometries, 1);
        assert_eq!(
            e.vertex_cache.borrow().work.geometries_built,
            work.geometries_built
        );
        assert_eq!(e.vertex_cache.borrow().work.full_syncs, work.full_syncs);
    }
    #[test]
    fn eager_surfaces_merge_without_rebuilding_vertices_or_mutating_document() {
        use crate::gizmo::MoveSnap;
        let mut e = mesh_scene(4, 144);
        for object in &mut e.document.objects {
            object.visible = false;
        }
        e.document.objects[1].visible = true;
        e.document.objects[1].position = [0.; 3];
        e.invalidate_document();
        let old = VertexIndex::new(&e, [800., 600.], false);
        let scene = e.vertex_cache.borrow().scene.clone();
        let prepared = PreparedVertices::document(&e.document).unwrap();
        let memory = prepared.memory();
        assert_eq!(memory.shapes, 1);
        assert_eq!(memory.ready_surface_shapes, 1);
        assert_eq!(memory.triangles, 32);
        assert!(memory.edge_tree_vec_bytes > 0 && memory.face_tree_vec_bytes > 0);
        let stamp = e.stamp();
        e.install_prepared_vertices(prepared);
        assert!(Arc::ptr_eq(&scene, &e.vertex_cache.borrow().scene));
        assert_eq!(e.vertex_cache.borrow().work.prepared_surface_indices, 1);
        let projection = Projection::new(&e, [800., 600.]);
        for (mode, world) in [
            (MoveSnap::Edge, [1., 0., 0.]),
            (MoveSnap::Face, [0.2, 0.3, 0.]),
        ] {
            let point = projection.project(world).unwrap().0;
            let hit = old.nearest_mode(point, mode).unwrap();
            for (actual, expected) in hit.world.into_iter().zip(world) {
                assert!((actual - expected).abs() < 0.0001);
            }
        }
        e.install_prepared_vertices(PreparedVertices::document(&e.document).unwrap());
        assert_eq!(e.vertex_cache.borrow().work.prepared_surface_indices, 1);
        assert_eq!(e.stamp(), stamp);
        let report = diagnostics(&e);
        assert!(report.last_preparation.is_some());
        assert!(report.buffer_bytes >= memory.buffer_bytes());
        let cancel = AtomicBool::new(true);
        let mesh = &e.document.meshes["shared"];
        assert!(crate::surface_snap::SurfaceIndex::prepare(
            &[],
            &mesh.triangles,
            None,
            Some(&cancel)
        )
        .is_err());
        e.document_epoch += 1;
        assert!(diagnostics(&e).last_preparation.is_none());
    }

    #[test]
    #[ignore = "Large synthetic cache retention measurement; run explicitly with --ignored --nocapture"]
    fn cache_retention_memory_measurement() {
        use crate::gizmo::MoveSnap;
        let mut e = mesh_scene(480, 1);
        e.document.objects[1].position = [3., 0., 0.];
        let mut asset = (*e.document.meshes["shared"]).clone();
        asset.edge_mask = Some("36".repeat(480 * 480));
        e.document.meshes.insert("shared".into(), Arc::new(asset));
        let prepared = PreparedDocument::new(e.document.clone()).unwrap();
        e.replace_prepared_document(prepared, None, false);
        e.set_snap_cache_budget(1).unwrap();
        let rss = || -> Option<u64> {
            let output = std::process::Command::new("/bin/ps")
                .args(["-o", "rss=", "-p", &std::process::id().to_string()])
                .output()
                .ok()?;
            String::from_utf8(output.stdout).ok()?.trim().parse().ok()
        };
        let old = VertexIndex::new(&e, [1280., 860.], false);
        let point = Projection::new(&e, [1280., 860.])
            .project([4., 0., 0.])
            .unwrap()
            .0;
        let before = diagnostics(&e);
        let rss_before = rss();
        let source = e.document.meshes["shared"].clone();
        let mut object = e.document.objects[1].clone();
        object.visible = false;
        e.execute(crate::editor::Command::Update { object })
            .unwrap();
        VertexIndex::new(&e, [1280., 860.], false);
        let blocked = e.trim_snap_cache(true);
        assert_eq!(blocked.evicted_shapes, 0);
        assert!(old.nearest_mode(point, MoveSnap::Edge).is_some());
        let deferred = diagnostics(&e);
        drop(old);
        let started = std::time::Instant::now();
        let trimmed = e.trim_snap_cache(false);
        let trim_us = started.elapsed().as_micros();
        let after = diagnostics(&e);
        let rss_after = rss();
        assert!(trimmed.evicted_buffer_bytes > 30 * 1024 * 1024);
        assert!(after.buffer_bytes < 1024 * 1024);
        e.undo();
        let started = std::time::Instant::now();
        assert!(VertexIndex::new(&e, [1280., 860.], false)
            .nearest_mode(point, MoveSnap::Edge)
            .is_some());
        let rebuild_us = started.elapsed().as_micros();
        assert!(Arc::ptr_eq(&source, &e.document.meshes["shared"]));
        println!(
            "SNAP_CACHE_METRICS {}",
            serde_json::json!({
                "profile":"debug","triangles":460800,"vertices":231361,"edges":461760,
                "before":before,"deferred":deferred,"trimmed":trimmed,"after":after,
                "trim_us":trim_us,"undo_first_edge_us":rebuild_us,
                "rss_kib":{"before":rss_before,"after":rss_after},
                "source_asset_preserved":true
            })
        );
    }

    #[test]
    #[ignore = "Fresh-process time and RSS measurement; use scripts/benchmark-snap-memory.py"]
    fn surface_preparation_measurement() {
        use crate::gizmo::MoveSnap;
        use std::time::Instant;
        let case = std::env::var("U3D_SNAP_MEMORY_CASE").unwrap_or("shared".into());
        let (grid, instances) = match case.as_str() {
            "shared" => (80, 144),
            "dense" | "masked" => (480, 1),
            "many" => (80, 9999),
            _ => panic!("Unknown measurement case"),
        };
        let mut e = mesh_scene(grid, instances);
        e.document.objects[1].position = [0.; 3];
        if case == "masked" {
            let mut mesh = (*e.document.meshes["shared"]).clone();
            mesh.edge_mask = Some("36".repeat(grid * grid));
            e.document.meshes.insert("shared".into(), Arc::new(mesh));
        }
        e.invalidate_document();
        let rss = || -> Option<u64> {
            let output = std::process::Command::new("/bin/ps")
                .args(["-o", "rss=", "-p", &std::process::id().to_string()])
                .output()
                .ok()?;
            String::from_utf8(output.stdout).ok()?.trim().parse().ok()
        };
        let before_rss = rss();
        let started = Instant::now();
        let scene_prewarmed =
            std::env::var("U3D_SNAP_PREWARM_SCENE").is_ok_and(|value| value == "1");
        let (prepared, prepared_document) = if scene_prewarmed {
            let document = std::mem::take(&mut e.document);
            (
                None,
                Some(
                    std::thread::spawn(move || PreparedDocument::new(document))
                        .join()
                        .unwrap()
                        .unwrap(),
                ),
            )
        } else {
            let document = &e.document;
            (
                Some(
                    std::thread::scope(|scope| {
                        scope
                            .spawn(|| PreparedVertices::document(document))
                            .join()
                            .unwrap()
                    })
                    .unwrap(),
                ),
                None,
            )
        };
        let prepare_us = started.elapsed().as_micros();
        let prepared_memory = prepared_document
            .as_ref()
            .map_or_else(|| prepared.as_ref().unwrap().memory(), |p| p.metrics.memory);
        let prepared_rss = rss();
        let started = Instant::now();
        if let Some(prepared) = prepared_document {
            e.replace_prepared_document(prepared, None, false);
        } else {
            e.install_prepared_vertices(prepared.unwrap());
        }
        let install_us = started.elapsed().as_micros();
        let mut queries = Vec::new();
        let mut scene_build_us = 0;
        let projection = Projection::new(&e, [1280., 860.]);
        for (mode, world) in [
            (MoveSnap::Edge, [1., 0., 0.]),
            (MoveSnap::Face, [0.2, 0.3, 0.]),
        ] {
            let point = projection.project(world).unwrap().0;
            let started = Instant::now();
            let index = VertexIndex::new(&e, [1280., 860.], false);
            if queries.is_empty() {
                scene_build_us = started.elapsed().as_micros();
            }
            assert!(index.nearest_mode(point, mode).is_some());
            queries.push(started.elapsed().as_micros());
        }
        let point = projection.project([1., 0., 0.]).unwrap().0;
        let started = Instant::now();
        assert!(VertexIndex::new(&e, [1280., 860.], false)
            .nearest_mode(point, MoveSnap::Edge)
            .is_some());
        let warm_edge_us = started.elapsed().as_micros();
        let scene = e.vertex_cache.borrow().scene.clone();
        let instance = &scene.instances[&(2, false)];
        let geometry = &instance.geometry;
        let (_, edge_work) = geometry.surfaces.get().unwrap().query_with_work(
            &geometry.points,
            geometry.faces(),
            instance.matrix,
            &projection,
            point,
            false,
        );
        let report = diagnostics(&e);
        let mesh = &e.document.meshes["shared"];
        let source_bytes = mesh.triangles.capacity() * std::mem::size_of::<crate::mesh::Face>()
            + mesh.edge_mask.as_ref().map_or(0, |s| s.capacity());
        println!(
            "SNAP_METRICS {}",
            serde_json::json!({
                "case":case,"profile":"debug","grid":grid,"instances":instances,"scene_prewarmed":scene_prewarmed,
                "prepare_us":prepare_us,"install_us":install_us,
                "first_edge_us":queries[0],"first_face_us":queries[1],
                "scene_build_us":scene_build_us,"warm_edge_us":warm_edge_us,
                "edge_candidates":edge_work.candidates,"edge_nodes":edge_work.nodes,
                "prepared_memory":prepared_memory,"diagnostics":report,
                "source_buffer_bytes":source_bytes,
                "rss_kib":{"before":before_rss,"prepared":prepared_rss,"queried":rss()}
            })
        );
    }

    #[test]
    #[ignore = "CPU timing benchmark; run explicitly with --ignored --nocapture"]
    fn prepared_first_snap_cpu_benchmark() {
        use std::hint::black_box;
        use std::time::Instant;
        for (label, grid, instances) in [("shared_144x6561", 80, 144), ("dense_1x231361", 480, 1)] {
            let mut cold_times = Vec::new();
            let mut prepare = Vec::new();
            let mut install = Vec::new();
            let mut first = Vec::new();
            for _ in 0..3 {
                let mut e = mesh_scene(grid, instances);
                if instances == 1 {
                    e.document.objects[1].position = [0.; 3];
                    e.invalidate_document();
                }
                let mut cold = Editor::new();
                cold.replace_document(e.document.clone(), None, false);
                cold.yaw = e.yaw;
                cold.pitch = e.pitch;
                cold.distance = e.distance;
                cold.camera_target = e.camera_target;
                let t = Instant::now();
                let expected = VertexIndex::new(&cold, [1280., 860.], false).nearest([640., 430.]);
                cold_times.push(t.elapsed().as_micros());
                drop(cold);
                let t = Instant::now();
                let prepared = PreparedVertices::document(&e.document).unwrap();
                prepare.push(t.elapsed().as_micros());
                let t = Instant::now();
                e.install_prepared_vertices(prepared);
                install.push(t.elapsed().as_micros());
                let t = Instant::now();
                let actual =
                    black_box(VertexIndex::new(&e, [1280., 860.], false).nearest([640., 430.]));
                assert_eq!(actual.map(|c| c.world), expected.map(|c| c.world));
                first.push(t.elapsed().as_micros());
                let work = e.vertex_cache.borrow().work;
                assert_eq!(work.prepared_geometries, 1);
                assert_eq!(work.geometries_built, 1);
            }
            cold_times.sort();
            prepare.sort();
            install.sort();
            first.sort();
            println!("{label}: lazy_first_snap_us={}, worker_preparation_us={}, publish_install_us={}, prepared_first_snap_us={}",cold_times[1],prepare[1],install[1],first[1]);
        }
    }
    #[test]
    fn surface_modes_follow_imported_affine_mirrors_visibility_and_revision_snapshots() {
        use crate::gizmo::MoveSnap;
        let mut e = mesh_scene(1, 1);
        e.yaw = 0.;
        e.pitch = 0.;
        e.distance = 7.;
        let mut object = e.document.objects[1].clone();
        object.position = [2., 0., 0.];
        object.rotation = [12., 19., 8.];
        object.scale = [-0.8, 1.2, 0.7];
        object.shear = [0.2, -0.4, 0.3];
        object.mirror = true;
        e.execute(crate::editor::Command::Update {
            object: object.clone(),
        })
        .unwrap();
        let projection = Projection::new(&e, [800., 600.]);
        for mirrored in [false, true] {
            for (mode, local) in [
                (MoveSnap::Edge, [1., 0., 0.]),
                (MoveSnap::Face, [0.2, 0.3, 0.]),
            ] {
                let expected = world_point(model_matrix(&object, mirrored), local);
                let point = projection.project(expected).unwrap().0;
                let index = VertexIndex::new(&e, [800., 600.], false);
                let hit = index.nearest_mode(point, mode).unwrap();
                for i in 0..3 {
                    assert!(
                        (hit.world[i] - expected[i]).abs() < 0.0001,
                        "{:?} != {expected:?}",
                        hit.world
                    );
                }
            }
        }
        let point = projection
            .project(world_point(model_matrix(&object, false), [0.2, 0.3, 0.]))
            .unwrap()
            .0;
        let old = VertexIndex::new(&e, [800., 600.], false);
        let old_hit = old.nearest_mode(point, MoveSnap::Face).unwrap().world;
        let IndexData::Targets(scene) = &old.data else {
            unreachable!()
        };
        let surface = scene.instances[&(object.id, false)]
            .geometry
            .surfaces
            .get()
            .unwrap() as *const _;
        let next = VertexIndex::new(&e, [800., 600.], false);
        let IndexData::Targets(next_scene) = &next.data else {
            unreachable!()
        };
        assert_eq!(
            surface,
            next_scene.instances[&(object.id, false)]
                .geometry
                .surfaces
                .get()
                .unwrap() as *const _
        );
        drop(next);
        object.visible = false;
        e.execute(crate::editor::Command::Update {
            object: object.clone(),
        })
        .unwrap();
        assert!(VertexIndex::new(&e, [800., 600.], false)
            .nearest_mode(point, MoveSnap::Face)
            .is_none());
        assert_eq!(
            old.nearest_mode(point, MoveSnap::Face).unwrap().world,
            old_hit
        );
        e.undo();
        assert!(VertexIndex::new(&e, [800., 600.], false)
            .nearest_mode(point, MoveSnap::Face)
            .is_some());
        e.select(Some(object.id));
        assert!(VertexIndex::new(&e, [800., 600.], false)
            .nearest_mode(point, MoveSnap::Face)
            .is_none());
    }
}
