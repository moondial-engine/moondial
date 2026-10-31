//! The event log on PostgreSQL.
//!
//! One table, `events`, holds every world's log: `(world, seq)` is the primary key, so
//! the sequence is dense per world and a duplicate append fails loudly instead of
//! forking the log. The driver's [`EventLog`] is synchronous and runs under the world
//! lock; the `postgres` client is synchronous too but owns a tokio runtime, which cannot
//! be entered from a tokio worker thread. So one dedicated thread owns the connection
//! and serves requests over a channel; the log blocks on the reply. An append is one
//! round trip. Reads are the same channel, so they see every append before them.
//!
//! The schema is created on open if it is missing. `MOONDIAL_LOG_URL=postgres://...`
//! selects this log in the driver; `moondial-driver import <dir>` copies a file log in.

use moondial_log::{Actor, Cursor, Event, EventLog, LogError, NewEvent, Seq, WorldId};
use moondial_time::Timestamp;
use postgres::{Client, NoTls};
use std::sync::mpsc;

/// What the connection thread does.
enum Request {
    Append {
        world: WorldId,
        event: NewEvent,
        reply: mpsc::Sender<Result<Event, LogError>>,
    },
    Read {
        world: WorldId,
        cursor: Cursor,
        limit: usize,
        reply: mpsc::Sender<Result<Vec<Event>, LogError>>,
    },
    Head {
        world: WorldId,
        reply: mpsc::Sender<Result<Option<Seq>, LogError>>,
    },
}

/// A log on PostgreSQL.
pub struct PostgresLog {
    tx: mpsc::Sender<Request>,
}

/// The advisory lock key schema creation takes.
const SCHEMA_LOCK: i64 = 0x6d6f_6f6e_6469_616c; // "moondial"

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    world   TEXT        NOT NULL,
    seq     BIGINT      NOT NULL,
    at      BIGINT      NOT NULL,
    kind    TEXT        NOT NULL,
    actor   JSONB       NOT NULL,
    cause   BIGINT      NULL,
    payload JSONB       NOT NULL,
    PRIMARY KEY (world, seq)
);
CREATE INDEX IF NOT EXISTS events_world_kind ON events (world, kind, seq);
";

/// How often, and how long between, connection attempts at open.
const CONNECT_TRIES: u32 = 20;
const CONNECT_WAIT_SECONDS: u64 = 3;

impl PostgresLog {
    /// Connect, create the schema if it is missing, and start the connection thread.
    pub fn open(url: &str) -> Result<Self, LogError> {
        // At boot the database may still be starting (a reboot brings both up at once):
        // try for a while before giving up.
        let mut client = {
            let mut tries = 0;
            loop {
                match Client::connect(url, NoTls) {
                    Ok(c) => break c,
                    Err(e) if tries < CONNECT_TRIES => {
                        tries += 1;
                        eprintln!("postgres: {e}; retrying ({tries}/{CONNECT_TRIES})");
                        std::thread::sleep(std::time::Duration::from_secs(CONNECT_WAIT_SECONDS));
                    }
                    Err(e) => return Err(storage(e)),
                }
            }
        };
        // Two openers at once (two tests, two drivers) must not both create the schema:
        // CREATE ... IF NOT EXISTS races. A transaction-scoped advisory lock serializes them.
        let mut tx = client.transaction().map_err(storage)?;
        tx.execute("SELECT pg_advisory_xact_lock($1)", &[&SCHEMA_LOCK])
            .map_err(storage)?;
        tx.batch_execute(SCHEMA).map_err(storage)?;
        tx.commit().map_err(storage)?;
        let (tx, rx) = mpsc::channel::<Request>();
        std::thread::Builder::new()
            .name("moondial-log-postgres".into())
            .spawn(move || serve(client, rx))
            .map_err(|e| LogError::Storage(e.to_string()))?;
        Ok(PostgresLog { tx })
    }

    fn ask<T>(
        &self,
        make: impl FnOnce(mpsc::Sender<Result<T, LogError>>) -> Request,
    ) -> Result<T, LogError> {
        let (reply, answer) = mpsc::channel();
        self.tx
            .send(make(reply))
            .map_err(|_| LogError::Storage("the log's connection thread is gone".into()))?;
        answer.recv().map_err(|_| {
            LogError::Storage("the log's connection thread stopped answering".into())
        })?
    }
}

fn storage(e: postgres::Error) -> LogError {
    LogError::Storage(e.to_string())
}

fn serve(mut client: Client, rx: mpsc::Receiver<Request>) {
    for req in rx {
        match req {
            Request::Append {
                world,
                event,
                reply,
            } => {
                let _ = reply.send(append(&mut client, &world, event));
            }
            Request::Read {
                world,
                cursor,
                limit,
                reply,
            } => {
                let _ = reply.send(read(&mut client, &world, cursor, limit));
            }
            Request::Head { world, reply } => {
                let _ = reply.send(head(&mut client, &world));
            }
        }
    }
}

fn append(client: &mut Client, world: &WorldId, event: NewEvent) -> Result<Event, LogError> {
    let mut tx = client.transaction().map_err(storage)?;
    // The world's last event, locked, so two appends cannot take the same seq.
    let last = tx
        .query_opt(
            "SELECT seq, at FROM events WHERE world = $1 ORDER BY seq DESC LIMIT 1 FOR UPDATE",
            &[&world.0],
        )
        .map_err(storage)?;
    let (seq, last_at) = match last {
        Some(row) => (row.get::<_, i64>(0) as u64 + 1, Some(row.get::<_, i64>(1))),
        None => (1, None),
    };
    if let Some(last_at) = last_at {
        if event.at.0 < last_at {
            return Err(LogError::OutOfOrder {
                at: event.at,
                last: Timestamp(last_at),
            });
        }
    }
    let actor = serde_json::to_value(&event.actor).map_err(|e| LogError::Storage(e.to_string()))?;
    tx.execute(
        "INSERT INTO events (world, seq, at, kind, actor, cause, payload) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        &[
            &world.0,
            &(seq as i64),
            &event.at.0,
            &event.kind,
            &actor,
            &event.cause.map(|c| c.0 as i64),
            &event.payload,
        ],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)?;
    Ok(Event {
        seq: Seq(seq),
        world: world.clone(),
        at: event.at,
        kind: event.kind,
        actor: event.actor,
        cause: event.cause,
        payload: event.payload,
    })
}

fn read(
    client: &mut Client,
    world: &WorldId,
    cursor: Cursor,
    limit: usize,
) -> Result<Vec<Event>, LogError> {
    let rows = client
        .query(
            "SELECT seq, at, kind, actor, cause, payload FROM events WHERE world = $1 AND seq > $2 ORDER BY seq LIMIT $3",
            &[&world.0, &(cursor.after as i64), &(limit as i64)],
        )
        .map_err(storage)?;
    rows.iter()
        .map(|row| {
            let actor: serde_json::Value = row.get(3);
            let actor: Actor =
                serde_json::from_value(actor).map_err(|e| LogError::Storage(e.to_string()))?;
            Ok(Event {
                seq: Seq(row.get::<_, i64>(0) as u64),
                world: world.clone(),
                at: Timestamp(row.get(1)),
                kind: row.get(2),
                actor,
                cause: row.get::<_, Option<i64>>(4).map(|c| Seq(c as u64)),
                payload: row.get(5),
            })
        })
        .collect()
}

fn head(client: &mut Client, world: &WorldId) -> Result<Option<Seq>, LogError> {
    let row = client
        .query_opt("SELECT max(seq) FROM events WHERE world = $1", &[&world.0])
        .map_err(storage)?;
    Ok(row
        .and_then(|r| r.get::<_, Option<i64>>(0))
        .map(|s| Seq(s as u64)))
}

impl EventLog for PostgresLog {
    fn append(&mut self, world: &WorldId, event: NewEvent) -> Result<Event, LogError> {
        let world = world.clone();
        self.ask(|reply| Request::Append {
            world,
            event,
            reply,
        })
    }

    fn read(&self, world: &WorldId, cursor: Cursor, limit: usize) -> Result<Vec<Event>, LogError> {
        let world = world.clone();
        self.ask(|reply| Request::Read {
            world,
            cursor,
            limit,
            reply,
        })
    }

    fn head(&self, world: &WorldId) -> Result<Option<Seq>, LogError> {
        let world = world.clone();
        self.ask(|reply| Request::Head { world, reply })
    }
}

/// Copy every event of `world` from one log into another, in order, refusing to write
/// over a world that already has events. For moving a file log to PostgreSQL.
pub fn copy_world(
    from: &dyn EventLog,
    to: &mut dyn EventLog,
    world: &WorldId,
) -> Result<u64, LogError> {
    if to.head(world)?.is_some() {
        return Err(LogError::Storage(format!(
            "world {} already has events in the destination; refusing to interleave",
            world.0
        )));
    }
    let mut n = 0;
    for e in from.read_all(world, Cursor::START)? {
        let got = to.append(
            world,
            NewEvent {
                at: e.at,
                kind: e.kind,
                actor: e.actor,
                cause: e.cause,
                payload: e.payload,
            },
        )?;
        if got.seq != e.seq {
            return Err(LogError::Storage(format!(
                "sequence drift while copying: source {} became {}",
                e.seq.0, got.seq.0
            )));
        }
        n += 1;
    }
    Ok(n)
}
