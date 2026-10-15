use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, VecDeque};
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
            o.validate()?;
            if !ids.insert(o.id) {
                return Err("Duplicate object ID".into());
            }
        }
        if self.selected.is_some_and(|id| !ids.contains(&id)) {
            return Err("Invalid selection".into());
        }
        Ok(())
    }
}
impl Object {
    pub fn validate(&self) -> Result<(), String> {
        if self.id == 0
            || self.id > 9_007_199_254_740_991
            || !["cube", "sphere", "plane"].contains(&self.kind.as_str())
            || self.name.len() > 256
            || self
                .position
                .iter()
                .chain(&self.rotation)
                .chain(&self.scale)
                .any(|v| !v.is_finite() || v.abs() > 10000.)
            || self.scale.iter().any(|v| v.abs() < 0.001)
        {
            return Err("Invalid object data".into());
        }
        Ok(())
    }
}
#[derive(Clone, Serialize)]
pub struct DocumentPatch {
    pub start: usize,
    pub remove: usize,
    pub objects: Vec<Object>,
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
    #[cfg(test)]
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
#[derive(Clone)]
pub enum RenderChange {
    Reset,
    Objects(Vec<u64>),
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
    pub camera_target: [f32; 3],
    pub frame: u32,
    pub revision: u64,
    pub pending: Vec<DocumentPatch>,
    pub pending_reset: bool,
    pub render_revision: u64,
    pub render_changes: VecDeque<(u64, RenderChange)>,
    indices: HashMap<u64, usize>,
    indexed_len: usize,
    ordered_ids: BTreeSet<u64>,
}
impl Editor {
    pub fn new() -> Self {
        let mut editor = Self {
            yaw: 0.7,
            pitch: 0.45,
            distance: 7.,
            frame: 1,
            ..Default::default()
        };
        editor.reindex();
        editor
    }
    fn reindex(&mut self) {
        self.indices = self
            .document
            .objects
            .iter()
            .enumerate()
            .map(|(i, o)| (o.id, i))
            .collect();
        self.indexed_len = self.document.objects.len();
        self.ordered_ids = self.indices.keys().copied().collect();
    }
    pub fn object_index(&mut self, id: u64) -> Option<usize> {
        if self.indexed_len != self.document.objects.len() {
            self.reindex();
        }
        self.indices.get(&id).copied()
    }
    pub fn indexed_object(&self, id: u64) -> Option<(usize, &Object)> {
        self.indices
            .get(&id)
            .map(|&index| (index, &self.document.objects[index]))
    }
    pub fn next_id(&mut self) -> Result<u64, String> {
        if self.indexed_len != self.document.objects.len() {
            self.reindex();
        }
        self.ordered_ids
            .last()
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .filter(|id| *id <= 9_007_199_254_740_991)
            .ok_or("Object ID limit reached".into())
    }
    fn changed(&mut self, change: RenderChange) {
        self.render_revision += 1;
        self.render_changes
            .push_back((self.render_revision, change));
        if self.render_changes.len() > 128 {
            self.render_changes.pop_front();
        }
    }
    pub fn invalidate_document(&mut self) {
        self.reindex();
        self.changed(RenderChange::Reset);
    }
    pub fn select(&mut self, selected: Option<u64>) {
        if self.document.selected != selected {
            let ids = self.document.selected.into_iter().chain(selected).collect();
            self.document.selected = selected;
            self.changed(RenderChange::Objects(ids));
        }
    }
    fn apply_entry(&mut self, entry: &HistoryEntry, forward: bool) {
        let (before, after, selected) = if forward {
            (&entry.before, &entry.after, entry.after_selected)
        } else {
            (&entry.after, &entry.before, entry.before_selected)
        };
        let ids = before
            .iter()
            .chain(after)
            .map(|o| o.id)
            .chain(
                self.document
                    .selected
                    .filter(|_| self.document.selected != selected),
            )
            .chain(selected.filter(|_| self.document.selected != selected))
            .collect();
        let shifted = before.len() != after.len();
        for object in before {
            self.indices.remove(&object.id);
            self.ordered_ids.remove(&object.id);
        }
        entry.apply(&mut self.document, forward);
        // Vec ordering is retained: only positions at/after a length-changing splice shift.
        let end = if shifted {
            self.document.objects.len()
        } else {
            entry.start + after.len()
        };
        for index in entry.start..end {
            let id = self.document.objects[index].id;
            self.indices.insert(id, index);
        }
        for object in after {
            self.ordered_ids.insert(object.id);
        }
        self.indexed_len = self.document.objects.len();
        self.changed(RenderChange::Objects(ids));
    }
    // Validated local splice. Only changed objects are cloned for history/notification.
    pub fn edit_span(
        &mut self,
        start: usize,
        remove: usize,
        objects: Vec<Object>,
        selected: Option<u64>,
    ) -> Result<(), String> {
        if start > self.document.objects.len()
            || remove > self.document.objects.len() - start
            || self.document.objects.len() - remove + objects.len() > 10000
        {
            return Err("Invalid edit range".into());
        }
        for object in &objects {
            object.validate()?;
        }
        if self.indexed_len != self.document.objects.len() {
            self.reindex();
        }
        let ids: std::collections::HashSet<u64> = objects.iter().map(|o| o.id).collect();
        if ids.len() != objects.len()
            || objects.iter().any(|o| {
                self.indices
                    .get(&o.id)
                    .is_some_and(|i| !(start..start + remove).contains(i))
            })
        {
            return Err("Duplicate object ID".into());
        }
        if selected.is_some_and(|id| {
            !ids.contains(&id)
                && !self
                    .indices
                    .get(&id)
                    .is_some_and(|i| !(start..start + remove).contains(i))
        }) {
            return Err("Invalid selection".into());
        }
        if self.document.objects[start..start + remove] == objects
            && self.document.selected == selected
        {
            return Ok(());
        }
        let entry = HistoryEntry {
            start,
            before: self.document.objects[start..start + remove].to_vec(),
            after: objects,
            before_selected: self.document.selected,
            after_selected: selected,
        };
        self.apply_entry(&entry, true);
        self.pending.push(DocumentPatch {
            start,
            remove,
            objects: entry.after.clone(),
        });
        self.record(entry);
        Ok(())
    }
    fn record(&mut self, entry: HistoryEntry) {
        self.undo.push(entry);
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }
    #[cfg(test)]
    pub fn commit(&mut self, next: Document) -> Result<(), String> {
        next.validate()?;
        if next != self.document {
            let entry = HistoryEntry::between(&self.document, &next);
            self.pending.push(DocumentPatch {
                start: entry.start,
                remove: entry.before.len(),
                objects: entry.after.clone(),
            });
            self.document = next;
            self.invalidate_document();
            self.record(entry);
        }
        Ok(())
    }
    pub fn undo(&mut self) {
        if let Some(mut entry) = self.undo.pop() {
            // Selection is UI state and can change between history operations.
            entry.after_selected = self.document.selected;
            self.apply_entry(&entry, false);
            self.pending.push(DocumentPatch {
                start: entry.start,
                remove: entry.after.len(),
                objects: entry.before.clone(),
            });
            self.redo.push(entry);
            self.dirty = true;
        }
    }
    pub fn redo(&mut self) {
        if let Some(mut entry) = self.redo.pop() {
            entry.before_selected = self.document.selected;
            self.apply_entry(&entry, true);
            self.pending.push(DocumentPatch {
                start: entry.start,
                remove: entry.before.len(),
                objects: entry.after.clone(),
            });
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
