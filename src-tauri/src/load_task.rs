//! Small, Rust-owned load status. Operation IDs fence delayed cancellation requests.
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Open,
    Recovery,
    Usd,
}
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Read,
    Convert,
    Validate,
    Vertices,
    Surfaces,
    Instances,
    Commit,
}
#[derive(Clone, serde::Serialize)]
pub struct Snapshot {
    pub id: String,
    pub kind: Kind,
    pub phase: Phase,
    pub completed: usize,
    pub total: usize,
    pub cancelling: bool,
    pub cancellable: bool,
}
struct Active {
    snapshot: Snapshot,
    cancel: Arc<AtomicBool>,
}
#[derive(Default)]
pub struct Tasks {
    next: AtomicU64,
    active: Arc<Mutex<Option<Active>>>,
}
#[derive(Clone)]
pub struct Task {
    id: String,
    active: Arc<Mutex<Option<Active>>>,
    cancel: Arc<AtomicBool>,
}
pub struct Guard {
    pub task: Task,
}
impl Tasks {
    pub fn begin(&self, kind: Kind) -> Result<Guard, String> {
        let mut active = self.active.lock().map_err(|e| e.to_string())?;
        if active.is_some() {
            return Err("Load operation already running".into());
        }
        let id = (self.next.fetch_add(1, Ordering::Relaxed) + 1).to_string();
        let cancel = Arc::new(AtomicBool::new(false));
        *active = Some(Active {
            snapshot: Snapshot {
                id: id.clone(),
                kind,
                phase: Phase::Read,
                completed: 0,
                total: 0,
                cancelling: false,
                cancellable: true,
            },
            cancel: cancel.clone(),
        });
        Ok(Guard {
            task: Task {
                id,
                active: self.active.clone(),
                cancel,
            },
        })
    }
    pub fn snapshot(&self) -> Option<Snapshot> {
        self.active
            .lock()
            .ok()?
            .as_ref()
            .map(|a| a.snapshot.clone())
    }
    pub fn cancel(&self, id: &str) -> bool {
        let Ok(mut active) = self.active.lock() else {
            return false;
        };
        let Some(active) = active
            .as_mut()
            .filter(|a| a.snapshot.id == id && a.snapshot.cancellable)
        else {
            return false;
        };
        active.cancel.store(true, Ordering::Relaxed);
        active.snapshot.cancelling = true;
        true
    }
}
impl Task {
    pub fn cancel_flag(&self) -> &AtomicBool {
        &self.cancel
    }
    pub fn cancel_arc(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
    pub fn check(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::Relaxed) {
            Err("LOAD_CANCELLED".into())
        } else {
            Ok(())
        }
    }
    pub fn progress(&self, phase: Phase, completed: usize, total: usize) -> Result<(), String> {
        self.check()?;
        let mut active = self.active.lock().map_err(|e| e.to_string())?;
        let current = active
            .as_mut()
            .filter(|a| a.snapshot.id == self.id && a.snapshot.cancellable)
            .ok_or("LOAD_CANCELLED")?;
        current.snapshot.phase = phase;
        current.snapshot.completed = completed.min(total);
        current.snapshot.total = total;
        self.check()
    }
    /// Linearize cancellation versus publication while holding the same lock as cancel().
    pub fn commit(&self) -> Result<(), String> {
        let mut active = self.active.lock().map_err(|e| e.to_string())?;
        let current = active
            .as_mut()
            .filter(|a| a.snapshot.id == self.id)
            .ok_or("LOAD_CANCELLED")?;
        self.check()?;
        current.snapshot.phase = Phase::Commit;
        current.snapshot.completed = 0;
        current.snapshot.total = 0;
        current.snapshot.cancellable = false;
        Ok(())
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.task.active.lock() {
            if active
                .as_ref()
                .is_some_and(|a| a.snapshot.id == self.task.id)
            {
                if active.as_ref().is_some_and(|a| a.snapshot.cancellable) {
                    self.task.cancel.store(true, Ordering::Relaxed);
                }
                *active = None;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commit_and_cancel_have_one_winner() {
        for _ in 0..32 {
            let tasks = Tasks::default();
            let guard = tasks.begin(Kind::Open).unwrap();
            let barrier = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                let task = &guard.task;
                let worker = scope.spawn(|| {
                    barrier.wait();
                    task.commit().is_ok()
                });
                barrier.wait();
                let cancelled = tasks.cancel(&guard.task.id);
                assert_ne!(worker.join().unwrap(), cancelled);
            });
        }
    }
    #[test]
    fn cancellation_is_scoped_and_commit_wins_only_if_not_cancelled() {
        let tasks = Tasks::default();
        let abandoned = tasks.begin(Kind::Usd).unwrap();
        let lingering = abandoned.task.clone();
        drop(abandoned);
        assert!(lingering.check().is_err());
        assert!(tasks.snapshot().is_none());
        let first = tasks.begin(Kind::Open).unwrap();
        let old = first.task.clone();
        assert!(tasks.begin(Kind::Usd).is_err());
        old.progress(Phase::Vertices, 2, 3).unwrap();
        assert_eq!(tasks.snapshot().unwrap().completed, 2);
        assert!(tasks.cancel(&old.id));
        assert!(old.commit().is_err());
        drop(first);
        assert!(old.check().is_err());
        assert!(tasks.snapshot().is_none());
        let next = tasks.begin(Kind::Recovery).unwrap();
        assert!(!tasks.cancel(&old.id));
        assert!(old.progress(Phase::Read, 0, 0).is_err());
        next.task.commit().unwrap();
        assert!(!tasks.cancel(&next.task.id));
        assert!(next.task.check().is_ok());
        assert!(!tasks.snapshot().unwrap().cancellable);
        assert!(next.task.progress(Phase::Read, 0, 0).is_err());
        drop(next);
        assert!(tasks.snapshot().is_none());
    }
}
