use crate::document::{Editor, Object};
use serde::Deserialize;
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    SetFrame { frame: u32 },
    Add { kind: String },
    Select { id: u64 },
    Update { object: Object },
    Delete,
    Undo,
    Redo,
    Camera { yaw: f32, pitch: f32, distance: f32 },
}

impl Editor {
    pub fn execute(&mut self, command: Command) -> Result<(), String> {
        match command {
            Command::Add { kind } => {
                let mut d = self.document.clone();
                if !["cube", "sphere", "plane"].contains(&kind.as_str()) {
                    return Err("Unknown primitive".into());
                }
                let id = d
                    .objects
                    .iter()
                    .map(|o| o.id)
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or("Object ID limit reached")?;
                d.objects.push(Object {
                    id,
                    name: format!("{} {}", kind, id),
                    kind,
                    position: [0.; 3],
                    rotation: [0.; 3],
                    scale: [1.; 3],
                    visible: true,
                    mirror: false,
                });
                d.selected = Some(id);
                self.commit(d)?;
            }
            Command::Select { id } => {
                if !self.document.objects.iter().any(|o| o.id == id) {
                    return Err("Object does not exist".into());
                }
                self.document.selected = Some(id);
            }
            Command::Update { object } => {
                let mut d = self.document.clone();
                let o = d
                    .objects
                    .iter_mut()
                    .find(|o| o.id == object.id)
                    .ok_or("Object does not exist")?;
                *o = object;
                self.commit(d)?;
            }
            Command::Delete => {
                let mut d = self.document.clone();
                if let Some(id) = d.selected {
                    d.objects.retain(|o| o.id != id);
                    d.selected = d.objects.first().map(|o| o.id);
                    self.commit(d)?;
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
                self.distance = distance.clamp(1.5, 60.);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!([e.yaw, e.pitch, e.distance], [1., 1.4, 60.]);
    }
}
