use super::*;
use crate::world::LogBacking;
use moondial_log::WorldId;
use moondial_registry::{ActorKind, Outcome};
use moondial_store::{Store, Viewer};
use moondial_time::Timestamp;

const PASS: &str = "correct-horse-battery";

fn open(dir: &std::path::Path, at: Timestamp) -> World {
    let mut w = World::open(
        WorldId("test".into()),
        Store::new(registry()),
        LogBacking::File(dir.to_path_buf()),
    )
    .unwrap();
    w.fixed_now = Some(at);
    w.seed(7);
    ensure_world(&mut w).unwrap();
    register(&mut w, Curves::with_half_life(Duration::seconds(10)));
    w.tick(); // the rooms fill
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
    assert_eq!(look.result["room"], json!("The Threshold"));
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
    let fog = rooms::room_by_key(&w, "fogbank").unwrap();
    assert_eq!(
        combat::creatures_in(&w, fog).len(),
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
        json!("The Threshold"),
        "you wake at the Threshold"
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

#[test]
fn observe_is_the_text_client_s_eyes() {
    let mut w = open(tempfile::tempdir().unwrap().path(), Timestamp(0));
    login(&mut w, "dave", "test", PASS).unwrap();
    login(&mut w, "tyler", "test", PASS).unwrap();
    admin(&mut w, "dave", "earn", json!(["hydra", 100, 0.5]));

    // `here` from dave's point of view, through the command, like the text port.
    let q = r#"{"root":"here","select":{"entropic.room":["name"]},"relations":{"contents":{"kinds":["citizen"],"select":{"entropic.citizen":["name"]}},"exits":{"select":{"entropic.room":["name"]}}}}"#;
    let out = ok(&mut w, "dave", "observe", json!([q]));
    let root = &out.result["root"];
    assert_eq!(
        root["components"]["entropic.room"]["name"],
        json!("The Threshold")
    );
    let names: Vec<&str> = root["relations"]["contents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            n["components"]["entropic.citizen"]["name"]
                .as_str()
                .unwrap()
        })
        .collect();
    assert_eq!(names, vec!["dave", "tyler"]);
    assert_eq!(root["relations"]["exits"][0]["via"], json!("east"));
    assert!(
        out.result["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "travel"),
        "legal commands come with the observation, even from inside a command"
    );
    let cursor = out.result["cursor"].as_u64().unwrap();

    // tyler asks for dave's window: sources yes, values no.
    let out = ok(
        &mut w,
        "tyler",
        "observe",
        json!([
            r#"{"root":{"window":"dave"},"select":{},"relations":{"contents":{"select":{"entropic.chunk":"*"}}}}"#
        ]),
    );
    let chunk = &out.result["root"]["relations"]["contents"][0]["components"]["entropic.chunk"];
    assert_eq!(chunk["source"], json!("hydra"));
    assert!(chunk.get("value").is_none() && chunk.get("vividity").is_none());

    // Events since the cursor: dave travels; tyler, still at the Threshold, sees dave leave.
    ok(&mut w, "dave", "travel", json!(["east"]));
    let out = ok(
        &mut w,
        "tyler",
        "observe",
        json!([format!(
            r#"{{"root":"self","select":{{}},"after":{cursor}}}"#
        )]),
    );
    let kinds: Vec<&str> = out.result["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"store.entity_moved"), "{kinds:?}");
    assert!(out.result["cursor"].as_u64().unwrap() > cursor);

    // Errors are legible.
    let err = call(
        &mut w,
        "dave",
        ActorKind::Citizen,
        "observe",
        json!([r#"{"root":"here","relations":{"content":{}}}"#]),
    )
    .unwrap_err();
    assert!(err.to_string().contains("did you mean contents"), "{err}");
    let err = call(
        &mut w,
        "dave",
        ActorKind::Citizen,
        "observe",
        json!(["{nope"]),
    )
    .unwrap_err();
    assert!(err.to_string().contains("not JSON"), "{err}");
}

#[test]
fn no_game_name_is_a_clear_reserved_word_or_alias() {
    let checker = crate::gate::checker();
    let mut names: Vec<String> = Vec::new();
    for r in rooms::ROOMS {
        names.push(r.name.into());
        names.push(r.key.into());
        for (exit, _) in r.exits {
            names.push((*exit).into());
        }
    }
    for c in combat::CREATURES {
        names.push(c.name.into());
    }
    let w = open(tempfile::tempdir().unwrap().path(), Timestamp(0));
    for spec in w.registry.specs() {
        names.push(spec.verb().into());
    }
    let bad: Vec<String> = names
        .iter()
        .filter_map(|n| checker.collision(n).map(|c| format!("{n}: {c}")))
        .collect();
    assert!(bad.is_empty(), "game names collide with CLEAR: {bad:?}");
    assert_eq!(
        checker.collision("The Gate").as_deref(),
        Some("the gate (an alias of CLEAR gate)")
    );
    assert_eq!(checker.collision("FACT room").as_deref(), Some("FACT"));
}

#[test]
fn the_world_moves_on_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let t0 = Timestamp(1_000_000);
    let mut w = open(dir.path(), t0);
    let f = login(&mut w, "dave", "test", PASS).unwrap();
    let narrative = |w: &World, after: u64| -> Vec<String> {
        w.log
            .read_all(&w.id, moondial_log::Cursor { after })
            .unwrap()
            .iter()
            .filter(|e| e.kind == combat::NARRATIVE && e.payload["to"] == "dave")
            .map(|e| e.payload["text"].as_str().unwrap().to_string())
            .collect()
    };

    // Rooms filled on the boot tick without anyone looking.
    let fog = rooms::room_by_key(&w, "fogbank").unwrap();
    assert_eq!(combat::creatures_in(&w, fog).len(), 2);

    // Kill a wisp: the room refills on its respawn job, not on a look.
    ok(&mut w, "dave", "travel", json!(["east"]));
    let out = fight(&mut w, "dave");
    assert_eq!(out.result["killed"], json!(true), "{}", out.summary);
    assert_eq!(combat::creatures_in(&w, fog).len(), 1);
    w.fixed_now = Some(t0.plus(Duration::seconds(30)));
    assert_eq!(w.tick(), 0, "respawn is 60 s");
    w.fixed_now = Some(t0.plus(Duration::seconds(61)));
    assert_eq!(w.tick(), 1);
    assert_eq!(
        combat::creatures_in(&w, fog).len(),
        2,
        "the fog refilled on its own"
    );

    // Engage a wisp and wait: it strikes on its own every 12 s and says so.
    let head = events(&w);
    let round = ok(&mut w, "dave", "attack", json!(["wisp"]));
    assert_eq!(round.result["killed"], json!(false), "{}", round.summary);
    let hp_after_round = combat::hp(&w, f.citizen);
    w.fixed_now = Some(t0.plus(Duration::seconds(61 + 13)));
    assert_eq!(w.tick(), 1, "one strike job due");
    assert!(
        combat::hp(&w, f.citizen) < hp_after_round + 1.3,
        "the wisp struck (healing is 0.1 per second)"
    );
    let lines = narrative(&w, head);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("The wisp hits you for"), "{lines:?}");
    assert!(
        w.jobs
            .has(combat::JOB_STRIKE, |p| p["citizen"] == f.citizen.0),
        "the next strike is scheduled"
    );

    // Retreat ends the chain.
    ok(&mut w, "dave", "retreat", json!([]));
    assert!(!w
        .jobs
        .has(combat::JOB_STRIKE, |p| p["citizen"] == f.citizen.0));
    w.fixed_now = Some(t0.plus(Duration::seconds(200)));
    assert_eq!(w.tick(), 0);

    // Walk into the Shallows: a hollow ambushes after a few seconds.
    ok(&mut w, "dave", "travel", json!(["north"]));
    assert!(w
        .jobs
        .has(combat::JOB_AMBUSH, |p| p["citizen"] == f.citizen.0));
    let head = events(&w);
    w.fixed_now = Some(t0.plus(Duration::seconds(205)));
    assert!(w.tick() >= 1);
    assert!(
        combat::engaged_with(&w, f.citizen).is_some(),
        "ambushed and engaged"
    );
    let lines = narrative(&w, head);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("The hollow stands up out of the water")),
        "{lines:?}"
    );
    // Only one hollow jumped him; the other ambush job was dropped because he is engaged.
    assert_eq!(lines.len(), 1, "{lines:?}");

    // A frozen world holds everything, including the hollow's next strike.
    admin(&mut w, "dave", "freeze", json!([]));
    w.fixed_now = Some(t0.plus(Duration::seconds(400)));
    assert_eq!(w.tick(), 0);
    admin(&mut w, "dave", "thaw", json!([]));
    assert!(w.tick() >= 1);

    // Everything above survives a restart: the pending strike is still pending.
    let pending_before: Vec<String> = w.jobs.pending().map(|j| j.kind.clone()).collect();
    drop(w);
    let w = open(dir.path(), t0.plus(Duration::seconds(401)));
    let pending_after: Vec<String> = w.jobs.pending().map(|j| j.kind.clone()).collect();
    assert_eq!(pending_before, pending_after);
}

#[test]
fn logging_in_where_something_hunts_is_arriving() {
    let dir = tempfile::tempdir().unwrap();
    let t0 = Timestamp(5_000_000);
    let mut w = open(dir.path(), t0);
    let f = login(&mut w, "dave", "test", PASS).unwrap();
    ok(&mut w, "dave", "travel", json!(["east"]));
    ok(&mut w, "dave", "travel", json!(["north"]));
    // Cancel the arrival ambush by hand, as if the driver had been down.
    w.cancel(combat::JOB_AMBUSH, |_| true).unwrap();
    assert!(!w.jobs.has(combat::JOB_AMBUSH, |_| true));
    login(&mut w, "dave", "test", PASS).unwrap();
    assert!(
        w.jobs
            .has(combat::JOB_AMBUSH, |p| p["citizen"] == f.citizen.0),
        "login in the Shallows schedules an ambush"
    );
    // A restart with dave standing there does the same, and gives old creatures sprites.
    drop(w);
    let w = open(dir.path(), t0.plus(Duration::seconds(1)));
    assert!(w
        .jobs
        .has(combat::JOB_AMBUSH, |p| p["citizen"] == f.citizen.0));
    let shallows = rooms::room_by_key(&w, "shallows").unwrap();
    for (id, _, _, _) in combat::creatures_in(&w, shallows) {
        assert!(w.store.entity(id).unwrap().components.contains_key(SPRITE));
    }
}
