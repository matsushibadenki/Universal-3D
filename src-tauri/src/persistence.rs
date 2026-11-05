//! File transactions and crash checkpoints. No WebView owns document content.
use crate::document::{Document, DocumentFile, Editor};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::io::Read;

pub const MAX_FILE: u64 = 128_000_000;
pub const INTERVAL: Duration = Duration::from_secs(30);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const CONFLICT: &str = "Document changed; retry the operation / 文書が変更されました。操作をやり直してください / 文档已更改，请重试";

pub struct FileOperation<'a>(&'a AtomicBool);
impl<'a> FileOperation<'a> {
    pub fn begin(flag: &'a AtomicBool) -> Result<Self, String> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| {
                "File operation in progress / ファイル操作中です / 文件操作正在进行".to_string()
            })?;
        Ok(Self(flag))
    }
}
impl Drop for FileOperation<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    epoch: u64,
    revision: u64,
}
impl Editor {
    pub fn stamp(&self) -> Stamp {
        Stamp {
            epoch: self.document_epoch,
            revision: self.content_revision,
        }
    }
    pub fn ensure_unchanged(&self, stamp: Stamp) -> Result<(), String> {
        self.ensure_not_dragging()?;
        if self.stamp() != stamp {
            return Err(CONFLICT.into());
        }
        Ok(())
    }
    pub fn saved(&mut self, stamp: Stamp, path: PathBuf) -> Result<(), String> {
        if self.document_epoch != stamp.epoch {
            return Err(CONFLICT.into());
        }
        if self.stamp() == stamp && self.transform_drag.is_none() {
            self.dirty = false;
        }
        self.path = Some(path);
        Ok(())
    }
    pub fn replace_document(&mut self, doc: Document, path: Option<PathBuf>, recovered: bool) {
        self.document = doc;
        self.document_epoch += 1;
        self.content_revision = 0;
        self.invalidate_document();
        self.pending.clear();
        self.pending_reset = true;
        self.undo.clear();
        self.redo.clear();
        self.dirty = recovered;
        self.path = path;
    }
}

fn unique() -> String {
    format!(
        "{}-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}
fn seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
pub fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_FILE {
        return Err("Document exceeds 128 MB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_FILE {
        return Err("Document exceeds 128 MB".into());
    }
    Ok(bytes)
}
pub fn read_document_with_task(
    path: &Path,
    task: &crate::load_task::Task,
) -> Result<Document, String> {
    crate::load_io::read_json(
        path,
        MAX_FILE as usize,
        crate::load_io::Context {
            task: Some(task),
            cancel: Some(task.cancel_flag()),
        },
    )
}
#[cfg(test)]
pub fn read_document(path: &Path) -> Result<Document, String> {
    let doc: Document = serde_json::from_slice(&read_bounded(path)?).map_err(|e| e.to_string())?;
    doc.validate()?;
    Ok(doc)
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() as u64 > MAX_FILE {
        return Err("Document exceeds 128 MB".into());
    }
    atomic_write_stream(path, MAX_FILE, |writer| {
        writer.write_all(bytes).map_err(|e| e.to_string())
    })
}

// Count serialized bytes before buffering: oversized output never reaches rename.
struct LimitedWriter<W> {
    inner: W,
    remaining: u64,
}
impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::other("Document exceeds 128 MB"));
        }
        let written = self.inner.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn atomic_write_stream(
    path: &Path,
    limit: u64,
    write: impl FnOnce(&mut dyn Write) -> Result<(), String>,
) -> Result<(), String> {
    let parent = path.parent().ok_or("Missing parent directory")?;
    let temporary = parent.join(format!(".u3d-{}.tmp", unique()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        {
            let mut writer = LimitedWriter {
                inner: BufWriter::with_capacity(64 * 1024, &mut file),
                remaining: limit,
            };
            write(&mut writer)?;
            writer.flush().map_err(|e| e.to_string())?;
        }
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(&temporary, path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[derive(Serialize)]
struct Checkpoint<'a> {
    version: u32,
    saved_at: u64,
    original_path: &'a Option<PathBuf>,
    document: DocumentFile<'a>,
}
#[derive(Deserialize)]
pub struct Recovered {
    version: u32,
    saved_at: u64,
    pub original_path: Option<PathBuf>,
    pub document: Document,
}
pub(crate) fn read_checkpoint_with_task(
    path: &Path,
    task: &crate::load_task::Task,
) -> Result<Recovered, String> {
    let recovered: Recovered = crate::load_io::read_json(
        path,
        MAX_FILE as usize,
        crate::load_io::Context {
            task: Some(task),
            cancel: Some(task.cancel_flag()),
        },
    )?;
    if recovered.version != 1 {
        return Err("Unsupported recovery version".into());
    }
    Ok(recovered)
}
#[cfg(test)]
pub(crate) fn benchmark_checkpoint_bytes(document: &Document) -> Result<Vec<u8>, String> {
    serde_json::to_vec(&Checkpoint {
        version: 1,
        saved_at: 0,
        original_path: &None,
        document: document.file(),
    })
    .map_err(|e| e.to_string())
}
fn read_checkpoint_cancellable(path: &Path, cancel: &AtomicBool) -> Result<Recovered, String> {
    let recovered: Recovered = crate::load_io::read_json(
        path,
        MAX_FILE as usize,
        crate::load_io::Context {
            task: None,
            cancel: Some(cancel),
        },
    )?;
    if recovered.version != 1 {
        return Err("Unsupported recovery version".into());
    }
    recovered.document.validate_cancellable(Some(cancel))?;
    Ok(recovered)
}
#[cfg(test)]
fn read_checkpoint(path: &Path) -> Result<Recovered, String> {
    read_checkpoint_cancellable(path, &AtomicBool::new(false))
}
#[derive(Clone, Serialize)]
pub struct Candidate {
    pub id: String,
    pub filename: String,
    pub saved_at: u64,
}
#[derive(Clone, Default, Serialize)]
pub struct RecoveryView {
    pub candidates: Vec<Candidate>,
    pub saved_at: Option<u64>,
    pub error: Option<String>,
    pub scanning: bool,
}
struct Claimed {
    info: Candidate,
    path: PathBuf,
    _lock: File,
}
pub struct RecoveryStore {
    path: PathBuf,
    _lock: File,
    candidates: Vec<Claimed>,
    written: Option<Stamp>,
    last_attempt: Instant,
    saved_at: Option<u64>,
    scan_error: Option<String>,
}
impl RecoveryStore {
    pub fn create(root: &Path) -> Result<Self, String> {
        fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let dir = root.join(format!("session-{}", unique()));
        fs::create_dir(&dir).map_err(|e| e.to_string())?;
        let lock = OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .open(dir.join("owner.lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock().map_err(|e| e.to_string())?;
        Ok(Self {
            path: dir.join("checkpoint.json"),
            _lock: lock,
            candidates: Vec::new(),
            written: None,
            last_attempt: Instant::now(),
            saved_at: None,
            scan_error: None,
        })
    }
    #[cfg(test)]
    pub fn new(root: &Path) -> Result<Self, String> {
        let mut store = Self::create(root)?;
        let job = ScanJob {
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let scan = scan_candidates(root, &job)?;
        store.candidates = scan.candidates;
        store.scan_error = scan.error;
        Ok(store)
    }
    pub fn due(&self, editor: &Editor, now: Instant) -> bool {
        editor.dirty
            && editor.transform_drag.is_none()
            && self.written != Some(editor.stamp())
            && now.duration_since(self.last_attempt) >= INTERVAL
    }
    pub fn write(
        &mut self,
        doc: &Document,
        original_path: &Option<PathBuf>,
        stamp: Stamp,
        now: Instant,
    ) -> Result<(), String> {
        self.last_attempt = now;
        let saved_at = seconds();
        atomic_write_stream(&self.path, MAX_FILE, |writer| {
            serde_json::to_writer(
                writer,
                &Checkpoint {
                    version: 1,
                    saved_at,
                    original_path,
                    document: doc.file(),
                },
            )
            .map_err(|e| e.to_string())
        })?;
        self.written = Some(stamp);
        self.saved_at = Some(saved_at);
        Ok(())
    }
    pub fn clear(&mut self) -> Result<(), String> {
        match fs::remove_file(&self.path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
        self.written = None;
        self.saved_at = None;
        Ok(())
    }
    pub fn recover_with_task(
        &self,
        id: &str,
        task: &crate::load_task::Task,
    ) -> Result<Recovered, String> {
        let candidate = self
            .candidates
            .iter()
            .find(|c| c.info.id == id)
            .ok_or("Recovery no longer available")?;
        read_checkpoint_with_task(&candidate.path, task)
    }
    #[cfg(test)]
    pub fn recover(&self, id: &str) -> Result<Recovered, String> {
        let candidate = self
            .candidates
            .iter()
            .find(|c| c.info.id == id)
            .ok_or("Recovery no longer available")?;
        read_checkpoint(&candidate.path)
    }
    pub fn consumed(&mut self, id: &str) -> Result<(), String> {
        if let Some(index) = self.candidates.iter().position(|c| c.info.id == id) {
            fs::remove_file(&self.candidates[index].path).map_err(|e| e.to_string())?;
            self.candidates.remove(index);
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct ScanJob {
    cancel: Arc<AtomicBool>,
}
impl ScanJob {
    fn check(&self) -> Result<(), String> {
        crate::snapping::check_cancel(Some(&self.cancel))
    }
}
pub struct RecoveryScan {
    candidates: Vec<Claimed>,
    error: Option<String>,
}
pub fn scan_candidates(root: &Path, job: &ScanJob) -> Result<RecoveryScan, String> {
    let mut candidates = Vec::new();
    let mut invalid = 0;
    job.check()?;
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        job.check()?;
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        let path = entry.path().join("checkpoint.json");
        if !path.is_file() {
            continue;
        }
        let Ok(claim) = OpenOptions::new()
            .read(true)
            .write(true)
            .open(entry.path().join("owner.lock"))
        else {
            continue;
        };
        // The OS releases the lock on abnormal exit; active app sessions are never recovered.
        if claim.try_lock().is_err() {
            continue;
        }
        match read_checkpoint_cancellable(&path, &job.cancel) {
            Ok(saved) => candidates.push(Claimed {
                info: Candidate {
                    id: entry.file_name().to_string_lossy().into_owned(),
                    filename: saved
                        .original_path
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Untitled.u3d".into()),
                    saved_at: saved.saved_at,
                },
                path,
                _lock: claim,
            }),
            Err(_) => {
                job.check()?;
                invalid += 1;
            } // Preserve invalid files for manual recovery.
        }
    }
    job.check()?;
    candidates.sort_by_key(|c| std::cmp::Reverse(c.info.saved_at));
    Ok(RecoveryScan { candidates, error: (invalid > 0).then(|| format!("{invalid} invalid recovery file(s) retained / 復旧ファイルを保持しました / 已保留无效恢复文件")) })
}
#[derive(Default)]
pub struct RecoveryService {
    pub store: Option<RecoveryStore>,
    pub error: Option<String>,
    scan: Option<ScanJob>,
}
impl RecoveryService {
    pub fn pending() -> Self {
        Self {
            store: None,
            error: None,
            scan: Some(ScanJob {
                cancel: Arc::new(AtomicBool::new(false)),
            }),
        }
    }
    pub fn begin_scan(&mut self) -> ScanJob {
        if let Some(old) = self.scan.take() {
            old.cancel.store(true, Ordering::Relaxed);
        }
        let job = ScanJob {
            cancel: Arc::new(AtomicBool::new(false)),
        };
        self.scan = Some(job.clone());
        job
    }
    fn accepts(&self, job: &ScanJob) -> bool {
        !job.cancel.load(Ordering::Relaxed)
            && self
                .scan
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(&current.cancel, &job.cancel))
    }
    pub fn install_store(&mut self, job: &ScanJob, store: RecoveryStore) -> bool {
        if !self.accepts(job) {
            return false;
        }
        if self.store.is_none() {
            self.store = Some(store);
            self.error = None;
        }
        true
    }
    pub fn finish_scan(&mut self, job: &ScanJob, result: Result<RecoveryScan, String>) {
        if !self.accepts(job) {
            return;
        }
        self.scan = None;
        match result {
            Ok(result) => {
                if let Some(store) = &mut self.store {
                    // Merge only discovery results; keep live checkpoint scheduling and errors.
                    store.candidates.extend(result.candidates);
                    store
                        .candidates
                        .sort_by_key(|c| std::cmp::Reverse(c.info.saved_at));
                    store.scan_error = result.error;
                }
            }
            Err(error) => {
                if let Some(store) = &mut self.store {
                    store.scan_error = Some(error);
                } else {
                    self.error = Some(error);
                }
            }
        }
    }
    pub fn shutdown(&mut self) {
        if let Some(job) = self.scan.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
        self.clear();
    }
    pub fn view(&self) -> RecoveryView {
        RecoveryView {
            scanning: self.scan.is_some(),
            candidates: self
                .store
                .as_ref()
                .map(|s| s.candidates.iter().map(|c| c.info.clone()).collect())
                .unwrap_or_default(),
            saved_at: self.store.as_ref().and_then(|s| s.saved_at),
            error: self
                .error
                .clone()
                .or_else(|| self.store.as_ref().and_then(|s| s.scan_error.clone())),
        }
    }
    pub fn clear(&mut self) {
        if let Some(store) = &mut self.store {
            self.error = store.clear().err();
        }
    }
}

impl Drop for RecoveryService {
    fn drop(&mut self) {
        if let Some(job) = &self.scan {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{editor::Command, gizmo};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("u3d-recovery-{}", unique()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn edit(e: &mut Editor) {
        e.execute(Command::Add {
            kind: "sphere".into(),
        })
        .unwrap();
    }

    #[test]
    fn streamed_checkpoint_matches_bytes_and_failures_preserve_destination() {
        let tmp = Temp::new();
        let path = tmp.0.join("stream.json");
        let editor = Editor::new();
        let checkpoint = Checkpoint {
            version: 1,
            saved_at: 0,
            original_path: &None,
            document: editor.document.file(),
        };
        let expected = serde_json::to_vec(&checkpoint).unwrap();
        atomic_write_stream(&path, expected.len() as u64, |writer| {
            serde_json::to_writer(writer, &checkpoint).map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), expected);
        assert!(
            atomic_write_stream(&path, expected.len() as u64 - 1, |writer| {
                serde_json::to_writer(writer, &checkpoint).map_err(|e| e.to_string())
            })
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), expected);
        assert!(atomic_write_stream(&path, MAX_FILE, |writer| {
            writer.write_all(&vec![b'x'; 128 * 1024]).unwrap();
            Err("serialization failed after partial output".into())
        })
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), expected);
        assert_eq!(fs::read_dir(&tmp.0).unwrap().count(), 1);
    }

    #[test]
    fn background_scan_keeps_checkpoint_state_live_and_merges_only_candidates() {
        let tmp = Temp::new();
        let e = Editor::new();
        let mut old = RecoveryStore::create(&tmp.0).unwrap();
        old.write(
            &e.document,
            &Some("old.u3d".into()),
            e.stamp(),
            Instant::now(),
        )
        .unwrap();
        let source = old.path.clone();
        let source_bytes = fs::read(&source).unwrap();
        drop(old);
        let mut active = RecoveryStore::create(&tmp.0).unwrap();
        active
            .write(&e.document, &None, e.stamp(), Instant::now())
            .unwrap();
        let invalid = RecoveryStore::create(&tmp.0).unwrap();
        fs::write(&invalid.path, b"not JSON").unwrap();
        drop(invalid);
        let mut service = RecoveryService::pending();
        assert!(service.view().scanning);
        let job = service.begin_scan();
        assert!(service.install_store(&job, RecoveryStore::create(&tmp.0).unwrap()));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let root = tmp.0.clone();
        let worker_job = job.clone();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            scan_candidates(&root, &worker_job)
        });
        started_rx.recv().unwrap();
        assert!(service.view().scanning);
        assert!(service.view().candidates.is_empty());
        let mut current = Editor::new();
        edit(&mut current);
        let stamp = current.stamp();
        let store = service.store.as_mut().unwrap();
        store
            .write(&current.document, &None, stamp, Instant::now())
            .unwrap();
        let saved_at = store.saved_at;
        let current_path = store.path.clone();
        service.error = Some("checkpoint warning".into());
        release_tx.send(()).unwrap();
        let result = worker.join().unwrap();
        service.finish_scan(&job, result);
        let view = service.view();
        assert!(!view.scanning);
        assert_eq!(view.candidates.len(), 1);
        assert_eq!(view.candidates[0].filename, "old.u3d");
        assert_eq!(view.saved_at, saved_at);
        assert_eq!(view.error.as_deref(), Some("checkpoint warning"));
        let store = service.store.as_ref().unwrap();
        assert_eq!(store.written, Some(stamp));
        assert!(store.scan_error.as_ref().unwrap().contains("1 invalid"));
        assert!(current_path.is_file());
        assert_eq!(fs::read(source).unwrap(), source_bytes);
        assert!(active.path.is_file());
    }
    #[test]
    fn stale_scan_and_shutdown_release_claims_without_recreating_checkpoint() {
        let tmp = Temp::new();
        let e = Editor::new();
        let mut old = RecoveryStore::create(&tmp.0).unwrap();
        old.write(&e.document, &None, e.stamp(), Instant::now())
            .unwrap();
        let source = old.path.clone();
        let owner = source.parent().unwrap().join("owner.lock");
        drop(old);
        let mut service = RecoveryService::pending();
        let first = service.begin_scan();
        service.install_store(&first, RecoveryStore::create(&tmp.0).unwrap());
        let result = scan_candidates(&tmp.0, &first).unwrap();
        assert_eq!(result.candidates.len(), 1);
        let second = service.begin_scan();
        assert!(first.check().is_err());
        service.finish_scan(&first, Ok(result));
        assert!(service.view().scanning);
        assert!(service.view().candidates.is_empty());
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&owner)
            .unwrap();
        lock.try_lock().unwrap();
        drop(lock);
        let result = scan_candidates(&tmp.0, &second).unwrap();
        let store = service.store.as_mut().unwrap();
        store
            .write(&e.document, &None, e.stamp(), Instant::now())
            .unwrap();
        let current = store.path.clone();
        service.shutdown();
        assert!(second.check().is_err());
        service.finish_scan(&second, Ok(result));
        assert!(!service.view().scanning);
        assert!(service.view().candidates.is_empty());
        assert!(!current.exists());
        assert!(source.exists());
        assert!(!service.install_store(&second, RecoveryStore::create(&tmp.0).unwrap()));
        assert_eq!(service.store.as_ref().unwrap().path, current);
        assert!(scan_candidates(&tmp.0, &second).is_err());
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(owner)
            .unwrap();
        lock.try_lock().unwrap();
    }
    #[test]
    fn scan_failures_finish_status_and_drop_cancels_outstanding_job() {
        let tmp = Temp::new();
        let mut service = RecoveryService::pending();
        let failed = service.begin_scan();
        service.finish_scan(&failed, Err("initialization failed".into()));
        assert!(!service.view().scanning);
        assert!(service.store.is_none());
        assert_eq!(
            service.view().error.as_deref(),
            Some("initialization failed")
        );
        let retry = service.begin_scan();
        assert!(service.install_store(&retry, RecoveryStore::create(&tmp.0).unwrap()));
        assert!(service.error.is_none());
        service.finish_scan(&retry, Err("scan failed".into()));
        assert!(!service.view().scanning);
        assert_eq!(service.view().error.as_deref(), Some("scan failed"));
        assert!(service.store.is_some());
        let pending = service.begin_scan();
        drop(service);
        assert!(pending.check().is_err());
    }
    #[test]
    fn task_reads_document_and_recovery_without_consuming_cancelled_source() {
        let tmp = Temp::new();
        let e = Editor::new();
        let path = tmp.0.join("input.u3d");
        fs::write(&path, e.document.file_bytes().unwrap()).unwrap();
        let tasks = crate::load_task::Tasks::default();
        let guard = tasks.begin(crate::load_task::Kind::Open).unwrap();
        let document = read_document_with_task(&path, &guard.task).unwrap();
        assert_eq!(document, e.document);
        assert_eq!(
            tasks.snapshot().unwrap().phase,
            crate::load_task::Phase::Parse
        );
        drop(guard);
        let root = tmp.0.join("recovery");
        let mut store = RecoveryStore::new(&root).unwrap();
        store
            .write(&e.document, &None, e.stamp(), Instant::now())
            .unwrap();
        drop(store);
        let store = RecoveryStore::new(&root).unwrap();
        let id = store.candidates[0].info.id.clone();
        let checkpoint = store.candidates[0].path.clone();
        let original = fs::read(&checkpoint).unwrap();
        let guard = tasks.begin(crate::load_task::Kind::Recovery).unwrap();
        assert_eq!(
            store.recover_with_task(&id, &guard.task).unwrap().document,
            e.document
        );
        tasks.cancel(&tasks.snapshot().unwrap().id);
        assert!(store
            .recover_with_task(&id, &guard.task)
            .err()
            .unwrap()
            .contains("LOAD_CANCELLED"));
        assert_eq!(fs::read(checkpoint).unwrap(), original);
        assert_eq!(store.candidates.len(), 1);
    }
    #[test]
    fn stamps_reject_replacement_and_preserve_edits_made_during_save() {
        let mut e = Editor::new();
        let initial = e.stamp();
        e.select(None);
        e.execute(Command::Camera {
            yaw: 1.,
            pitch: 0.3,
            distance: 5.,
        })
        .unwrap();
        assert_eq!(e.stamp(), initial);
        edit(&mut e);
        let saving = e.stamp();
        edit(&mut e);
        assert!(e.ensure_unchanged(saving).is_err());
        e.saved(saving, "scene.u3d".into()).unwrap();
        assert!(e.dirty);
        e.undo();
        assert!(e.ensure_unchanged(saving).is_err()); // edit+undo still fences an asynchronous open
        e.redo();
        let latest = e.stamp();
        e.saved(latest, "scene.u3d".into()).unwrap();
        assert!(!e.dirty);
        e.replace_document(Document::default(), Some("other.u3d".into()), false);
        assert!(e.saved(latest, "scene.u3d".into()).is_err());
        assert_eq!(e.path, Some("other.u3d".into()));
    }

    #[test]
    fn checkpoint_scheduling_ignores_camera_selection_and_drag_previews() {
        let tmp = Temp::new();
        let mut store = RecoveryStore::new(&tmp.0).unwrap();
        let mut e = Editor::new();
        let due = store.last_attempt + INTERVAL;
        assert!(!store.due(&e, due));
        edit(&mut e);
        assert!(!store.due(&e, due - Duration::from_secs(1)));
        assert!(store.due(&e, due));
        store.write(&e.document, &e.path, e.stamp(), due).unwrap();
        e.select(Some(1));
        assert!(!store.due(&e, due + INTERVAL));
        e.tool = gizmo::Tool::Move;
        let h = gizmo::handles(&e, [800., 600.]).unwrap();
        let stamp = e.stamp();
        e.execute(Command::TransformBegin {
            vertex_snap: false,
            token: "checkpoint-test".into(),
            x: h.origin[0],
            y: h.origin[1],
            width: 800.,
            height: 600.,
        })
        .unwrap();
        e.execute(Command::TransformUpdate {
            vertex_snap: false,
            token: "checkpoint-test".into(),
            points: vec![[h.origin[0] + 50., h.origin[1]]],
            snap_toggle: false,
        })
        .unwrap();
        assert!(e.transform_drag.is_some());
        assert!(!store.due(&e, due + INTERVAL));
        e.finish_transform(true);
        assert_eq!(stamp, e.stamp());
        assert!(!store.due(&e, due + INTERVAL));
        edit(&mut e);
        assert!(store.due(&e, due + INTERVAL));
    }

    #[test]
    fn checkpoints_embed_meshes_preserve_original_and_exclude_live_sessions() {
        let tmp = Temp::new();
        let original = tmp.0.join("original.u3d");
        fs::write(&original, b"do not change").unwrap();
        let mut e = Editor::new();
        e.import_usd(crate::usd::tests::fixture()).unwrap();
        e.path = Some(original.clone());
        let mut store = RecoveryStore::new(&tmp.0).unwrap();
        store
            .write(&e.document, &e.path, e.stamp(), Instant::now())
            .unwrap();
        let concurrent = RecoveryStore::new(&tmp.0).unwrap();
        assert!(concurrent.candidates.is_empty());
        drop(concurrent);
        drop(store);
        let mut restarted = RecoveryStore::new(&tmp.0).unwrap();
        assert_eq!(restarted.candidates.len(), 1);
        let id = restarted.candidates[0].info.id.clone();
        let saved = restarted.recover(&id).unwrap();
        assert_eq!(saved.document, e.document);
        assert_eq!(saved.original_path, e.path);
        assert!(restarted.recover("../../original.u3d").is_err());
        e.replace_document(saved.document, None, true);
        assert!(e.dirty && e.path.is_none() && e.undo.is_empty());
        restarted
            .write(&e.document, &e.path, e.stamp(), Instant::now())
            .unwrap();
        restarted.consumed(&id).unwrap();
        assert!(restarted.candidates.is_empty());
        assert_eq!(fs::read(original).unwrap(), b"do not change");
        restarted.clear().unwrap();
        drop(restarted);
        assert!(RecoveryStore::new(&tmp.0).unwrap().candidates.is_empty());
    }

    #[test]
    fn invalid_checkpoint_is_retained_and_atomic_failure_preserves_previous_file() {
        let tmp = Temp::new();
        let mut store = RecoveryStore::new(&tmp.0).unwrap();
        let mut e = Editor::new();
        edit(&mut e);
        store
            .write(&e.document, &None, e.stamp(), Instant::now())
            .unwrap();
        let path = store.path.clone();
        let previous = fs::read(&path).unwrap();
        let directory = tmp.0.join("directory");
        fs::create_dir(&directory).unwrap();
        assert!(atomic_write(&directory, b"invalid destination").is_err());
        assert_eq!(fs::read(&path).unwrap(), previous);
        assert!(!fs::read_dir(&tmp.0).unwrap().any(|entry| entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|x| x == "tmp")));
        fs::write(&path, b"broken").unwrap();
        drop(store);
        let reopened = RecoveryStore::new(&tmp.0).unwrap();
        assert!(reopened.candidates.is_empty() && reopened.scan_error.is_some());
        assert_eq!(fs::read(path).unwrap(), b"broken");
        let big = tmp.0.join("too-large.u3d");
        File::create(&big).unwrap().set_len(MAX_FILE + 1).unwrap();
        assert!(read_document(&big).is_err());
    }

    #[test]
    fn file_operation_gate_releases_on_cancel_and_failure() {
        let flag = AtomicBool::new(false);
        {
            let _guard = FileOperation::begin(&flag).unwrap();
            assert!(FileOperation::begin(&flag).is_err());
        }
        let fail = || -> Result<(), String> {
            let _guard = FileOperation::begin(&flag)?;
            Err("cancelled".into())
        };
        assert!(fail().is_err());
        assert!(FileOperation::begin(&flag).is_ok());
    }

    // Invoked only by the parent process test; kill() deliberately bypasses all Drop handlers.
    #[test]
    #[ignore = "Helper for abnormal_exit_releases_session_lock_and_restores_meshes"]
    fn crash_writer_process() {
        let root = PathBuf::from(
            std::env::var_os("U3D_CRASH_TEST_ROOT")
                .expect("parent supplies private fixture directory"),
        );
        let mut store = RecoveryStore::new(&root).unwrap();
        let mut editor = Editor::new();
        editor.import_usd(crate::usd::tests::fixture()).unwrap();
        store
            .write(&editor.document, &None, editor.stamp(), Instant::now())
            .unwrap();
        fs::write(root.join("ready"), b"ready").unwrap();
        loop {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn abnormal_exit_releases_session_lock_and_restores_meshes() {
        let tmp = Temp::new();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "persistence::tests::crash_writer_process",
                "--ignored",
            ])
            .env("U3D_CRASH_TEST_ROOT", &tmp.0)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !tmp.0.join("ready").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if !tmp.0.join("ready").exists() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child never wrote checkpoint");
        }
        assert!(RecoveryStore::new(&tmp.0).unwrap().candidates.is_empty());
        child.kill().unwrap();
        child.wait().unwrap();
        let reopened = RecoveryStore::new(&tmp.0).unwrap();
        assert_eq!(reopened.candidates.len(), 1);
        let recovered = reopened.recover(&reopened.candidates[0].info.id).unwrap();
        assert_eq!(recovered.document.meshes.len(), 1);
        assert_eq!(recovered.document.objects.len(), 2);
        recovered.document.validate().unwrap();
    }
}
