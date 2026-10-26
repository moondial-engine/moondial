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
use std::time::{SystemTime, UNIX_EPOCH};

/// Where the log lives.
pub enum LogBacking {
    /// In memory; gone at exit. For tests and `--ephemeral`.
    Memory,
    /// JSON lines under a directory; replayed at start.
    File(std::path::PathBuf),
}

/// The driver's world.
pub struct World {
    pub id: WorldId,
    pub store: Store,
    pub log: Box<dyn EventLog + Send>,
    pub registry: Registry<World>,
    pub started: Timestamp,
    /// Set by tests to freeze the clock; `None` reads the system clock.
    pub fixed_now: Option<Timestamp>,
}

impl World {
    /// Open a world, replaying its log into a fresh store.
    pub fn open(id: WorldId, store: Store, backing: LogBacking) -> Result<Self, String> {
        let log: Box<dyn EventLog + Send> = match backing {
            LogBacking::Memory => Box::new(MemoryLog::new()),
            LogBacking::File(dir) => Box::new(FileLog::open(dir).map_err(|e| e.to_string())?),
        };
        let mut world = World {
            id,
            store,
            log,
            registry: Registry::new(),
            started: now_ms(),
            fixed_now: None,
        };
        let head = world.log.head(&world.id).map_err(|e| e.to_string())?;
        if head.is_some() {
            // Replay into a fresh store that knows the same schemas.
            let mut fresh = Store::new(world.store.registry().clone());
            moondial_log::replay(world.log.as_ref(), &world.id, Cursor::START, &mut fresh)
                .map_err(|e| e.to_string())?;
            world.store = fresh;
        }
        Ok(world)
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

    /// Run a command through the registry. The registry borrows the world mutably, so the
    /// registry itself is moved out for the duration of the call and put back after.
    pub fn call(
        &mut self,
        ctx: &CallContext,
        name: &str,
        args: &Value,
    ) -> Result<Outcome, CommandError> {
        let registry = std::mem::take(&mut self.registry);
        let out = registry.call(self, ctx, name, args);
        self.registry = registry;
        out
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
