//! Screen-sized transform handles. Document, geometry and gesture state remain in Rust.
use crate::affine::{world_scaled_axes_bounded, world_scaled_bounded};
use crate::document::{Editor, Object};
use crate::renderer::camera_basis;
use crate::snapping::{Projection, VertexIndex};
use serde::{Deserialize, Serialize};
use std::f32::consts::{PI, TAU};
use std::sync::Arc;

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    #[default]
    Select,
    Move,
    Rotate,
    Scale,
}
#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Space {
    World,
    Local,
}
#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum MoveSnap {
    #[default]
    Relative,
    Grid,
    Vertex,
    Edge,
    Face,
}
#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Debug)]
pub struct Settings {
    pub move_space: Space,
    pub rotate_space: Space,
    pub scale_space: Space,
    pub snap: bool,
    #[serde(default)]
    pub move_snap: MoveSnap,
    pub move_step: f32,
    pub rotate_step: f32,
    pub scale_step: f32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            move_space: Space::World,
            rotate_space: Space::Local,
            scale_space: Space::Local,
            snap: false,
            move_snap: MoveSnap::Relative,
            move_step: 1.,
            rotate_step: 15.,
            scale_step: 0.1,
        }
    }
}
impl Settings {
    pub fn space(&self, tool: Tool) -> Space {
        match tool {
            Tool::Rotate => self.rotate_space,
            Tool::Scale => self.scale_space,
            _ => self.move_space,
        }
    }
    pub fn set_space(&mut self, tool: Tool, space: Space) {
        match tool {
            Tool::Rotate => self.rotate_space = space,
            Tool::Scale => self.scale_space = space,
            _ => {
                self.move_space = space;
                if space == Space::Local && self.move_snap == MoveSnap::Grid {
                    self.move_snap = MoveSnap::Relative;
                }
            }
        }
    }
    pub fn set_move_snap(&mut self, mode: MoveSnap) -> Result<(), String> {
        if mode == MoveSnap::Grid && self.move_space != Space::World {
            return Err("Grid snap requires World axes / グリッドスナップにはワールド軸が必要です / 网格吸附需要世界坐标轴".into());
        }
        self.move_snap = mode;
        Ok(())
    }
    pub fn set_step(&mut self, tool: Tool, step: f32) -> Result<(), String> {
        let range = match tool {
            Tool::Rotate => 0.1..=180.,
            Tool::Scale => 0.01..=10.,
            _ => 0.001..=10000.,
        };
        if !step.is_finite() || !range.contains(&step) {
            return Err("Invalid snap increment".into());
        }
        match tool {
            Tool::Rotate => self.rotate_step = step,
            Tool::Scale => self.scale_step = step,
            _ => self.move_step = step,
        }
        Ok(())
    }
}
fn snap(value: f32, step: f32, enabled: bool) -> f32 {
    if enabled {
        (value / step).round() * step
    } else {
        value
    }
}
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Handle {
    Axis(usize),
    // The omitted axis is the plane normal: 0=YZ, 1=ZX, 2=XY.
    Plane(usize),
    Screen,
}
#[derive(Clone)]
pub struct TransformDrag {
    pub token: String,
    pub before: Object,
    pub dirty_before: bool,
    pub handle: Handle,
    tool: Tool,
    settings: Settings,
    basis: [[f32; 3]; 3],
    eye: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    forward: [f32; 3],
    normal: [f32; 3],
    grab: [f32; 3],
    size: [f32; 2],
    start: [f32; 2],
    tangent: [f32; 2],
    ring_basis: [[f32; 3]; 2],
    plane_rotation: bool,
    previous_phase: f32,
    angle: f32,
    previous_rotation: [f32; 3],
    vertex_source: [f32; 3],
    vertex_targets: Option<Arc<VertexIndex>>,
    projection: Projection,
    pub vertex_markers: Option<([f32; 2], Option<[f32; 2]>)>,
}
pub struct Ring {
    pub handle: Handle,
    pub points: Vec<Option<[f32; 2]>>,
    basis: [[f32; 3]; 2],
}
pub struct PlaneHandle {
    pub normal: usize,
    pub corners: [[f32; 2]; 4],
    depth: f32,
}
pub struct Handles {
    pub origin: [f32; 2],
    pub tips: [Option<[f32; 2]>; 3],
    pub rings: Vec<Ring>,
    // Back to front, so hit testing can use the reverse of draw order.
    pub planes: Vec<PlaneHandle>,
    pub tool: Tool,
}
struct Hit {
    handle: Handle,
    tangent: [f32; 2],
    basis: [[f32; 3]; 2],
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|i| a[i] * b[i]).sum()
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn axis(index: usize) -> [f32; 3] {
    std::array::from_fn(|i| if i == index { 1. } else { 0. })
}
pub fn rotation_basis(degrees: [f32; 3]) -> [[f32; 3]; 3] {
    std::array::from_fn(|i| {
        let mut p = axis(i);
        for (j, degrees) in degrees.iter().enumerate() {
            let a = (j + 1) % 3;
            let b = (j + 2) % 3;
            let (s, c) = degrees.to_radians().sin_cos();
            let x = p[a] * c - p[b] * s;
            p[b] = p[a] * s + p[b] * c;
            p[a] = x;
        }
        p
    })
}
// Rz * Ry * Rx, matching renderer::model_matrix. Choose the equivalent Euler
// solution closest to the previous sample to avoid jumps at +/-180 and gimbal poles.
pub(crate) fn euler_near(b: [[f32; 3]; 3], reference: [f32; 3]) -> [f32; 3] {
    let y = (-b[0][2]).clamp(-1., 1.).asin();
    let (x, z) = if y.cos().abs() > 0.0001 {
        (b[1][2].atan2(b[2][2]), b[0][1].atan2(b[0][0]))
    } else {
        let x = reference[0].to_radians();
        (x, (-b[1][0]).atan2(b[1][1]) + y.signum() * x)
    };
    let near = |v: [f32; 3]| {
        std::array::from_fn(|i| {
            let d = v[i].to_degrees();
            d + ((reference[i] - d) / 360.).round() * 360.
        })
    };
    let a: [f32; 3] = near([x, y, z]);
    let other: [f32; 3] = near([x + PI, PI - y, z + PI]);
    let distance = |v: [f32; 3]| (0..3).map(|i| (v[i] - reference[i]).powi(2)).sum::<f32>();
    if distance(a) <= distance(other) {
        a
    } else {
        other
    }
}
pub fn valid_pointer(x: f32, y: f32, size: [f32; 2]) -> Result<(), String> {
    if ![x, y, size[0], size[1]].iter().all(|v| v.is_finite())
        || size.iter().any(|v| !(1. ..=32768.).contains(v))
        || x.abs().max(y.abs()) > 100000.
    {
        return Err("Invalid viewport pointer".into());
    }
    Ok(())
}
pub fn handles(editor: &Editor, size: [f32; 2]) -> Option<Handles> {
    if editor.tool == Tool::Select || valid_pointer(0., 0., size).is_err() {
        return None;
    }
    let (_, object) = editor.indexed_object(editor.document.selected?)?;
    if !object.visible {
        return None;
    }
    let (eye, right, up, forward) = camera_basis(editor);
    let near = (editor.distance * 0.1).min(0.1);
    let far = (editor.distance * 4.).max(100.);
    let project = |p| {
        let v = sub(p, eye);
        let depth = dot(v, forward);
        if depth <= near || depth >= far {
            return None;
        }
        let unit = 0.9 * size[1] / depth;
        Some([
            size[0] * 0.5 + dot(v, right) * unit,
            size[1] * 0.5 - dot(v, up) * unit,
        ])
    };
    let origin = project(object.position)?;
    if origin[0] < 0. || origin[0] > size[0] || origin[1] < 0. || origin[1] > size[1] {
        return None;
    }
    let world_per_pixel = dot(sub(object.position, eye), forward) / (0.9 * size[1]);
    // Freeze rotation rings while dragging: the grabbed ring must not move under the pointer.
    let rotation = editor
        .transform_drag
        .as_ref()
        .filter(|d| matches!(d.tool, Tool::Rotate | Tool::Scale))
        .map_or(object.rotation, |d| d.before.rotation);
    let basis = if editor.transform_settings.space(editor.tool) == Space::World {
        [axis(0), axis(1), axis(2)]
    } else {
        rotation_basis(rotation)
    };
    let tips = std::array::from_fn(|i| {
        project(std::array::from_fn(|j| {
            object.position[j] + basis[i][j] * world_per_pixel * 88.
        }))
        .filter(|p| (p[0] - origin[0]).hypot(p[1] - origin[1]) >= 16.)
    });
    let mut rings = Vec::new();
    let mut planes = Vec::new();
    if matches!(editor.tool, Tool::Move | Tool::Scale) {
        let view = sub(object.position, eye);
        let view_length = dot(view, view).sqrt();
        for normal in 0..3 {
            // Nearly edge-on planes have unstable intersections and unusable hit targets.
            if dot(view, basis[normal]).abs() / view_length < 0.18 {
                continue;
            }
            let a = basis[(normal + 1) % 3];
            let b = basis[(normal + 2) % 3];
            let world = |x, y| {
                std::array::from_fn(|j| {
                    object.position[j] + world_per_pixel * (a[j] * x + b[j] * y)
                })
            };
            let points =
                [[24., 24.], [44., 24.], [44., 44.], [24., 44.]].map(|[x, y]| project(world(x, y)));
            let [Some(p0), Some(p1), Some(p2), Some(p3)] = points else {
                continue;
            };
            let corners = [p0, p1, p2, p3];
            if polygon_area(corners).abs() < 60. {
                continue;
            }
            planes.push(PlaneHandle {
                normal,
                corners,
                depth: dot(sub(world(34., 34.), eye), forward),
            });
        }
        planes.sort_by(|a, b| b.depth.total_cmp(&a.depth));
    }
    if editor.tool == Tool::Rotate {
        for i in 0..4 {
            let (handle, plane, radius) = if i < 3 {
                (
                    Handle::Axis(i),
                    [basis[(i + 1) % 3], basis[(i + 2) % 3]],
                    72.,
                )
            } else {
                (Handle::Screen, [right, up], 96.)
            };
            let points = (0..=64)
                .map(|step| {
                    let (s, c) = (step as f32 * TAU / 64.).sin_cos();
                    project(std::array::from_fn(|j| {
                        object.position[j]
                            + world_per_pixel * radius * (plane[0][j] * c + plane[1][j] * s)
                    }))
                })
                .collect();
            rings.push(Ring {
                handle,
                points,
                basis: plane,
            });
        }
    }
    Some(Handles {
        origin,
        tips,
        rings,
        planes,
        tool: editor.tool,
    })
}
fn polygon_area(points: [[f32; 2]; 4]) -> f32 {
    // Translate first to avoid catastrophic cancellation for large screen coordinates.
    let p = points.map(|p| [p[0] - points[0][0], p[1] - points[0][1]]);
    (p[1][0] * p[2][1] - p[1][1] * p[2][0] + p[2][0] * p[3][1] - p[2][1] * p[3][0]) * 0.5
}
fn inside_plane(point: [f32; 2], corners: [[f32; 2]; 4]) -> bool {
    let sign = polygon_area(corners).signum();
    (0..4).all(|i| {
        let a = corners[i];
        let b = corners[(i + 1) % 4];
        ((b[0] - a[0]) * (point[1] - a[1]) - (b[1] - a[1]) * (point[0] - a[0])) * sign >= 0.
    })
}
fn segment_distance(point: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let v = [b[0] - a[0], b[1] - a[1]];
    let d = [point[0] - a[0], point[1] - a[1]];
    let t = ((d[0] * v[0] + d[1] * v[1]) / (v[0] * v[0] + v[1] * v[1]).max(0.00001)).clamp(0., 1.);
    (d[0] - t * v[0]).hypot(d[1] - t * v[1])
}
impl Handles {
    fn hit(&self, point: [f32; 2]) -> Option<Hit> {
        let mut nearest = 9.;
        let mut hit = None;
        if self.tool == Tool::Rotate {
            for ring in &self.rings {
                for segment in ring.points.windows(2) {
                    let (Some(a), Some(b)) = (segment[0], segment[1]) else {
                        continue;
                    };
                    let distance = segment_distance(point, a, b);
                    if distance < nearest {
                        let t = [b[0] - a[0], b[1] - a[1]];
                        let length = t[0].hypot(t[1]);
                        if length < 0.1 {
                            continue;
                        }
                        nearest = distance;
                        hit = Some(Hit {
                            handle: ring.handle,
                            tangent: t.map(|v| v / length),
                            basis: ring.basis,
                        });
                    }
                }
            }
            return hit;
        }
        let d = [point[0] - self.origin[0], point[1] - self.origin[1]];
        if d[0].abs().max(d[1].abs()) <= 8. {
            return Some(Hit {
                handle: Handle::Screen,
                tangent: [0.; 2],
                basis: [[0.; 3]; 2],
            });
        }
        for (i, tip) in self.tips.iter().enumerate() {
            let Some(tip) = tip else { continue };
            let distance = segment_distance(point, self.origin, *tip);
            if distance < nearest {
                nearest = distance;
                let t = [tip[0] - self.origin[0], tip[1] - self.origin[1]];
                let length = t[0].hypot(t[1]);
                hit = Some(Hit {
                    handle: Handle::Axis(i),
                    tangent: t.map(|v| v / length),
                    basis: [[0.; 3]; 2],
                });
            }
        }
        // Axis shafts and the center remain easy to grab where projected handles overlap.
        hit.or_else(|| {
            self.planes
                .iter()
                .rev()
                .find(|plane| inside_plane(point, plane.corners))
                .map(|plane| Hit {
                    handle: Handle::Plane(plane.normal),
                    tangent: {
                        let center: [f32; 2] = std::array::from_fn(|i| {
                            plane.corners.iter().map(|p| p[i]).sum::<f32>() * 0.25 - self.origin[i]
                        });
                        let length = center[0].hypot(center[1]).max(0.001);
                        center.map(|v| v / length)
                    },
                    basis: [[0.; 3]; 2],
                })
        })
    }
}
impl TransformDrag {
    pub fn begin(
        editor: &Editor,
        token: String,
        point: [f32; 2],
        size: [f32; 2],
        vertex_snap: bool,
    ) -> Option<Self> {
        let (_, object) = editor.indexed_object(editor.document.selected?)?;
        if !object.visible {
            return None;
        }
        let vertex_enabled = editor.tool == Tool::Move
            && (vertex_snap
                || (editor.transform_settings.snap
                    && matches!(
                        editor.transform_settings.move_snap,
                        MoveSnap::Vertex | MoveSnap::Edge | MoveSnap::Face
                    )));
        let handle_hit = handles(editor, size).and_then(|h| h.hit(point));
        let source = vertex_enabled
            .then(|| VertexIndex::new(editor, size, true).nearest(point))
            .flatten()
            // Preserve a manipulator hit unless the pointer is directly on a vertex.
            .filter(|v| {
                handle_hit.is_none() || (v.screen[0] - point[0]).hypot(v.screen[1] - point[1]) <= 6.
            });
        // A source vertex can be grabbed directly, even when the pivot is off screen.
        let hit = if source.is_some() {
            Hit {
                handle: Handle::Screen,
                tangent: [0.; 2],
                basis: [[0.; 3]; 2],
            }
        } else {
            handle_hit?
        };
        let (eye, right, up, forward) = camera_basis(editor);
        let basis = if editor.transform_settings.space(editor.tool) == Space::World {
            [axis(0), axis(1), axis(2)]
        } else {
            rotation_basis(object.rotation)
        };
        let normal = if editor.tool == Tool::Rotate {
            cross(hit.basis[0], hit.basis[1])
        } else {
            match hit.handle {
                Handle::Screen => forward,
                Handle::Plane(i) => basis[i],
                Handle::Axis(i) => {
                    let a = basis[i];
                    std::array::from_fn(|j| forward[j] - a[j] * dot(forward, a))
                }
            }
        };
        let mut drag = Self {
            token,
            before: object.clone(),
            dirty_before: editor.dirty,
            handle: hit.handle,
            tool: editor.tool,
            settings: editor.transform_settings,
            basis,
            eye,
            right,
            up,
            forward,
            normal,
            grab: [0.; 3],
            size,
            start: point,
            tangent: hit.tangent,
            ring_basis: hit.basis,
            plane_rotation: dot(normal, forward).abs() > 0.15,
            previous_phase: 0.,
            angle: 0.,
            previous_rotation: object.rotation,
            vertex_source: source.map_or(object.position, |v| v.world),
            vertex_targets: None,
            projection: Projection::new(editor, size),
            vertex_markers: None,
        };
        if vertex_enabled {
            drag.prepare_vertices(editor);
        }
        if drag.tool == Tool::Move {
            drag.grab = drag.intersection(point)?;
        }
        if drag.tool == Tool::Rotate && drag.plane_rotation {
            drag.previous_phase = drag.phase(point)?;
        }
        Some(drag)
    }
    fn geometry_mode(&self, snap_toggle: bool, held: bool) -> Option<MoveSnap> {
        if self.tool != Tool::Move {
            return None;
        }
        if held {
            return Some(MoveSnap::Vertex);
        }
        let mode = self.settings.move_snap;
        (matches!(mode, MoveSnap::Vertex | MoveSnap::Edge | MoveSnap::Face)
            && (self.settings.snap ^ snap_toggle))
            .then_some(mode)
    }
    pub fn vertex_enabled(&self, snap_toggle: bool, held: bool) -> bool {
        self.geometry_mode(snap_toggle, held).is_some()
    }
    pub fn prepare_vertices(&mut self, editor: &Editor) {
        if self.vertex_targets.is_none() {
            self.vertex_targets = Some(Arc::new(VertexIndex::new(editor, self.size, false)));
        }
    }
    fn intersection(&self, point: [f32; 2]) -> Option<[f32; 3]> {
        let x = (point[0] - self.size[0] * 0.5) / (0.9 * self.size[1]);
        let y = (self.size[1] * 0.5 - point[1]) / (0.9 * self.size[1]);
        let ray = std::array::from_fn(|i| self.forward[i] + self.right[i] * x + self.up[i] * y);
        let divisor = dot(ray, self.normal);
        if divisor.abs() < 0.00001 {
            return None;
        }
        let distance = dot(sub(self.before.position, self.eye), self.normal) / divisor;
        if !distance.is_finite() || distance <= 0. {
            return None;
        }
        Some(std::array::from_fn(|i| self.eye[i] + distance * ray[i]))
    }
    fn phase(&self, point: [f32; 2]) -> Option<f32> {
        let v = sub(self.intersection(point)?, self.before.position);
        let x = dot(v, self.ring_basis[0]);
        let y = dot(v, self.ring_basis[1]);
        if x.hypot(y) < 0.00001 {
            return None;
        }
        Some(y.atan2(x))
    }
    pub fn object_at(
        &mut self,
        x: f32,
        y: f32,
        snap_toggle: bool,
        vertex_snap: bool,
    ) -> Result<Object, String> {
        valid_pointer(x, y, self.size)?;
        let snapping = self.settings.snap ^ snap_toggle;
        let delta = [x - self.start[0], y - self.start[1]];
        let mut object = self.before.clone();
        self.vertex_markers = None;
        match self.tool {
            Tool::Move => {
                let mode = self.geometry_mode(snap_toggle, vertex_snap);
                let vertex = mode.is_some();
                let target = vertex
                    .then(|| {
                        self.vertex_targets
                            .as_ref()
                            .and_then(|index| index.nearest_mode([x, y], mode.unwrap()))
                    })
                    .flatten();
                // A vertex supplies an exact 3D target; it does not need the cursor ray
                // to intersect the drag plane (which may be behind the eye at this pixel).
                let delta = if target.is_some() {
                    [0.; 3]
                } else {
                    sub(
                        self.intersection([x, y])
                            .ok_or("Transform plane is parallel to the pointer ray")?,
                        self.grab,
                    )
                };
                let grid = !vertex && snapping && self.settings.move_snap == MoveSnap::Grid;
                let relative = !vertex && snapping && !grid;
                let mut displacement = match self.handle {
                    Handle::Axis(i) => {
                        let distance =
                            snap(dot(delta, self.basis[i]), self.settings.move_step, relative);
                        self.basis[i].map(|v| v * distance)
                    }
                    Handle::Plane(normal) => {
                        let a = self.basis[(normal + 1) % 3];
                        let b = self.basis[(normal + 2) % 3];
                        let x = snap(dot(delta, a), self.settings.move_step, relative);
                        let y = snap(dot(delta, b), self.settings.move_step, relative);
                        std::array::from_fn(|i| a[i] * x + b[i] * y)
                    }
                    Handle::Screen => {
                        // Relative snap stays in the camera plane. Grid snap below instead
                        // quantizes all world coordinates, so its center handle may change depth.
                        let x = snap(dot(delta, self.right), self.settings.move_step, relative);
                        let y = snap(dot(delta, self.up), self.settings.move_step, relative);
                        std::array::from_fn(|i| self.right[i] * x + self.up[i] * y)
                    }
                };
                if let Some(target) = target {
                    let delta = sub(target.world, self.vertex_source);
                    displacement = match self.handle {
                        Handle::Screen => delta,
                        Handle::Axis(i) => self.basis[i].map(|v| v * dot(delta, self.basis[i])),
                        Handle::Plane(i) => std::array::from_fn(|j| {
                            delta[j] - self.basis[i][j] * dot(delta, self.basis[i])
                        }),
                    };
                }
                if grid {
                    let step = self.settings.move_step;
                    object.position = std::array::from_fn(|i| {
                        let affected = match self.handle {
                            Handle::Axis(axis) => i == axis,
                            Handle::Plane(normal) => i != normal,
                            Handle::Screen => true,
                        };
                        if affected {
                            // Snap absolute coordinates to a grid through world origin. Clamp
                            // grid indices, rather than coordinates, so endpoints stay on-grid.
                            ((self.before.position[i] + displacement[i]) / step)
                                .round()
                                .clamp((-10000. / step).ceil(), (10000. / step).floor())
                                * step
                        } else {
                            self.before.position[i]
                        }
                    });
                    object.validate()?;
                    return Ok(object);
                }
                // Clamp the distance as a whole, preserving the chosen axis/plane at document limits.
                let mut fraction = 1f32;
                for (i, d) in displacement.iter().enumerate() {
                    if *d > 0. {
                        fraction = fraction.min((10000. - object.position[i]) / d);
                    }
                    if *d < 0. {
                        fraction = fraction.min((-10000. - object.position[i]) / d);
                    }
                }
                object.position = std::array::from_fn(|i| {
                    (object.position[i] + displacement[i] * fraction).clamp(-10000., 10000.)
                });
                if vertex {
                    let source = std::array::from_fn(|i| {
                        self.vertex_source[i] + object.position[i] - self.before.position[i]
                    });
                    self.vertex_markers = self
                        .projection
                        .project(source)
                        .map(|(p, _)| (p, target.map(|v| v.screen)));
                }
            }

            Tool::Scale => {
                let amount = if self.handle == Handle::Screen {
                    (delta[0] - delta[1]) / 100.
                } else {
                    (delta[0] * self.tangent[0] + delta[1] * self.tangent[1]) / 80.
                };
                let affected = |i| match self.handle {
                    Handle::Screen => true,
                    Handle::Axis(axis) => i == axis,
                    Handle::Plane(normal) => i != normal,
                };
                let mut min_factor: f32 = (-20f32).exp();
                let mut max_factor = 20f32.exp();
                for i in 0..3 {
                    // World scaling limits are evaluated after affine decomposition. A local
                    // channel at its limit may be perpendicular to the affected world axes.
                    if affected(i)
                        && (self.settings.scale_space == Space::Local
                            || self.handle == Handle::Screen)
                    {
                        min_factor = min_factor.max(0.001 / self.before.scale[i].abs());
                        max_factor = max_factor.min(10000. / self.before.scale[i].abs());
                    }
                }
                let raw_factor = amount.clamp(-20., 20.).exp();
                let factor = (1. + snap(raw_factor - 1., self.settings.scale_step, snapping))
                    .clamp(min_factor, max_factor);
                if self.settings.scale_space == Space::World && self.handle != Handle::Screen {
                    object = match self.handle {
                        Handle::Axis(axis) => world_scaled_bounded(&self.before, axis, factor),
                        Handle::Plane(_) => world_scaled_axes_bounded(
                            &self.before,
                            std::array::from_fn(|i| if affected(i) { factor } else { 1. }),
                        ),
                        Handle::Screen => unreachable!(),
                    };
                } else {
                    for i in 0..3 {
                        if affected(i) {
                            // Retain reflections; never cross zero. Clamp roundoff at the document limits.
                            object.scale[i] = self.before.scale[i].signum()
                                * (self.before.scale[i].abs() * factor).clamp(0.001, 10000.);
                        }
                    }
                }
            }
            Tool::Rotate => {
                let mut angle = self.angle;
                let mut phase = self.previous_phase;
                if self.plane_rotation {
                    // Near the pivot/vanishing line keep the previous valid angle rather than jumping.
                    if let Some(next) = self.phase([x, y]) {
                        angle += (next - phase + PI).rem_euclid(TAU) - PI;
                        phase = next;
                    }
                } else {
                    // Edge-on rings have no stable ray/plane intersection: use their grabbed screen tangent.
                    angle = (delta[0] * self.tangent[0] + delta[1] * self.tangent[1]) / 72.;
                }
                let applied_angle =
                    snap(angle.to_degrees(), self.settings.rotate_step, snapping).to_radians();
                if applied_angle.abs() > 0.000001 {
                    let (s, c) = applied_angle.sin_cos();
                    let basis = rotation_basis(self.before.rotation).map(|v| {
                        let crossed = cross(self.normal, v);
                        std::array::from_fn(|i| {
                            v[i] * c
                                + crossed[i] * s
                                + self.normal[i] * dot(self.normal, v) * (1. - c)
                        })
                    });
                    object.rotation =
                        euler_near(basis, self.previous_rotation).map(|v| v.clamp(-10000., 10000.));
                }
                self.angle = angle;
                self.previous_phase = phase;
                self.previous_rotation = object.rotation;
            }
            Tool::Select => unreachable!(),
        }
        object.validate()?;
        Ok(object)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Command;
    const SIZE: [f32; 2] = [800., 600.];
    fn begin(editor: &mut Editor, point: [f32; 2], token: &str) {
        editor
            .execute(Command::TransformBegin {
                vertex_snap: false,
                token: token.into(),
                x: point[0],
                y: point[1],
                width: SIZE[0],
                height: SIZE[1],
            })
            .unwrap();
    }
    fn update(editor: &mut Editor, point: [f32; 2], token: &str) {
        editor
            .execute(Command::TransformUpdate {
                vertex_snap: false,
                snap_toggle: false,
                token: token.into(),
                points: vec![point],
            })
            .unwrap();
    }
    fn end(editor: &mut Editor, token: &str, cancel: bool) {
        editor
            .execute(Command::TransformEnd {
                token: token.into(),
                cancel,
            })
            .unwrap();
    }
    #[test]
    fn each_world_axis_moves_only_that_axis_and_commits_one_history_entry() {
        for axis in 0..3 {
            let mut e = Editor::new();
            e.tool = Tool::Move;
            let h = handles(&e, SIZE).unwrap();
            let tip = h.tips[axis].unwrap();
            let start = std::array::from_fn(|i| h.origin[i] + (tip[i] - h.origin[i]) * 0.8);
            let original = e.document.clone();
            let unchanged_allocation = e.document.objects.as_ptr();
            let revision = e.render_revision;
            begin(&mut e, start, "axis");
            assert_eq!(
                e.transform_drag.as_ref().unwrap().handle,
                Handle::Axis(axis)
            );
            for step in 1..=20 {
                update(
                    &mut e,
                    std::array::from_fn(|i| start[i] + (tip[i] - h.origin[i]) * step as f32 / 20.),
                    "axis",
                );
            }
            assert!(e.undo.is_empty());
            assert!(e.render_revision > revision);
            assert!(e
                .pending
                .iter()
                .all(|patch| patch.objects.len() == 1 && patch.remove == 1));
            assert_eq!(unchanged_allocation, e.document.objects.as_ptr());
            let moved = e.document.clone();
            for i in 0..3 {
                if i == axis {
                    assert!(moved.objects[0].position[i] > 0.1);
                } else {
                    assert_eq!(moved.objects[0].position[i], 0.);
                }
            }
            end(&mut e, "axis", false);
            assert_eq!(e.undo.len(), 1);
            e.execute(Command::Undo).unwrap();
            assert_eq!(e.document, original);
            e.execute(Command::Redo).unwrap();
            assert_eq!(e.document, moved);
        }
    }
    #[test]
    fn screen_drag_cancel_and_noop_preserve_redo_dirty_and_sync() {
        let mut e = Editor::new();
        e.execute(Command::Add {
            kind: "sphere".into(),
        })
        .unwrap();
        e.execute(Command::Undo).unwrap();
        e.dirty = false;
        e.tool = Tool::Move;
        let original = e.document.clone();
        e.pending.clear();
        let mut replica = original.clone();
        let origin = handles(&e, SIZE).unwrap().origin;
        begin(&mut e, origin, "screen");
        update(&mut e, [origin[0] + 40., origin[1] - 20.], "screen");
        assert_eq!(e.transform_drag.as_ref().unwrap().handle, Handle::Screen);
        assert!(e.dirty);
        end(&mut e, "screen", true);
        for patch in &e.pending {
            replica.objects.splice(
                patch.start..patch.start + patch.remove,
                patch.objects.clone(),
            );
        }
        assert_eq!(replica, original);
        assert_eq!(e.document, original);
        assert!(!e.dirty);
        assert_eq!(e.redo.len(), 1);
        begin(&mut e, origin, "noop");
        end(&mut e, "noop", false);
        assert_eq!(e.redo.len(), 1);
        assert!(e.undo.is_empty());
        assert!(!e.dirty);
    }
    #[test]
    fn stale_events_and_conflicting_edits_do_not_corrupt_active_drag() {
        let mut e = Editor::new();
        e.tool = Tool::Move;
        let original = e.document.clone();
        let origin = handles(&e, SIZE).unwrap().origin;
        begin(&mut e, origin, "current");
        update(&mut e, [origin[0] + 40., origin[1]], "old");
        end(&mut e, "old", true);
        assert_eq!(e.document, original);
        assert!(e.transform_drag.is_some());
        assert!(e.ensure_not_dragging().is_err());
        for command in [
            Command::Delete,
            Command::Undo,
            Command::CameraReset,
            Command::Select { id: 1 },
        ] {
            assert!(e.execute(command).is_err());
        }
        assert!(e
            .execute(Command::TransformUpdate {
                vertex_snap: false,
                snap_toggle: false,
                token: "current".into(),
                points: vec![[f32::NAN, 0.]]
            })
            .is_err());
        update(&mut e, [origin[0] + 20., origin[1]], "current");
        e.execute(Command::SetTool { tool: Tool::Select }).unwrap();
        assert!(e.transform_drag.is_none());
        assert_eq!(e.document, original);
        assert!(!e.dirty);
        update(&mut e, [origin[0] + 80., origin[1]], "current");
        assert_eq!(e.document, original);
    }
    #[test]
    fn projection_is_pixel_sized_and_hides_occluded_axis_or_unavailable_selection() {
        let mut e = Editor::new();
        e.tool = Tool::Move;
        e.yaw = 0.;
        e.pitch = 0.;
        let h = handles(&e, SIZE).unwrap();
        assert_eq!(h.origin, [400., 300.]);
        assert!(h.tips[2].is_none()); // Pointing at the camera: no unstable depth-axis drag.
        assert!((h.tips[0].unwrap()[0] - 488.).abs() < 0.01);
        e.distance = 70.;
        let far = handles(&e, SIZE).unwrap();
        assert_eq!(h.origin, far.origin);
        assert!((far.tips[0].unwrap()[0] - 488.).abs() < 0.01);
        e.document.objects[0].visible = false;
        assert!(handles(&e, SIZE).is_none());
        e.document.objects[0].visible = true;
        e.document.objects[0].position = [0., 0., 80.];
        assert!(handles(&e, SIZE).is_none());
        assert!(valid_pointer(0., 0., [0., 600.]).is_err());
    }
    fn matrix_close(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) {
        for i in 0..3 {
            for j in 0..3 {
                assert!((a[i][j] - b[i][j]).abs() < 0.001, "{a:?} != {b:?}");
            }
        }
    }
    fn ring_start(h: &Handles, handle: Handle) -> usize {
        let ring = h.rings.iter().find(|r| r.handle == handle).unwrap();
        (1..64)
            .find(|&i| {
                ring.points[i].is_some_and(|p| h.hit(p).is_some_and(|hit| hit.handle == handle))
            })
            .unwrap()
    }
    #[test]
    fn local_rotation_rings_rotate_about_the_object_axes_and_roundtrip_history() {
        for axis in 0..3 {
            let mut e = Editor::new();
            e.tool = Tool::Rotate;
            e.document.objects[0].rotation = [20., 30., 40.];
            let original = e.document.clone();
            let basis = rotation_basis(original.objects[0].rotation);
            let normal = basis[axis];
            e.yaw = normal[0].atan2(normal[2]);
            e.pitch = normal[1].asin();
            let h = handles(&e, SIZE).unwrap();
            let start = ring_start(&h, Handle::Axis(axis));
            begin(&mut e, h.rings[axis].points[start].unwrap(), "rotate");
            assert!(e.transform_drag.as_ref().unwrap().plane_rotation);
            for step in 1..=16 {
                update(
                    &mut e,
                    h.rings[axis].points[(start + step) % 64].unwrap(),
                    "rotate",
                );
            }
            let changed = e.document.clone();
            let expected = basis.map(|v| {
                let c = cross(normal, v);
                std::array::from_fn(|i| c[i] + normal[i] * dot(normal, v))
            });
            matrix_close(rotation_basis(changed.objects[0].rotation), expected);
            assert_eq!(changed.objects[0].position, original.objects[0].position);
            assert_eq!(changed.objects[0].scale, original.objects[0].scale);
            assert!(e.undo.is_empty());
            // Ring orientation stays at its starting orientation for the duration of a drag.
            assert_eq!(
                handles(&e, SIZE).unwrap().rings[axis].points,
                h.rings[axis].points
            );
            end(&mut e, "rotate", false);
            assert_eq!(e.undo.len(), 1);
            e.execute(Command::Undo).unwrap();
            assert_eq!(e.document, original);
            e.execute(Command::Redo).unwrap();
            assert_eq!(e.document, changed);
        }
    }
    #[test]
    fn batched_rotation_keeps_multiple_turns_and_rejects_invalid_samples_atomically() {
        let mut e = Editor::new();
        e.tool = Tool::Rotate;
        e.yaw = 0.;
        e.pitch = 0.;
        let h = handles(&e, SIZE).unwrap();
        let ring = &h.rings[3];
        begin(&mut e, ring.points[0].unwrap(), "turns");
        let original = e.document.clone();
        let phase = e.transform_drag.as_ref().unwrap().previous_phase;
        for points in [
            vec![ring.points[8].unwrap(), [f32::NAN, 0.]],
            vec![],
            vec![[0., 0.]; 257],
        ] {
            assert!(e
                .execute(Command::TransformUpdate {
                    vertex_snap: false,
                    snap_toggle: false,
                    token: "turns".into(),
                    points
                })
                .is_err());
            assert_eq!(e.document, original);
            assert_eq!(e.transform_drag.as_ref().unwrap().previous_phase, phase);
            assert_eq!(e.transform_drag.as_ref().unwrap().angle, 0.);
        }
        e.pending.clear();
        let revision = e.render_revision;
        e.execute(Command::TransformUpdate {
            vertex_snap: false,
            snap_toggle: false,
            token: "turns".into(),
            points: (1..=128).map(|i| ring.points[i % 64].unwrap()).collect(),
        })
        .unwrap();
        assert!((e.document.objects[0].rotation[2] - 720.).abs() < 0.01);
        assert_eq!(e.render_revision, revision + 1);
        assert_eq!(e.pending.len(), 1);
        end(&mut e, "turns", true);
        assert_eq!(e.document, original);
        assert!(!e.dirty);
        assert!(e.undo.is_empty());
    }
    #[test]
    fn euler_conversion_preserves_orientation_at_poles_and_wrap_boundaries() {
        for rotation in [
            [179., 89.99, -179.],
            [25., 90., 50.],
            [25., -90., 50.],
            [360., 270., -720.],
            [-200., 160., 400.],
        ] {
            let basis = rotation_basis(rotation);
            matrix_close(rotation_basis(euler_near(basis, rotation)), basis);
        }
        let result = euler_near(rotation_basis([0., 0., -179.]), [0., 0., 179.]);
        assert!((result[2] - 181.).abs() < 0.001);
    }
    #[test]
    fn edge_on_rotation_uses_a_finite_screen_tangent() {
        let mut e = Editor::new();
        e.tool = Tool::Rotate;
        e.yaw = 0.;
        e.pitch = 0.;
        let h = handles(&e, SIZE).unwrap();
        let start = ring_start(&h, Handle::Axis(0));
        let p = h.rings[0].points[start].unwrap();
        begin(&mut e, p, "edge");
        let drag = e.transform_drag.as_ref().unwrap();
        assert!(!drag.plane_rotation);
        let target = std::array::from_fn(|i| p[i] + drag.tangent[i] * 72.);
        update(&mut e, target, "edge");
        assert!((e.document.objects[0].rotation[0] - 1f32.to_degrees()).abs() < 0.01);
        e.document.objects[0].validate().unwrap();
    }
    #[test]
    fn local_scale_axes_preserve_reflections_and_commit_one_history_entry() {
        for axis in 0..3 {
            let mut e = Editor::new();
            e.tool = Tool::Scale;
            e.document.objects[0].rotation = [10., 20., 30.];
            e.document.objects[0].scale = [-2., 3., -4.];
            let original = e.document.clone();
            let h = handles(&e, SIZE).unwrap();
            let start = h.tips[axis].unwrap();
            begin(&mut e, start, "scale");
            assert_eq!(
                e.transform_drag.as_ref().unwrap().handle,
                Handle::Axis(axis)
            );
            let tangent = e.transform_drag.as_ref().unwrap().tangent;
            for step in 1..=20 {
                update(
                    &mut e,
                    std::array::from_fn(|i| start[i] + tangent[i] * step as f32 * 4.),
                    "scale",
                );
            }
            let changed = e.document.clone();
            for i in 0..3 {
                let expected =
                    original.objects[0].scale[i] * if i == axis { 1f32.exp() } else { 1. };
                assert!((changed.objects[0].scale[i] - expected).abs() < 0.001);
            }
            assert_eq!(changed.objects[0].rotation, original.objects[0].rotation);
            assert_eq!(changed.objects[0].position, original.objects[0].position);
            assert!(e.undo.is_empty());
            end(&mut e, "scale", false);
            assert_eq!(e.undo.len(), 1);
            e.execute(Command::Undo).unwrap();
            assert_eq!(e.document, original);
            e.execute(Command::Redo).unwrap();
            assert_eq!(e.document, changed);
        }
    }
    #[test]
    fn uniform_scale_preserves_ratios_and_limits_and_cancel_preserves_redo() {
        for tool in [Tool::Rotate, Tool::Scale] {
            let mut e = Editor::new();
            e.document.objects[0].scale = [-2., 3., -4.];
            e.execute(Command::Add {
                kind: "sphere".into(),
            })
            .unwrap();
            e.execute(Command::Undo).unwrap();
            e.dirty = false;
            e.tool = tool;
            let original = e.document.clone();
            let h = handles(&e, SIZE).unwrap();
            let start = if tool == Tool::Scale {
                h.origin
            } else {
                h.rings[3].points[0].unwrap()
            };
            begin(&mut e, start, "cancel");
            for distance in [40., 90000., -90000.] {
                update(&mut e, [start[0] + distance, start[1] - 40.], "cancel");
                e.document.objects[0].validate().unwrap();
                if tool == Tool::Scale {
                    let scale = e.document.objects[0].scale;
                    assert!((scale[0] / scale[2] - 0.5).abs() < 0.0001);
                    assert!((scale[1] / scale[2] + 0.75).abs() < 0.0001);
                    assert!(scale[0] < 0. && scale[1] > 0. && scale[2] < 0.);
                }
            }
            assert!(e.dirty);
            end(&mut e, "cancel", true);
            assert_eq!(e.document, original);
            assert!(!e.dirty);
            assert_eq!(e.redo.len(), 1);
            assert!(e.undo.is_empty());
            begin(&mut e, start, "noop");
            end(&mut e, "noop", false);
            assert_eq!(e.redo.len(), 1);
            assert!(!e.dirty);
            // A tool switch also cancels a live preview; delayed pointer-up cannot commit it.
            begin(&mut e, start, "switch");
            update(&mut e, [start[0] + 30., start[1] - 30.], "switch");
            e.execute(Command::SetTool { tool: Tool::Select }).unwrap();
            end(&mut e, "switch", false);
            assert_eq!(e.document, original);
            assert_eq!(e.redo.len(), 1);
        }
    }
    #[test]
    fn local_move_snaps_distance_along_rotated_axis_and_screen_plane() {
        let mut e = Editor::new();
        e.tool = Tool::Move;
        e.document.objects[0].rotation = [0., 0., 35.];
        e.document.objects[0].position = [0.23, -0.17, 0.];
        e.transform_settings.move_space = Space::Local;
        e.transform_settings.snap = true;
        e.transform_settings.move_step = 0.5;
        let before = e.document.objects[0].clone();
        let h = handles(&e, SIZE).unwrap();
        let start = h.tips[0].unwrap();
        begin(&mut e, start, "local");
        update(&mut e, [start[0] + 63., start[1] - 35.], "local");
        let displacement = sub(e.document.objects[0].position, before.position);
        let basis = rotation_basis(before.rotation);
        let distance = dot(displacement, basis[0]);
        assert!(distance.abs() > 0.1);
        assert!((distance / 0.5 - (distance / 0.5).round()).abs() < 0.0001);
        assert!(dot(displacement, basis[1]).abs() < 0.0001);
        assert!(dot(displacement, basis[2]).abs() < 0.0001);
        end(&mut e, "local", true);
        assert_eq!(e.document.objects[0], before);
        begin(&mut e, h.origin, "screen");
        update(&mut e, [h.origin[0] + 47., h.origin[1] - 38.], "screen");
        let displacement = sub(e.document.objects[0].position, before.position);
        let (_, right, up, forward) = camera_basis(&e);
        for axis in [right, up] {
            let units = dot(displacement, axis) / 0.5;
            assert!((units - units.round()).abs() < 0.0001);
        }
        assert!(dot(displacement, forward).abs() < 0.0001);
    }
    #[test]
    fn world_rotation_snaps_world_axis_and_temporary_toggle_does_not_lose_raw_angle() {
        let mut e = Editor::new();
        e.tool = Tool::Rotate;
        e.yaw = 0.;
        e.pitch = 0.;
        e.document.objects[0].rotation = [20., 30., 40.];
        e.transform_settings.rotate_space = Space::World;
        e.transform_settings.snap = true;
        let before = e.document.clone();
        let h = handles(&e, SIZE).unwrap();
        let start = ring_start(&h, Handle::Axis(2));
        begin(&mut e, h.rings[2].points[start].unwrap(), "snap");
        let points = (1..=4)
            .map(|i| h.rings[2].points[(start + i) % 64].unwrap())
            .collect::<Vec<_>>();
        let last = *points.last().unwrap();
        e.execute(Command::TransformUpdate {
            vertex_snap: false,
            token: "snap".into(),
            points,
            snap_toggle: false,
        })
        .unwrap();
        let snapped = e.document.objects[0].clone();
        // Four ring segments = 22.5 degrees; snapping chooses a 15 degree increment.
        let angle = e.transform_drag.as_ref().unwrap().angle;
        let expected = snap(angle.to_degrees(), 15., true);
        let mut target = before.objects[0].rotation;
        target[2] += expected;
        matrix_close(rotation_basis(snapped.rotation), rotation_basis(target));
        e.execute(Command::TransformUpdate {
            vertex_snap: false,
            token: "snap".into(),
            points: vec![last],
            snap_toggle: true,
        })
        .unwrap();
        target[2] = before.objects[0].rotation[2] + 22.5;
        matrix_close(
            rotation_basis(e.document.objects[0].rotation),
            rotation_basis(target),
        );
        update(&mut e, last, "snap");
        assert_eq!(e.document.objects[0], snapped);
        assert!(e.undo.is_empty());
        end(&mut e, "snap", false);
        assert_eq!(e.undo.len(), 1);
        e.execute(Command::Undo).unwrap();
        assert_eq!(e.document, before);
        e.execute(Command::Redo).unwrap();
        assert_eq!(e.document.objects[0], snapped);
    }
    #[test]
    fn world_scale_snap_retains_shear_in_history_and_cancel_preserves_redo() {
        let mut e = Editor::new();
        e.tool = Tool::Scale;
        e.transform_settings.scale_space = Space::World;
        e.transform_settings.snap = true;
        e.document.objects[0].rotation = [25., 35., 45.];
        e.document.objects[0].scale = [-2., 3., 4.];
        let original = e.document.clone();
        let h = handles(&e, SIZE).unwrap();
        let start = h.tips[0].unwrap();
        begin(&mut e, start, "world");
        let tangent = e.transform_drag.as_ref().unwrap().tangent;
        update(
            &mut e,
            std::array::from_fn(|i| start[i] + tangent[i] * 80. * 1.43f32.ln()),
            "world",
        );
        let result = e.document.clone();
        assert!(result.objects[0].shear.iter().any(|v| v.abs() > 0.01));
        let expected = crate::affine::linear_basis(&original.objects[0]).map(|mut c| {
            c[0] *= 1.4;
            c
        });
        matrix_close(crate::affine::linear_basis(&result.objects[0]), expected);
        end(&mut e, "world", false);
        assert_eq!(e.undo.len(), 1);
        e.execute(Command::Undo).unwrap();
        assert_eq!(e.document, original);
        e.execute(Command::Redo).unwrap();
        assert_eq!(e.document, result);
        e.execute(Command::Undo).unwrap();
        e.dirty = false;
        begin(&mut e, start, "cancel");
        update(&mut e, [start[0] + 30., start[1]], "cancel");
        end(&mut e, "cancel", true);
        assert_eq!(e.document, original);
        assert!(!e.dirty);
        assert_eq!(e.redo.len(), 1);
    }
    #[test]
    fn settings_are_shared_without_history_and_invalid_changes_are_atomic() {
        let mut e = Editor::new();
        e.tool = Tool::Move;
        let before = e.document.clone();
        let point = handles(&e, SIZE).unwrap().origin;
        begin(&mut e, point, "settings");
        update(&mut e, [point[0] + 30., point[1]], "settings");
        let preview = e.document.clone();
        for step in [0., -1., f32::NAN, f32::INFINITY, 10001.] {
            assert!(e
                .execute(Command::SetSnapStep {
                    tool: Tool::Move,
                    step
                })
                .is_err());
            assert_eq!(e.document, preview);
            assert!(e.transform_drag.is_some());
        }
        e.execute(Command::SetSpace {
            space: Space::Local,
        })
        .unwrap();
        assert_eq!(e.document, before);
        assert!(e.transform_drag.is_none());
        assert!(!e.dirty);
        update(&mut e, [point[0] + 50., point[1]], "settings");
        end(&mut e, "settings", false);
        assert_eq!(e.document, before);
        e.execute(Command::SetSnap { enabled: true }).unwrap();
        e.execute(Command::SetSnapStep {
            tool: Tool::Rotate,
            step: 30.,
        })
        .unwrap();
        assert_eq!(e.transform_settings.move_space, Space::Local);
        assert_eq!(e.transform_settings.rotate_space, Space::Local);
        assert_eq!(e.transform_settings.rotate_step, 30.);
        assert!(e.undo.is_empty());
        assert!(!e.dirty);
    }
    fn plane_point(h: &Handles, normal: usize) -> [f32; 2] {
        let plane = h
            .planes
            .iter()
            .find(|p| p.normal == normal)
            .expect("visible plane");
        let [a, b, c, d] = plane.corners;
        for x in [0.5, 0.25, 0.75] {
            for y in [0.5, 0.25, 0.75] {
                let p = std::array::from_fn(|i| {
                    a[i] * (1. - x) * (1. - y)
                        + b[i] * x * (1. - y)
                        + c[i] * x * y
                        + d[i] * (1. - x) * y
                });
                if h.hit(p)
                    .is_some_and(|hit| hit.handle == Handle::Plane(normal))
                {
                    return p;
                }
            }
        }
        panic!("plane {normal} has no reachable interior");
    }
    #[test]
    fn world_planes_lock_normal_and_roundtrip_history_cancel_and_autosave_revision() {
        for normal in 0..3 {
            let mut e = Editor::new();
            e.tool = Tool::Move;
            e.document.objects[0].position = [0.2, -0.3, 0.4];
            e.camera_target = e.document.objects[0].position;
            let original = e.document.clone();
            let stamp = e.stamp();
            let start = plane_point(&handles(&e, SIZE).unwrap(), normal);
            begin(&mut e, start, "plane");
            assert_eq!(
                e.transform_drag.as_ref().unwrap().handle,
                Handle::Plane(normal)
            );
            update(&mut e, [start[0] + 43., start[1] - 31.], "plane");
            let result = e.document.clone();
            assert_eq!(
                result.objects[0].position[normal],
                original.objects[0].position[normal]
            );
            assert!((0..3)
                .filter(|&i| i != normal)
                .any(
                    |i| (result.objects[0].position[i] - original.objects[0].position[i]).abs()
                        > 0.05
                ));
            assert_eq!(e.stamp(), stamp);
            assert!(e.undo.is_empty());
            end(&mut e, "plane", false);
            assert_eq!(e.undo.len(), 1);
            assert_ne!(e.stamp(), stamp);
            e.undo();
            assert_eq!(e.document, original);
            e.redo();
            assert_eq!(e.document, result);
            e.undo();
            e.dirty = false;
            let stamp = e.stamp();
            begin(&mut e, start, "cancel-plane");
            update(&mut e, [start[0] + 28., start[1] + 17.], "cancel-plane");
            end(&mut e, "cancel-plane", true);
            assert_eq!(e.document, original);
            assert_eq!(e.stamp(), stamp);
            assert!(!e.dirty);
            assert_eq!(e.redo.len(), 1);
        }
    }
    #[test]
    fn local_planes_follow_rotation_and_snap_two_components_without_normal_drift() {
        for normal in 0..3 {
            let mut e = Editor::new();
            e.tool = Tool::Move;
            let o = &mut e.document.objects[0];
            o.rotation = [18., 33., 12.];
            o.scale = [-2., 0.4, 3.];
            o.shear = [0.2, -0.1, 0.3];
            let before = o.clone();
            let basis = rotation_basis(o.rotation);
            let eye: [f32; 3] = std::array::from_fn(|i| basis[0][i] + basis[1][i] + basis[2][i]);
            e.yaw = eye[0].atan2(eye[2]);
            e.pitch = (eye[1] / dot(eye, eye).sqrt()).asin();
            e.transform_settings.move_space = Space::Local;
            e.transform_settings.snap = true;
            e.transform_settings.move_step = 0.25;
            let start = plane_point(&handles(&e, SIZE).unwrap(), normal);
            begin(&mut e, start, "local-plane");
            update(&mut e, [start[0] + 63., start[1] - 49.], "local-plane");
            let delta = sub(e.document.objects[0].position, before.position);
            assert!(dot(delta, basis[normal]).abs() < 0.00001);
            for i in [(normal + 1) % 3, (normal + 2) % 3] {
                let units = dot(delta, basis[i]) / 0.25;
                assert!((units - units.round()).abs() < 0.00001);
            }
            assert_eq!(e.document.objects[0].scale, before.scale);
            assert_eq!(e.document.objects[0].shear, before.shear);
            end(&mut e, "local-plane", true);
            assert_eq!(e.document.objects[0], before);
        }
    }
    #[test]
    fn grid_snap_quantizes_world_positions_and_preserves_constrained_coordinates() {
        for handle in [
            Handle::Axis(0),
            Handle::Axis(1),
            Handle::Axis(2),
            Handle::Plane(0),
            Handle::Plane(1),
            Handle::Plane(2),
            Handle::Screen,
        ] {
            let mut e = Editor::new();
            e.tool = Tool::Move;
            e.document.objects[0].position = [0.37, -0.43, 0.29];
            e.camera_target = e.document.objects[0].position;
            e.transform_settings.move_snap = MoveSnap::Grid;
            e.transform_settings.snap = true;
            e.transform_settings.move_step = 0.5;
            let before = e.document.clone();
            let h = handles(&e, SIZE).unwrap();
            let start = match handle {
                Handle::Axis(i) => std::array::from_fn(|j| {
                    h.origin[j] + (h.tips[i].unwrap()[j] - h.origin[j]) * 0.8
                }),
                Handle::Plane(i) => plane_point(&h, i),
                Handle::Screen => h.origin,
            };
            begin(&mut e, start, "grid");
            assert_eq!(e.transform_drag.as_ref().unwrap().handle, handle);
            let point = [start[0] + 59., start[1] - 36.];
            update(&mut e, point, "grid");
            let snapped = e.document.clone();
            for i in 0..3 {
                let affected = match handle {
                    Handle::Axis(a) => i == a,
                    Handle::Plane(n) => i != n,
                    Handle::Screen => true,
                };
                if affected {
                    let v = snapped.objects[0].position[i] / 0.5;
                    assert!((v - v.round()).abs() < 0.00001);
                } else {
                    assert_eq!(
                        snapped.objects[0].position[i],
                        before.objects[0].position[i]
                    );
                }
            }
            e.execute(Command::TransformUpdate {
                vertex_snap: false,
                token: "grid".into(),
                points: vec![point],
                snap_toggle: true,
            })
            .unwrap();
            assert_ne!(e.document, snapped);
            update(&mut e, point, "grid");
            assert_eq!(e.document, snapped); // no accumulated snap drift
            end(&mut e, "grid", false);
            assert_eq!(e.undo.len(), 1);
            e.undo();
            assert_eq!(e.document, before);
            e.redo();
            assert_eq!(e.document, snapped);
        }
    }
    #[test]
    fn grid_limits_remain_on_grid_and_invalid_setting_does_not_cancel_preview() {
        let mut e = Editor::new();
        e.tool = Tool::Move;
        e.transform_settings.move_snap = MoveSnap::Grid;
        e.transform_settings.snap = true;
        for step in [3., 6000.] {
            e.transform_settings.move_step = step;
            e.document.objects[0].position = [9999.2, 0.3, 0.4];
            e.camera_target = e.document.objects[0].position;
            let h = handles(&e, SIZE).unwrap();
            let tip = h.tips[0].unwrap();
            let start = std::array::from_fn(|i| h.origin[i] + (tip[i] - h.origin[i]) * 0.8);
            begin(&mut e, start, "limit");
            update(&mut e, [start[0] + 10000., start[1]], "limit");
            let x = e.document.objects[0].position[0];
            assert!(x.abs() <= 10000.);
            assert!((x / step - (x / step).round()).abs() < 0.00001);
            end(&mut e, "limit", true);
        }
        e.document.objects[0].position = [0.; 3];
        e.camera_target = [0.; 3];
        e.execute(Command::SetSpace {
            space: Space::Local,
        })
        .unwrap();
        assert_eq!(e.transform_settings.move_snap, MoveSnap::Relative);
        let start = handles(&e, SIZE).unwrap().origin;
        begin(&mut e, start, "invalid");
        update(&mut e, [start[0] + 33., start[1]], "invalid");
        let preview = e.document.clone();
        assert!(e
            .execute(Command::SetMoveSnap {
                mode: MoveSnap::Grid
            })
            .is_err());
        assert_eq!(e.document, preview);
        assert!(e.transform_drag.is_some());
        e.execute(Command::SetMoveSnap {
            mode: MoveSnap::Relative,
        })
        .unwrap();
        assert!(e.transform_drag.is_none());
        assert!(e.undo.is_empty());
        assert!(!e.dirty);
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old.as_object_mut().unwrap().remove("move_snap");
        assert_eq!(
            serde_json::from_value::<Settings>(old).unwrap().move_snap,
            MoveSnap::Relative
        );
    }
    #[test]
    fn plane_handles_are_pixel_sized_and_hide_edge_on_faces() {
        let mut e = Editor::new();
        e.tool = Tool::Move;
        e.yaw = 0.;
        e.pitch = 0.;
        let h = handles(&e, SIZE).unwrap();
        assert_eq!(h.planes.len(), 1);
        assert_eq!(h.planes[0].normal, 2);
        let original = h.planes[0]
            .corners
            .map(|p| [p[0] - h.origin[0], p[1] - h.origin[1]]);
        for (distance, size) in [(70., SIZE), (7., [1400., 900.]), (700., [375., 600.])] {
            e.distance = distance;
            let h = handles(&e, size).unwrap();
            let offset = h.planes[0]
                .corners
                .map(|p| [p[0] - h.origin[0], p[1] - h.origin[1]]);
            for (p, q) in original.iter().zip(offset) {
                assert!((p[0] - q[0]).abs() < 0.001 && (p[1] - q[1]).abs() < 0.001);
            }
            assert_eq!(h.hit(h.origin).unwrap().handle, Handle::Screen);
        }
        e.tool = Tool::Scale;
        assert!(!handles(&e, SIZE).unwrap().planes.is_empty());
        e.tool = Tool::Rotate;
        assert!(handles(&e, SIZE).unwrap().planes.is_empty());
    }
    fn vertex_scene() -> Editor {
        let mut e = Editor::new();
        e.tool = Tool::Move;
        let mut target = e.document.objects[0].clone();
        target.id = 2;
        target.position = [2., 0.8, -0.3];
        target.rotation = [17., 29., -11.];
        target.scale = [-0.7, 1.2, 0.8];
        target.shear = [0.3, -0.2, 0.1];
        e.document.objects.push(target);
        e.invalidate_document();
        e
    }
    fn vertex_update(e: &mut Editor, point: [f32; 2], held: bool) {
        e.execute(Command::TransformUpdate {
            token: "vertex".into(),
            points: vec![point],
            snap_toggle: false,
            vertex_snap: held,
        })
        .unwrap();
    }
    fn assert_close(a: [f32; 3], b: [f32; 3]) {
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < 0.0001, "{a:?} != {b:?}");
        }
    }
    #[test]
    fn vertex_to_vertex_drag_snaps_in_3d_and_shares_index_across_atomic_updates() {
        let mut e = vertex_scene();
        e.transform_settings.snap = true;
        e.transform_settings.move_snap = MoveSnap::Grid;
        let original = e.document.clone();
        let stamp = e.stamp();
        let source = VertexIndex::new(&e, SIZE, true)
            .nearest(
                Projection::new(&e, SIZE)
                    .project([0.5, 0.5, 0.5])
                    .unwrap()
                    .0,
            )
            .unwrap();
        let target_object = &e.document.objects[1];
        let target_world = crate::renderer::world_point(
            crate::renderer::model_matrix(target_object, false),
            [0.5, 0.5, 0.5],
        );
        let target = VertexIndex::new(&e, SIZE, false)
            .nearest(Projection::new(&e, SIZE).project(target_world).unwrap().0)
            .unwrap();
        e.execute(Command::TransformBegin {
            token: "vertex".into(),
            x: source.screen[0],
            y: source.screen[1],
            width: SIZE[0],
            height: SIZE[1],
            vertex_snap: true,
        })
        .unwrap();
        let index = e
            .transform_drag
            .as_ref()
            .unwrap()
            .vertex_targets
            .clone()
            .unwrap();
        vertex_update(&mut e, target.screen, true);
        let snapped = e.document.clone();
        assert_close(
            std::array::from_fn(|i| source.world[i] + snapped.objects[0].position[i]),
            target.world,
        );
        assert_eq!(snapped.objects[1], original.objects[1]);
        assert_eq!(e.stamp(), stamp);
        assert!(e
            .transform_drag
            .as_ref()
            .unwrap()
            .vertex_markers
            .unwrap()
            .1
            .is_some());
        assert!(Arc::ptr_eq(
            &index,
            e.transform_drag
                .as_ref()
                .unwrap()
                .vertex_targets
                .as_ref()
                .unwrap()
        ));
        // A rejected batch cannot leak its first valid sample or its marker state.
        let markers = e.transform_drag.as_ref().unwrap().vertex_markers;
        assert!(e
            .execute(Command::TransformUpdate {
                token: "vertex".into(),
                points: vec![[10., 20.], [f32::NAN, 0.]],
                snap_toggle: false,
                vertex_snap: true
            })
            .is_err());
        assert_eq!(e.document, snapped);
        assert_eq!(e.transform_drag.as_ref().unwrap().vertex_markers, markers);
        vertex_update(&mut e, target.screen, false); // release V: restore raw grid behavior
        assert_ne!(e.document, snapped);
        vertex_update(&mut e, target.screen, true);
        assert_eq!(e.document, snapped);
        end(&mut e, "vertex", false);
        assert_eq!(e.undo.len(), 1);
        e.undo();
        assert_eq!(e.document, original);
        e.redo();
        assert_eq!(e.document, snapped);
        e.undo();
        e.dirty = false;
        let start = handles(&e, SIZE).unwrap().origin;
        begin(&mut e, start, "vertex");
        vertex_update(&mut e, target.screen, true);
        end(&mut e, "vertex", true);
        assert_eq!(e.document, original);
        assert!(!e.dirty);
        assert_eq!(e.redo.len(), 1);
    }
    #[test]
    fn vertex_snap_preserves_world_and_local_axis_plane_constraints_and_releases_outside_radius() {
        for space in [Space::World, Space::Local] {
            for handle in [
                Handle::Axis(0),
                Handle::Axis(1),
                Handle::Axis(2),
                Handle::Plane(0),
                Handle::Plane(1),
                Handle::Plane(2),
                Handle::Screen,
            ] {
                let mut e = vertex_scene();
                e.document.objects[0].rotation = [12., 19., 8.];
                e.transform_settings.move_space = space;
                let h = handles(&e, SIZE).unwrap();
                let start = match handle {
                    Handle::Axis(i) => std::array::from_fn(|j| {
                        h.origin[j] + (h.tips[i].unwrap()[j] - h.origin[j]) * 0.85
                    }),
                    Handle::Plane(i) => plane_point(&h, i),
                    Handle::Screen => h.origin,
                };
                begin(&mut e, start, "vertex");
                assert_eq!(e.transform_drag.as_ref().unwrap().handle, handle);
                let index = VertexIndex::new(&e, SIZE, false);
                let world = e.document.objects[1].position;
                // Find a target near the projected cube corner.
                let point = Projection::new(&e, SIZE)
                    .project(crate::renderer::world_point(
                        crate::renderer::model_matrix(&e.document.objects[1], false),
                        [0.5, 0.5, 0.5],
                    ))
                    .unwrap()
                    .0;
                let target = index.nearest(point).unwrap();
                vertex_update(&mut e, target.screen, true);
                let basis = if space == Space::World {
                    [axis(0), axis(1), axis(2)]
                } else {
                    rotation_basis([12., 19., 8.])
                };
                let expected = match handle {
                    Handle::Axis(i) => basis[i].map(|v| v * dot(target.world, basis[i])),
                    Handle::Plane(i) => std::array::from_fn(|j| {
                        target.world[j] - basis[i][j] * dot(target.world, basis[i])
                    }),
                    Handle::Screen => target.world,
                };
                assert_close(e.document.objects[0].position, expected);
                vertex_update(&mut e, [start[0] + 15., start[1] - 15.], true);
                assert!(e
                    .transform_drag
                    .as_ref()
                    .unwrap()
                    .vertex_markers
                    .unwrap()
                    .1
                    .is_none());
                assert_eq!(e.document.objects[1].position, world);
                end(&mut e, "vertex", true);
            }
        }
    }
    #[test]
    fn vertex_mode_settings_local_switch_and_legacy_modifier_defaults() {
        let mut e = vertex_scene();
        e.execute(Command::SetMoveSnap {
            mode: MoveSnap::Vertex,
        })
        .unwrap();
        e.execute(Command::SetSpace {
            space: Space::Local,
        })
        .unwrap();
        assert_eq!(e.transform_settings.move_snap, MoveSnap::Vertex);
        assert!(e.undo.is_empty());
        assert!(!e.dirty);
        let old: Command = serde_json::from_str(
            r#"{"type":"transform_begin","token":"old","x":400,"y":300,"width":800,"height":600}"#,
        )
        .unwrap();
        e.execute(old).unwrap();
        end(&mut e, "old", true);
        e.transform_settings.snap = true;
        let start = handles(&e, SIZE).unwrap().origin;
        begin(&mut e, start, "vertex");
        vertex_update(&mut e, [500., 200.], false);
        assert!(e.transform_drag.as_ref().unwrap().vertex_targets.is_some());
        e.execute(Command::SetMoveSnap {
            mode: MoveSnap::Relative,
        })
        .unwrap();
        assert!(e.transform_drag.is_none());
        assert_eq!(e.document.objects[0].position, [0.; 3]);
    }
    #[test]
    fn plane_scale_changes_two_local_channels_and_two_world_matrix_rows_with_history_and_cancel() {
        for space in [Space::World, Space::Local] {
            for normal in 0..3 {
                let mut e = Editor::new();
                e.tool = Tool::Scale;
                e.transform_settings.scale_space = space;
                e.transform_settings.snap = true;
                e.transform_settings.scale_step = 0.1;
                let o = &mut e.document.objects[0];
                o.rotation = [18., 33., 12.];
                o.scale = [-2., 0.4, 3.];
                o.shear = [0.2, -0.1, 0.3];
                // Keep all planes reachable in either coordinate system.
                let basis = if space == Space::World {
                    [axis(0), axis(1), axis(2)]
                } else {
                    rotation_basis(o.rotation)
                };
                let eye: [f32; 3] =
                    std::array::from_fn(|i| basis[0][i] + basis[1][i] + basis[2][i]);
                e.yaw = eye[0].atan2(eye[2]);
                e.pitch = (eye[1] / dot(eye, eye).sqrt()).asin();
                let original = e.document.clone();
                let stamp = e.stamp();
                let h = handles(&e, SIZE).unwrap();
                let start = plane_point(&h, normal);
                let tangent = h.hit(start).unwrap().tangent;
                begin(&mut e, start, "plane-scale");
                update(
                    &mut e,
                    std::array::from_fn(|i| start[i] + tangent[i] * 50.),
                    "plane-scale",
                );
                let result = e.document.clone();
                assert_eq!(result.objects[0].position, original.objects[0].position);
                assert_eq!(result.objects[0].scale.map(f32::signum), [-1., 1., 1.]);
                let before = crate::affine::linear_basis(&original.objects[0]);
                let after = crate::affine::linear_basis(&result.objects[0]);
                let factor = 1.9; // exp(50/80), snapped to 10% increments
                if space == Space::World {
                    for i in 0..3 {
                        for j in 0..3 {
                            assert!(
                                (after[i][j]
                                    - before[i][j] * if j == normal { 1. } else { factor })
                                .abs()
                                    < 0.0001
                            );
                        }
                    }
                } else {
                    for i in 0..3 {
                        assert!(
                            (result.objects[0].scale[i]
                                - original.objects[0].scale[i]
                                    * if i == normal { 1. } else { factor })
                            .abs()
                                < 0.0001
                        );
                    }
                    assert_eq!(result.objects[0].rotation, original.objects[0].rotation);
                    assert_eq!(result.objects[0].shear, original.objects[0].shear);
                }
                assert_eq!(e.stamp(), stamp);
                end(&mut e, "plane-scale", false);
                assert_eq!(e.undo.len(), 1);
                e.undo();
                assert_eq!(e.document, original);
                e.redo();
                assert_eq!(e.document, result);
                e.undo();
                e.dirty = false;
                begin(&mut e, start, "plane-scale");
                update(
                    &mut e,
                    std::array::from_fn(|i| start[i] - tangent[i] * 50.),
                    "plane-scale",
                );
                end(&mut e, "plane-scale", true);
                assert_eq!(e.document, original);
                assert!(!e.dirty);
                assert_eq!(e.redo.len(), 1);
            }
        }
    }
    #[test]
    fn vertex_mode_preserves_plane_hits_near_but_not_on_source_vertices() {
        let mut e = Editor::new();
        e.tool = Tool::Move;
        e.transform_settings.move_snap = MoveSnap::Vertex;
        e.transform_settings.snap = true;
        let h = handles(&e, SIZE).unwrap();
        for normal in 0..3 {
            let plane = h.planes.iter().find(|p| p.normal == normal).unwrap();
            let mut found = false;
            for x in [0.2, 0.4, 0.6, 0.8] {
                for y in [0.2, 0.4, 0.6, 0.8] {
                    let [a, b, c, d] = plane.corners;
                    let p = std::array::from_fn(|i| {
                        a[i] * (1. - x) * (1. - y)
                            + b[i] * x * (1. - y)
                            + c[i] * x * y
                            + d[i] * (1. - x) * y
                    });
                    if !h
                        .hit(p)
                        .is_some_and(|hit| hit.handle == Handle::Plane(normal))
                    {
                        continue;
                    }
                    if VertexIndex::new(&e, SIZE, true)
                        .nearest(p)
                        .is_some_and(|v| (v.screen[0] - p[0]).hypot(v.screen[1] - p[1]) <= 6.)
                    {
                        continue;
                    }
                    let drag = TransformDrag::begin(&e, "plane".into(), p, SIZE, false).unwrap();
                    assert_eq!(drag.handle, Handle::Plane(normal));
                    found = true;
                    break;
                }
                if found {
                    break;
                }
            }
            assert!(found);
        }
    }
    #[test]
    fn world_plane_scale_does_not_clamp_an_unaffected_rotated_channel() {
        let mut e = Editor::new();
        e.tool = Tool::Scale;
        e.transform_settings.scale_space = Space::World;
        e.document.objects[0].rotation = [0., 0., 90.];
        e.document.objects[0].scale = [1., 10000., 1.];
        let h = handles(&e, SIZE).unwrap();
        let start = plane_point(&h, 0);
        let t = h.hit(start).unwrap().tangent;
        begin(&mut e, start, "world-limit");
        update(
            &mut e,
            std::array::from_fn(|i| start[i] + t[i] * 40.),
            "world-limit",
        );
        let result = &e.document.objects[0];
        result.validate().unwrap();
        assert!((result.scale[0] - 0.5f32.exp()).abs() < 0.0001);
        assert!((result.scale[1] - 10000.).abs() < 0.01);
        assert!((result.scale[2] - 0.5f32.exp()).abs() < 0.0001);
        end(&mut e, "world-limit", true);
    }
    #[test]
    fn edge_and_face_modes_obey_constraints_history_cancel_and_temporary_vertex_override() {
        for mode in [MoveSnap::Edge, MoveSnap::Face] {
            for space in [Space::World, Space::Local] {
                for handle in [Handle::Screen, Handle::Axis(0), Handle::Plane(1)] {
                    let mut e = vertex_scene();
                    e.document.objects[0].rotation = [12., 19., 8.];
                    e.transform_settings.move_snap = mode;
                    e.transform_settings.snap = true;
                    e.transform_settings.move_space = space;
                    let original = e.document.clone();
                    let stamp = e.stamp();
                    let h = handles(&e, SIZE).unwrap();
                    let start = match handle {
                        Handle::Screen => h.origin,
                        Handle::Axis(i) => std::array::from_fn(|j| {
                            h.origin[j] + (h.tips[i].unwrap()[j] - h.origin[j]) * 0.85
                        }),
                        Handle::Plane(i) => plane_point(&h, i),
                    };
                    let target = crate::renderer::world_point(
                        crate::renderer::model_matrix(&e.document.objects[1], false),
                        [0.5, 0.5, 0.],
                    );
                    let point = Projection::new(&e, SIZE).project(target).unwrap().0;
                    let candidate = VertexIndex::new(&e, SIZE, false)
                        .nearest_mode(point, mode)
                        .unwrap();
                    begin(&mut e, start, "surface");
                    assert_eq!(e.transform_drag.as_ref().unwrap().handle, handle);
                    update(&mut e, point, "surface");
                    let basis = if space == Space::World {
                        [axis(0), axis(1), axis(2)]
                    } else {
                        rotation_basis([12., 19., 8.])
                    };
                    let expected = match handle {
                        Handle::Screen => candidate.world,
                        Handle::Axis(i) => basis[i].map(|v| v * dot(candidate.world, basis[i])),
                        Handle::Plane(i) => std::array::from_fn(|j| {
                            candidate.world[j] - basis[i][j] * dot(candidate.world, basis[i])
                        }),
                    };
                    assert_close(e.document.objects[0].position, expected);
                    assert_eq!(e.stamp(), stamp);
                    let snapped = e.document.clone();
                    e.execute(Command::TransformUpdate {
                        token: "surface".into(),
                        points: vec![point],
                        snap_toggle: true,
                        vertex_snap: false,
                    })
                    .unwrap();
                    assert_ne!(e.document, snapped);
                    update(&mut e, point, "surface");
                    assert_eq!(e.document, snapped);
                    assert_eq!(
                        e.transform_drag
                            .as_ref()
                            .unwrap()
                            .geometry_mode(false, true),
                        Some(MoveSnap::Vertex)
                    );
                    let mut vertex_editor = vertex_scene();
                    vertex_editor.document.objects[0].rotation = [12., 19., 8.];
                    vertex_editor.transform_settings.move_space = space;
                    vertex_editor.transform_settings.move_snap = MoveSnap::Vertex;
                    vertex_editor.transform_settings.snap = true;
                    begin(&mut vertex_editor, start, "override");
                    update(&mut vertex_editor, point, "override");
                    e.execute(Command::TransformUpdate {
                        token: "surface".into(),
                        points: vec![point],
                        snap_toggle: false,
                        vertex_snap: true,
                    })
                    .unwrap();
                    assert_eq!(e.document, vertex_editor.document);
                    update(&mut e, point, "surface");
                    assert_eq!(e.document, snapped);
                    end(&mut vertex_editor, "override", true);

                    end(&mut e, "surface", false);
                    assert_eq!(e.undo.len(), 1);
                    e.undo();
                    assert_eq!(e.document, original);
                    e.redo();
                    assert_eq!(e.document, snapped);
                    e.undo();
                    e.dirty = false;
                    begin(&mut e, start, "surface");
                    update(&mut e, point, "surface");
                    end(&mut e, "surface", true);
                    assert_eq!(e.document, original);
                    assert!(!e.dirty);
                    assert_eq!(e.redo.len(), 1);
                    // Late release after Escape must not commit the cancelled preview.
                    end(&mut e, "surface", false);
                    assert_eq!(e.document, original);
                    assert_eq!(e.redo.len(), 1);
                }
            }
        }
    }
}
