//! Versioned workspace preferences, independent of document content and history.
use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    version: u32,
    pub floating: Vec<String>,
}
impl Default for Workspace {
    fn default() -> Self {
        Self::new(vec![])
    }
}
impl Workspace {
    pub fn new(floating: Vec<String>) -> Self {
        Self {
            version: 1,
            floating,
        }
    }
    fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.floating.len() > 3 {
            return Err("Unsupported workspace".into());
        }
        let mut seen = std::collections::HashSet::new();
        if self
            .floating
            .iter()
            .any(|p| !["scene", "inspector", "timeline"].contains(&p.as_str()) || !seen.insert(p))
        {
            return Err("Invalid panel layout".into());
        }
        Ok(())
    }
    // Invalid/unsupported preferences recover to the default docked workspace.
    pub fn load(path: &Path) -> Self {
        let parsed = (|| {
            if std::fs::metadata(path).ok()?.len() > 4096 {
                return None;
            }
            let layout: Self = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
            layout.validate().ok()?;
            Some(layout)
        })();
        parsed.unwrap_or_else(|| Self::new(vec![]))
    }
    // Caller serializes saves; rename publishes a complete file, never partial JSON.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        use std::io::Write;
        self.validate()?;
        let parent = path.parent().ok_or("No workspace directory")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let temporary = path.with_extension("json.tmp");
        let result = (|| {
            let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
            file.write_all(&serde_json::to_vec(self).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            std::fs::rename(&temporary, path).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_roundtrip_and_invalid_layout_recovery() {
        let root =
            std::env::temp_dir().join(format!("universal3d-workspace-{}", std::process::id()));
        let path = root.join("layout.json");
        let _ = std::fs::remove_dir_all(&root);
        assert!(Workspace::load(&path).floating.is_empty());
        let valid = Workspace::new(vec!["inspector".into(), "timeline".into()]);
        valid.save(&path).unwrap();
        assert_eq!(Workspace::load(&path), valid);
        assert!(!path.with_extension("json.tmp").exists());
        let invalid = Workspace::new(vec!["scene".into(), "scene".into()]);
        assert!(invalid.save(&path).is_err());
        assert_eq!(Workspace::load(&path), valid);
        for bytes in [
            br#"{"version":2,"floating":[]}"#.as_slice(),
            br#"{"version":1,"floating":["bad"]}"#,
            br#"{"version":1,"floating":["scene","scene"]}"#,
            b"broken",
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert!(Workspace::load(&path).floating.is_empty());
        }
        std::fs::write(&path, vec![b' '; 4097]).unwrap();
        assert!(Workspace::load(&path).floating.is_empty());
        Workspace::new(vec![]).save(&path).unwrap();
        assert!(Workspace::load(&path).floating.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
