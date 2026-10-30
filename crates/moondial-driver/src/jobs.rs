//! The scheduler: jobs that run when their time comes.
//!
//! A job is `{id, due, kind, payload}`. Scheduling one appends `job.scheduled` to the
//! log; running it appends `job.done` (or `job.failed` with the reason). The pending set
//! is a projection over those events, rebuilt on open, so jobs survive a restart and
//! replay never runs one twice. A module registers a handler per kind at boot, as it
//! registers commands; the driver's tick runs every due job through its handler, oldest
//! first, and a world whose clock is frozen runs none until it thaws.
//!
//! Handlers mutate the world only through [`World::record`] and [`World::schedule`],
//! so everything a job does is in the log like everything else.

use crate::world::World;
use moondial_log::Actor;
use moondial_store::{ClockId, WORLD_CLOCK};
use moondial_time::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// A job's identity: the sequence number of the event that scheduled it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct JobId(pub u64);

/// A scheduled job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub due: Timestamp,
    pub kind: String,
    pub payload: Value,
}

/// What runs a job. It may read the world, record events, and schedule more jobs.
pub type JobHandler = Box<dyn Fn(&mut World, &Job) -> Result<(), String> + Send + Sync>;

/// The pending jobs, keyed by `(due, id)` so the earliest comes first.
#[derive(Debug, Default)]
pub struct Jobs {
    pending: BTreeMap<(Timestamp, JobId), Job>,
}

impl Jobs {
    /// Apply one log event to the pending set. Non-job events are ignored.
    pub fn apply(&mut self, kind: &str, seq: u64, payload: &Value) {
        match kind {
            "job.scheduled" => {
                let job = Job {
                    id: JobId(seq),
                    due: Timestamp(payload["due"].as_i64().unwrap_or(0)),
                    kind: payload["kind"].as_str().unwrap_or("").to_string(),
                    payload: payload["payload"].clone(),
                };
                self.pending.insert((job.due, job.id), job);
            }
            "job.done" | "job.failed" | "job.cancelled" => {
                if let Some(id) = payload["id"].as_u64() {
                    self.pending.retain(|(_, j), _| j.0 != id);
                }
            }
            _ => {}
        }
    }

    /// Every pending job, earliest first.
    pub fn pending(&self) -> impl Iterator<Item = &Job> {
        self.pending.values()
    }

    /// Whether a job of this kind whose payload matches `pred` is pending.
    pub fn has(&self, kind: &str, pred: impl Fn(&Value) -> bool) -> bool {
        self.pending
            .values()
            .any(|j| j.kind == kind && pred(&j.payload))
    }

    /// Drop a job from the pending set, for the tick that is about to run it.
    fn take(&mut self, job: &Job) {
        self.pending.remove(&(job.due, job.id));
    }

    /// The jobs due at or before `now`, earliest first.
    fn due(&self, now: Timestamp) -> Vec<Job> {
        self.pending
            .values()
            .take_while(|j| j.due <= now)
            .cloned()
            .collect()
    }
}

impl World {
    /// Schedule a job. Its id is the sequence number of the event that recorded it.
    pub fn schedule(
        &mut self,
        kind: &str,
        due: Timestamp,
        payload: Value,
    ) -> Result<JobId, String> {
        let event = self.note(
            Actor::Driver,
            "job.scheduled",
            json!({"due": due.0, "kind": kind, "payload": payload}),
        )?;
        self.jobs
            .apply("job.scheduled", event.seq.0, &event.payload);
        Ok(JobId(event.seq.0))
    }

    /// Schedule a job unless one of the same kind with a matching payload is pending.
    pub fn schedule_once(
        &mut self,
        kind: &str,
        due: Timestamp,
        payload: Value,
        same: impl Fn(&Value) -> bool,
    ) -> Result<Option<JobId>, String> {
        if self.jobs.has(kind, same) {
            return Ok(None);
        }
        self.schedule(kind, due, payload).map(Some)
    }

    /// Cancel every pending job of `kind` whose payload matches.
    pub fn cancel(&mut self, kind: &str, pred: impl Fn(&Value) -> bool) -> Result<usize, String> {
        let ids: Vec<JobId> = self
            .jobs
            .pending()
            .filter(|j| j.kind == kind && pred(&j.payload))
            .map(|j| j.id)
            .collect();
        for id in &ids {
            let event = self.note(Actor::Driver, "job.cancelled", json!({"id": id.0}))?;
            self.jobs
                .apply("job.cancelled", event.seq.0, &event.payload);
        }
        Ok(ids.len())
    }

    /// Register a job handler. At boot only, like commands.
    pub fn on_job(&mut self, kind: &str, handler: JobHandler) {
        std::sync::Arc::get_mut(&mut self.job_handlers)
            .expect("job handlers are registered at boot, not inside a tick")
            .insert(kind.to_string(), handler);
    }

    /// Whether the world's own clock is paused.
    pub fn frozen(&self) -> bool {
        let now = self.now();
        self.store
            .clock(&ClockId::from(WORLD_CLOCK))
            .is_some_and(|c| c.is_paused_at(now))
    }

    /// Run every job due by now, earliest first. Returns how many ran. A frozen world
    /// runs none; a job without a handler fails and is dropped, with the reason logged.
    pub fn tick(&mut self) -> usize {
        if self.frozen() {
            return 0;
        }
        let sweepers = std::sync::Arc::clone(&self.sweepers);
        for s in sweepers.iter() {
            if let Err(e) = s(self) {
                eprintln!("tick sweep: {e}");
            }
        }
        let now = self.now();
        let due = self.jobs.due(now);
        let handlers = std::sync::Arc::clone(&self.job_handlers);
        let mut ran = 0;
        for job in due {
            // Out of the pending set before it runs, so a handler that reschedules
            // itself does not see itself as already pending.
            self.jobs.take(&job);
            let outcome = match handlers.get(&job.kind) {
                Some(h) => h(self, &job),
                None => Err(format!("no handler for job kind {}", job.kind)),
            };
            let (kind, payload) = match outcome {
                Ok(()) => ("job.done", json!({"id": job.id.0, "kind": job.kind})),
                Err(e) => (
                    "job.failed",
                    json!({"id": job.id.0, "kind": job.kind, "error": e}),
                ),
            };
            if let Ok(event) = self.note(Actor::Driver, kind, payload) {
                self.jobs.apply(kind, event.seq.0, &event.payload);
            }
            ran += 1;
        }
        ran
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::LogBacking;
    use moondial_log::WorldId;
    use moondial_store::{Registry, Store};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn open(dir: &std::path::Path, at: Timestamp, counter: Arc<AtomicUsize>) -> World {
        let mut w = World::open(
            WorldId("jobs".into()),
            Store::new(Registry::new()),
            LogBacking::File(dir.to_path_buf()),
        )
        .unwrap();
        w.fixed_now = Some(at);
        let c = Arc::clone(&counter);
        w.on_job(
            "count",
            Box::new(move |_, job| {
                c.fetch_add(
                    job.payload["by"].as_u64().unwrap_or(1) as usize,
                    Ordering::SeqCst,
                );
                Ok(())
            }),
        );
        w.on_job("boom", Box::new(|_, _| Err("it broke".into())));
        w
    }

    #[test]
    fn jobs_run_when_due_in_order_and_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let mut w = open(dir.path(), Timestamp(1000), Arc::clone(&counter));
        w.schedule("count", Timestamp(3000), json!({"by": 10}))
            .unwrap();
        w.schedule("count", Timestamp(2000), json!({"by": 1}))
            .unwrap();
        w.schedule("boom", Timestamp(2500), json!({})).unwrap();
        assert_eq!(
            w.schedule_once("count", Timestamp(2000), json!({"by": 1}), |p| p["by"] == 1)
                .unwrap(),
            None,
            "already pending"
        );
        assert_eq!(w.tick(), 0, "nothing due at 1000");
        w.fixed_now = Some(Timestamp(2000));
        assert_eq!(w.tick(), 1);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        drop(w);

        // Reopen at 2600: the failed job and the 3000 job are still pending; done one is not.
        let mut w = open(dir.path(), Timestamp(2600), Arc::clone(&counter));
        let pending: Vec<&str> = w.jobs.pending().map(|j| j.kind.as_str()).collect();
        assert_eq!(pending, vec!["boom", "count"]);
        assert_eq!(w.tick(), 1, "boom runs, fails, and is dropped");
        let last = w.recent_events(1).pop().unwrap();
        assert_eq!(last.kind, "job.failed");
        assert_eq!(last.payload["error"], "it broke");
        assert_eq!(w.jobs.pending().count(), 1);
        w.fixed_now = Some(Timestamp(5000));
        assert_eq!(w.tick(), 1);
        assert_eq!(counter.load(Ordering::SeqCst), 11);
        assert_eq!(w.jobs.pending().count(), 0);
        drop(w);
        let w = open(dir.path(), Timestamp(9000), counter);
        assert_eq!(
            w.jobs.pending().count(),
            0,
            "replay does not resurrect finished jobs"
        );
    }

    #[test]
    fn a_frozen_world_runs_nothing_and_cancel_drops_by_payload() {
        let dir = tempfile::tempdir().unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let mut w = open(dir.path(), Timestamp(1000), Arc::clone(&counter));
        w.schedule("count", Timestamp(1000), json!({"by": 1, "room": 7}))
            .unwrap();
        w.schedule("count", Timestamp(1000), json!({"by": 1, "room": 8}))
            .unwrap();
        w.record(
            Actor::Driver,
            moondial_store::StoreEvent::ClockPaused {
                clock: ClockId::from(WORLD_CLOCK),
            },
        )
        .unwrap();
        assert!(w.frozen());
        assert_eq!(w.tick(), 0);
        w.record(
            Actor::Driver,
            moondial_store::StoreEvent::ClockResumed {
                clock: ClockId::from(WORLD_CLOCK),
            },
        )
        .unwrap();
        assert_eq!(w.cancel("count", |p| p["room"] == 7).unwrap(), 1);
        assert_eq!(w.tick(), 1);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
