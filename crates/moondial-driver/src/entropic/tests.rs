use super::*;
use crate::world::LogBacking;
use moondial_log::WorldId;
use moondial_registry::{ActorKind, Outcome};
use moondial_store::{Registry, Store, Viewer};
use moondial_time::Timestamp;

const PASS: &str = "correct-horse-battery";

fn open(dir: &std::path::Path, at: Timestamp) -> World {
    let mut registry = Registry::new();
    for s in schemas() {
        registry.register(s);
    }
    let mut w = World::open(
        WorldId("test".into()),
        Store::new(registry),
        LogBacking::File(dir.to_path_buf()),
    )
    .unwrap();
    w.fixed_now = Some(at);
    w.seed(7);
    ensure_world(&mut w).unwrap();
    register(&mut w, Curves::with_half_life(Duration::seconds(10)));
    w
}

fn call(
    w: &mut World,
    actor: &str,
    kind: ActorKind,
    verb: &str,
    args: Value,
) -> Result<Outcome, CommandError> {
    w.call(
        &CallContext {
            actor: actor.into(),
            kind,
        },
        verb,
        &args,
    )
}

fn ok(w: &mut World, actor: &str, verb: &str, args: Value) -> Outcome {
    call(w, actor, ActorKind::Citizen, verb, args.clone())
        .unwrap_or_else(|e| panic!("{verb} {args}: {e}"))
}

fn admin(w: &mut World, actor: &str, verb: &str, args: Value) -> Outcome {
    call(w, actor, ActorKind::Admin, verb, args.clone())
        .unwrap_or_else(|e| panic!("{verb} {args}: {e}"))
}

fn level(w: &mut World, actor: &str) -> f64 {
    ok(w, actor, "chunks", json!([])).result["level"]
        .as_f64()
        .unwrap()
}

fn events(w: &World) -> u64 {
    w.log.head(&w.id).unwrap().unwrap().0
}

/// Fight whatever is here until it dies or you do. Returns the last outcome.
fn fight(w: &mut World, actor: &str) -> Outcome {
    for _ in 0..40 {
        let out = ok(w, actor, "attack", json!([]));
        if out.result["killed"] == json!(true) || out.result["died"] == json!(true) {
            return out;
        }
    }
    panic!("fight did not end")
}

#[test]
fn a_passphrase_is_set_on_first_login_and_required_after() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = open(dir.path(), Timestamp(0));
    assert_eq!(
        login(&mut w, "dave", "test", "short").err(),
        Some(LoginError::TooShort {
            min: identity::MIN_PASSPHRASE
        })
    );
    assert_eq!(
        login(&mut w, "dave", "test", "").err(),
        Some(LoginError::Missing)
    );
    assert!(login(&mut w, "dave", "test", PASS).is_ok());
    assert_eq!(
        login(&mut w, "dave", "test", "not-it-at-all").err(),
        Some(LoginError::WrongPassphrase {
            name: "dave".into()
        })
    );
    assert_eq!(
        login(&mut w, "dave", "test", "").err(),
        Some(LoginError::Missing)
    );
    let f = login(&mut w, "dave", "test", PASS).unwrap();
    // Only an admin sees the hash; the owner does not.
    let now = w.now();
    let mine = w
        .store
        .view(f.citizen, &Viewer::actor("dave"), now)
        .unwrap();
    assert!(mine["components"][ACCOUNT].get("passphrase_hash").is_none());
    let theirs = w.store.view(f.citizen, &Viewer::admin(), now).unwrap();
    assert!(theirs["components"][ACCOUNT]["passphrase_hash"]
        .as_str()
        .unwrap()
        .starts_with("$argon2id$"));
}

#[test]
fn the_map_is_built_once_and_travel_follows_exits() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = open(dir.path(), Timestamp(0));
    let built = events(&w);
    assert_eq!(
        w.store.entities().filter(|e| e.kind == "room").count(),
        rooms::ROOMS.len()
    );
    login(&mut w, "dave", "test", PASS).unwrap();
    let look = ok(&mut w, "dave", "look", json!([]));
    assert_eq!(look.result["room"], json!("The Gate"));
    assert_eq!(look.result["exits"], json!(["east"]));
    assert!(matches!(
        call(&mut w, "dave", ActorKind::Citizen, "travel", json!(["up"])),
        Err(CommandError::Precondition { .. })
    ));
    let fog = ok(&mut w, "dave", "travel", json!(["east"]));
    assert_eq!(fog.result["room"], json!("The Fogbank"));
    assert_eq!(
        fog.result["creatures"].as_array().unwrap().len(),
        2,
        "the fog fills to its count on arrival"
    );
    drop(w);
    // Reopening builds nothing: the rooms are in the log.
    let w = open(dir.path(), Timestamp(1000));
    assert_eq!(
        w.store.entities().filter(|e| e.kind == "room").count(),
        rooms::ROOMS.len()
    );
    assert!(events(&w) > built, "the login and travel were recorded");
    let mut w = w;
    let before = events(&w);
    ensure_world(&mut w).unwrap();
    assert_eq!(events(&w), before);
}

#[test]
fn a_fight_earns_a_chunk_whose_vividity_is_how_close_you_came() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = open(dir.path(), Timestamp(0));
    login(&mut w, "dave", "test", PASS).unwrap();
    ok(&mut w, "dave", "travel", json!(["east"]));
    assert_eq!(level(&mut w, "dave"), 0.0);
    let win = fight(&mut w, "dave");
    assert_eq!(win.result["killed"], json!(true));
    let hp = win.result["hp"].as_f64().unwrap();
    let vividity = win.result["vividity"].as_f64().unwrap();
    assert!((vividity - (1.0 - hp / MAX_HP)).abs() < 1e-9);
    assert!(
        (level(&mut w, "dave") - 15.0).abs() < 1e-9,
        "a wisp is worth 15"
    );
    assert_eq!(
        w.store.entities().filter(|e| e.kind == "creature").count(),
        1,
        "one wisp left; the other respawns later"
    );

    // Attack follows level.
    assert_eq!(combat::attack_for_level(0.0), 3.0);
    assert_eq!(combat::attack_for_level(75.0), 10.0);
    assert_eq!(
        ok(&mut w, "dave", "chunks", json!([])).result["attack"],
        json!(4.0)
    );
}

#[test]
fn vivid_chunks_fade_slower() {
    let dir = tempfile::tempdir().unwrap();
    let t0 = Timestamp(0);
    let mut w = open(dir.path(), t0);
    login(&mut w, "dave", "test", PASS).unwrap();
    let dull = admin(&mut w, "dave", "earn", json!(["dull", 100, 0.0])).result["chunk"]
        .as_u64()
        .unwrap();
    let vivid = admin(&mut w, "dave", "earn", json!(["vivid", 100, 1.0])).result["chunk"]
        .as_u64()
        .unwrap();
    w.fixed_now = Some(t0.plus(Duration::seconds(10)));
    let read = |w: &World, id: u64| {
        w.store
            .read_field(EntityId(id), CHUNK, "value", w.now())
            .unwrap()
            .as_f64()
            .unwrap()
    };
    assert!((read(&w, dull) - 50.0).abs() < 1e-6, "one half-life");
    assert!(
        (read(&w, vivid) - 100.0 * 0.5f64.powf(1.0 / 3.0)).abs() < 1e-6,
        "three times the half-life"
    );
    // Shelving keeps the stretch.
    ok(&mut w, "dave", "shelve", json!([format!("chunk-{vivid}")]));
    ok(&mut w, "dave", "shelve", json!([format!("chunk-{dull}")]));
    w.fixed_now = Some(t0.plus(Duration::seconds(10 + 50)));
    assert!(
        (read(&w, dull) - 25.0).abs() < 1e-6,
        "palace halves at 5x the half-life"
    );
    let vivid_at_shelve = 100.0 * 0.5f64.powf(1.0 / 3.0);
    assert!(
        (read(&w, vivid) - vivid_at_shelve / (1.0 + 50.0 / 150.0)).abs() < 1e-6,
        "the vivid palace curve has three times the scale"
    );
}

#[test]
fn death_is_a_concussion_and_a_walk_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = open(dir.path(), Timestamp(0));
    login(&mut w, "dave", "test", PASS).unwrap();
    // Three dull chunks to lose one of.
    for i in 0..3 {
        admin(&mut w, "dave", "earn", json!([format!("memory-{i}"), 10]));
    }
    ok(&mut w, "dave", "travel", json!(["east"]));
    ok(&mut w, "dave", "travel", json!(["north"]));
    ok(&mut w, "dave", "travel", json!(["east"]));
    let out = fight(&mut w, "dave");
    assert_eq!(
        out.result["died"],
        json!(true),
        "a level-30 citizen does not beat a revenant: {}",
        out.summary
    );
    let look = ok(&mut w, "dave", "look", json!([]));
    assert_eq!(
        look.result["room"],
        json!("The Gate"),
        "you wake at the Gate"
    );
    let f = login(&mut w, "dave", "test", PASS).unwrap();
    assert_eq!(combat::hp(&w, f.citizen), MAX_HP);
    assert!(combat::engaged_with(&w, f.citizen).is_none());
    let chunks = ok(&mut w, "dave", "chunks", json!([])).result["chunks"]
        .as_array()
        .unwrap()
        .len();
    match out.result["lost"].as_str() {
        Some(_) => assert_eq!(chunks, 2, "concussion took one"),
        None => assert_eq!(chunks, 3),
    }
    // The same seed gives the same fight.
    let dir2 = tempfile::tempdir().unwrap();
    let mut w2 = open(dir2.path(), Timestamp(0));
    login(&mut w2, "dave", "test", PASS).unwrap();
    for i in 0..3 {
        admin(&mut w2, "dave", "earn", json!([format!("memory-{i}"), 10]));
    }
    for exit in ["east", "north", "east"] {
        ok(&mut w2, "dave", "travel", json!([exit]));
    }
    assert_eq!(fight(&mut w2, "dave").summary, out.summary);
}

#[test]
fn you_cannot_travel_mid_fight_and_retreat_costs_a_hit() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = open(dir.path(), Timestamp(0));
    let f = login(&mut w, "dave", "test", PASS).unwrap();
    ok(&mut w, "dave", "travel", json!(["east"]));
    assert!(matches!(
        call(&mut w, "dave", ActorKind::Citizen, "retreat", json!([])),
        Err(CommandError::Precondition { .. })
    ));
    let round = ok(&mut w, "dave", "attack", json!(["wisp"]));
    assert_eq!(round.result["killed"], json!(false), "{}", round.summary);
    assert!(combat::engaged_with(&w, f.citizen).is_some());
    let err = call(
        &mut w,
        "dave",
        ActorKind::Citizen,
        "travel",
        json!(["west"]),
    )
    .unwrap_err();
    assert!(err.to_string().contains("Retreat"), "{err}");
    let before = combat::hp(&w, f.citizen);
    let out = ok(&mut w, "dave", "retreat", json!([]));
    assert!(combat::hp(&w, f.citizen) < before, "{}", out.summary);
    assert!(combat::engaged_with(&w, f.citizen).is_none());
    ok(&mut w, "dave", "travel", json!(["west"]));
    // Healing is on the citizen's clock: PTO stops it, resume restarts it.
    let hurt = combat::hp(&w, f.citizen);
    ok(&mut w, "dave", "pto", json!([]));
    w.fixed_now = Some(Timestamp(100_000));
    assert_eq!(combat::hp(&w, f.citizen), hurt);
    ok(&mut w, "dave", "resume", json!([]));
    w.fixed_now = Some(Timestamp(200_000));
    assert!(
        (combat::hp(&w, f.citizen) - (hurt + 10.0).min(MAX_HP)).abs() < 1e-6,
        "0.1 hp per second for 100 s"
    );
    let rest = ok(&mut w, "dave", "rest", json!([]));
    assert!(rest.result["full_in_seconds"].as_i64().is_some());
}

#[test]
fn a_session_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let t0 = Timestamp(1_000_000);
    let mut w = open(dir.path(), t0);
    login(&mut w, "dave", "test", PASS).unwrap();

    let earned = admin(&mut w, "dave", "earn", json!(["hydra", 100]));
    let chunk = format!("chunk-{}", earned.result["chunk"]);
    assert!((level(&mut w, "dave") - 100.0).abs() < 1e-9);

    // One half-life later: 50. Shelve it; the palace reads the same value at that instant.
    w.fixed_now = Some(t0.plus(Duration::seconds(10)));
    assert!((level(&mut w, "dave") - 50.0).abs() < 1e-6);
    let shelved = ok(&mut w, "dave", "shelve", json!([chunk.clone()]));
    assert!((shelved.result["value"].as_f64().unwrap() - 50.0).abs() < 1e-6);
    assert_eq!(
        level(&mut w, "dave"),
        0.0,
        "a shelved chunk counts for nothing"
    );

    // Errors are legible and do not touch the log.
    let head = events(&w);
    assert!(matches!(
        call(
            &mut w,
            "dave",
            ActorKind::Citizen,
            "shelve",
            json!(["chunk-99"])
        ),
        Err(CommandError::Precondition { .. })
    ));
    assert!(matches!(
        call(&mut w, "dave", ActorKind::Citizen, "freeze", json!([])),
        Err(CommandError::NotAllowed { .. })
    ));
    assert!(matches!(
        call(&mut w, "dave", ActorKind::Citizen, "earn", json!(["x", 1])),
        Err(CommandError::NotAllowed { .. })
    ));
    assert!(
        matches!(
            call(&mut w, "tyler", ActorKind::Citizen, "chunks", json!([])),
            Err(CommandError::Precondition { .. })
        ),
        "tyler has not arrived"
    );
    assert_eq!(events(&w), head);

    // PTO freezes dave's palace; the world freeze is admin-only and stacks with it.
    ok(&mut w, "dave", "pto", json!([]));
    admin(&mut w, "dave", "freeze", json!([]));
    let recorded = events(&w);
    drop(w);

    // Reopen fifty seconds later: same events, same values, clocks still paused.
    let mut w = open(dir.path(), t0.plus(Duration::seconds(60)));
    assert_eq!(events(&w), recorded, "nothing recorded by reopening");
    let palace = ok(&mut w, "dave", "palace", json!([]));
    assert!(
        (palace.result["chunks"][0]["value"].as_f64().unwrap() - 50.0).abs() < 1e-6,
        "paused clocks hold the value across a restart"
    );
    let time = ok(&mut w, "dave", "time", json!([]));
    assert_eq!(time.result["world_frozen"], json!(true));
    assert_eq!(time.result["on_pto"], json!(true));

    // Thaw the world; dave is still on PTO so nothing of his moves. Resume: it fades.
    admin(&mut w, "dave", "thaw", json!([]));
    ok(&mut w, "dave", "resume", json!([]));
    w.fixed_now = Some(t0.plus(Duration::seconds(60 + 50)));
    let back = ok(&mut w, "dave", "reminisce", json!([chunk]));
    assert!(
        (back.result["value"].as_f64().unwrap() - 25.0).abs() < 1e-6,
        "power law halves at its scale"
    );
    assert!(
        (level(&mut w, "dave") - 25.0).abs() < 1e-6,
        "and counts again"
    );
}

#[test]
fn strangers_see_a_chunk_exists_but_not_what_it_is_worth() {
    let mut w = open(tempfile::tempdir().unwrap().path(), Timestamp(0));
    login(&mut w, "dave", "test", PASS).unwrap();
    let id = admin(&mut w, "dave", "earn", json!(["lich", 10])).result["chunk"]
        .as_u64()
        .unwrap();
    let now = w.now();
    let mine = w
        .store
        .view(EntityId(id), &Viewer::actor("dave"), now)
        .unwrap();
    let theirs = w
        .store
        .view(EntityId(id), &Viewer::actor("tyler"), now)
        .unwrap();
    assert_eq!(mine["components"][CHUNK]["value"], json!(10.0));
    assert_eq!(theirs["components"][CHUNK]["source"], json!("lich"));
    assert!(theirs["components"][CHUNK].get("value").is_none());
    assert!(theirs["components"][CHUNK].get("size").is_none());
}
