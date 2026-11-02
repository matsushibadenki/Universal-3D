#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod affine;
mod document;
mod editor;
mod gizmo;
mod mesh;
mod persistence;
mod renderer;
mod snap_budget;
mod snapping;
mod spatial;
mod surface_snap;
mod usd;
mod viewport;
mod workspace;
use document::{Document, DocumentPatch, Editor};
use editor::Command;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(target_os = "macos"))]
use std::sync::Arc;
use std::sync::Mutex;
use tauri::{Emitter, Manager};
struct State {
    quit_approved: AtomicBool,
    quitting: AtomicBool,
    editor: Mutex<Editor>,
    renderer: Mutex<Option<renderer::Renderer>>,
    workspace_write: Mutex<()>,
    viewport_visible: AtomicBool,
    usd_busy: AtomicBool,
    file_busy: AtomicBool,
    recovery: Mutex<persistence::RecoveryService>,
    usd_cancel: std::sync::Arc<AtomicBool>,
}
fn save_workspace(app: &tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<State>();
    let _write = state.workspace_write.lock().map_err(|e| e.to_string())?;
    let floating = state
        .editor
        .lock()
        .map_err(|e| e.to_string())?
        .floating
        .clone();
    let path = app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("workspace.json");
    workspace::Workspace::new(floating).save(&path)
}
fn create_panel(app: &tauri::AppHandle, panel: &str) -> Result<(), String> {
    if !["scene", "inspector", "timeline"].contains(&panel) {
        return Err("Unknown panel".into());
    }
    tauri::WebviewWindowBuilder::new(
        app,
        format!("panel-{panel}"),
        tauri::WebviewUrl::App(format!("index.html?panel={panel}").into()),
    )
    .title(format!("Universal 3D · {panel}"))
    .inner_size(380., 560.)
    .build()
    .map_err(|e| e.to_string())?;
    app.state::<State>()
        .editor
        .lock()
        .map_err(|e| e.to_string())?
        .floating
        .push(panel.into());
    Ok(())
}
fn request_quit(app: &tauri::AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = handle.state::<State>();
        if state.quitting.swap(true, Ordering::SeqCst) {
            return;
        }
        let result = quit_document(&handle, &state).await;
        state.quitting.store(false, Ordering::SeqCst);
        if let Err(error) = result {
            rfd::AsyncMessageDialog::new()
                .set_title("Universal 3D")
                .set_description(&error)
                .show()
                .await;
        }
    });
}
async fn quit_document(app: &tauri::AppHandle, state: &State) -> Result<(), String> {
    let _operation = persistence::FileOperation::begin(&state.file_busy)?;
    let dirty = {
        let mut e = state.editor.lock().map_err(|e| e.to_string())?;
        e.finish_transform(true);
        e.dirty
    };
    broadcast(app, state)?;
    if dirty {
        let dialog = rfd::AsyncMessageDialog::new().set_title("Universal 3D")
            .set_description("Unsaved changes / 未保存の変更 / 未保存的更改\nDiscard and quit? / 破棄して終了しますか？ / 放弃并退出？")
            .set_buttons(rfd::MessageButtons::YesNo);
        let dialog = if let Some(window) = app.get_window("main") {
            dialog.set_parent(&window)
        } else {
            dialog
        };
        if dialog.show().await != rfd::MessageDialogResult::Yes {
            return Ok(());
        }
    }
    save_workspace(app)?;
    // Lock order is always recovery -> editor. No checkpoint can reappear after exit cleanup.
    state.recovery.lock().map_err(|e| e.to_string())?.clear();
    state.quit_approved.store(true, Ordering::SeqCst);
    app.exit(0);
    Ok(())
}
fn start_recovery_worker(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(5));
        let state = app.state::<State>();
        let mut recovery = state.recovery.lock().unwrap();
        if state.quitting.load(Ordering::SeqCst) || state.quit_approved.load(Ordering::SeqCst) {
            continue;
        }
        let Some(store) = &mut recovery.store else {
            continue;
        };
        let now = std::time::Instant::now();
        let captured = {
            let editor = state.editor.lock().unwrap();
            store
                .due(&editor, now)
                .then(|| (editor.document.clone(), editor.path.clone(), editor.stamp()))
        };
        if let Some((doc, path, stamp)) = captured {
            recovery.error = store.write(&doc, &path, stamp, now).err();
        }
    });
}
#[tauri::command]
async fn get_recovery_state(app: tauri::AppHandle) -> Result<persistence::RecoveryView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<State>().recovery.lock().unwrap().view()
    })
    .await
    .map_err(|e| e.to_string())
}
#[tauri::command]
async fn restore_recovery(
    app: tauri::AppHandle,
    state: tauri::State<'_, State>,
    id: String,
) -> Result<(), String> {
    let _operation = persistence::FileOperation::begin(&state.file_busy)?;
    let stamp = {
        let e = state.editor.lock().unwrap();
        e.ensure_not_dragging()?;
        e.stamp()
    };
    if state.editor.lock().unwrap().dirty && !confirm_replace().await {
        return Ok(());
    }
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let state = handle.state::<State>();
        let mut recovery = state.recovery.lock().unwrap();
        let store = recovery.store.as_mut().ok_or("Recovery unavailable")?;
        let recovered = store.recover(&id)?;
        let prepared = snapping::PreparedDocument::new(recovered.document)?;
        let mut editor = state.editor.lock().unwrap();
        editor.ensure_unchanged(stamp)?;
        // Publish a self-contained checkpoint before replacing the document or consuming the source.
        // Restore as an unsaved copy; Save As cannot silently overwrite the original document.
        store.write(prepared.document(), &None, stamp, std::time::Instant::now())?;
        editor.replace_prepared_document(prepared, None, true);
        recovery.error = store.consumed(&id).err();
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())??;
    notify(&app, &state, true)
}
async fn confirm_replace() -> bool {
    rfd::AsyncMessageDialog::new().set_title("Universal 3D")
        .set_description("Unsaved changes / 未保存の変更 / 未保存的更改\nDiscard and open? / 破棄して開きますか？ / 放弃并打开？")
        .set_buttons(rfd::MessageButtons::YesNo).show().await == rfd::MessageDialogResult::Yes
}
#[derive(Clone, Serialize)]
struct Metadata {
    revision: u64,
    can_undo: bool,
    can_redo: bool,
    dirty: bool,
    filename: String,
    floating: Vec<String>,
    viewport: bool,
    frame: u32,
    camera: [f32; 3],
    tool: gizmo::Tool,
    transform_settings: gizmo::Settings,
    dragging: bool,
}
#[derive(Clone, Serialize)]
struct Snapshot {
    document: Document,
    #[serde(flatten)]
    metadata: Metadata,
}
#[derive(Serialize, Clone)]
struct Change {
    base_revision: u64,
    patches: Vec<DocumentPatch>,
    reset: Option<Document>,
    selected: Option<u64>,
    #[serde(flatten)]
    metadata: Metadata,
}
fn metadata(e: &Editor, viewport: bool) -> Metadata {
    Metadata {
        revision: e.revision,
        can_undo: !e.undo.is_empty(),
        can_redo: !e.redo.is_empty(),
        dirty: e.dirty,
        filename: e
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled.u3d".into()),
        floating: e.floating.clone(),
        viewport,
        frame: e.frame,
        camera: [e.yaw, e.pitch, e.distance],
        tool: e.tool,
        transform_settings: e.transform_settings,
        dragging: e.transform_drag.is_some(),
    }
}
fn snapshot(s: &State) -> Snapshot {
    let mut e = s.editor.lock().unwrap();
    // A snapshot can race between a mutation and its broadcast. Fence captured
    // patches so subscribers never apply them twice to this newer document.
    if !e.pending.is_empty() {
        e.revision += 1;
        e.pending.clear();
        e.pending_reset = true;
    }
    Snapshot {
        document: e.document.clone(),
        metadata: metadata(&e, s.renderer.lock().unwrap().is_some()),
    }
}
fn broadcast(app: &tauri::AppHandle, s: &State) -> Result<(), String> {
    notify(app, s, false)
}
fn notify(app: &tauri::AppHandle, s: &State, reset: bool) -> Result<(), String> {
    {
        // Serialize sequence assignment, snapshot reads and emission under the same lock.
        let mut e = s.editor.lock().map_err(|e| e.to_string())?;
        let reset = reset || e.pending_reset;
        let base_revision = e.revision;
        e.revision = e.revision.checked_add(1).ok_or("Revision limit reached")?;
        let change = Change {
            base_revision,
            patches: if reset { vec![] } else { e.pending.clone() },
            reset: reset.then(|| e.document.clone()),
            selected: e.document.selected,
            metadata: metadata(&e, s.renderer.lock().unwrap().is_some()),
        };
        e.pending.clear();
        e.pending_reset = false;
        app.emit("editor-changed", &change)
            .map_err(|e| e.to_string())?;
    }
    draw(s)?;
    Ok(())
}
fn draw(s: &State) -> Result<(), String> {
    if !s.viewport_visible.load(Ordering::Acquire) {
        return Ok(());
    }
    let e = s.editor.lock().unwrap();
    if let Some(r) = s.renderer.lock().unwrap().as_mut() {
        r.draw(&e)?;
    }
    Ok(())
}
#[tauri::command]
fn get_snap_diagnostics(state: tauri::State<State>) -> Result<snapping::SnapDiagnostics, String> {
    let editor = state.editor.lock().map_err(|e| e.to_string())?;
    Ok(snapping::diagnostics(&editor))
}
#[tauri::command]
fn get_snapshot(state: tauri::State<State>) -> Snapshot {
    snapshot(&state)
}
#[tauri::command]
fn execute(
    app: tauri::AppHandle,
    state: tauri::State<State>,
    command: Command,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    if command.is_transform_gesture() && window.label() != "main" {
        return Err("Only the main viewport can transform objects directly".into());
    }
    {
        let mut e = state.editor.lock().map_err(|e| e.to_string())?;
        if state.quitting.load(Ordering::SeqCst) {
            return Err("Closing document / 終了確認中です / 正在关闭文档".into());
        }
        e.execute(command)?;
    }
    broadcast(&app, &state)
}
#[tauri::command]
async fn save_document(
    app: tauri::AppHandle,
    state: tauri::State<'_, State>,
) -> Result<(), String> {
    let _operation = persistence::FileOperation::begin(&state.file_busy)?;
    let (doc, path, stamp) = {
        let e = state.editor.lock().unwrap();
        e.ensure_not_dragging()?;
        (e.document.clone(), e.path.clone(), e.stamp())
    };
    let path = match path {
        Some(p) => p,
        None => match rfd::AsyncFileDialog::new()
            .add_filter("Universal 3D", &["u3d"])
            .set_file_name("Untitled.u3d")
            .save_file()
            .await
        {
            Some(f) => f.path().to_owned(),
            None => return Ok(()),
        },
    };
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let bytes = doc.file_bytes().map_err(|e| e.to_string())?;
        persistence::atomic_write(&path, &bytes)?;
        let state = handle.state::<State>();
        let mut recovery = state.recovery.lock().unwrap();
        let mut e = state.editor.lock().unwrap();
        e.saved(stamp, path)?;
        if !e.dirty {
            recovery.clear();
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())??;
    broadcast(&app, &state)
}
#[tauri::command]
async fn open_document(
    app: tauri::AppHandle,
    state: tauri::State<'_, State>,
) -> Result<(), String> {
    let _operation = persistence::FileOperation::begin(&state.file_busy)?;
    let (stamp, dirty) = {
        let e = state.editor.lock().unwrap();
        e.ensure_not_dragging()?;
        (e.stamp(), e.dirty)
    };
    if dirty && !confirm_replace().await {
        return Ok(());
    }
    let Some(file) = rfd::AsyncFileDialog::new()
        .add_filter("Universal 3D", &["u3d"])
        .pick_file()
        .await
    else {
        return Ok(());
    };
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let doc = persistence::read_document(file.path())?;
        let prepared = snapping::PreparedDocument::new(doc)?;
        let state = handle.state::<State>();
        let mut recovery = state.recovery.lock().unwrap();
        let mut e = state.editor.lock().unwrap();
        e.ensure_unchanged(stamp)?;
        e.replace_prepared_document(prepared, Some(file.path().to_owned()), false);
        recovery.clear();
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())??;
    notify(&app, &state, true)
}
#[tauri::command]
fn cancel_usd_import(state: tauri::State<State>) {
    state.usd_cancel.store(true, Ordering::Relaxed);
}
#[tauri::command]
fn browse_simready() -> Result<(), String> {
    let url = "https://huggingface.co/spaces/nvidia/simready-central";
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    result.map(|_| ()).map_err(|e| e.to_string())
}
#[tauri::command]
async fn import_usd(
    app: tauri::AppHandle,
    state: tauri::State<'_, State>,
    source: Option<String>,
) -> Result<Option<usd::Report>, String> {
    let _operation = persistence::FileOperation::begin(&state.file_busy)?;
    if state.usd_busy.swap(true, Ordering::SeqCst) {
        return Err("USD import already running".into());
    }
    struct BusyGuard<'a>(&'a AtomicBool);
    impl Drop for BusyGuard<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let _guard = BusyGuard(&state.usd_busy);
    state.usd_cancel.store(false, Ordering::Relaxed);
    state
        .editor
        .lock()
        .map_err(|e| e.to_string())?
        .ensure_not_dragging()?;
    let source = match source {
        Some(source)
            if source.len() <= 8192 && source.starts_with("https://huggingface.co/datasets/") =>
        {
            source
        }
        Some(_) => return Err("Invalid Hugging Face package URL".into()),
        None => match rfd::AsyncFileDialog::new()
            .add_filter("OpenUSD", &["usd", "usda", "usdc", "usdz"])
            .pick_file()
            .await
        {
            Some(file) => file.path().to_string_lossy().into_owned(),
            None => return Ok(None),
        },
    };
    let cache = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("openusd");
    let cancel = state.usd_cancel.clone();
    let (imported, prepared) =
        tauri::async_runtime::spawn_blocking(move || -> Result<_, String> {
            let imported = usd::load(source, cache, cancel.clone())?;
            let prepared = snapping::PreparedVertices::meshes(
                imported.meshes.iter().map(|(_, _, mesh)| mesh),
                Some(&cancel),
            )?;
            Ok((imported, prepared))
        })
        .await
        .map_err(|e| e.to_string())??;
    let report = {
        let mut editor = state.editor.lock().map_err(|e| e.to_string())?;
        if state.usd_cancel.load(Ordering::Relaxed) {
            return Err("USD_IMPORT_CANCELLED_OR_TIMED_OUT".into());
        }
        let report = editor.import_usd(imported)?;
        editor.install_prepared_vertices(prepared);
        report
    };
    broadcast(&app, &state)?;
    Ok(Some(report))
}
#[tauri::command]
async fn float_panel(
    app: tauri::AppHandle,
    state: tauri::State<'_, State>,
    panel: String,
) -> Result<(), String> {
    if !["scene", "inspector", "timeline"].contains(&panel.as_str()) {
        return Err("Unknown panel".into());
    }
    let label = format!("panel-{panel}");
    if let Some(w) = app.get_webview_window(&label) {
        w.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }
    create_panel(&app, &panel)?;
    broadcast(&app, &state)?;
    save_workspace(&app)?;
    Ok(())
}
#[tauri::command]
fn dock_panel(
    app: tauri::AppHandle,
    state: tauri::State<State>,
    panel: String,
) -> Result<(), String> {
    if !["scene", "inspector", "timeline"].contains(&panel.as_str()) {
        return Err("Unknown panel".into());
    }
    if let Some(w) = app.get_webview_window(&format!("panel-{panel}")) {
        w.close().map_err(|e| e.to_string())?;
    }
    state
        .editor
        .lock()
        .unwrap()
        .floating
        .retain(|p| p != &panel);
    broadcast(&app, &state)?;
    save_workspace(&app)?;
    Ok(())
}
#[tauri::command]
async fn set_viewport_rect(
    window: tauri::WebviewWindow,
    rect: viewport::ViewportRect,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Only the main window owns the embedded viewport".into());
    }
    #[cfg(target_os = "macos")]
    {
        let handle = window.app_handle().clone();
        let native_window = window.ns_window().map_err(|e| e.to_string())? as usize;
        let scale = window.scale_factor().map_err(|e| e.to_string())?;
        let (tx, rx) = std::sync::mpsc::channel();
        window
            .with_webview(move |web| {
                let state = handle.state::<State>();
                let result =
                    viewport::update(web.inner(), native_window as *mut _, rect, scale, &state)
                        .and_then(|created| {
                            if created {
                                broadcast(&handle, &state)
                            } else {
                                draw(&state)
                            }
                        });
                let _ = tx.send(result);
            })
            .map_err(|e| e.to_string())?;
        tauri::async_runtime::spawn_blocking(move || rx.recv().map_err(|e| e.to_string())?)
            .await
            .map_err(|e| e.to_string())?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = rect;
        Err("Embedded viewport is currently supported on macOS".into())
    }
}
#[tauri::command]
async fn open_viewport(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        app.get_webview_window("main")
            .ok_or("Missing main window")?
            .set_focus()
            .map_err(|e| e.to_string())?;
        app.emit_to("main", "viewport-focus", ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        open_detached_viewport(app).await
    }
}
#[cfg(not(target_os = "macos"))]
#[tauri::command]
async fn open_detached_viewport(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_window("viewport") {
        w.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }
    // Window and surface creation must stay on the native event loop thread.
    let handle = app.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let result = (|| {
            let window = tauri::WindowBuilder::new(&handle, "viewport")
                .title("Universal 3D · wgpu Viewport")
                .inner_size(900., 650.)
                .build()
                .map_err(|e| e.to_string())?;
            match pollster::block_on(renderer::Renderer::new(Arc::new(window.clone()))) {
                Ok(r) => {
                    *handle.state::<State>().renderer.lock().unwrap() = Some(r);
                    broadcast(&handle, &handle.state::<State>())?;
                    Ok(())
                }
                Err(err) => {
                    let _ = window.close();
                    Err(err)
                }
            }
        })();
        let _ = tx.send(result);
    })
    .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || rx.recv().map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.to_string())?
}
fn main() {
    let application = tauri::Builder::default()
        .menu(|app| {
            use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
            let quit =
                MenuItem::with_id(app, "quit", "Quit / 終了 / 退出", true, Some("CmdOrCtrl+Q"))?;
            let application = Submenu::with_items(app, "Universal 3D", true, &[&quit])?;
            let edit = Submenu::with_items(
                app,
                "Edit",
                true,
                &[
                    &PredefinedMenuItem::cut(app, None)?,
                    &PredefinedMenuItem::copy(app, None)?,
                    &PredefinedMenuItem::paste(app, None)?,
                    &PredefinedMenuItem::select_all(app, None)?,
                ],
            )?;
            let window = Submenu::with_items(
                app,
                "Window",
                true,
                &[
                    &PredefinedMenuItem::minimize(app, None)?,
                    &PredefinedMenuItem::close_window(app, None)?,
                ],
            )?;
            #[cfg(target_os = "macos")]
            window.set_as_windows_menu_for_nsapp()?;
            Menu::with_items(app, &[&application, &edit, &window])
        })
        .on_menu_event(|app, event| {
            if event.id().as_ref() == "quit" {
                request_quit(app);
            }
        })
        .manage(State {
            quit_approved: AtomicBool::new(false),
            quitting: AtomicBool::new(false),
            editor: Mutex::new(Editor::new()),
            renderer: Mutex::new(None),
            workspace_write: Mutex::new(()),
            usd_busy: AtomicBool::new(false),
            file_busy: AtomicBool::new(false),
            recovery: Mutex::new(persistence::RecoveryService::default()),
            usd_cancel: std::sync::Arc::new(AtomicBool::new(false)),
            viewport_visible: AtomicBool::new(true),
        })
        .setup(|app| {
            let recovery_root = app.path().app_data_dir()?.join("recovery");
            *app.state::<State>().recovery.lock().unwrap() =
                persistence::RecoveryService::start(&recovery_root);
            start_recovery_worker(app.handle().clone());
            let path = app.path().app_config_dir()?.join("workspace.json");
            let layout = workspace::Workspace::load(&path);
            for panel in layout.floating {
                if let Err(error) = create_panel(app.handle(), &panel) {
                    eprintln!("Panel restore failed: {error}");
                }
            }
            broadcast(app.handle(), &app.state::<State>()).map_err(std::io::Error::other)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_snap_diagnostics,
            get_recovery_state,
            restore_recovery,
            execute,
            save_document,
            open_document,
            import_usd,
            cancel_usd_import,
            browse_simready,
            float_panel,
            dock_panel,
            open_viewport,
            set_viewport_rect
        ])
        .on_window_event(|window, event| {
            let app = window.app_handle();
            let s = app.state::<State>();
            match event {
                tauri::WindowEvent::Focused(false) if window.label() == "main" => {
                    let cancelled = s.editor.lock().unwrap().finish_transform(true);
                    if cancelled {
                        let _ = broadcast(app, &s);
                    }
                }
                tauri::WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                    api.prevent_close();
                    request_quit(app);
                }

                tauri::WindowEvent::Resized(_) | tauri::WindowEvent::ScaleFactorChanged { .. }
                    if window.label() == "main" =>
                {
                    let _ = app.emit_to("main", "viewport-layout-needed", ());
                }
                #[cfg(target_os = "macos")]
                tauri::WindowEvent::Destroyed if window.label() == "main" => {
                    *s.renderer.lock().unwrap() = None;
                    viewport::destroy();
                }
                tauri::WindowEvent::Resized(size) if window.label() == "viewport" => {
                    if let Some(r) = s.renderer.lock().unwrap().as_mut() {
                        r.resize(size.width, size.height);
                    }
                    let _ = draw(&s);
                }
                tauri::WindowEvent::Destroyed if window.label() == "viewport" => {
                    *s.renderer.lock().unwrap() = None;
                    let _ = broadcast(app, &s);
                }
                tauri::WindowEvent::Destroyed if window.label().starts_with("panel-") => {
                    if s.quit_approved.load(Ordering::SeqCst) {
                        return;
                    }
                    s.editor
                        .lock()
                        .unwrap()
                        .floating
                        .retain(|p| format!("panel-{p}") != window.label());
                    let _ = broadcast(app, &s);
                    if let Err(error) = save_workspace(app) {
                        eprintln!("Workspace save failed: {error}");
                    }
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("Unable to start Universal 3D");
    application.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let state = app.state::<State>();
            if !state.quit_approved.load(Ordering::SeqCst) {
                api.prevent_exit();
                request_quit(app);
            }
        }
    });
}

#[cfg(test)]
mod sync_tests {
    use super::*;
    fn state() -> State {
        State {
            quit_approved: AtomicBool::new(false),
            quitting: AtomicBool::new(false),
            editor: Mutex::new(Editor::new()),
            renderer: Mutex::new(None),
            workspace_write: Mutex::new(()),
            viewport_visible: AtomicBool::new(true),
            usd_busy: AtomicBool::new(false),
            file_busy: AtomicBool::new(false),
            recovery: Mutex::new(persistence::RecoveryService::default()),
            usd_cancel: std::sync::Arc::new(AtomicBool::new(false)),
        }
    }
    #[test]
    fn snapshot_fences_pending_edits_and_requests_reset() {
        let state = state();
        state
            .editor
            .lock()
            .unwrap()
            .execute(Command::Add {
                kind: "sphere".into(),
            })
            .unwrap();
        let captured = snapshot(&state);
        assert_eq!(captured.metadata.revision, 1);
        let editor = state.editor.lock().unwrap();
        assert!(editor.pending.is_empty());
        assert!(editor.pending_reset);
        assert_eq!(captured.document, editor.document);
        drop(editor);
        assert_eq!(snapshot(&state).metadata.revision, 1);
    }
    #[test]
    fn update_payload_has_one_object_and_no_full_document() {
        let mut editor = Editor::new();
        for id in 2..=10_000 {
            let mut o = editor.document.objects[0].clone();
            o.id = id;
            editor.document.objects.push(o);
        }
        let mut object = editor.document.objects[7000].clone();
        object.position[0] = 2.;
        editor.execute(Command::Update { object }).unwrap();
        editor.revision = 1;
        let change = Change {
            base_revision: 0,
            patches: editor.pending.clone(),
            reset: None,
            selected: editor.document.selected,
            metadata: metadata(&editor, false),
        };
        let json = serde_json::to_value(&change).unwrap();
        assert!(json.get("document").is_none());
        assert_eq!(json["revision"], 1);
        assert_eq!(json["patches"][0]["objects"].as_array().unwrap().len(), 1);
        assert_eq!(json["patches"][0]["start"], 7000);
        assert!(serde_json::to_vec(&change).unwrap().len() < 1024);
    }
}
