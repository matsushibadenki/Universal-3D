//! Small, Rust-owned load status. Operation IDs fence delayed cancellation requests.
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Open,
    Recovery,
    Usd,
    Snap,
}
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Read,
    Parse,
    Extract,
    Triangulate,
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
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Running,
    Success,
    Cancelled,
    Failed,
}
#[derive(Clone, serde::Serialize)]
pub struct PhaseTiming {
    pub phase: &'static str,
    pub elapsed_us: u64,
    pub entries: u32,
}
#[derive(Clone, serde::Serialize)]
pub struct Profile {
    pub id: String,
    pub kind: Kind,
    pub outcome: Outcome,
    pub commit_started: bool,
    pub total_us: u64,
    pub phases: Vec<PhaseTiming>,
}
fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}
fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Read => "read",
        Phase::Parse => "parse",
        Phase::Extract => "extract",
        Phase::Triangulate => "triangulate",
        Phase::Convert => "convert",
        Phase::Validate => "validate",
        Phase::Vertices => "vertices",
        Phase::Surfaces => "surfaces",
        Phase::Instances => "instances",
        Phase::Commit => "commit",
    }
}
struct Timing {
    started: Instant,
    changed: Instant,
    phase: &'static str,
    durations: BTreeMap<&'static str, (Duration, u32)>,
    finished: Option<(Outcome, Duration)>,
    committed: bool,
    #[cfg(test)]
    trace: bool,
}
impl Timing {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            changed: now,
            phase: "read",
            durations: BTreeMap::from([("read", (Duration::ZERO, 1))]),
            finished: None,
            committed: false,
            #[cfg(test)]
            trace: false,
        }
    }
    fn transition(&mut self, phase: &'static str) {
        if self.finished.is_some() || phase == self.phase {
            return;
        }
        let now = Instant::now();
        self.durations.entry(self.phase).or_default().0 += now - self.changed;
        self.phase = phase;
        self.changed = now;
        self.durations.entry(phase).or_default().1 += 1;
        #[cfg(test)]
        self.emit(phase);
    }
    fn finish(&mut self, outcome: Outcome) {
        if self.finished.is_some() {
            return;
        }
        let now = Instant::now();
        self.durations.entry(self.phase).or_default().0 += now - self.changed;
        self.finished = Some((outcome, now - self.started));
        #[cfg(test)]
        self.emit("done");
    }
    fn snapshot(&self, id: String, kind: Kind) -> Profile {
        let mut durations = self.durations.clone();
        let (outcome, total) = if let Some(done) = self.finished {
            done
        } else {
            let now = Instant::now();
            durations.entry(self.phase).or_default().0 += now - self.changed;
            (Outcome::Running, now - self.started)
        };
        Profile {
            id,
            kind,
            outcome,
            commit_started: self.committed,
            total_us: micros(total),
            phases: durations
                .into_iter()
                .map(|(phase, (elapsed, entries))| PhaseTiming {
                    phase,
                    elapsed_us: micros(elapsed),
                    entries,
                })
                .collect(),
        }
    }
    #[cfg(test)]
    fn emit(&self, phase: &'static str) {
        if self.trace {
            use std::io::Write;
            println!(
                "LOAD_STAGE {}",
                serde_json::json!({"phase":phase,"at_us":micros(self.finished.map_or_else(|| self.started.elapsed(), |(_, total)| total)),"pid":std::process::id()})
            );
            let _ = std::io::stdout().flush();
        }
    }
}
struct Active {
    snapshot: Snapshot,
    cancel: Arc<AtomicBool>,
    timing: Timing,
}
#[derive(Default)]
pub struct Tasks {
    next: AtomicU64,
    active: Arc<Mutex<Option<Active>>>,
    last: Arc<Mutex<Option<Profile>>>,
}
#[derive(Clone)]
pub struct Task {
    id: String,
    active: Arc<Mutex<Option<Active>>>,
    last: Arc<Mutex<Option<Profile>>>,
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
            timing: Timing::new(),
        });
        Ok(Guard {
            task: Task {
                id,
                active: self.active.clone(),
                last: self.last.clone(),
                cancel,
            },
        })
    }
    pub fn profile(&self) -> Option<Profile> {
        let active = self.active.lock().ok()?;
        if let Some(active) = active.as_ref() {
            return Some(
                active
                    .timing
                    .snapshot(active.snapshot.id.clone(), active.snapshot.kind),
            );
        }
        self.last.lock().ok()?.clone()
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
    /// CPU notification and render submission only; never implies GPU completion.
    pub fn finishing(&self) -> Result<(), String> {
        let mut active = self.active.lock().map_err(|e| e.to_string())?;
        let current = active
            .as_mut()
            .filter(|a| a.snapshot.id == self.id && !a.snapshot.cancellable)
            .ok_or("LOAD_CANCELLED")?;
        current.timing.transition("notify_draw");
        Ok(())
    }
    pub fn succeed(&self) -> Result<(), String> {
        let mut active = self.active.lock().map_err(|e| e.to_string())?;
        let current = active
            .as_mut()
            .filter(|a| a.snapshot.id == self.id && !a.snapshot.cancellable)
            .ok_or("LOAD_CANCELLED")?;
        current.timing.finish(Outcome::Success);
        Ok(())
    }
    #[cfg(test)]
    pub fn trace(&self) {
        let mut active = self.active.lock().unwrap();
        let current = active
            .as_mut()
            .filter(|a| a.snapshot.id == self.id)
            .unwrap();
        current.timing.trace = true;
        current.timing.emit(current.timing.phase);
    }
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
        current.timing.transition(phase_name(phase));
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
        current.timing.transition("commit");
        current.timing.committed = true;
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
                let mut current = active.take().unwrap();
                let outcome = if self.task.cancel.load(Ordering::Relaxed) {
                    Outcome::Cancelled
                } else {
                    Outcome::Failed
                };
                current.timing.finish(outcome);
                if let Ok(mut last) = self.task.last.lock() {
                    *last = Some(
                        current
                            .timing
                            .snapshot(self.task.id.clone(), current.snapshot.kind),
                    );
                }
                if current.snapshot.cancellable {
                    self.task.cancel.store(true, Ordering::Relaxed);
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_accumulates_reentered_phases_and_preserves_success_after_guard_drop() {
        let tasks = Tasks::default();
        let guard = tasks.begin(Kind::Open).unwrap();
        guard.task.progress(Phase::Vertices, 0, 2).unwrap();
        guard.task.progress(Phase::Surfaces, 0, 2).unwrap();
        guard.task.progress(Phase::Vertices, 1, 2).unwrap();
        let running = tasks.profile().unwrap();
        assert_eq!(running.outcome, Outcome::Running);
        assert_eq!(
            running
                .phases
                .iter()
                .find(|p| p.phase == "vertices")
                .unwrap()
                .entries,
            2
        );
        guard.task.commit().unwrap();
        guard.task.finishing().unwrap();
        guard.task.succeed().unwrap();
        let done = tasks.profile().unwrap();
        assert_eq!(done.outcome, Outcome::Success);
        assert!(done.commit_started);
        let total = done.phases.iter().map(|p| p.elapsed_us).sum::<u64>();
        assert!(total <= done.total_us && done.total_us - total <= done.phases.len() as u64);
        drop(guard);
        let retained = tasks.profile().unwrap();
        assert_eq!(retained.total_us, done.total_us);
        assert_eq!(retained.outcome, Outcome::Success);
        assert!(tasks.snapshot().is_none());
    }
    #[test]
    fn profiles_distinguish_cancel_failure_and_commit_failure_and_fence_late_success() {
        let tasks = Tasks::default();
        let guard = tasks.begin(Kind::Usd).unwrap();
        let old = guard.task.clone();
        let id = tasks.snapshot().unwrap().id;
        tasks.cancel(&id);
        drop(guard);
        assert_eq!(tasks.profile().unwrap().outcome, Outcome::Cancelled);
        let next = tasks.begin(Kind::Recovery).unwrap();
        assert!(old.succeed().is_err());
        assert_eq!(tasks.profile().unwrap().outcome, Outcome::Running);
        drop(next);
        assert_eq!(tasks.profile().unwrap().outcome, Outcome::Failed);
        assert!(!tasks.profile().unwrap().commit_started);
        let next = tasks.begin(Kind::Open).unwrap();
        next.task.commit().unwrap();
        drop(next); // e.g. checkpoint write or draw failed after commit started
        assert_eq!(tasks.profile().unwrap().outcome, Outcome::Failed);
        assert!(tasks.profile().unwrap().commit_started);
    }
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
