#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod document;
mod renderer;
use document::{Document, Editor, Object};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
struct State {
    quit_approved: AtomicBool,
    quitting: AtomicBool,
    editor: Mutex<Editor>,
    renderer: Mutex<Option<renderer::Renderer>>,
}
fn request_quit(app: &tauri::AppHandle) {
    let state = app.state::<State>();
    if !state.editor.lock().unwrap().dirty {
        app.exit(0);
        return;
    }
    if state.quitting.swap(true, Ordering::SeqCst) {
        return;
    }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let dialog=rfd::AsyncMessageDialog::new().set_title("Universal 3D")
            .set_description("Unsaved changes / 未保存の変更 / 未保存的更改\nDiscard and quit? / 破棄して終了しますか？ / 放弃并退出？")
            .set_buttons(rfd::MessageButtons::YesNo);
        let dialog = if let Some(window) = handle.get_window("main") {
            dialog.set_parent(&window)
        } else {
            dialog
        };
        let answer = dialog.show().await;
        let state = handle.state::<State>();
        if answer == rfd::MessageDialogResult::Yes {
            state.quit_approved.store(true, Ordering::SeqCst);
            handle.exit(0);
        } else {
            state.quitting.store(false, Ordering::SeqCst);
        }
    });
}
#[derive(Clone, Serialize)]
struct Snapshot {
    document: Document,
    can_undo: bool,
    can_redo: bool,
    dirty: bool,
    filename: String,
    floating: Vec<String>,
    viewport: bool,
    frame: u32,
    camera: [f32; 3],
}
fn snapshot(s: &State) -> Snapshot {
    let e = s.editor.lock().unwrap();
    Snapshot {
        document: e.document.clone(),
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
        viewport: s.renderer.lock().unwrap().is_some(),
        frame: e.frame,
        camera: [e.yaw, e.pitch, e.distance],
    }
}
fn broadcast(app: &tauri::AppHandle, s: &State) -> Result<Snapshot, String> {
    let snap = snapshot(s);
    app.emit("editor-changed", &snap)
        .map_err(|e| e.to_string())?;
    draw(s)?;
    Ok(snap)
}
fn draw(s: &State) -> Result<(), String> {
    let e = s.editor.lock().unwrap();
    if let Some(r) = s.renderer.lock().unwrap().as_mut() {
        r.draw(&e)?;
    }
    Ok(())
}
#[tauri::command]
fn get_snapshot(state: tauri::State<State>) -> Snapshot {
    snapshot(&state)
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Command {
    SetFrame { frame: u32 },
    Add { kind: String },
    Select { id: u64 },
    Update { object: Object },
    Delete,
    Undo,
    Redo,
    Camera { yaw: f32, pitch: f32, distance: f32 },
}
#[tauri::command]
fn execute(
    app: tauri::AppHandle,
    state: tauri::State<State>,
    command: Command,
) -> Result<Snapshot, String> {
    {
        let mut e = state.editor.lock().map_err(|e| e.to_string())?;
        let mut d = e.document.clone();
        match command {
            Command::Add { kind } => {
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
                e.commit(d)?;
            }
            Command::Select { id } => {
                if !d.objects.iter().any(|o| o.id == id) {
                    return Err("Object does not exist".into());
                }
                e.document.selected = Some(id);
            }
            Command::Update { object } => {
                let o = d
                    .objects
                    .iter_mut()
                    .find(|o| o.id == object.id)
                    .ok_or("Object does not exist")?;
                *o = object;
                e.commit(d)?;
            }
            Command::Delete => {
                if let Some(id) = d.selected {
                    d.objects.retain(|o| o.id != id);
                    d.selected = d.objects.first().map(|o| o.id);
                    e.commit(d)?;
                }
            }
            Command::SetFrame { frame } => e.frame = frame.clamp(1, 120),
            Command::Undo => e.undo(),
            Command::Redo => e.redo(),
            Command::Camera {
                yaw,
                pitch,
                distance,
            } => {
                if ![yaw, pitch, distance].iter().all(|v| v.is_finite()) {
                    return Err("Invalid camera".into());
                }
                e.yaw = yaw;
                e.pitch = pitch.clamp(-1.4, 1.4);
                e.distance = distance.clamp(1.5, 60.);
            }
        }
    }
    broadcast(&app, &state)
}
#[tauri::command]
async fn save_document(
    app: tauri::AppHandle,
    state: tauri::State<'_, State>,
) -> Result<(), String> {
    let (doc, path) = {
        let e = state.editor.lock().unwrap();
        (e.document.clone(), e.path.clone())
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
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
    let temporary = path.with_extension("u3d.tmp");
    std::fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
    {
        let mut e = state.editor.lock().unwrap();
        if e.document == doc {
            e.dirty = false;
        }
        e.path = Some(path);
    }
    broadcast(&app, &state)?;
    Ok(())
}
#[tauri::command]
async fn open_document(
    app: tauri::AppHandle,
    state: tauri::State<'_, State>,
) -> Result<(), String> {
    if state.editor.lock().unwrap().dirty {
        let answer=rfd::AsyncMessageDialog::new().set_title("Universal 3D").set_description("Unsaved changes / 未保存の変更 / 未保存的更改\nDiscard and open? / 破棄して開きますか？ / 放弃并打开？").set_buttons(rfd::MessageButtons::YesNo).show().await;
        if answer != rfd::MessageDialogResult::Yes {
            return Ok(());
        }
    }
    let Some(file) = rfd::AsyncFileDialog::new()
        .add_filter("Universal 3D", &["u3d"])
        .pick_file()
        .await
    else {
        return Ok(());
    };
    if std::fs::metadata(file.path())
        .map_err(|e| e.to_string())?
        .len()
        > 10_000_000
    {
        return Err("Document exceeds 10 MB".into());
    }
    let doc: Document =
        serde_json::from_slice(&std::fs::read(file.path()).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    doc.validate()?;
    {
        let mut e = state.editor.lock().unwrap();
        e.document = doc;
        e.undo.clear();
        e.redo.clear();
        e.dirty = false;
        e.path = Some(file.path().to_owned());
    }
    broadcast(&app, &state)?;
    Ok(())
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
    tauri::WebviewWindowBuilder::new(
        &app,
        &label,
        tauri::WebviewUrl::App(format!("index.html?panel={panel}").into()),
    )
    .title(format!("Universal 3D · {panel}"))
    .inner_size(380., 560.)
    .build()
    .map_err(|e| e.to_string())?;
    state.editor.lock().unwrap().floating.push(panel);
    broadcast(&app, &state)?;
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
    Ok(())
}
#[tauri::command]
async fn open_viewport(app: tauri::AppHandle) -> Result<(), String> {
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
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            execute,
            save_document,
            open_document,
            float_panel,
            dock_panel,
            open_viewport
        ])
        .on_window_event(|window, event| {
            let app = window.app_handle();
            let s = app.state::<State>();
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                    api.prevent_close();
                    request_quit(app);
                }

                tauri::WindowEvent::Resized(size) if window.label() == "viewport" => {
                    if let Some(r) = s.renderer.lock().unwrap().as_mut() {
                        r.resize(size.width, size.height);
                    }
                    let _ = draw(&s);
                }
                tauri::WindowEvent::Destroyed if window.label() == "viewport" => {
                    *s.renderer.lock().unwrap() = None;
                    let _ = app.emit("editor-changed", snapshot(&s));
                }
                tauri::WindowEvent::Destroyed if window.label().starts_with("panel-") => {
                    s.editor
                        .lock()
                        .unwrap()
                        .floating
                        .retain(|p| format!("panel-{p}") != window.label());
                    let _ = app.emit("editor-changed", snapshot(&s));
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("Unable to start Universal 3D");
    application.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let state = app.state::<State>();
            if !state.quit_approved.load(Ordering::SeqCst) && state.editor.lock().unwrap().dirty {
                api.prevent_exit();
                request_quit(app);
            }
        }
    });
}
