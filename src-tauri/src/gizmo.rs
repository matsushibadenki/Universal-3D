//! Screen-sized transform handles. Document, geometry and gesture state remain in Rust.
use crate::affine::world_scaled_bounded;
use crate::document::{Editor, Object};
use crate::renderer::camera_basis;
use serde::{Deserialize, Serialize};
use std::f32::consts::{PI, TAU};

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
#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Debug)]
pub struct Settings {
    pub move_space: Space,
    pub rotate_space: Space,
    pub scale_space: Space,
    pub snap: bool,
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
            _ => self.move_space = space,
        }
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
}
pub struct Ring {
    pub handle: Handle,
    pub points: Vec<Option<[f32; 2]>>,
    basis: [[f32; 3]; 2],
}
pub struct Handles {
    pub origin: [f32; 2],
    pub tips: [Option<[f32; 2]>; 3],
    pub rings: Vec<Ring>,
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
        .filter(|d| d.tool == Tool::Rotate)
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
        tool: editor.tool,
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
        hit
    }
}
impl TransformDrag {
    pub fn begin(editor: &Editor, token: String, point: [f32; 2], size: [f32; 2]) -> Option<Self> {
        let hit = handles(editor, size)?.hit(point)?;
        let (_, object) = editor.indexed_object(editor.document.selected?)?;
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
        };
        if drag.tool == Tool::Move {
            drag.grab = drag.intersection(point)?;
        }
        if drag.tool == Tool::Rotate && drag.plane_rotation {
            drag.previous_phase = drag.phase(point)?;
        }
        Some(drag)
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
    pub fn object_at(&mut self, x: f32, y: f32, snap_toggle: bool) -> Result<Object, String> {
        valid_pointer(x, y, self.size)?;
        let snapping = self.settings.snap ^ snap_toggle;
        let delta = [x - self.start[0], y - self.start[1]];
        let mut object = self.before.clone();
        match self.tool {
            Tool::Move => {
                let point = self
                    .intersection([x, y])
                    .ok_or("Transform plane is parallel to the pointer ray")?;
                let delta = sub(point, self.grab);
                let displacement = if let Handle::Axis(i) = self.handle {
                    let distance =
                        snap(dot(delta, self.basis[i]), self.settings.move_step, snapping);
                    self.basis[i].map(|v| v * distance)
                } else {
                    // Free movement stays in the camera plane, including when snapped.
                    let x = snap(dot(delta, self.right), self.settings.move_step, snapping);
                    let y = snap(dot(delta, self.up), self.settings.move_step, snapping);
                    std::array::from_fn(|i| self.right[i] * x + self.up[i] * y)
                };
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
            }

            Tool::Scale => {
                let amount = if self.handle == Handle::Screen {
                    (delta[0] - delta[1]) / 100.
                } else {
                    (delta[0] * self.tangent[0] + delta[1] * self.tangent[1]) / 80.
                };
                let affected = |i| self.handle == Handle::Screen || self.handle == Handle::Axis(i);
                let mut min_factor: f32 = 0.;
                let mut max_factor = f32::INFINITY;
                for i in 0..3 {
                    if affected(i) {
                        min_factor = min_factor.max(0.001 / self.before.scale[i].abs());
                        max_factor = max_factor.min(10000. / self.before.scale[i].abs());
                    }
                }
                let raw_factor = amount.clamp(-20., 20.).exp();
                let factor = (1. + snap(raw_factor - 1., self.settings.scale_step, snapping))
                    .clamp(min_factor, max_factor);
                if self.settings.scale_space == Space::World && self.handle != Handle::Screen {
                    if let Handle::Axis(axis) = self.handle {
                        object = world_scaled_bounded(&self.before, axis, factor);
                    }
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
}
