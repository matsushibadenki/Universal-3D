//! Immutable edge/triangle acceleration shared by instances of a shape.
use crate::mesh::Face;
use crate::renderer::world_point;
use crate::snapping::check_cancel;
use crate::snapping::Projection;
use crate::spatial::Bounds;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

struct Node {
    bounds: Bounds,
    range: std::ops::Range<usize>,
    children: Option<[usize; 2]>,
}
struct Tree {
    items: Vec<usize>,
    nodes: Vec<Node>,
    center_scratch_bytes: usize,
}
impl Tree {
    fn new(bounds: &[Bounds], cancel: Option<&AtomicBool>) -> Result<Self, String> {
        check_cancel(cancel)?;
        let mut centers = Vec::with_capacity(bounds.len());
        for (i, bounds) in bounds.iter().enumerate() {
            if i.is_multiple_of(1024) {
                check_cancel(cancel)?;
            }
            centers.push(std::array::from_fn(|axis| {
                bounds.min[axis] * 0.5 + bounds.max[axis] * 0.5
            }));
        }
        let mut tree = Self {
            items: (0..bounds.len()).collect(),
            nodes: Vec::with_capacity(Self::node_count(bounds.len())),
            center_scratch_bytes: centers.capacity() * std::mem::size_of::<[f32; 3]>(),
        };
        if !bounds.is_empty() {
            tree.build(0..bounds.len(), bounds, &centers, cancel)?;
        }
        check_cancel(cancel)?;
        Ok(tree)
    }
    fn node_count(items: usize) -> usize {
        if items == 0 {
            return 0;
        }
        if items <= 8 {
            return 1;
        }
        let base = 1usize << (usize::BITS - 1 - (items / 8).leading_zeros());
        let leaves = base + base.min(items - base * 8);
        leaves * 2 - 1
    }
    fn build(
        &mut self,
        range: std::ops::Range<usize>,
        bounds: &[Bounds],
        centers: &[[f32; 3]],
        cancel: Option<&AtomicBool>,
    ) -> Result<usize, String> {
        if self.nodes.len().is_multiple_of(1024) {
            check_cancel(cancel)?;
        }
        let mut b = bounds[self.items[range.start]];
        for (position, item) in self.items[range.start + 1..range.end].iter().enumerate() {
            if position.is_multiple_of(4096) {
                check_cancel(cancel)?;
            }
            let next = bounds[*item];
            b.min[0] = b.min[0].min(next.min[0]);
            b.max[0] = b.max[0].max(next.max[0]);
            b.min[1] = b.min[1].min(next.min[1]);
            b.max[1] = b.max[1].max(next.max[1]);
            b.min[2] = b.min[2].min(next.min[2]);
            b.max[2] = b.max[2].max(next.max[2]);
        }
        let index = self.nodes.len();
        self.nodes.push(Node {
            bounds: b,
            range: range.clone(),
            children: None,
        });
        if range.len() > 8 {
            let axis = (0..3)
                .max_by(|&a, &c| (b.max[a] - b.min[a]).total_cmp(&(b.max[c] - b.min[c])))
                .unwrap();
            let middle = range.start + range.len() / 2;
            self.items[range.clone()].select_nth_unstable_by(range.len() / 2, |a, c| {
                centers[*a][axis].total_cmp(&centers[*c][axis])
            });
            check_cancel(cancel)?;
            let a = self.build(range.start..middle, bounds, centers, cancel)?;
            let c = self.build(middle..range.end, bounds, centers, cancel)?;
            self.nodes[index].children = Some([a, c]);
        }
        Ok(index)
    }
    #[cfg(test)]
    fn new_previous(bounds: &[Bounds], cancel: Option<&AtomicBool>) -> Result<Self, String> {
        check_cancel(cancel)?;
        let mut tree = Self {
            items: (0..bounds.len()).collect(),
            nodes: Vec::new(),
            center_scratch_bytes: 0,
        };
        if !bounds.is_empty() {
            tree.build_previous(0..bounds.len(), bounds, cancel)?;
        }
        check_cancel(cancel)?;
        Ok(tree)
    }
    #[cfg(test)]
    fn build_previous(
        &mut self,
        range: std::ops::Range<usize>,
        bounds: &[Bounds],
        cancel: Option<&AtomicBool>,
    ) -> Result<usize, String> {
        if self.nodes.len().is_multiple_of(1024) {
            check_cancel(cancel)?;
        }
        let b = self.items[range.clone()]
            .iter()
            .map(|i| bounds[*i])
            .reduce(Bounds::union)
            .unwrap();
        let index = self.nodes.len();
        self.nodes.push(Node {
            bounds: b,
            range: range.clone(),
            children: None,
        });
        if range.len() > 8 {
            let axis = (0..3)
                .max_by(|&a, &c| (b.max[a] - b.min[a]).total_cmp(&(b.max[c] - b.min[c])))
                .unwrap();
            let middle = range.start + range.len() / 2;
            self.items[range.clone()].select_nth_unstable_by(range.len() / 2, |a, c| {
                let center = |i: usize| bounds[i].min[axis] * 0.5 + bounds[i].max[axis] * 0.5;
                center(*a).total_cmp(&center(*c))
            });
            let a = self.build_previous(range.start..middle, bounds, cancel)?;
            let c = self.build_previous(middle..range.end, bounds, cancel)?;
            self.nodes[index].children = Some([a, c]);
        }
        Ok(index)
    }
    fn vec_bytes(&self) -> usize {
        self.items.capacity() * std::mem::size_of::<usize>()
            + self.nodes.capacity() * std::mem::size_of::<Node>()
    }
}
pub struct SurfaceIndex {
    edges: Vec<[usize; 2]>,
    edge_tree: Tree,
    face_tree: Tree,
    build: SurfaceBuild,
}
/// Construction wall time; immutable history, not time spent by a later cache hit.
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct SurfaceBuild {
    pub shapes: usize,
    pub total_us: u64,
    pub vertex_map_us: u64,
    pub topology_us: u64,
    pub edge_sort_us: u64,
    pub edge_bounds_us: u64,
    pub edge_bvh_us: u64,
    pub bvh_center_scratch_peak_bytes: usize,
    pub face_bounds_us: u64,
    pub face_bvh_us: u64,
}
impl SurfaceBuild {
    pub fn add(&mut self, other: Self) {
        self.shapes += other.shapes;
        self.bvh_center_scratch_peak_bytes = self
            .bvh_center_scratch_peak_bytes
            .max(other.bvh_center_scratch_peak_bytes);
        self.total_us += other.total_us;
        self.vertex_map_us += other.vertex_map_us;
        self.topology_us += other.topology_us;
        self.edge_sort_us += other.edge_sort_us;
        self.edge_bounds_us += other.edge_bounds_us;
        self.edge_bvh_us += other.edge_bvh_us;
        self.face_bounds_us += other.face_bounds_us;
        self.face_bvh_us += other.face_bvh_us;
    }
}
fn lap(since: &mut Instant) -> u64 {
    let now = Instant::now();
    let elapsed = (now - *since).as_micros().min(u128::from(u64::MAX)) as u64;
    *since = now;
    elapsed
}
/// Allocated Vec capacities, not RSS, allocator metadata, or VRAM.
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct SurfaceMemory {
    pub edges: usize,
    pub triangles: usize,
    pub edge_nodes: usize,
    pub face_nodes: usize,
    pub edge_vec_bytes: usize,
    pub edge_tree_vec_bytes: usize,
    pub face_tree_vec_bytes: usize,
}
#[derive(Default, Debug)]
pub struct QueryWork {
    pub nodes: usize,
    pub candidates: usize,
}
#[derive(Clone, Copy)]
pub struct Hit {
    pub world: [f32; 3],
    pub screen: [f32; 2],
    pub depth: f32,
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn bits(p: [f32; 3]) -> [u32; 3] {
    p.map(|v| if v == 0. { 0 } else { v.to_bits() })
}
impl SurfaceIndex {
    pub fn new(points: &[[f32; 3]], faces: &[Face], edge_mask: Option<&str>) -> Self {
        Self::prepare(points, faces, edge_mask, None)
            .expect("Uncancelled surface construction cannot fail")
    }
    pub fn prepare(
        points: &[[f32; 3]],
        faces: &[Face],
        edge_mask: Option<&str>,
        cancel: Option<&AtomicBool>,
    ) -> Result<Self, String> {
        #[cfg(test)]
        if std::env::var("U3D_SURFACE_PREVIOUS").as_deref() == Ok("1") {
            return Self::prepare_previous(points, faces, edge_mask, cancel);
        }
        let started = Instant::now();
        let mut checkpoint = started;
        let mut build = SurfaceBuild {
            shapes: 1,
            ..SurfaceBuild::default()
        };
        check_cancel(cancel)?;
        let mut ids = HashMap::with_capacity(points.len());
        for (i, p) in points.iter().enumerate() {
            if i.is_multiple_of(1024) {
                check_cancel(cancel)?;
            }
            ids.insert(bits(*p), i);
        }
        build.vertex_map_us = lap(&mut checkpoint);
        let mut adjacency: HashMap<[usize; 2], ([f64; 3], usize, bool)> = if edge_mask.is_none() {
            HashMap::with_capacity(faces.len())
        } else {
            HashMap::new()
        };
        let mut original_edges = if edge_mask.is_some() {
            std::collections::HashSet::with_capacity(faces.len())
        } else {
            std::collections::HashSet::new()
        };
        for (face_index, face) in faces.iter().enumerate() {
            if face_index.is_multiple_of(1024) {
                check_cancel(cancel)?;
            }
            // Explicit polygon masks need no normal or coplanarity reconstruction.
            let n = if edge_mask.is_none() {
                let [a, b, c] = face.map(|p| p.map(f64::from));
                let n = cross(sub(b, a), sub(c, a));
                let length = dot(n, n).sqrt();
                if length < 1e-20 {
                    continue;
                }
                n.map(|v| v / length)
            } else {
                [0.; 3]
            };
            let v = face.map(|p| ids[&bits(p)]);
            for (side, [a, b]) in [[v[0], v[1]], [v[1], v[2]], [v[2], v[0]]]
                .into_iter()
                .enumerate()
            {
                if a == b {
                    continue;
                }
                let key = [a.min(b), a.max(b)];
                if let Some(mask) = edge_mask {
                    if (mask.as_bytes()[face_index] - b'0') & (1 << side) != 0 {
                        original_edges.insert(key);
                    }
                    continue;
                }
                adjacency
                    .entry(key)
                    .and_modify(|(first, count, coplanar)| {
                        *count += 1;
                        *coplanar &= dot(*first, n).abs() > 1. - 1e-10;
                    })
                    .or_insert((n, 1, true));
            }
        }
        // Original USD polygon boundaries take precedence over the legacy crease heuristic.
        let mut edges: Vec<_> = if edge_mask.is_some() {
            original_edges.into_iter().collect()
        } else {
            adjacency
                .into_iter()
                .filter(|(_, (_, count, flat))| *count != 2 || !*flat)
                .map(|(edge, _)| edge)
                .collect()
        };
        drop(ids);
        check_cancel(cancel)?;
        build.topology_us = lap(&mut checkpoint);
        edges.sort_unstable_by(|a, b| match a[0].cmp(&b[0]) {
            std::cmp::Ordering::Equal => a[1].cmp(&b[1]),
            order => order,
        });
        build.edge_sort_us = lap(&mut checkpoint);
        check_cancel(cancel)?;
        let mut edge_bounds = Vec::with_capacity(edges.len());
        for (i, [a, b]) in edges.iter().enumerate() {
            if i.is_multiple_of(1024) {
                check_cancel(cancel)?;
            }
            let a = points[*a];
            let b = points[*b];
            edge_bounds.push(Bounds {
                min: [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
                max: [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
            });
        }
        build.edge_bounds_us = lap(&mut checkpoint);
        let edge_tree = Tree::new(&edge_bounds, cancel)?;
        drop(edge_bounds);
        build.edge_bvh_us = lap(&mut checkpoint);
        let mut face_bounds = Vec::with_capacity(faces.len());
        for (i, face) in faces.iter().enumerate() {
            if i.is_multiple_of(1024) {
                check_cancel(cancel)?;
            }
            let [a, b, c] = *face;
            face_bounds.push(Bounds {
                min: [
                    a[0].min(b[0]).min(c[0]),
                    a[1].min(b[1]).min(c[1]),
                    a[2].min(b[2]).min(c[2]),
                ],
                max: [
                    a[0].max(b[0]).max(c[0]),
                    a[1].max(b[1]).max(c[1]),
                    a[2].max(b[2]).max(c[2]),
                ],
            });
        }
        build.face_bounds_us = lap(&mut checkpoint);
        let face_tree = Tree::new(&face_bounds, cancel)?;
        drop(face_bounds);
        build.face_bvh_us = lap(&mut checkpoint);
        check_cancel(cancel)?;
        build.bvh_center_scratch_peak_bytes = edge_tree
            .center_scratch_bytes
            .max(face_tree.center_scratch_bytes);
        build.total_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        Ok(Self {
            edge_tree,
            face_tree,
            edges,
            build,
        })
    }
    #[cfg(test)]
    fn prepare_previous(
        points: &[[f32; 3]],
        faces: &[Face],
        edge_mask: Option<&str>,
        cancel: Option<&AtomicBool>,
    ) -> Result<Self, String> {
        let started = Instant::now();
        let mut checkpoint = started;
        let mut build = SurfaceBuild {
            shapes: 1,
            ..SurfaceBuild::default()
        };
        check_cancel(cancel)?;
        let mut ids = HashMap::new();
        for (i, p) in points.iter().enumerate() {
            if i.is_multiple_of(1024) {
                check_cancel(cancel)?;
            }
            ids.insert(bits(*p), i);
        }
        build.vertex_map_us = lap(&mut checkpoint);
        let mut adjacency: HashMap<[usize; 2], ([f64; 3], usize, bool)> = HashMap::new();
        let mut original_edges = std::collections::HashSet::new();
        for (face_index, face) in faces.iter().enumerate() {
            if face_index.is_multiple_of(1024) {
                check_cancel(cancel)?;
            }
            let [a, b, c] = face.map(|p| p.map(f64::from));
            let n = cross(sub(b, a), sub(c, a));
            let length = dot(n, n).sqrt();
            if length < 1e-20 && edge_mask.is_none() {
                continue;
            }
            let n = if length < 1e-20 {
                [0.; 3]
            } else {
                n.map(|v| v / length)
            };
            let v = face.map(|p| ids[&bits(p)]);
            for (side, [a, b]) in [[v[0], v[1]], [v[1], v[2]], [v[2], v[0]]]
                .into_iter()
                .enumerate()
            {
                if a == b {
                    continue;
                }
                let key = [a.min(b), a.max(b)];
                if let Some(mask) = edge_mask {
                    if (mask.as_bytes()[face_index] - b'0') & (1 << side) != 0 {
                        original_edges.insert(key);
                    }
                    continue;
                }
                adjacency
                    .entry(key)
                    .and_modify(|(first, count, coplanar)| {
                        *count += 1;
                        *coplanar &= dot(*first, n).abs() > 1. - 1e-10;
                    })
                    .or_insert((n, 1, true));
            }
        }
        // Original USD polygon boundaries take precedence over the legacy crease heuristic.
        let mut edges: Vec<_> = if edge_mask.is_some() {
            original_edges.into_iter().collect()
        } else {
            adjacency
                .into_iter()
                .filter(|(_, (_, count, flat))| *count != 2 || !*flat)
                .map(|(edge, _)| edge)
                .collect()
        };
        drop(ids);
        check_cancel(cancel)?;
        build.topology_us = lap(&mut checkpoint);
        edges.sort_unstable();
        build.edge_sort_us = lap(&mut checkpoint);
        check_cancel(cancel)?;
        let mut edge_bounds = Vec::with_capacity(edges.len());
        for (i, [a, b]) in edges.iter().enumerate() {
            if i.is_multiple_of(1024) {
                check_cancel(cancel)?;
            }
            edge_bounds.push(Bounds::point(points[*a]).union(Bounds::point(points[*b])));
        }
        build.edge_bounds_us = lap(&mut checkpoint);
        let edge_tree = Tree::new_previous(&edge_bounds, cancel)?;
        drop(edge_bounds);
        build.edge_bvh_us = lap(&mut checkpoint);
        let mut face_bounds = Vec::with_capacity(faces.len());
        for (i, face) in faces.iter().enumerate() {
            if i.is_multiple_of(1024) {
                check_cancel(cancel)?;
            }
            face_bounds.push(
                face.iter()
                    .copied()
                    .map(Bounds::point)
                    .reduce(Bounds::union)
                    .unwrap(),
            );
        }
        build.face_bounds_us = lap(&mut checkpoint);
        let face_tree = Tree::new_previous(&face_bounds, cancel)?;
        drop(face_bounds);
        build.face_bvh_us = lap(&mut checkpoint);
        check_cancel(cancel)?;
        build.total_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        Ok(Self {
            edge_tree,
            face_tree,
            edges,
            build,
        })
    }
    pub fn build_profile(&self) -> SurfaceBuild {
        self.build
    }
    pub fn memory(&self) -> SurfaceMemory {
        SurfaceMemory {
            edges: self.edges.len(),
            triangles: self.face_tree.items.len(),
            edge_nodes: self.edge_tree.nodes.len(),
            face_nodes: self.face_tree.nodes.len(),
            edge_vec_bytes: self.edges.capacity() * std::mem::size_of::<[usize; 2]>(),
            edge_tree_vec_bytes: self.edge_tree.vec_bytes(),
            face_tree_vec_bytes: self.face_tree.vec_bytes(),
        }
    }
    pub fn query(
        &self,
        points: &[[f32; 3]],
        faces: &[Face],
        matrix: [[f32; 4]; 4],
        projection: &Projection,
        pointer: [f32; 2],
        face: bool,
    ) -> Option<Hit> {
        self.query_with_work(points, faces, matrix, projection, pointer, face)
            .0
    }
    pub fn query_with_work(
        &self,
        points: &[[f32; 3]],
        faces: &[Face],
        matrix: [[f32; 4]; 4],
        projection: &Projection,
        pointer: [f32; 2],
        face: bool,
    ) -> (Option<Hit>, QueryWork) {
        let planes = projection.surface_planes(pointer, face);
        let to_local = |planes: [[f64; 4]; 6]| {
            planes.map(|p| {
                std::array::from_fn(|j| {
                    (0..3).map(|i| p[i] * f64::from(matrix[j][i])).sum::<f64>()
                        + if j == 3 { p[3] } else { 0. }
                })
            })
        };
        let mut local = to_local(planes);
        let mut work = QueryWork::default();
        let mut best: Option<(f32, [u32; 3], Hit)> = None;
        let tree = if face {
            &self.face_tree
        } else {
            &self.edge_tree
        };
        let priority = |i: usize| {
            let b = tree.nodes[i].bounds;
            let center = std::array::from_fn(|j| b.min[j] * 0.5 + b.max[j] * 0.5);
            projection
                .project(world_point(matrix, center))
                .map_or(f32::INFINITY, |(p, _)| {
                    (p[0] - pointer[0]).powi(2) + (p[1] - pointer[1]).powi(2)
                })
        };
        let mut stack = Vec::new();
        if !tree.nodes.is_empty() {
            stack.push(0);
        }
        while let Some(i) = stack.pop() {
            work.nodes += 1;
            let node = &tree.nodes[i];
            if !node.bounds.intersects(&local) {
                continue;
            }
            if let Some([a, b]) = node.children {
                // Ordering is heuristic only. Culling always uses conservative bounds.
                if !face && priority(a) > priority(b) {
                    stack.extend([a, b]);
                } else {
                    stack.extend([b, a]);
                }
                continue;
            }
            for &index in &tree.items[node.range.clone()] {
                work.candidates += 1;
                let world = if face {
                    triangle_hit(
                        faces[index].map(|p| world_point(matrix, p)),
                        projection,
                        pointer,
                    )
                } else {
                    let edge = self.edges[index].map(|i| world_point(matrix, points[i]));
                    segment_hit(edge, projection, pointer)
                };
                let Some(world) = world else {
                    continue;
                };
                let Some((screen, depth)) = projection.project(world) else {
                    continue;
                };
                if !projection.on_screen(screen, if face { 0.01 } else { 14. }) {
                    continue;
                }
                let distance = if face {
                    0.
                } else {
                    (screen[0] - pointer[0]).powi(2) + (screen[1] - pointer[1]).powi(2)
                };
                if !face && distance > 14. * 14. {
                    continue;
                }
                let rank = bits(world);
                let hit = Hit {
                    world,
                    screen,
                    depth,
                };
                if best.is_none_or(|(d, old, h)| {
                    distance < d
                        || (distance == d && (depth < h.depth || (depth == h.depth && rank < old)))
                }) {
                    best = Some((distance, rank, hit));
                    if !face {
                        // Retain ties and f32 projection roundoff at the new cone boundary.
                        local = to_local(
                            projection.planes_with_radius(pointer, distance.sqrt() + 0.001),
                        );
                    }
                }
            }
        }
        (best.map(|(_, _, hit)| hit), work)
    }
}
fn triangle_hit(face: Face, projection: &Projection, pointer: [f32; 2]) -> Option<[f32; 3]> {
    let (eye, ray) = projection.ray(pointer);
    let [a, b, c] = face.map(|p| p.map(f64::from));
    let ab = sub(b, a);
    let ac = sub(c, a);
    let h = cross(ray, ac);
    let det = dot(ab, h);
    if det.abs() <= 1e-12 * (dot(ab, ab) * dot(ac, ac) * dot(ray, ray)).sqrt() {
        return None;
    }
    let s = sub(eye, a);
    let u = dot(s, h) / det;
    if !(-1e-9..=1. + 1e-9).contains(&u) {
        return None;
    }
    let q = cross(s, ab);
    let v = dot(ray, q) / det;
    if v < -1e-9 || u + v > 1. + 1e-9 {
        return None;
    }
    let t = dot(ac, q) / det;
    if t <= 0. {
        return None;
    }
    Some(std::array::from_fn(|i| (eye[i] + t * ray[i]) as f32))
}
fn segment_hit(
    edge: [[f32; 3]; 2],
    projection: &Projection,
    pointer: [f32; 2],
) -> Option<[f32; 3]> {
    let [a, b] = edge.map(|p| p.map(f64::from));
    let [da, db] = edge.map(|p| projection.depth(p));
    let (near, far) = projection.clip_depth();
    let difference = db - da;
    let mut lo = 0f64;
    let mut hi = 1f64;
    if difference.abs() < 1e-15 {
        if da <= near || da >= far {
            return None;
        }
    } else {
        let t0 = (near + 1e-6 - da) / difference;
        let t1 = (far - 1e-6 - da) / difference;
        lo = lo.max(t0.min(t1));
        hi = hi.min(t0.max(t1));
        if lo > hi {
            return None;
        }
    }
    let at = |t: f64| std::array::from_fn(|i| (a[i] + (b[i] - a[i]) * t) as f32);
    let a = at(lo);
    let b = at(hi);
    let (pa, za) = projection.project(a)?;
    let (pb, zb) = projection.project(b)?;
    let delta = [f64::from(pb[0] - pa[0]), f64::from(pb[1] - pa[1])];
    let length = delta[0] * delta[0] + delta[1] * delta[1];
    let t = if length < 1e-15 {
        if za <= zb {
            0.
        } else {
            1.
        }
    } else {
        ((f64::from(pointer[0] - pa[0]) * delta[0] + f64::from(pointer[1] - pa[1]) * delta[1])
            / length)
            .clamp(0., 1.)
    };
    let t = (t / f64::from(zb)) / ((1. - t) / f64::from(za) + t / f64::from(zb));
    Some(std::array::from_fn(|i| {
        (f64::from(a[i]) * (1. - t) + f64::from(b[i]) * t) as f32
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Editor;
    fn projection() -> Projection {
        let mut e = Editor::new();
        e.yaw = 0.;
        e.pitch = 0.;
        Projection::new(&e, [800., 600.])
    }
    fn close(a: [f32; 3], b: [f32; 3]) {
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < 0.0001, "{a:?} != {b:?}");
        }
    }
    #[test]
    fn median_tree_reservation_matches_topology_and_previous_bounds() {
        fn oracle(n: usize) -> usize {
            if n == 0 {
                0
            } else if n <= 8 {
                1
            } else {
                1 + oracle(n / 2) + oracle(n - n / 2)
            }
        }
        for n in (0..1024).chain([4096, 4097, 8191, 8192, 8193, 458714, 500000, 1000000]) {
            assert_eq!(Tree::node_count(n), oracle(n));
        }
        for n in [0, 1, 8, 9, 16, 17, 31, 32, 33, 129, 1025] {
            let bounds: Vec<_> = (0..n)
                .map(|i| Bounds::point([(i % 7) as f32, (i % 13) as f32, 0.]))
                .collect();
            let new = Tree::new(&bounds, None).unwrap();
            let old = Tree::new_previous(&bounds, None).unwrap();
            assert_eq!(new.nodes.len(), Tree::node_count(n));
            assert_eq!(new.items, old.items);
            for (new, old) in new.nodes.iter().zip(&old.nodes) {
                assert_eq!(new.bounds, old.bounds);
                assert_eq!(new.range, old.range);
                assert_eq!(new.children, old.children);
            }
        }
    }
    #[test]
    fn optimized_surface_matches_previous_for_masks_legacy_and_degenerate_faces() {
        let points = [
            [0., 0., 0.],
            [1., 0., 0.],
            [1., 1., 0.],
            [0., 1., 0.],
            [0., 0., 1.],
        ];
        let faces = [
            [points[0], points[1], points[2]],
            [points[0], points[2], points[3]],
            [points[0], points[1], points[4]],
            [points[0], points[1], points[1]],
        ];
        for mask in [None, Some("7777"), Some("5307"), Some("0000")] {
            let new = SurfaceIndex::prepare(&points, &faces, mask, None).unwrap();
            let old = SurfaceIndex::prepare_previous(&points, &faces, mask, None).unwrap();
            assert_eq!(new.edges, old.edges);
            assert_eq!(new.face_tree.items, old.face_tree.items);
            assert_eq!(new.edge_tree.items, old.edge_tree.items);
            for (a, b) in new.edge_tree.nodes.iter().zip(&old.edge_tree.nodes) {
                assert_eq!(a.bounds, b.bounds);
            }
            let timings = new.build_profile();
            let sum = timings.vertex_map_us
                + timings.topology_us
                + timings.edge_sort_us
                + timings.edge_bounds_us
                + timings.edge_bvh_us
                + timings.face_bounds_us
                + timings.face_bvh_us;
            assert!(sum <= timings.total_us);
            assert_eq!(timings.shapes, 1);
            let cancel = AtomicBool::new(true);
            assert!(SurfaceIndex::prepare(&points, &faces, mask, Some(&cancel)).is_err());
        }
    }
    #[test]
    fn dense_edge_search_prunes_candidates_and_preserves_brute_force_ties() {
        let grid = 64;
        let mut points = Vec::new();
        for y in 0..=grid {
            for x in 0..=grid {
                points.push([x as f32 / 32. - 1., y as f32 / 32. - 1., 0.]);
            }
        }
        let mut faces = Vec::new();
        for y in 0..grid {
            for x in 0..grid {
                let a = y * (grid + 1) + x;
                let [a, b, c, d] = [
                    points[a],
                    points[a + 1],
                    points[a + grid + 2],
                    points[a + grid + 1],
                ];
                faces.extend([[a, b, c], [a, c, d]]);
            }
        }
        let index = SurfaceIndex::new(&points, &faces, Some(&"36".repeat(grid * grid)));
        let p = projection();
        let matrix = [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ];
        for pointer in [
            [400., 300.],
            [401.25, 303.5],
            [430., 320.],
            [475., 320.],
            [480., 300.],
        ] {
            let (hit, work) = index.query_with_work(&points, &faces, matrix, &p, pointer, false);
            let expected = index
                .edges
                .iter()
                .filter_map(|edge| {
                    let world = segment_hit(edge.map(|i| points[i]), &p, pointer)?;
                    let (screen, depth) = p.project(world)?;
                    let distance =
                        (screen[0] - pointer[0]).powi(2) + (screen[1] - pointer[1]).powi(2);
                    (p.on_screen(screen, 14.) && distance <= 196.).then_some((
                        distance,
                        depth,
                        bits(world),
                        world,
                    ))
                })
                .min_by(|a, b| {
                    a.0.total_cmp(&b.0)
                        .then(a.1.total_cmp(&b.1))
                        .then(a.2.cmp(&b.2))
                });
            assert_eq!(hit.map(|h| h.world), expected.map(|(_, _, _, w)| w));
            assert!(work.candidates < index.edges.len() / 10, "{work:?}");
        }
    }
    #[test]
    fn perspective_edge_interpolation_hits_the_3d_segment_and_clips_crossing_edges() {
        let p = projection();
        let edge = [[-1., 0., 2.], [1., 0., -2.]];
        let a = p.project(edge[0]).unwrap().0;
        let b = p.project(edge[1]).unwrap().0;
        let pointer = [(a[0] + b[0]) * 0.5, 300.];
        let hit = segment_hit(edge, &p, pointer).unwrap();
        let t = 5. / 14.;
        close(hit, [-1. + 2. * t, 0., 2. - 4. * t]);
        assert!((p.project(hit).unwrap().0[0] - pointer[0]).abs() < 0.0001);
        let hit = segment_hit([[0., 0., 8.], [0., 0., 0.]], &p, [400., 300.]).unwrap();
        assert!(p.project(hit).is_some());
        assert!(segment_hit([[0., 0., 8.], [1., 0., 9.]], &p, [400., 300.]).is_none());
    }
    #[test]
    fn topology_removes_flat_diagonals_but_retains_boundaries_creases_and_nonmanifold_edges() {
        let points = [
            [-1., -1., 0.],
            [1., -1., 0.],
            [1., 1., 0.],
            [-1., 1., 0.],
            [0., 0., 1.],
        ];
        let mut faces = vec![
            [points[0], points[1], points[2]],
            [points[0], points[2], points[3]],
        ];
        let flat = SurfaceIndex::new(&points, &faces, None);
        assert_eq!(flat.edges.len(), 4);
        assert!(!flat.edges.contains(&[0, 2]));
        faces.push([points[0], points[2], points[4]]);
        let nonmanifold = SurfaceIndex::new(&points, &faces, None);
        assert!(nonmanifold.edges.contains(&[0, 2]));
        let crease = SurfaceIndex::new(&points, &[faces[0], faces[2]], None);
        assert!(crease.edges.contains(&[0, 2]));
        let p = projection();
        let identity = [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ];
        assert!(flat
            .query(&points, &faces[..2], identity, &p, [400., 300.], false)
            .is_none());
        close(
            flat.query(&points, &faces[..2], identity, &p, [400., 300.], true)
                .unwrap()
                .world,
            [0.; 3],
        );
    }
    #[test]
    fn triangle_intersection_is_two_sided_degenerate_safe_and_chooses_nearest_depth() {
        let p = projection();
        let face = [[-1., -1., 0.], [1., -1., 0.], [0., 1., 0.]];
        close(triangle_hit(face, &p, [400., 300.]).unwrap(), [0.; 3]);
        close(
            triangle_hit([face[2], face[1], face[0]], &p, [400., 300.]).unwrap(),
            [0.; 3],
        );
        assert!(triangle_hit([[0.; 3]; 3], &p, [400., 300.]).is_none());
        assert!(triangle_hit(face, &p, [0., 0.]).is_none());
        let points = vec![
            face[0],
            face[1],
            face[2],
            [-1., -1., 2.],
            [1., -1., 2.],
            [0., 1., 2.],
        ];
        let faces = [face, [points[3], points[4], points[5]]];
        let index = SurfaceIndex::new(&points, &faces, None);
        let identity = [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ];
        close(
            index
                .query(&points, &faces, identity, &p, [400., 300.], true)
                .unwrap()
                .world,
            [0., 0., 2.],
        );
    }
    #[test]
    fn bounds_search_matches_all_segments_and_triangles_across_clipping_and_transforms() {
        let mut e = Editor::new();
        let object = e.document.objects[0].clone();
        let owned_faces = crate::renderer::object_geometry(&e, &object).to_vec();
        let faces = owned_faces.as_slice();
        let mut points = Vec::new();
        for p in faces.iter().flatten() {
            if !points.contains(p) {
                points.push(*p);
            }
        }
        let index = SurfaceIndex::new(&points, faces, None);
        for (yaw, pitch, distance) in [(0., 0., 7.), (0.7, 0.45, 3.), (-1.2, -0.3, 0.2)] {
            e.yaw = yaw;
            e.pitch = pitch;
            e.distance = distance;
            let p = Projection::new(&e, [800., 600.]);
            let mut object = object.clone();
            object.rotation = [17., 29., -13.];
            object.scale = [-1.2, 0.6, 1.8];
            object.shear = [0.2, -0.4, 0.3];
            for mirrored in [false, true] {
                let matrix = crate::renderer::model_matrix(&object, mirrored);
                for face in [false, true] {
                    for x in (20..800).step_by(31) {
                        for y in (10..600).step_by(29) {
                            let pointer = [x as f32, y as f32];
                            let accelerated =
                                index.query(&points, faces, matrix, &p, pointer, face);
                            let mut brute = Vec::new();
                            if face {
                                for triangle in faces {
                                    if let Some(world) = triangle_hit(
                                        triangle.map(|v| world_point(matrix, v)),
                                        &p,
                                        pointer,
                                    ) {
                                        brute.push(world);
                                    }
                                }
                            } else {
                                for edge in &index.edges {
                                    if let Some(world) = segment_hit(
                                        edge.map(|i| world_point(matrix, points[i])),
                                        &p,
                                        pointer,
                                    ) {
                                        brute.push(world);
                                    }
                                }
                            }
                            let expected = brute
                                .into_iter()
                                .filter_map(|world| {
                                    let (screen, depth) = p.project(world)?;
                                    if !p.on_screen(screen, if face { 0.01 } else { 14. }) {
                                        return None;
                                    }
                                    let distance = if face {
                                        0.
                                    } else {
                                        (screen[0] - pointer[0]).powi(2)
                                            + (screen[1] - pointer[1]).powi(2)
                                    };
                                    (face || distance <= 196.).then_some((
                                        distance,
                                        depth,
                                        bits(world),
                                        world,
                                    ))
                                })
                                .min_by(|a, b| {
                                    a.0.total_cmp(&b.0)
                                        .then(a.1.total_cmp(&b.1))
                                        .then(a.2.cmp(&b.2))
                                })
                                .map(|(_, _, _, w)| w);
                            assert_eq!(
                                accelerated.map(|h| h.world),
                                expected,
                                "pointer={pointer:?}, face={face}, distance={distance}"
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn explicit_polygon_masks_keep_real_flat_edges_and_exclude_generated_diagonals() {
        let points = [[-1., -1., 0.], [1., -1., 0.], [1., 1., 0.], [-1., 1., 0.]];
        let faces = [
            [points[0], points[1], points[2]],
            [points[0], points[2], points[3]],
        ];
        let quad_mask = "36";
        assert_eq!(
            SurfaceIndex::new(&points, &faces, Some(quad_mask))
                .edges
                .len(),
            4
        );
        let triangle_mask = "77";
        assert_eq!(
            SurfaceIndex::new(&points, &faces, Some(triangle_mask))
                .edges
                .len(),
            5
        );
    }
}
