//! The Moondial event log.
//!
//! Every change to a world is an appended [`Event`]; current state is a projection over
//! the log. That gives replay, audit, rollback after a bad module swap, and an
//! observation feed for agents, all from one structure. Only the driver appends; modules
//! and clients read.
//!
//! The log is per world. Within a world, events have a dense sequence ([`Seq`]) assigned
//! at append, and the timestamp the driver stamped them with. Sequence is the order of
//! truth; timestamps may repeat within a millisecond.
//!
//! [`EventLog`] is the trait the driver codes against. [`MemoryLog`] is the in-process
//! implementation used by tests, tools and single-process worlds; the Postgres
//! implementation lives in the driver and satisfies the same trait and the same tests.
//!
//! A [`Projection`] is anything that folds events into state. [`replay`] drives a
//! projection from a cursor, which is how a store is rebuilt, how a bad swap is undone,
//! and how the property tests check lazy evaluation against the record.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use moondial_time::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// A world's name: the tenant the event belongs to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorldId(pub String);

impl From<&str> for WorldId {
    fn from(s: &str) -> Self {
        WorldId(s.to_string())
    }
}

impl fmt::Display for WorldId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Position of an event in its world's log. Dense, starting at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(pub u64);

/// Who or what caused an event: a citizen, an agent, a module, a job, or the driver.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Actor {
    /// A human or agent citizen, by their actor id.
    Citizen(String),
    /// A familiar acting for a citizen: (familiar id, bound citizen id).
    Familiar(String, String),
    /// A module, by name.
    Module(String),
    /// A scheduled job, by name.
    Job(String),
    /// The driver itself (system events, swaps, clocks).
    Driver,
}

/// An event as the driver records it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Position in the world's log.
    pub seq: Seq,
    /// The world the event belongs to.
    pub world: WorldId,
    /// When the driver stamped it.
    pub at: Timestamp,
    /// Event type, namespaced by the module that defines it: `store.entity_created`,
    /// `clock.paused`, `entropic.chunk_earned`.
    pub kind: String,
    /// Who caused it.
    pub actor: Actor,
    /// The event this one was caused by, if any (a command's resolution points at the
    /// command; a concussion points at the death).
    pub cause: Option<Seq>,
    /// The event's data, as the defining module declares it.
    pub payload: serde_json::Value,
}

/// An event before it has a sequence: what a caller hands to [`EventLog::append`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewEvent {
    /// When the driver stamped it.
    pub at: Timestamp,
    /// Event type (see [`Event::kind`]).
    pub kind: String,
    /// Who caused it.
    pub actor: Actor,
    /// The causing event, if any.
    pub cause: Option<Seq>,
    /// The event's data.
    pub payload: serde_json::Value,
}

impl NewEvent {
    /// A convenience constructor for the common case.
    pub fn new(
        at: Timestamp,
        kind: impl Into<String>,
        actor: Actor,
        payload: serde_json::Value,
    ) -> Self {
        NewEvent {
            at,
            kind: kind.into(),
            actor,
            cause: None,
            payload,
        }
    }

    /// Attach the causing event.
    pub fn caused_by(mut self, cause: Seq) -> Self {
        self.cause = Some(cause);
        self
    }
}

/// Errors from log operations.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LogError {
    /// An append carried a timestamp earlier than the world's last event. Clocks only move forward.
    #[error("event at {at} is earlier than the world's last event at {last}")]
    OutOfOrder {
        /// The offending timestamp.
        at: Timestamp,
        /// The world's last timestamp.
        last: Timestamp,
    },
    /// The backing store failed.
    #[error("log storage error: {0}")]
    Storage(String),
}

/// A position to read from: events with `seq > after`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Cursor {
    /// The last sequence already seen; `0` means from the beginning.
    pub after: u64,
}

impl Cursor {
    /// From the beginning of the log.
    pub const START: Cursor = Cursor { after: 0 };
    /// A cursor positioned just after `seq`.
    pub fn after(seq: Seq) -> Cursor {
        Cursor { after: seq.0 }
    }
}

/// The append-only, per-world event log.
pub trait EventLog {
    /// Append an event to a world's log and return it with its sequence assigned.
    fn append(&mut self, world: &WorldId, event: NewEvent) -> Result<Event, LogError>;

    /// Read up to `limit` events after the cursor, in sequence order.
    fn read(&self, world: &WorldId, cursor: Cursor, limit: usize) -> Result<Vec<Event>, LogError>;

    /// The last sequence in a world's log, or `None` if it is empty.
    fn head(&self, world: &WorldId) -> Result<Option<Seq>, LogError>;

    /// Read every event after the cursor.
    fn read_all(&self, world: &WorldId, cursor: Cursor) -> Result<Vec<Event>, LogError> {
        let mut out = Vec::new();
        let mut cur = cursor;
        loop {
            let batch = self.read(world, cur, 1024)?;
            if batch.is_empty() {
                return Ok(out);
            }
            cur = Cursor::after(batch.last().expect("non-empty").seq);
            out.extend(batch);
        }
    }
}

/// Something that folds events into state.
pub trait Projection {
    /// The error a projection can raise while applying an event it understands.
    type Error: fmt::Debug;
    /// Apply one event. Events of kinds the projection does not know are ignored by
    /// convention, so several projections can share a log.
    fn apply(&mut self, event: &Event) -> Result<(), Self::Error>;
}

/// Drive a projection from a cursor to the head of a world's log. Returns the cursor
/// positioned after the last event applied.
pub fn replay<P: Projection>(
    log: &dyn EventLog,
    world: &WorldId,
    cursor: Cursor,
    projection: &mut P,
) -> Result<Cursor, ReplayError<P::Error>> {
    let events = log.read_all(world, cursor).map_err(ReplayError::Log)?;
    let mut last = cursor;
    for ev in &events {
        projection.apply(ev).map_err(|e| ReplayError::Projection {
            at: ev.seq,
            error: e,
        })?;
        last = Cursor::after(ev.seq);
    }
    Ok(last)
}

/// Why a replay stopped.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError<E: fmt::Debug> {
    /// The log could not be read.
    #[error("log: {0}")]
    Log(LogError),
    /// The projection rejected an event.
    #[error("projection failed at seq {at}: {error:?}")]
    Projection {
        /// The event the projection rejected.
        at: Seq,
        /// Its error.
        error: E,
    },
}

/// An in-process event log. Deterministic and cheap; used by tests, tools and
/// single-process worlds.
#[derive(Debug, Default, Clone)]
pub struct MemoryLog {
    worlds: BTreeMap<WorldId, Vec<Event>>,
}

impl MemoryLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// The worlds that have at least one event.
    pub fn worlds(&self) -> impl Iterator<Item = &WorldId> {
        self.worlds.keys()
    }
}

impl EventLog for MemoryLog {
    fn append(&mut self, world: &WorldId, event: NewEvent) -> Result<Event, LogError> {
        let entries = self.worlds.entry(world.clone()).or_default();
        if let Some(last) = entries.last() {
            if event.at < last.at {
                return Err(LogError::OutOfOrder {
                    at: event.at,
                    last: last.at,
                });
            }
        }
        let ev = Event {
            seq: Seq(entries.len() as u64 + 1),
            world: world.clone(),
            at: event.at,
            kind: event.kind,
            actor: event.actor,
            cause: event.cause,
            payload: event.payload,
        };
        entries.push(ev.clone());
        Ok(ev)
    }

    fn read(&self, world: &WorldId, cursor: Cursor, limit: usize) -> Result<Vec<Event>, LogError> {
        let Some(entries) = self.worlds.get(world) else {
            return Ok(Vec::new());
        };
        let start = (cursor.after as usize).min(entries.len());
        Ok(entries[start..].iter().take(limit).cloned().collect())
    }

    fn head(&self, world: &WorldId) -> Result<Option<Seq>, LogError> {
        Ok(self
            .worlds
            .get(world)
            .and_then(|e| e.last().map(|ev| ev.seq)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(at: i64, kind: &str) -> NewEvent {
        NewEvent::new(Timestamp(at), kind, Actor::Driver, json!({"k": kind}))
    }

    #[test]
    fn sequences_are_dense_per_world() {
        let mut log = MemoryLog::new();
        let a = WorldId::from("a");
        let b = WorldId::from("b");
        assert_eq!(log.append(&a, ev(1, "x")).unwrap().seq, Seq(1));
        assert_eq!(log.append(&a, ev(1, "y")).unwrap().seq, Seq(2));
        assert_eq!(log.append(&b, ev(5, "z")).unwrap().seq, Seq(1));
        assert_eq!(log.head(&a).unwrap(), Some(Seq(2)));
        assert_eq!(log.head(&WorldId::from("none")).unwrap(), None);
    }

    #[test]
    fn time_only_moves_forward() {
        let mut log = MemoryLog::new();
        let w = WorldId::from("w");
        log.append(&w, ev(10, "x")).unwrap();
        assert_eq!(
            log.append(&w, ev(9, "y")).unwrap_err(),
            LogError::OutOfOrder {
                at: Timestamp(9),
                last: Timestamp(10)
            }
        );
        log.append(&w, ev(10, "same-ms-is-fine")).unwrap();
    }

    #[test]
    fn cursors_page_through_the_log() {
        let mut log = MemoryLog::new();
        let w = WorldId::from("w");
        for i in 0..10 {
            log.append(&w, ev(i, "x")).unwrap();
        }
        let first = log.read(&w, Cursor::START, 3).unwrap();
        assert_eq!(
            first.iter().map(|e| e.seq.0).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        let next = log
            .read(&w, Cursor::after(first.last().unwrap().seq), 3)
            .unwrap();
        assert_eq!(
            next.iter().map(|e| e.seq.0).collect::<Vec<_>>(),
            vec![4, 5, 6]
        );
        assert_eq!(log.read_all(&w, Cursor { after: 8 }).unwrap().len(), 2);
        assert!(log.read(&w, Cursor { after: 100 }, 3).unwrap().is_empty());
    }

    struct Counter(u64, Vec<String>);
    impl Projection for Counter {
        type Error = String;
        fn apply(&mut self, event: &Event) -> Result<(), String> {
            if event.kind == "boom" {
                return Err("boom".into());
            }
            self.0 += 1;
            self.1.push(event.kind.clone());
            Ok(())
        }
    }

    #[test]
    fn replay_drives_a_projection_and_reports_where_it_failed() {
        let mut log = MemoryLog::new();
        let w = WorldId::from("w");
        log.append(&w, ev(1, "a")).unwrap();
        log.append(&w, ev(2, "b")).unwrap();
        let mut c = Counter(0, vec![]);
        let cur = replay(&log, &w, Cursor::START, &mut c).unwrap();
        assert_eq!((c.0, cur), (2, Cursor { after: 2 }));
        log.append(&w, ev(3, "boom")).unwrap();
        match replay(&log, &w, cur, &mut c) {
            Err(ReplayError::Projection { at, error }) => {
                assert_eq!((at, error.as_str()), (Seq(3), "boom"))
            }
            other => panic!("expected projection failure, got {other:?}"),
        }
    }

    #[test]
    fn events_round_trip_through_json() {
        let e = Event {
            seq: Seq(7),
            world: WorldId::from("entropic"),
            at: Timestamp(1_700_000_000_000),
            kind: "entropic.chunk_earned".into(),
            actor: Actor::Familiar("fam-1".into(), "spacepants".into()),
            cause: Some(Seq(6)),
            payload: json!({"value": 4200.5, "source": "void-lich"}),
        };
        let back: Event = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }
}
