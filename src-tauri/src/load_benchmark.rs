//! Explicit headless replay of production load paths. Inputs are never modified.
use crate::{
    document::Editor,
    load_task::{Kind, Tasks},
    snapping,
};
use std::{io::Write, path::PathBuf, time::Instant};
fn write_new(path: &std::path::Path, bytes: &[u8]) {
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}
#[test]
#[ignore = "Real-scene load replay; use scripts/benchmark-load.py"]
fn measure() {
    let mode = std::env::var("U3D_LOAD_MODE").unwrap();
    let input = PathBuf::from(std::env::var_os("U3D_LOAD_INPUT").unwrap());
    let work = PathBuf::from(std::env::var_os("U3D_LOAD_WORK").unwrap());
    std::fs::create_dir_all(&work).unwrap();
    let mut editor = Editor::new();
    if mode == "prepare" {
        let imported = crate::usd::load(
            input.to_string_lossy().into_owned(),
            work.join("sdk"),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();
        let report = editor.import_usd(imported).unwrap();
        write_new(
            &work.join("scene.u3d"),
            &editor.document.file_bytes().unwrap(),
        );
        write_new(
            &work.join("checkpoint.json"),
            &crate::persistence::benchmark_checkpoint_bytes(&editor.document).unwrap(),
        );
        println!(
            "LOAD_FIXTURES {}",
            serde_json::json!({"objects":editor.document.objects.len(),"triangles":report.triangles})
        );
        return;
    }
    // A native app already has its current recovery session before restore is requested.
    let mut recovery = (mode == "recovery")
        .then(|| crate::persistence::RecoveryStore::create(&work.join("recovery")).unwrap());
    let tasks = Tasks::default();
    let guard = tasks
        .begin(match mode.as_str() {
            "usd" => Kind::Usd,
            "open" => Kind::Open,
            "recovery" => Kind::Recovery,
            _ => panic!("Unknown mode"),
        })
        .unwrap();
    guard.task.trace();
    match mode.as_str() {
        "usd" => {
            guard
                .task
                .progress(crate::load_task::Phase::Convert, 0, 0)
                .unwrap();
            let imported = crate::usd::load_with_task(
                input.to_string_lossy().into_owned(),
                work.join("sdk"),
                guard.task.cancel_arc(),
                &guard.task,
            )
            .unwrap();
            let base = editor.capture_usd_append().unwrap();
            let prepared = snapping::PreparedUsdAppend::build(base, imported, &guard.task).unwrap();
            prepared.commit(&mut editor, &guard.task).unwrap();
        }
        "open" | "recovery" => {
            let document = if mode == "open" {
                crate::persistence::read_document_with_task(&input, &guard.task).unwrap()
            } else {
                crate::persistence::read_checkpoint_with_task(&input, &guard.task)
                    .unwrap()
                    .document
            };
            let prepared = snapping::PreparedDocument::with_task_budget(
                document,
                &guard.task,
                editor.snap_build_budget().unwrap(),
            )
            .unwrap();
            prepared.check_publication(&editor).unwrap();
            guard.task.commit().unwrap();
            if let Some(store) = &mut recovery {
                store
                    .write(prepared.document(), &None, editor.stamp(), Instant::now())
                    .unwrap();
            }
            editor.replace_prepared_document(prepared, None, mode == "recovery");
        }
        _ => unreachable!(),
    }
    guard.task.succeed().unwrap();
    drop(guard);
    let profile = tasks.profile().unwrap();
    assert_eq!(profile.outcome, crate::load_task::Outcome::Success);
    println!(
        "LOAD_STAGE {}",
        serde_json::json!({"phase":"first_snap","at_us":profile.total_us,"pid":std::process::id()})
    );
    std::io::stdout().flush().unwrap();
    let before = editor.vertex_cache.borrow().work;
    let started = Instant::now();
    let index = snapping::VertexIndex::new(&editor, [1280., 860.], false);
    let acquire_us = started.elapsed().as_micros();
    let started = Instant::now();
    let edge_hit = index
        .nearest_mode([640., 430.], crate::gizmo::MoveSnap::Edge)
        .is_some();
    let edge_us = started.elapsed().as_micros();
    let started = Instant::now();
    let face_hit = index
        .nearest_mode([640., 430.], crate::gizmo::MoveSnap::Face)
        .is_some();
    let face_us = started.elapsed().as_micros();
    let after = editor.vertex_cache.borrow().work;
    assert_eq!(before.objects_visited, after.objects_visited);
    assert_eq!(before.geometries_built, after.geometries_built);
    assert_eq!(before.full_syncs, after.full_syncs);
    let diagnostics = snapping::diagnostics(&editor);
    println!(
        "LOAD_METRICS {}",
        serde_json::json!({"mode":mode,"profile":profile,
        "objects":editor.document.objects.len(), "mesh_assets":editor.document.meshes.len(),
        "triangles":editor.document.meshes.values().map(|m|m.triangles.len()).sum::<usize>(),
        "diagnostics":diagnostics,"first_snap":{"acquire_us":acquire_us,"edge_us":edge_us,"face_us":face_us,
        "edge_hit":edge_hit,"face_hit":face_hit,"objects_visited":0,"geometries_built":0},
        "scope":"Headless production CPU load path; no dialog, IPC notification, renderer upload or GPU completion. Recovery writes a private checkpoint but never consumes the input."})
    );
}
