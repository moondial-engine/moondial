//! The phase-1 gate from the architecture doc, in miniature: "a chunk decays correctly
//! across logout and a paused clock." A citizen's chunk is created through the log,
//! the citizen logs out (nothing happens: decay is lazy), the account's clock is paused
//! and resumed, and the value read from the live store equals the value read from a
//! store rebuilt by replaying the log, at every moment that matters.
//!
//! The property test extends that to random event sequences: live store and replayed
//! store agree on every entity's timed fields at random later times.

use moondial_log::{replay, Actor, Cursor, EventLog, MemoryLog, WorldId};
use moondial_store::{
    ClockId, ComponentSchema, EntityId, Field, FieldKind, Registry, Store, StoreEvent, Viewer,
    Visibility, WORLD_CLOCK,
};
use moondial_time::{Curve, Duration, TimedValue, Timestamp};
use proptest::prelude::*;
use std::collections::BTreeMap;

const T0: Timestamp = Timestamp(1_700_000_000_000);

fn registry() -> Registry {
    let mut r = Registry::new();
    r.register(
        ComponentSchema::new("entropic.chunk", 1)
            .field("value", FieldKind::Timed, Visibility::Owner)
            .field("source", FieldKind::Plain, Visibility::Public),
    );
    r
}

fn chunk(v0: f64, at: Timestamp, curve: Curve) -> BTreeMap<String, Field> {
    let mut f = BTreeMap::new();
    f.insert("value".into(), Field::Timed(TimedValue::new(v0, at, curve)));
    f.insert("source".into(), Field::Plain(serde_json::json!("hydra")));
    f
}

#[test]
fn a_chunk_decays_correctly_across_logout_and_a_paused_clock() {
    let world = WorldId::from("entropic-test");
    let mut log = MemoryLog::new();
    let mut live = Store::new(registry());
    let dave = Actor::Citizen("dave".into());
    let account: ClockId = "account:dave".into();

    // Day 0: dave arrives, earns a chunk worth 100 on a one-day half-life.
    let window = live.allocate_id();
    let chunk_id = live.allocate_id();
    live.record(
        &mut log,
        &world,
        T0,
        dave.clone(),
        StoreEvent::EntityCreated {
            id: window,
            kind: "window".into(),
            owner: Some("dave".into()),
            parent: None,
            scopes: vec![account.clone()],
        },
    )
    .unwrap();
    live.record(
        &mut log,
        &world,
        T0,
        dave.clone(),
        StoreEvent::EntityCreated {
            id: chunk_id,
            kind: "chunk".into(),
            owner: Some("dave".into()),
            parent: Some(window),
            scopes: vec![],
        },
    )
    .unwrap();
    live.record(
        &mut log,
        &world,
        T0,
        dave.clone(),
        StoreEvent::ComponentSet {
            id: chunk_id,
            component: "entropic.chunk".into(),
            fields: chunk(
                100.0,
                T0,
                Curve::Exponential {
                    half_life: Duration::days(1),
                },
            ),
        },
    )
    .unwrap();

    // Day 1: dave logs out. Nothing is recorded; the world does not tick.
    let day = |d: i64| Timestamp(T0.0 + Duration::days(d).0);
    let read = |s: &Store, t: Timestamp| {
        s.read_field(chunk_id, "entropic.chunk", "value", t)
            .unwrap()
            .as_f64()
            .unwrap()
    };
    assert!(
        (read(&live, day(1)) - 50.0).abs() < 1e-9,
        "one half-life while logged out"
    );

    // Day 2: PTO starts (the account clock pauses). Day 9: PTO ends.
    live.record(
        &mut log,
        &world,
        day(2),
        Actor::Driver,
        StoreEvent::ClockPaused {
            clock: account.clone(),
        },
    )
    .unwrap();
    live.record(
        &mut log,
        &world,
        day(9),
        Actor::Driver,
        StoreEvent::ClockResumed {
            clock: account.clone(),
        },
    )
    .unwrap();

    // Inside the pause the value is frozen: two active days before it started, so 25.
    assert!(
        (read(&live, day(5)) - 25.0).abs() < 1e-9,
        "frozen during PTO"
    );
    // Day 10: three active days of decay in ten wall-clock days; value is 12.5.
    assert!(
        (read(&live, day(10)) - 12.5).abs() < 1e-9,
        "paused week does not decay"
    );

    // Day 10: dave shelves the chunk into the palace: a recurve onto the long-tail curve.
    live.record(
        &mut log,
        &world,
        day(10),
        dave.clone(),
        StoreEvent::FieldRecurved {
            id: chunk_id,
            component: "entropic.chunk".into(),
            field: "value".into(),
            curve: Curve::PowerLaw {
                scale: Duration::days(10),
                exponent: 1.0,
            },
        },
    )
    .unwrap();
    // Day 20: one scale later the hyperbola halves: 6.25.
    assert!((read(&live, day(20)) - 6.25).abs() < 1e-9, "palace tail");

    // A world-wide maintenance freeze on day 20 holds everything.
    live.record(
        &mut log,
        &world,
        day(20),
        Actor::Driver,
        StoreEvent::ClockPaused {
            clock: WORLD_CLOCK.into(),
        },
    )
    .unwrap();
    assert!((read(&live, day(400)) - 6.25).abs() < 1e-9, "world freeze");

    // Replay the log into an empty store: identical, at every moment above and after.
    let mut rebuilt = Store::new(registry());
    let cursor = replay(&log, &world, Cursor::START, &mut rebuilt).unwrap();
    assert_eq!(cursor.after, log.head(&world).unwrap().unwrap().0);
    assert_eq!(rebuilt, live);
    for d in [0, 1, 2, 5, 9, 10, 15, 20, 400] {
        assert_eq!(read(&live, day(d)), read(&rebuilt, day(d)), "day {d}");
    }

    // The owner sees the value; a stranger sees only the source.
    let own = live
        .view(chunk_id, &Viewer::actor("dave"), day(10))
        .unwrap();
    assert!(
        (own["components"]["entropic.chunk"]["value"]
            .as_f64()
            .unwrap()
            - 12.5)
            .abs()
            < 1e-9
    );
    let stranger = live
        .view(chunk_id, &Viewer::actor("someone"), day(10))
        .unwrap();
    assert!(stranger["components"]["entropic.chunk"]
        .get("value")
        .is_none());
    assert_eq!(stranger["components"]["entropic.chunk"]["source"], "hydra");
}

/// A random, valid sequence of store events over a few entities and two clock scopes.
#[derive(Debug, Clone)]
enum Step {
    Create { owner: u8, scoped: bool },
    Commit { which: usize, value: f64 },
    Recurve { which: usize, half_life_days: i64 },
    Pause { account: bool },
    Resume { account: bool },
    Move { which: usize, parent: Option<usize> },
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0u8..3, any::<bool>()).prop_map(|(owner, scoped)| Step::Create { owner, scoped }),
        (0usize..6, 0.0f64..1000.0).prop_map(|(which, value)| Step::Commit { which, value }),
        (0usize..6, 1i64..30).prop_map(|(which, half_life_days)| Step::Recurve {
            which,
            half_life_days
        }),
        any::<bool>().prop_map(|account| Step::Pause { account }),
        any::<bool>().prop_map(|account| Step::Resume { account }),
        (0usize..6, prop::option::of(0usize..6))
            .prop_map(|(which, parent)| Step::Move { which, parent }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn live_store_and_replayed_store_agree(steps in prop::collection::vec((step_strategy(), 1i64..100_000), 1..40), probe in 0i64..10_000_000) {
        let world = WorldId::from("prop");
        let mut log = MemoryLog::new();
        let mut live = Store::new(registry());
        let mut ids: Vec<EntityId> = Vec::new();
        let mut t = T0.0;
        let mut paused = [false, false]; // [world, account]
        for (step, gap) in steps {
            t += gap;
            let at = Timestamp(t);
            let ev = match step {
                Step::Create { owner, scoped } => {
                    let id = live.allocate_id();
                    ids.push(id);
                    let scopes = if scoped { vec![ClockId::from("account:x")] } else { vec![] };
                    live.record(&mut log, &world, at, Actor::Driver, StoreEvent::EntityCreated { id, kind: "chunk".into(), owner: Some(format!("o{owner}")), parent: None, scopes }).unwrap();
                    StoreEvent::ComponentSet { id, component: "entropic.chunk".into(), fields: chunk(100.0, at, Curve::Exponential { half_life: Duration::days(2) }) }
                }
                Step::Commit { which, value } => {
                    if ids.is_empty() { continue; }
                    StoreEvent::FieldCommitted { id: ids[which % ids.len()], component: "entropic.chunk".into(), field: "value".into(), value }
                }
                Step::Recurve { which, half_life_days } => {
                    if ids.is_empty() { continue; }
                    StoreEvent::FieldRecurved { id: ids[which % ids.len()], component: "entropic.chunk".into(), field: "value".into(), curve: Curve::Exponential { half_life: Duration::days(half_life_days) } }
                }
                Step::Pause { account } => {
                    let i = usize::from(account);
                    if paused[i] { continue; }
                    paused[i] = true;
                    StoreEvent::ClockPaused { clock: if account { "account:x".into() } else { WORLD_CLOCK.into() } }
                }
                Step::Resume { account } => {
                    let i = usize::from(account);
                    if !paused[i] { continue; }
                    paused[i] = false;
                    StoreEvent::ClockResumed { clock: if account { "account:x".into() } else { WORLD_CLOCK.into() } }
                }
                Step::Move { which, parent } => {
                    if ids.is_empty() { continue; }
                    let id = ids[which % ids.len()];
                    let parent = parent.map(|p| ids[p % ids.len()]).filter(|p| *p != id);
                    // Skip moves that would make a cycle; the store rejects them and the log would hold a rejected event.
                    let mut cur = parent;
                    let mut cyclic = false;
                    while let Some(c) = cur { if c == id { cyclic = true; break; } cur = live.entity(c).and_then(|e| e.parent); }
                    if cyclic { continue; }
                    StoreEvent::EntityMoved { id, parent }
                }
            };
            live.record(&mut log, &world, at, Actor::Driver, ev).unwrap();
        }
        let mut rebuilt = Store::new(registry());
        replay(&log, &world, Cursor::START, &mut rebuilt).unwrap();
        prop_assert_eq!(&rebuilt, &live);
        let now = Timestamp(t + probe);
        for id in &ids {
            let a = live.read_field(*id, "entropic.chunk", "value", now).unwrap();
            let b = rebuilt.read_field(*id, "entropic.chunk", "value", now).unwrap();
            prop_assert_eq!(a, b);
        }
    }
}
