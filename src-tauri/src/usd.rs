use crate::{
    document::{Editor, Object},
    mesh::{self, Mesh, Source},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct RawMesh {
    name: String,
    prim_path: String,
    points: Vec<[f32; 3]>,
    counts: Vec<usize>,
    indices: Vec<usize>,
    holes: Vec<usize>,
    left_handed: bool,
    color: [f32; 3],
}
#[derive(Deserialize)]
struct RawImport {
    meshes: Vec<RawMesh>,
    source: Source,
    warnings: Vec<String>,
    meters_per_unit: f64,
    up_axis: String,
}
pub struct Imported {
    pub meshes: Vec<(String, [f32; 3], Arc<Mesh>)>,
    pub report: Report,
}
#[derive(Clone, Serialize)]
pub struct Report {
    pub objects: usize,
    pub triangles: usize,
    pub source: Source,
    pub warnings: Vec<String>,
    pub meters_per_unit: f64,
    pub up_axis: String,
}
static SEQUENCE: AtomicU64 = AtomicU64::new(1);
pub fn python() -> PathBuf {
    if let Some(path) = std::env::var_os("UNIVERSAL3D_USD_PYTHON") {
        return path.into();
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.usd-runtime");
    let path = root.join(if cfg!(windows) {
        "Scripts/python.exe"
    } else {
        "bin/python"
    });
    if path.is_file() {
        path
    } else {
        PathBuf::from(if cfg!(windows) { "python" } else { "python3" })
    }
}
#[cfg(test)]
pub fn load(source: String, cache: PathBuf, cancel: Arc<AtomicBool>) -> Result<Imported, String> {
    load_context(source, cache, cancel, None)
}
pub fn load_with_task(
    source: String,
    cache: PathBuf,
    cancel: Arc<AtomicBool>,
    task: &crate::load_task::Task,
) -> Result<Imported, String> {
    load_context(source, cache, cancel, Some(task))
}
fn load_context(
    source: String,
    cache: PathBuf,
    cancel: Arc<AtomicBool>,
    task: Option<&crate::load_task::Task>,
) -> Result<Imported, String> {
    let context = crate::load_io::Context {
        task,
        cancel: Some(&cancel),
    };
    context.check()?;
    let id = format!(
        "{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let work = cache.join(format!("import-{id}"));
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let output = work.join("mesh.json");
    let errors = work.join("errors.txt");
    let progress_path = work.join("progress.json");
    let result = (|| {
        let mut child = Command::new(python())
            .arg("-I")
            .arg("-c")
            .arg(include_str!("../usd/import_usd.py"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&errors).map_err(|e| e.to_string())?)
            .spawn()
            .map_err(|e| format!("USD_RUNTIME_MISSING: {e}"))?;
        let request = serde_json::json!({"source":source,"package_dir":work.join("package"),"output":output,"progress":progress_path});
        if let Err(e) = child
            .stdin
            .take()
            .unwrap()
            .write_all(request.to_string().as_bytes())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e.to_string());
        }
        let started = Instant::now();
        loop {
            if let Some(task) = task {
                if let Ok(bytes) = std::fs::read(&progress_path) {
                    #[derive(Deserialize)]
                    struct ChildProgress {
                        phase: String,
                        completed: usize,
                        total: usize,
                    }
                    if bytes.len() <= 4096 {
                        if let Ok(p) = serde_json::from_slice::<ChildProgress>(&bytes) {
                            let phase = match p.phase.as_str() {
                                "read" => Some(crate::load_task::Phase::Read),
                                "parse" => Some(crate::load_task::Phase::Parse),
                                "extract" => Some(crate::load_task::Phase::Extract),
                                "convert" => Some(crate::load_task::Phase::Convert),
                                _ => None,
                            };
                            if let Some(phase) = phase {
                                // Cancellation is handled below so the child is always reaped.
                                let _ = task.progress(phase, p.completed, p.total);
                            }
                        }
                    }
                }
            }
            if cancel.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(300) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(if cancel.load(Ordering::Relaxed) {
                    "LOAD_CANCELLED"
                } else {
                    "USD_IMPORT_TIMED_OUT"
                }
                .into());
            }
            if std::fs::metadata(&output).is_ok_and(|m| m.len() > 64 * 1024 * 1024)
                || std::fs::metadata(&errors).is_ok_and(|m| m.len() > 1024 * 1024)
            {
                let _ = child.kill();
                let _ = child.wait();
                return Err("USD import output exceeds limit".into());
            }
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                if !status.success() {
                    let text = std::fs::read_to_string(&errors).unwrap_or_default();
                    return Err(if text.contains("No module named 'pxr'") {
                        "USD_RUNTIME_MISSING: install usd-core 26.11".into()
                    } else {
                        text.chars().take(4000).collect()
                    });
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if std::fs::metadata(&output).map_err(|e| e.to_string())?.len() > 64 * 1024 * 1024 {
            return Err("USD output exceeds 64 MiB".into());
        }
        let raw: RawImport = crate::load_io::read_json(&output, 64 * 1024 * 1024, context)?;
        let converted = convert_context(raw, context)?;
        context.check()?;
        // Keep downloaded dependencies, license and metadata for provenance / future material support.
        if work.join("package").exists() {
            std::fs::rename(work.join("package"), cache.join(format!("package-{id}")))
                .map_err(|e| e.to_string())?;
        }
        Ok(converted)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}
#[cfg(test)]
fn convert(raw: RawImport) -> Result<Imported, String> {
    convert_context(
        raw,
        crate::load_io::Context {
            task: None,
            cancel: None,
        },
    )
}
fn convert_context(
    raw: RawImport,
    context: crate::load_io::Context<'_>,
) -> Result<Imported, String> {
    context.check()?;
    let faces_total = raw.meshes.iter().map(|m| m.counts.len()).sum();
    let mut faces_done = 0;
    context.progress(crate::load_task::Phase::Triangulate, 0, faces_total)?;
    if raw.meshes.is_empty()
        || raw.meshes.len() > 1000
        || !raw.meters_per_unit.is_finite()
        || raw.meters_per_unit <= 0.
        || !["Y", "Z"].contains(&raw.up_axis.as_str())
    {
        return Err("Invalid USD import".into());
    }
    let mut meshes = Vec::new();
    let mut total = 0;
    for m in raw.meshes {
        context.check()?;
        if m.points.len() > 1_500_000
            || m.points
                .iter()
                .flatten()
                .any(|x| !x.is_finite() || x.abs() > 10000.)
            || m.counts.iter().try_fold(0usize, |a, b| a.checked_add(*b)) != Some(m.indices.len())
        {
            return Err("Invalid USD mesh data".into());
        }
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for (index, p) in m.points.iter().enumerate() {
            if index.is_multiple_of(4096) {
                context.check()?;
            }
            for i in 0..3 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
            }
        }
        let center = std::array::from_fn(|i| (min[i] + max[i]) * 0.5);
        let points: Vec<[f32; 3]> = m
            .points
            .iter()
            .map(|p| std::array::from_fn(|i| p[i] - center[i]))
            .collect();
        let holes: HashSet<usize> = m.holes.into_iter().collect();
        if holes.iter().any(|&i| i >= m.counts.len()) {
            return Err("Invalid USD hole index".into());
        }
        let mut triangles = Vec::new();
        let mut edge_mask = String::new();
        let mut start = 0;
        for (face, count) in m.counts.into_iter().enumerate() {
            if face.is_multiple_of(128) {
                context.progress(
                    crate::load_task::Phase::Triangulate,
                    faces_done,
                    faces_total,
                )?;
            }
            faces_done += 1;
            if !holes.contains(&face) {
                let mut result = mesh::triangulate_cancellable(
                    &points,
                    &m.indices[start..start + count],
                    context.cancel,
                )?;
                if m.left_handed {
                    for t in &mut result {
                        t.swap(1, 2);
                    }
                }
                // Preserve polygon boundaries before flattening to triangles, including flat
                // neighboring polygons. The generated n-gon triangulation diagonals stay hidden.
                let polygon = &m.indices[start..start + count];
                let edge_key = |a: [f32; 3], b: [f32; 3]| {
                    let bits = |p: [f32; 3]| p.map(|v| if v == 0. { 0 } else { v.to_bits() });
                    let a = bits(a);
                    let b = bits(b);
                    if a <= b {
                        (a, b)
                    } else {
                        (b, a)
                    }
                };
                let boundaries: HashSet<_> = (0..polygon.len())
                    .map(|i| edge_key(points[polygon[i]], points[polygon[(i + 1) % polygon.len()]]))
                    .collect();
                edge_mask.extend(result.iter().map(|triangle| {
                    let mask = [[0, 1], [1, 2], [2, 0]].into_iter().enumerate().fold(
                        0u8,
                        |mask, (side, [a, b])| {
                            if boundaries.contains(&edge_key(triangle[a], triangle[b])) {
                                mask | (1 << side)
                            } else {
                                mask
                            }
                        },
                    );
                    char::from(b'0' + mask)
                }));
                total += result.len();
                if total > mesh::MAX_TRIANGLES {
                    return Err("USD exceeds 500,000 triangles".into());
                }
                triangles.extend(result);
            }
            start += count;
        }
        if triangles.is_empty() {
            continue;
        }
        let asset = Arc::new(Mesh {
            triangles,
            edge_mask: Some(edge_mask),
            color: m.color.map(|x| x.clamp(0., 1.)),
            source: raw.source.clone(),
            prim_path: m.prim_path,
        });
        asset.validate_cancellable(context.cancel)?;
        meshes.push((m.name.chars().take(60).collect(), center, asset));
    }
    if meshes.is_empty() {
        return Err("No non-degenerate USD faces".into());
    }
    context.progress(
        crate::load_task::Phase::Triangulate,
        faces_total,
        faces_total,
    )?;
    Ok(Imported {
        report: Report {
            objects: meshes.len(),
            triangles: total,
            source: raw.source,
            warnings: raw.warnings,
            meters_per_unit: raw.meters_per_unit,
            up_axis: raw.up_axis,
        },
        meshes,
    })
}
/// Immutable append candidate. Expensive validation and framing happen before publication.
pub struct AppendPlan {
    document: crate::document::Document,
    publication: AppendPublication,
}
pub(crate) struct AppendPublication {
    start: usize,
    report: Report,
    target: [f32; 3],
    distance: f32,
}
impl AppendPlan {
    pub fn prepare(
        mut document: crate::document::Document,
        first: u64,
        imported: Imported,
        cancel: Option<&AtomicBool>,
    ) -> Result<Self, String> {
        crate::snapping::check_cancel(cancel)?;
        if imported.meshes.is_empty() {
            return Err(
                "No mesh objects in USD / USDにメッシュがありません / USD中没有网格对象".into(),
            );
        }
        if document.objects.len() + imported.meshes.len() > 10000
            || document.meshes.len() + imported.meshes.len() > 1000
            || document
                .meshes
                .values()
                .map(|m| m.triangles.len())
                .sum::<usize>()
                + imported.report.triangles
                > mesh::MAX_TRIANGLES
        {
            return Err("Document exceeds mesh budget".into());
        }
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for (_, position, mesh) in &imported.meshes {
            crate::snapping::check_cancel(cancel)?;
            for (index, point) in mesh.triangles.iter().flatten().enumerate() {
                if index.is_multiple_of(3072) {
                    crate::snapping::check_cancel(cancel)?;
                }
                for i in 0..3 {
                    let v = point[i] + position[i];
                    min[i] = min[i].min(v);
                    max[i] = max[i].max(v);
                }
            }
        }
        let mut objects = Vec::new();
        let mut assets = Vec::new();
        for (i, (name, position, mesh)) in imported.meshes.into_iter().enumerate() {
            crate::snapping::check_cancel(cancel)?;
            mesh.validate_cancellable(cancel)?;
            let id = first
                .checked_add(i as u64)
                .filter(|id| *id <= 9_007_199_254_740_991)
                .ok_or("Object ID limit reached")?;
            let mut serial = document.meshes.len() + assets.len();
            let key = loop {
                let key = format!("usd-{serial}");
                if !document.meshes.contains_key(&key) && !assets.iter().any(|(k, _)| k == &key) {
                    break key;
                }
                serial += 1;
            };
            let object = Object {
                id,
                name,
                kind: "mesh".into(),
                mesh_id: Some(key.clone()),
                position,
                rotation: [0.; 3],
                scale: [1.; 3],
                shear: [0.; 3],
                visible: true,
                mirror: false,
            };
            object.validate()?;
            objects.push(object);
            assets.push((key, mesh));
        }
        let start = document.objects.len();
        document.objects.extend(objects);
        for (key, mesh) in assets {
            document.meshes.insert(key, mesh);
        }
        document.version = 3;
        document.selected = Some(first);
        crate::snapping::check_cancel(cancel)?;
        Ok(Self {
            document,
            publication: AppendPublication {
                start,
                report: imported.report,
                target: std::array::from_fn(|i| (min[i] + max[i]) * 0.5),
                distance: ((0..3)
                    .map(|i| (max[i] - min[i]).powi(2))
                    .sum::<f32>()
                    .sqrt()
                    * 1.5)
                    .clamp(0.05, 50000.),
            },
        })
    }
    pub(crate) fn into_parts(self) -> (crate::document::Document, AppendPublication) {
        (self.document, self.publication)
    }
}
impl AppendPublication {
    pub(crate) fn publish(
        self,
        editor: &mut Editor,
        mut document: crate::document::Document,
    ) -> Result<Report, String> {
        // The caller fences the base document before entering this small publication step.
        let objects = document.objects.split_off(self.start);
        let mut keys = Vec::new();
        for object in &objects {
            let key = object.mesh_id.as_ref().ok_or("Missing imported mesh")?;
            if editor.document.meshes.contains_key(key) {
                return Err("USD append asset conflict".into());
            }
            keys.push(key.clone());
        }
        for key in &keys {
            editor
                .document
                .meshes
                .insert(key.clone(), document.meshes[key].clone());
        }
        if let Err(error) = editor.edit_span(self.start, 0, objects, document.selected) {
            for key in keys {
                editor.document.meshes.remove(&key);
            }
            return Err(error);
        }
        editor.document.version = 3;
        editor.camera_target = self.target;
        editor.distance = self.distance;
        Ok(self.report)
    }
}
impl Editor {
    #[cfg(test)]
    pub fn import_usd(&mut self, imported: Imported) -> Result<Report, String> {
        self.ensure_not_dragging()?;
        let first = self.next_id()?;
        let plan = AppendPlan::prepare(self.document.clone(), first, imported, None)?;
        let (document, publication) = plan.into_parts();
        document.validate()?;
        publication.publish(self, document)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    pub fn fixture() -> Imported {
        convert(serde_json::from_value(serde_json::json!({
            "meshes":[{"name":"Concave","prim_path":"/Root/Mesh","points":[[0,0,0],[2,0,0],[2,2,0],[1,1,0],[0,2,0]],"counts":[5],"indices":[0,1,2,3,4],"holes":[],"left_handed":false,"color":[0.3,0.5,0.7]}],
            "source":{"origin":"fixture.usda","revision":"","license":"test"},"warnings":[],"meters_per_unit":1,"up_axis":"Y"
        })).unwrap()).unwrap()
    }
    #[test]
    fn import_history_and_self_contained_save_without_mesh_ipc() {
        let mut e = Editor::new();
        let count = e.document.objects.len();
        let report = e.import_usd(fixture()).unwrap();
        assert_eq!(report.triangles, 3);
        assert_eq!(e.undo.len(), 1);
        let key = e.document.objects.last().unwrap().mesh_id.clone().unwrap();
        let mesh = e.document.meshes[&key].clone();
        e.undo();
        assert_eq!(e.document.objects.len(), count);
        e.redo();
        assert_eq!(e.document.objects.len(), count + 1);
        assert!(Arc::ptr_eq(&mesh, &e.document.meshes[&key]));
        let wire = serde_json::to_string(&e.document).unwrap();
        assert!(!wire.contains("triangles"));
        assert!(wire.contains("mesh_id"));
        let file = e.document.file_bytes().unwrap();
        let loaded: crate::document::Document = serde_json::from_slice(&file).unwrap();
        loaded.validate().unwrap();
        assert_eq!(loaded, e.document);
        let mut object = e.document.objects.last().unwrap().clone();
        object.position[0] += 2.;
        object.mirror = true;
        e.execute(crate::editor::Command::Update { object })
            .unwrap();
        e.undo();
        assert!(Arc::ptr_eq(&mesh, &e.document.meshes[&key]));
    }
    #[test]
    fn rejected_import_does_not_change_document_or_history() {
        let mut e = Editor::new();
        let before = e.document.clone();
        let mut imported = fixture();
        imported.meshes[0].1[0] = f32::NAN;
        assert!(e.import_usd(imported).is_err());
        assert_eq!(e.document, before);
        assert!(e.undo.is_empty());
        let mut object = e.document.objects[0].clone();
        object.kind = "mesh".into();
        object.mesh_id = Some("missing".into());
        assert!(e
            .execute(crate::editor::Command::Update { object })
            .is_err());
        assert_eq!(e.document, before);
    }
    #[test]
    fn import_after_undo_does_not_reuse_asset_ids() {
        let mut e = Editor::new();
        e.import_usd(fixture()).unwrap();
        let a = e.document.objects.last().unwrap().mesh_id.clone();
        e.undo();
        e.import_usd(fixture()).unwrap();
        assert_ne!(a, e.document.objects.last().unwrap().mesh_id);
        assert!(e.redo.is_empty());
    }
    #[test]
    #[ignore = "Requires usd-core and U3D_USD_SOURCE"]
    fn official_sdk_process_and_cancel() {
        let source = std::env::var("U3D_USD_SOURCE").unwrap();
        let cache = std::env::temp_dir().join(format!("u3d-usd-process-{}", std::process::id()));
        let imported = load(
            source.clone(),
            cache.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(!imported.meshes.is_empty());
        assert!(load(source, cache.clone(), Arc::new(AtomicBool::new(true))).is_err());
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 0);
        std::fs::remove_dir(cache).unwrap();
    }
    pub fn real_fixture() -> Imported {
        let path = std::env::var("U3D_USD_FIXTURE").unwrap();
        let raw: RawImport = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        convert(raw).unwrap()
    }
    #[test]
    #[ignore = "Requires downloaded real SimReady fixture via U3D_USD_FIXTURE"]
    fn real_simready_import_roundtrip() {
        let path = std::env::var("U3D_USD_FIXTURE").unwrap();
        let raw: RawImport = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let imported = convert(raw).unwrap();
        println!(
            "SimReady: {} meshes / {} triangles",
            imported.report.objects, imported.report.triangles
        );
        let mut e = Editor::new();
        e.import_usd(imported).unwrap();
        e.document.validate().unwrap();
        let bytes = e.document.file_bytes().unwrap();
        assert!(bytes.len() < 128_000_000);
        let loaded: crate::document::Document = serde_json::from_slice(&bytes).unwrap();
        loaded.validate().unwrap();
        assert_eq!(loaded, e.document);
        e.undo();
        assert_eq!(e.document.objects.len(), 1);
        e.redo();
        assert!(e.document.objects.len() > 1);
    }
    #[test]
    fn imported_polygon_edges_survive_concave_triangulation_handedness_and_roundtrip() {
        let imported = fixture();
        let mesh = &imported.meshes[0].2;
        assert_eq!(
            mesh.edge_mask
                .as_ref()
                .unwrap()
                .bytes()
                .map(|byte| (byte - b'0').count_ones() as usize)
                .sum::<usize>(),
            5
        );
        for left_handed in [false, true] {
            for triangles in [false, true] {
                let raw:RawImport=serde_json::from_value(serde_json::json!({
                "meshes":[{"name":"Flat","prim_path":"/Flat","points":[[0,0,0],[2,0,0],[2,2,0],[0,2,0]],
                    "counts":if triangles {vec![3,3]}else{vec![4]}, "indices":if triangles {vec![0,1,2,0,2,3]}else{vec![0,1,2,3]},
                    "holes":[],"left_handed":left_handed,"color":[0.3,0.5,0.7]}],
                "source":{"origin":"fixture","revision":"","license":"test"},"warnings":[],"meters_per_unit":1,"up_axis":"Y"
            })).unwrap();
                let imported = convert(raw).unwrap();
                let mut e = Editor::new();
                e.import_usd(imported).unwrap();
                let saved = e.document.file_bytes().unwrap();
                let loaded: crate::document::Document = serde_json::from_slice(&saved).unwrap();
                loaded.validate().unwrap();
                let mesh = loaded.meshes.values().next().unwrap();
                assert_eq!(
                    mesh.edge_mask
                        .as_ref()
                        .unwrap()
                        .bytes()
                        .map(|byte| (byte - b'0').count_ones() as usize)
                        .sum::<usize>(),
                    if triangles { 6 } else { 4 }
                );
                let mut legacy = serde_json::to_value(mesh).unwrap();
                legacy.as_object_mut().unwrap().remove("edge_mask");
                let mut legacy: Mesh = serde_json::from_value(legacy).unwrap();
                assert!(legacy.edge_mask.is_none());
                legacy.validate().unwrap();
                legacy.edge_mask = Some(String::new());
                assert!(legacy.validate().is_err());
            }
        }
    }
}
