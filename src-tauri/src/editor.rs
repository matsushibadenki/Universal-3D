use crate::document::{Editor, Object};
use serde::Deserialize;
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    SetTool {
        tool: crate::gizmo::Tool,
    },
    SetSpace {
        space: crate::gizmo::Space,
    },
    SetSnap {
        enabled: bool,
    },
    SetMoveSnap {
        mode: crate::gizmo::MoveSnap,
    },
    SetSnapStep {
        tool: crate::gizmo::Tool,
        step: f32,
    },
    TransformBegin {
        token: String,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        #[serde(default)]
        vertex_snap: bool,
    },
    TransformUpdate {
        token: String,
        points: Vec<[f32; 2]>,
        #[serde(default)]
        snap_toggle: bool,
        #[serde(default)]
        vertex_snap: bool,
    },
    TransformEnd {
        token: String,
        cancel: bool,
    },
    Navigate {
        mode: Navigation,
        dx: f32,
        dy: f32,
        height: f32,
    },
    CameraReset,
    FrameSelection {
        aspect: f32,
    },
    ViewportPick {
        x: f32,
        y: f32,
        aspect: f32,
    },
    SetFrame {
        frame: u32,
    },
    Add {
        kind: String,
    },
    Select {
        id: u64,
    },
    Update {
        object: Object,
    },
    Delete,
    Undo,
    Redo,
    Camera {
        yaw: f32,
        pitch: f32,
        distance: f32,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Navigation {
    Orbit,
    Pan,
    Dolly,
}

impl Command {
    pub fn needs_snap_preparation(&self, editor: &Editor) -> bool {
        match self {
            Self::TransformBegin { vertex_snap, .. } => {
                editor.tool == crate::gizmo::Tool::Move
                    && (*vertex_snap
                        || (editor.transform_settings.snap
                            && matches!(
                                editor.transform_settings.move_snap,
                                crate::gizmo::MoveSnap::Vertex
                                    | crate::gizmo::MoveSnap::Edge
                                    | crate::gizmo::MoveSnap::Face
                            )))
            }
            Self::TransformUpdate {
                token,
                snap_toggle,
                vertex_snap,
                ..
            } => editor.transform_drag.as_ref().is_some_and(|drag| {
                drag.token == *token
                    && !drag.has_vertex_targets()
                    && drag.vertex_enabled(*snap_toggle, *vertex_snap)
            }),
            _ => false,
        }
    }
    pub fn is_transform_gesture(&self) -> bool {
        matches!(
            self,
            Self::TransformBegin { .. } | Self::TransformUpdate { .. } | Self::TransformEnd { .. }
        )
    }
}
impl Editor {
    pub fn execute(&mut self, command: Command) -> Result<(), String> {
        if !matches!(
            command,
            Command::TransformUpdate { .. }
                | Command::TransformEnd { .. }
                | Command::SetTool { .. }
                | Command::SetSpace { .. }
                | Command::SetSnap { .. }
                | Command::SetMoveSnap { .. }
                | Command::SetSnapStep { .. }
        ) {
            self.ensure_not_dragging()?;
        }
        match command {
            Command::SetTool { tool } => {
                self.finish_transform(true);
                self.tool = tool;
            }
            Command::SetSpace { space } => {
                self.finish_transform(true);
                self.transform_settings.set_space(self.tool, space);
            }
            Command::SetSnap { enabled } => {
                self.finish_transform(true);
                self.transform_settings.snap = enabled;
            }
            Command::SetMoveSnap { mode } => {
                let mut settings = self.transform_settings;
                settings.set_move_snap(mode)?;
                self.finish_transform(true);
                self.transform_settings = settings;
            }
            Command::SetSnapStep { tool, step } => {
                let mut settings = self.transform_settings;
                settings.set_step(tool, step)?;
                self.finish_transform(true);
                self.transform_settings = settings;
            }
            Command::TransformBegin {
                token,
                x,
                y,
                width,
                height,
                vertex_snap,
            } => {
                crate::gizmo::valid_pointer(x, y, [width, height])?;
                if token.is_empty() || token.len() > 80 {
                    return Err("Invalid transform token".into());
                }
                self.transform_drag = crate::gizmo::TransformDrag::begin(
                    self,
                    token,
                    [x, y],
                    [width, height],
                    vertex_snap,
                );
                if self.transform_drag.is_none() {
                    let selected = crate::renderer::pick(
                        self,
                        x / width * 2. - 1.,
                        1. - y / height * 2.,
                        width / height,
                    )?;
                    self.select(selected);
                }
            }
            Command::TransformUpdate {
                token,
                points,
                snap_toggle,
                vertex_snap,
            } => {
                if let Some(mut drag) = self
                    .transform_drag
                    .as_ref()
                    .filter(|drag| drag.token == token)
                    .cloned()
                {
                    if points.is_empty() || points.len() > 256 {
                        return Err("Invalid transform sample count".into());
                    }
                    // Keep angular crossings, but publish only the final object. A rejected
                    // sample leaves both the document and the accumulated gesture unchanged.
                    if drag.vertex_enabled(snap_toggle, vertex_snap) {
                        drag.prepare_vertices(self);
                    }
                    let mut object = drag.before.clone();
                    for [x, y] in points {
                        object = drag.object_at(x, y, snap_toggle, vertex_snap)?;
                    }
                    self.preview_transform(object);
                    self.transform_drag = Some(drag);
                }
            }
            Command::TransformEnd { token, cancel } => {
                if self
                    .transform_drag
                    .as_ref()
                    .is_some_and(|drag| drag.token == token)
                {
                    self.finish_transform(cancel);
                }
            }
            Command::CameraReset => {
                self.yaw = 0.7;
                self.pitch = 0.45;
                self.distance = 7.;
                self.camera_target = [0.; 3];
            }
            Command::FrameSelection { aspect } => crate::renderer::frame_selection(self, aspect)?,
            Command::ViewportPick { x, y, aspect } => {
                let selected = crate::renderer::pick(self, x, y, aspect)?;
                self.select(selected);
            }
            Command::Navigate {
                mode,
                dx,
                dy,
                height,
            } => {
                if ![dx, dy, height].iter().all(|v| v.is_finite())
                    || height < 1.
                    || dx.abs() > 100000.
                    || dy.abs() > 100000.
                {
                    return Err("Invalid camera gesture".into());
                }
                match mode {
                    Navigation::Orbit => {
                        self.yaw = (self.yaw - dx * 0.008).rem_euclid(std::f32::consts::TAU);
                        self.pitch = (self.pitch + dy * 0.008).clamp(-1.4, 1.4);
                    }
                    Navigation::Dolly => {
                        self.distance = (self.distance * ((dx + dy) * 0.008).clamp(-8., 8.).exp())
                            .clamp(0.05, 50000.)
                    }
                    Navigation::Pan => {
                        let (_, right, up, _) = crate::renderer::camera_basis(self);
                        let units = 2. * self.distance / (1.8 * height);
                        for i in 0..3 {
                            self.camera_target[i] = (self.camera_target[i]
                                + units * (-dx * right[i] + dy * up[i]))
                                .clamp(-100000., 100000.);
                        }
                    }
                }
            }
            Command::Add { kind } => {
                if !["cube", "sphere", "plane"].contains(&kind.as_str()) {
                    return Err("Unknown primitive".into());
                }
                let id = self.next_id()?;
                let object = Object {
                    id,
                    name: format!("{} {}", kind, id),
                    mesh_id: None,
                    kind,
                    position: [0.; 3],
                    rotation: [0.; 3],
                    scale: [1.; 3],
                    shear: [0.; 3],
                    visible: true,
                    mirror: false,
                };
                self.edit_span(self.document.objects.len(), 0, vec![object], Some(id))?;
            }
            Command::Select { id } => {
                if self.object_index(id).is_none() {
                    return Err("Object does not exist".into());
                }
                self.select(Some(id));
            }
            Command::Update { object } => {
                let index = self
                    .object_index(object.id)
                    .ok_or("Object does not exist")?;
                self.edit_span(index, 1, vec![object], self.document.selected)?;
            }
            Command::Delete => {
                if let Some(id) = self.document.selected {
                    let index = self.object_index(id).ok_or("Object does not exist")?;
                    let selected = self
                        .document
                        .objects
                        .get(usize::from(index == 0))
                        .map(|o| o.id);
                    self.edit_span(index, 1, vec![], selected)?;
                }
            }
            Command::SetFrame { frame } => self.frame = frame.clamp(1, 120),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::Camera {
                yaw,
                pitch,
                distance,
            } => {
                if ![yaw, pitch, distance].iter().all(|v| v.is_finite()) {
                    return Err("Invalid camera".into());
                }
                self.yaw = yaw;
                self.pitch = pitch.clamp(-1.4, 1.4);
                self.distance = distance.clamp(0.05, 50000.);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_validation_and_history_keep_ids_consistent() {
        let mut e = Editor::new();
        e.execute(Command::Add {
            kind: "sphere".into(),
        })
        .unwrap();
        assert_eq!(e.object_index(2), Some(1));
        let revision = e.render_revision;
        let object = e.document.objects[0].clone();
        assert!(e.edit_span(1, 0, vec![object], Some(1)).is_err());
        assert!(e.edit_span(1, 1, vec![], Some(2)).is_err());
        assert_eq!(e.render_revision, revision);
        e.execute(Command::Delete).unwrap();
        assert_eq!(e.object_index(2), None);
        e.execute(Command::Undo).unwrap();
        assert_eq!(e.object_index(2), Some(1));
        e.execute(Command::Redo).unwrap();
        assert_eq!(e.object_index(2), None);
        e.execute(Command::Add {
            kind: "plane".into(),
        })
        .unwrap();
        assert_eq!(e.document.objects[1].id, 2);
        assert_eq!(e.object_index(2), Some(1));
    }
    #[test]
    fn local_edits_preserve_unmodified_allocations_and_emit_reversible_patches() {
        let mut e = Editor::new();
        e.execute(Command::Add {
            kind: "sphere".into(),
        })
        .unwrap();
        e.pending.clear();
        let mut replica = e.document.clone();
        let unchanged_name = e.document.objects[0].name.as_ptr();
        let allocation = e.document.objects.as_ptr();
        let mut object = e.document.objects[1].clone();
        object.position[0] = 5.;
        let commands = [
            Command::Update { object },
            Command::Delete,
            Command::Undo,
            Command::Undo,
            Command::Redo,
            Command::Add {
                kind: "plane".into(),
            },
        ];
        for command in commands {
            e.execute(command).unwrap();
            for patch in std::mem::take(&mut e.pending) {
                assert!(patch.objects.len() <= 1);
                replica
                    .objects
                    .splice(patch.start..patch.start + patch.remove, patch.objects);
            }
            replica.selected = e.document.selected;
            assert_eq!(replica, e.document);
            assert_eq!(unchanged_name, e.document.objects[0].name.as_ptr());
        }
        // All operations fit within the vector's pre-existing capacity.
        assert_eq!(allocation, e.document.objects.as_ptr());
        e.pending.clear();
        let mut invalid = e.document.objects[0].clone();
        invalid.scale[0] = 0.;
        assert!(e.execute(Command::Update { object: invalid }).is_err());
        assert!(e.pending.is_empty());
        let same = e.document.objects[0].clone();
        e.execute(Command::Update { object: same }).unwrap();
        assert!(e.pending.is_empty());
    }
    #[test]
    fn commands_edit_without_window_and_preserve_history() {
        let mut e = Editor::new();
        e.execute(serde_json::from_str(r#"{"type":"add","kind":"sphere"}"#).unwrap())
            .unwrap();
        let id = e.document.selected.unwrap();
        assert_eq!(e.document.objects.len(), 2);
        let mut o = e.document.objects[1].clone();
        o.position = [2., 3., 4.];
        e.execute(Command::Update { object: o }).unwrap();
        e.execute(Command::Delete).unwrap();
        assert_eq!(e.document.objects.len(), 1);
        e.execute(Command::Undo).unwrap();
        assert_eq!(e.document.selected, Some(id));
        assert_eq!(e.document.objects[1].position, [2., 3., 4.]);
        e.execute(Command::Redo).unwrap();
        assert_eq!(e.document.objects.len(), 1);
    }
    #[test]
    fn invalid_commands_are_atomic() {
        let mut e = Editor::new();
        let original = e.document.clone();
        for command in [
            Command::Add { kind: "bad".into() },
            Command::Select { id: 99 },
            Command::Camera {
                yaw: f32::NAN,
                pitch: 0.,
                distance: 7.,
            },
        ] {
            assert!(e.execute(command).is_err());
            assert_eq!(e.document, original);
            assert!(e.undo.is_empty());
            assert!(!e.dirty);
        }
        let mut o = e.document.objects[0].clone();
        o.scale[0] = 0.;
        assert!(e.execute(Command::Update { object: o }).is_err());
        assert_eq!(e.document, original);
        assert!(e.undo.is_empty());
        assert_eq!([e.yaw, e.pitch, e.distance], [0.7, 0.45, 7.]);
    }
    #[test]
    fn view_commands_do_not_create_document_history() {
        let mut e = Editor::new();
        let original = e.document.clone();
        let allocation = e.document.objects.as_ptr();
        e.execute(Command::SetFrame { frame: 500 }).unwrap();
        e.execute(Command::Camera {
            yaw: 1.,
            pitch: 4.,
            distance: 500.,
        })
        .unwrap();
        e.execute(Command::Select { id: 1 }).unwrap();
        assert_eq!(e.document, original);
        assert_eq!(allocation, e.document.objects.as_ptr());
        assert!(e.undo.is_empty());
        assert!(!e.dirty);
        assert_eq!(e.frame, 120);
        assert_eq!([e.yaw, e.pitch, e.distance], [1., 1.4, 500.]);
    }
}

#[cfg(test)]
mod navigation_tests {
    use super::*;
    #[test]
    fn gestures_are_relative_finite_and_outside_document_history() {
        let mut editor = Editor::new();
        let original = editor.document.clone();
        editor
            .execute(Command::Navigate {
                mode: Navigation::Pan,
                dx: 100.,
                dy: 20.,
                height: 600.,
            })
            .unwrap();
        assert_ne!(editor.camera_target, [0.; 3]);
        let target = editor.camera_target;
        let old_distance = editor.distance;
        editor
            .execute(Command::Navigate {
                mode: Navigation::Orbit,
                dx: 10.,
                dy: 12.,
                height: 600.,
            })
            .unwrap();
        assert_eq!(editor.camera_target, target);
        editor
            .execute(Command::Navigate {
                mode: Navigation::Dolly,
                dx: 0.,
                dy: -20.,
                height: 600.,
            })
            .unwrap();
        assert!(editor.distance < old_distance);
        let before = (
            editor.yaw,
            editor.pitch,
            editor.distance,
            editor.camera_target,
        );
        assert!(editor
            .execute(Command::Navigate {
                mode: Navigation::Pan,
                dx: f32::NAN,
                dy: 0.,
                height: 600.
            })
            .is_err());
        assert_eq!(
            (
                editor.yaw,
                editor.pitch,
                editor.distance,
                editor.camera_target
            ),
            before
        );
        editor.execute(Command::CameraReset).unwrap();
        assert_eq!(
            (
                editor.yaw,
                editor.pitch,
                editor.distance,
                editor.camera_target
            ),
            (0.7, 0.45, 7., [0.; 3])
        );
        assert_eq!(editor.document, original);
        assert!(editor.undo.is_empty());
        assert!(!editor.dirty);
    }
}
