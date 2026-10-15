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
// A reversible splice preserves ordering while retaining only the changed span.
// Single-object edits keep two objects, independent of document size.
pub struct HistoryEntry {
    start: usize,
    before: Vec<Object>,
    after: Vec<Object>,
    before_selected: Option<u64>,
    after_selected: Option<u64>,
}
impl HistoryEntry {
    fn between(before: &Document, after: &Document) -> Self {
        let start = before
            .objects
            .iter()
            .zip(&after.objects)
            .take_while(|(a, b)| a == b)
            .count();
        let tail = before.objects[start..]
            .iter()
            .rev()
            .zip(after.objects[start..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        Self {
            start,
            before: before.objects[start..before.objects.len() - tail].to_vec(),
            after: after.objects[start..after.objects.len() - tail].to_vec(),
            before_selected: before.selected,
            after_selected: after.selected,
        }
    }
    fn apply(&self, document: &mut Document, forward: bool) {
        let (remove, insert, selected) = if forward {
            (&self.before, &self.after, self.after_selected)
        } else {
            (&self.after, &self.before, self.before_selected)
        };
        document.objects.splice(
            self.start..self.start + remove.len(),
            insert.iter().cloned(),
        );
        document.selected = selected;
    }
}
#[derive(Default)]
pub struct Editor {
    pub document: Document,
    pub undo: Vec<HistoryEntry>,
    pub redo: Vec<HistoryEntry>,
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
            self.undo.push(HistoryEntry::between(&self.document, &next));
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
        if let Some(mut entry) = self.undo.pop() {
            // Selection is UI state and can change between history operations.
            entry.after_selected = self.document.selected;
            entry.apply(&mut self.document, false);
            self.redo.push(entry);
            self.dirty = true;
        }
    }
    pub fn redo(&mut self) {
        if let Some(mut entry) = self.redo.pop() {
            entry.before_selected = self.document.selected;
            entry.apply(&mut self.document, true);
            self.undo.push(entry);
            self.dirty = true;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn splices_restore_insert_delete_reorder_and_selection() {
        let mut e = Editor::new();
        let mut states = vec![e.document.clone()];
        for id in 2..=5 {
            let mut next = e.document.clone();
            let mut object = next.objects[0].clone();
            object.id = id;
            next.objects.insert(0, object);
            next.selected = Some(id);
            e.commit(next).unwrap();
            states.push(e.document.clone());
        }
        let mut next = e.document.clone();
        next.objects.reverse();
        e.commit(next).unwrap();
        states.push(e.document.clone());
        let mut next = e.document.clone();
        next.objects.remove(2);
        e.commit(next).unwrap();
        states.push(e.document.clone());
        let mut next = e.document.clone();
        next.objects.clear();
        next.selected = None;
        e.commit(next).unwrap();
        states.push(e.document.clone());
        for expected in states.iter().rev().skip(1) {
            e.undo();
            assert_eq!(&e.document, expected);
            e.document.validate().unwrap();
        }
        for expected in states.iter().skip(1) {
            e.redo();
            assert_eq!(&e.document, expected);
            e.document.validate().unwrap();
        }
    }
    #[test]
    fn selection_between_undo_redo_matches_snapshot_behavior() {
        let mut e = Editor::new();
        let mut next = e.document.clone();
        let mut object = next.objects[0].clone();
        object.id = 2;
        next.objects.push(object);
        next.selected = Some(2);
        e.commit(next).unwrap();
        e.document.selected = Some(1);
        e.undo();
        assert_eq!(e.document.selected, Some(1));
        e.redo();
        assert_eq!(e.document.selected, Some(1));
        e.document.selected = Some(2);
        let mut next = e.document.clone();
        next.objects[0].name = "Edited".into();
        e.commit(next).unwrap();
        e.document.selected = Some(1);
        e.undo();
        assert_eq!(e.document.selected, Some(2));
        e.document.selected = None;
        e.redo();
        assert_eq!(e.document.selected, Some(1));
        e.undo();
        assert_eq!(e.document.selected, None);
    }
    #[test]
    fn no_op_invalid_edit_and_history_limit_preserve_contract() {
        let mut e = Editor::new();
        for step in 1..=105 {
            let mut next = e.document.clone();
            next.objects[0].position[0] = step as f32;
            e.commit(next).unwrap();
        }
        assert_eq!(e.undo.len(), 100);
        for _ in 0..100 {
            e.undo();
        }
        assert_eq!(e.document.objects[0].position[0], 5.);
        let count = e.redo.len();
        e.commit(e.document.clone()).unwrap();
        assert_eq!(e.redo.len(), count);
        let original = e.document.clone();
        let mut invalid = original.clone();
        invalid.objects[0].scale[0] = 0.;
        assert!(e.commit(invalid).is_err());
        assert_eq!(e.document, original);
        assert_eq!(e.redo.len(), count);
        e.redo();
        assert_eq!(e.document.objects[0].position[0], 6.);
    }
    #[test]
    fn large_document_history_retains_only_changed_objects() {
        let mut e = Editor::new();
        for id in 2..=10_000 {
            let mut o = e.document.objects[0].clone();
            o.id = id;
            e.document.objects.push(o);
        }
        for step in 1..=100 {
            let mut next = e.document.clone();
            next.objects[5000].position[0] = step as f32;
            e.commit(next).unwrap();
        }
        let retained: usize = e
            .undo
            .iter()
            .map(|entry| entry.before.len() + entry.after.len())
            .sum();
        assert_eq!(retained, 200);
        let delta_bytes: usize = e.undo.capacity() * std::mem::size_of::<HistoryEntry>()
            + e.undo
                .iter()
                .map(|entry| {
                    (entry.before.capacity() + entry.after.capacity())
                        * std::mem::size_of::<Object>()
                        + entry
                            .before
                            .iter()
                            .chain(&entry.after)
                            .map(|o| o.name.capacity() + o.kind.capacity())
                            .sum::<usize>()
                })
                .sum::<usize>();
        let old_object_bytes = 100 * 10_000 * std::mem::size_of::<Object>();
        assert!(delta_bytes * 100 < old_object_bytes);
        println!("History allocation estimate: delta={delta_bytes} bytes; previous object storage alone={old_object_bytes} bytes");
        for _ in 0..100 {
            e.undo();
        }
        assert_eq!(e.document.objects[5000].position[0], 0.);
        for _ in 0..100 {
            e.redo();
        }
        assert_eq!(e.document.objects[5000].position[0], 100.);
    }
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
