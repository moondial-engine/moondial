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
    /// The door each actor last arrived by (terminal, http, session, gate, mcp).
    doors: std::collections::HashMap<String, String>,
    /// When each greeter last greeted each visitor.
    greetings: std::collections::HashMap<String, Timestamp>,
    /// Lines for the operator (a Discord channel, say), drained by the tick loop.
    notices: Vec<String>,
    /// Wrong passphrases per name: how many in a row, and when the last was. Five in a
    /// row lock the name for [`LOCKOUT`]. In this process only; a restart forgets.
    failures: std::collections::HashMap<String, (u32, Timestamp)>,
    /// Run on every tick before the due jobs, by modules that watch the clock
    /// (who has gone quiet, say). Registered at boot.
    pub(crate) sweepers: Arc<Vec<Sweeper>>,
    /// Run when an arrival is recorded, with the actor's name. Registered at boot.
    arrival_hooks: Arc<Vec<ArrivalHook>>,
}

/// A per-tick check a module registers with [`World::on_tick`].
pub type Sweeper = Box<dyn Fn(&mut World) -> Result<(), String> + Send + Sync>;
/// A module's reaction to an arrival, registered with [`World::on_arrival`].
pub type ArrivalHook = Box<dyn Fn(&mut World, &str) -> Result<(), String> + Send + Sync>;

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
            doors: std::collections::HashMap::new(),
            greetings: std::collections::HashMap::new(),
            notices: Vec::new(),
            failures: std::collections::HashMap::new(),
            sweepers: Arc::new(Vec::new()),
            arrival_hooks: Arc::new(Vec::new()),
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
        // A command is not a door; the channel that logged the actor in is.
        if origin != "command" {
            self.doors.insert(actor.to_string(), origin.to_string());
        }
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
        let hooks = Arc::clone(&self.arrival_hooks);
        for h in hooks.iter() {
            if let Err(e) = h(self, actor) {
                eprintln!("arrival hook for {actor}: {e}");
            }
        }
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

    /// Forget an actor's presence: their next action is an arrival, whenever it comes.
    pub fn depart(&mut self, actor: &str) {
        self.presence.remove(actor);
    }

    /// When this actor was last active in this process, if they have been.
    pub fn last_active(&self, actor: &str) -> Option<Timestamp> {
        self.presence.get(actor).copied()
    }

    /// Whether `greeter` should greet `visitor` now: true once an hour per pair.
    pub fn greeted(&mut self, greeter: &str, visitor: &str) -> bool {
        let now = self.now();
        let key = format!("{greeter}\u{0}{visitor}");
        if self
            .greetings
            .get(&key)
            .is_some_and(|t| now.since(*t).0 < 3_600_000)
        {
            return false;
        }
        self.greetings.insert(key, now);
        true
    }

    /// The door an actor last came by, while the process remembers them.
    pub fn door_of(&self, actor: &str) -> Option<&str> {
        self.doors.get(actor).map(String::as_str)
    }

    /// How a door reads to the room: the play page and the text port have a keyboard
    /// behind them; the gate, HTTP and MCP are how agents come.
    pub fn door_label(origin: &str) -> Option<&'static str> {
        match origin {
            "session" | "terminal" => Some("at a keyboard"),
            "gate" | "mcp" | "http" => Some("through the gate"),
            _ => None,
        }
    }

    /// Register a per-tick check. Boot only.
    pub fn on_tick(&mut self, f: Sweeper) {
        Arc::get_mut(&mut self.sweepers)
            .expect("sweepers are registered at boot, not inside a tick")
            .push(f);
    }

    /// Register a reaction to arrivals. Boot only.
    pub fn on_arrival(&mut self, f: ArrivalHook) {
        Arc::get_mut(&mut self.arrival_hooks)
            .expect("arrival hooks are registered at boot, not inside a call")
            .push(f);
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
        let before = self.last_active(&ctx.actor);
        let out = registry.call(self, ctx, name, args);
        // A citizen who acted is present; after an absence, that is an arrival. A refused
        // command touches nothing, so a name the world does not know leaves no trace. A
        // command that was a departure (presence known before, gone after) is not an
        // arrival.
        let departed = before.is_some() && self.last_active(&ctx.actor).is_none();
        if out.is_ok() && ctx.kind == ActorKind::Citizen && !departed {
            self.arrive(&ctx.actor, "command", false);
        }
        out
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
