use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Object {
    pub id: u64,
    pub name: String,
    pub kind: String,
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
    pub visible: bool,
    pub mirror: bool,
}
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Document {
    pub version: u32,
    pub objects: Vec<Object>,
    pub selected: Option<u64>,
}
impl Default for Document {
    fn default() -> Self {
        Self {
            version: 1,
            objects: vec![Object {
                id: 1,
                name: "Cube".into(),
                kind: "cube".into(),
                position: [0.; 3],
                rotation: [0.; 3],
                scale: [1.; 3],
                visible: true,
                mirror: false,
            }],
            selected: Some(1),
        }
    }
}
impl Document {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.objects.len() > 10000 {
            return Err("Unsupported document version or too many objects".into());
        }
        let mut ids = std::collections::HashSet::new();
        for o in &self.objects {
            if o.id == 0
                || o.id > 9_007_199_254_740_991
                || !ids.insert(o.id)
                || !["cube", "sphere", "plane"].contains(&o.kind.as_str())
                || o.name.len() > 256
                || o.position
                    .iter()
                    .chain(&o.rotation)
                    .chain(&o.scale)
                    .any(|v| !v.is_finite() || v.abs() > 10000.)
                || o.scale.iter().any(|v| v.abs() < 0.001)
            {
                return Err("Invalid object data".into());
            }
        }
        if self.selected.is_some_and(|id| !ids.contains(&id)) {
            return Err("Invalid selection".into());
        }
        Ok(())
    }
}
#[derive(Default)]
pub struct Editor {
    pub document: Document,
    pub undo: Vec<Document>,
    pub redo: Vec<Document>,
    pub dirty: bool,
    pub path: Option<std::path::PathBuf>,
    pub floating: Vec<String>,
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub frame: u32,
}
impl Editor {
    pub fn new() -> Self {
        Self {
            yaw: 0.7,
            pitch: 0.45,
            distance: 7.,
            frame: 1,
            ..Default::default()
        }
    }
    pub fn commit(&mut self, next: Document) -> Result<(), String> {
        next.validate()?;
        if next != self.document {
            self.undo.push(self.document.clone());
            if self.undo.len() > 100 {
                self.undo.remove(0);
            }
            self.redo.clear();
            self.document = next;
            self.dirty = true;
        }
        Ok(())
    }
    pub fn undo(&mut self) {
        if let Some(d) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.document, d));
            self.dirty = true;
        }
    }
    pub fn redo(&mut self) {
        if let Some(d) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.document, d));
            self.dirty = true;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_roundtrip() {
        let mut e = Editor::new();
        let initial = e.document.clone();
        let mut d = initial.clone();
        d.objects[0].position[0] = 3.;
        e.commit(d.clone()).unwrap();
        e.undo();
        assert_eq!(e.document, initial);
        e.redo();
        assert_eq!(e.document, d);
    }
    #[test]
    fn reject_invalid_import() {
        let mut d = Document::default();
        d.objects[0].scale[0] = 0.;
        assert!(d.validate().is_err());
        d.objects[0].scale[0] = 1.;
        d.objects.push(d.objects[0].clone());
        assert!(d.validate().is_err());
    }
    #[test]
    fn edit_after_undo_discards_redo() {
        let mut e = Editor::new();
        let mut d = e.document.clone();
        d.objects[0].mirror = true;
        e.commit(d).unwrap();
        e.undo();
        let mut d = e.document.clone();
        d.objects[0].name = "Renamed".into();
        e.commit(d).unwrap();
        assert!(e.redo.is_empty());
    }
}
