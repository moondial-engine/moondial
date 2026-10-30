//! The driver's world: a store, its log, the command registry, and the real clock.
//!
//! The driver's single mutation path is [`World::record`], which appends to the log and
//! applies to the store. Commands run inside the registry with `&mut World`, so a module's
//! handler can read the store and record events but cannot bypass the log.

use moondial_log::{Actor, Cursor, Event, EventLog, FileLog, MemoryLog, WorldId};
use moondial_registry::{ActorKind, CallContext, CommandError, Outcome, Registry};
use moondial_store::{Store, StoreError, StoreEvent};
use moondial_time::Timestamp;
use serde_json::Value;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Where the log lives.
pub enum LogBacking {
    /// In memory; gone at exit. For tests and `--ephemeral`.
    Memory,
    /// JSON lines under a directory; replayed at start.
    File(std::path::PathBuf),
    /// PostgreSQL, by connection URL; replayed at start.
    Postgres(String),
}

/// The driver's world.
pub struct World {
    pub id: WorldId,
    pub store: Store,
    pub log: Box<dyn EventLog + Send>,
    /// The command registry. Shared so a handler running inside [`World::call`] can
    /// still ask what commands exist (an observation lists the legal ones).
    pub registry: Arc<Registry<World>>,
    pub started: Timestamp,
    /// Set by tests to freeze the clock; `None` reads the system clock.
    pub fixed_now: Option<Timestamp>,
    /// Dice state. Outcomes are recorded as events, so replay never rolls again.
    rng: u64,
    /// Pending jobs, a projection over `job.*` events.
    pub jobs: crate::jobs::Jobs,
    /// Job handlers by kind, registered at boot.
    pub(crate) job_handlers: Arc<std::collections::HashMap<String, crate::jobs::JobHandler>>,
    /// When each actor was last active, in this process. A gap longer than
    /// [`ABSENCE`] makes the next activity an arrival.
    presence: std::collections::HashMap<String, Timestamp>,
    /// Lines for the operator (a Discord channel, say), drained by the tick loop.
    notices: Vec<String>,
    /// Wrong passphrases per name: how many in a row, and when the last was. Five in a
    /// row lock the name for [`LOCKOUT`]. In this process only; a restart forgets.
    failures: std::collections::HashMap<String, (u32, Timestamp)>,
}

/// Wrong passphrases in a row before a name is locked.
pub const MAX_FAILURES: u32 = 5;
/// How long a locked name stays locked.
pub const LOCKOUT: moondial_time::Duration = moondial_time::Duration(5 * 60 * 1000);

/// How long an actor must be gone for their return to count as an arrival.
pub const ABSENCE: moondial_time::Duration = moondial_time::Duration(10 * 60 * 1000);

/// The arrival event: `{actor, origin, fresh, away_ms}`.
pub const ARRIVED: &str = "citizen.arrived";

impl World {
    /// Open a world, replaying its log into a fresh store.
    pub fn open(id: WorldId, store: Store, backing: LogBacking) -> Result<Self, String> {
        let log: Box<dyn EventLog + Send> = match backing {
            LogBacking::Memory => Box::new(MemoryLog::new()),
            LogBacking::File(dir) => Box::new(FileLog::open(dir).map_err(|e| e.to_string())?),
            LogBacking::Postgres(url) => {
                Box::new(moondial_log_postgres::PostgresLog::open(&url).map_err(|e| e.to_string())?)
            }
        };
        let started = now_ms();
        let mut world = World {
            id,
            store,
            log,
            registry: Arc::new(Registry::new()),
            started,
            fixed_now: None,
            rng: started.0 as u64 | 1,
            jobs: crate::jobs::Jobs::default(),
            job_handlers: Arc::new(std::collections::HashMap::new()),
            presence: std::collections::HashMap::new(),
            notices: Vec::new(),
            failures: std::collections::HashMap::new(),
        };
        let head = world.log.head(&world.id).map_err(|e| e.to_string())?;
        if head.is_some() {
            // Replay into a fresh store that knows the same schemas, and rebuild the
            // pending jobs from the same pass over the log.
            let mut fresh = Store::new(world.store.registry().clone());
            moondial_log::replay(world.log.as_ref(), &world.id, Cursor::START, &mut fresh)
                .map_err(|e| match e {
                    // A log written under another set of modules: say so, and what to do.
                    moondial_log::ReplayError::Projection {
                        at,
                        error: moondial_store::StoreError::UnknownComponent(c),
                    } => format!(
                        "the log for world {} was written by modules this driver does not have: \
                         event #{} uses component {c}. Move that log aside, or run this driver \
                         under a different MOONDIAL_WORLD, or run the modules that wrote it.",
                        world.id.0, at.0
                    ),
                    e => e.to_string(),
                })?;
            world.store = fresh;
            for e in world
                .log
                .read_all(&world.id, Cursor::START)
                .map_err(|e| e.to_string())?
            {
                if e.kind.starts_with("job.") {
                    world.jobs.apply(&e.kind, e.seq.0, &e.payload);
                }
            }
        }
        Ok(world)
    }

    /// An actor is active now, by way of `origin` (terminal, http, session, gate, mcp).
    /// The first activity after an absence is recorded as an arrival, and the operator
    /// is told about the ones worth knowing: a first arrival, an agent's channel, or a
    /// return after a day away.
    pub fn arrive(&mut self, actor: &str, origin: &str, fresh: bool) {
        let now = self.now();
        let last = self.presence.insert(actor.to_string(), now);
        let away = last.map(|t| now.since(t));
        if !fresh && away.is_some_and(|d| d.0 < ABSENCE.0) {
            return;
        }
        let away_ms = away.map(|d| d.0);
        let _ = self.note(
            Actor::Citizen(actor.to_string()),
            ARRIVED,
            serde_json::json!({"actor": actor, "origin": origin, "fresh": fresh, "away_ms": away_ms}),
        );
        let agent_channel = matches!(origin, "gate" | "mcp");
        let long_away = away_ms.is_some_and(|ms| ms >= 86_400_000);
        if fresh {
            self.notices.push(format!(
                "**{actor}** arrived for the first time, via {origin}."
            ));
        } else if agent_channel {
            self.notices
                .push(format!("**{actor}** is back, via {origin}."));
        } else if long_away {
            self.notices.push(format!(
                "**{actor}** is back after {} days, via {origin}.",
                away_ms.unwrap_or(0) / 86_400_000
            ));
        }
    }

    /// Minutes this name stays locked, if it is: five wrong passphrases in a row within
    /// the lockout window.
    pub fn locked_for(&self, actor: &str) -> Option<i64> {
        let (n, last) = self.failures.get(actor)?;
        if *n < MAX_FAILURES {
            return None;
        }
        let left = LOCKOUT.0 - self.now().since(*last).0;
        (left > 0).then(|| (left + 59_999) / 60_000)
    }

    /// A wrong passphrase for this name.
    pub fn failed_login(&mut self, actor: &str) {
        let now = self.now();
        let e = self.failures.entry(actor.to_string()).or_insert((0, now));
        if now.since(e.1).0 > LOCKOUT.0 {
            e.0 = 0;
        }
        e.0 += 1;
        e.1 = now;
    }

    /// A right passphrase: the count starts over.
    pub fn succeeded_login(&mut self, actor: &str) {
        self.failures.remove(actor);
    }

    /// A line for the operator.
    pub fn notice(&mut self, line: String) {
        self.notices.push(line);
    }

    /// Take the pending operator notices.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// Seed the dice, for tests that want the same fight twice.
    #[cfg(test)]
    pub fn seed(&mut self, seed: u64) {
        self.rng = seed | 1;
    }

    /// A roll in `0..n` (splitmix64; not for anything that needs to be unpredictable).
    pub fn roll(&mut self, n: u32) -> u32 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        if n == 0 {
            0
        } else {
            (z % n as u64) as u32
        }
    }

    /// The current moment: the system clock, or the fixed one in tests.
    pub fn now(&self) -> Timestamp {
        self.fixed_now.unwrap_or_else(now_ms)
    }

    /// Append an event to the log and apply it to the store.
    pub fn record(&mut self, actor: Actor, event: StoreEvent) -> Result<Event, StoreError> {
        let at = self.now();
        self.store
            .record(self.log.as_mut(), &self.id, at, actor, event)
    }

    /// Append an event that is not a store event: a message through the Gate, say. The
    /// store ignores it on replay; it is in the log for the record, which is the point.
    pub fn note(&mut self, actor: Actor, kind: &str, payload: Value) -> Result<Event, String> {
        let at = self.now();
        self.log
            .append(
                &self.id,
                moondial_log::NewEvent {
                    at,
                    kind: kind.to_string(),
                    actor,
                    cause: None,
                    payload,
                },
            )
            .map_err(|e| e.to_string())
    }

    /// The last `n` events, oldest first.
    pub fn recent_events(&self, n: usize) -> Vec<Event> {
        let head = self
            .log
            .head(&self.id)
            .ok()
            .flatten()
            .map(|s| s.0)
            .unwrap_or(0);
        let after = head.saturating_sub(n as u64);
        self.log
            .read_all(&self.id, Cursor { after })
            .unwrap_or_default()
    }

    /// Run a command through the registry. The handler borrows the world mutably while
    /// the registry is shared, so the registry stays readable during the call.
    pub fn call(
        &mut self,
        ctx: &CallContext,
        name: &str,
        args: &Value,
    ) -> Result<Outcome, CommandError> {
        let registry = Arc::clone(&self.registry);
        registry.call(self, ctx, name, args)
    }

    /// Register a command. Only possible while nothing else holds the registry, which
    /// is the case at boot; a module registering mid-call is a bug and panics.
    pub fn register(
        &mut self,
        spec: moondial_registry::CommandSpec,
        handler: moondial_registry::Handler<World>,
    ) {
        Arc::get_mut(&mut self.registry)
            .expect("commands are registered at boot, not inside a call")
            .register(spec, handler);
    }

    /// The commands an actor kind may call, as the registry describes them.
    pub fn commands_for(&self, kind: ActorKind) -> Vec<&moondial_registry::CommandSpec> {
        self.registry.available(kind)
    }
}

/// Milliseconds since the Unix epoch from the system clock.
pub fn now_ms() -> Timestamp {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp(d.as_millis() as i64)
}
