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
    assert_eq!(look.result["exits"], json!(["east", "north"]));
    assert!(matches!(
        call(
            &mut w,
            "dave",
            ActorKind::Citizen,
            "travel",
            json!(["sideways"])
        ),
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
    let q = r#"{"root":"here","select":{"disremembered.room":["name"]},"relations":{"contents":{"kinds":["citizen"],"select":{"disremembered.citizen":["name"]}},"exits":{"select":{"disremembered.room":["name"]}}}}"#;
    let out = ok(&mut w, "dave", "observe", json!([q]));
    let root = &out.result["root"];
    assert_eq!(
        root["components"]["disremembered.room"]["name"],
        json!("The Threshold")
    );
    let names: Vec<&str> = root["relations"]["contents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            n["components"]["disremembered.citizen"]["name"]
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
            r#"{"root":{"window":"dave"},"select":{},"relations":{"contents":{"select":{"disremembered.chunk":"*"}}}}"#
        ]),
    );
    let chunk =
        &out.result["root"]["relations"]["contents"][0]["components"]["disremembered.chunk"];
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
        assert_eq!(plain_str(&w, id, SPRITE, "frame"), Some("hollow"));
    }
    assert_eq!(plain_str(&w, f.citizen, SPRITE, "frame"), Some("citizen"));
}

#[test]
fn sprites_from_before_frames_are_upgraded() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = open(dir.path(), Timestamp(0));
    let f = login(&mut w, "dave", "test", PASS).unwrap();
    let fog = rooms::room_by_key(&w, "fogbank").unwrap();
    let wisp = combat::creatures_in(&w, fog)[0].0;
    // Strip the frame from a wisp and from dave, as an older log would have them.
    for id in [wisp, f.citizen] {
        let mut old = std::collections::BTreeMap::new();
        old.insert(
            "glyph".to_string(),
            moondial_store::Field::Plain(json!("x")),
        );
        old.insert(
            "tint".to_string(),
            moondial_store::Field::Plain(json!("#fff")),
        );
        w.record(
            moondial_log::Actor::Driver,
            moondial_store::StoreEvent::ComponentSet {
                id,
                component: SPRITE.into(),
                fields: old,
            },
        )
        .unwrap();
    }
    assert!(plain_str(&w, wisp, SPRITE, "frame").is_none());
    drop(w);
    let mut w = open(dir.path(), Timestamp(1000));
    assert_eq!(
        plain_str(&w, wisp, SPRITE, "frame"),
        Some("wisp"),
        "boot upgrades creatures"
    );
    assert!(
        plain_str(&w, f.citizen, SPRITE, "frame").is_none(),
        "citizens wait for their login"
    );
    login(&mut w, "dave", "test", PASS).unwrap();
    assert_eq!(plain_str(&w, f.citizen, SPRITE, "frame"), Some("citizen"));
}

#[test]
fn a_misshapen_query_is_the_asker_s_mistake_and_says_what_was_expected() {
    let mut w = open(tempfile::tempdir().unwrap().path(), Timestamp(0));
    login(&mut w, "dave", "test", PASS).unwrap();
    let q = r#"{"root":"here","relations":{"contents":{"where":{"disremembered.creature.hp":{"lt":8}}}}}"#;
    let e = call(&mut w, "dave", ActorKind::Citizen, "observe", json!([q])).unwrap_err();
    let text = e.to_string();
    assert!(
        matches!(e, CommandError::Precondition { .. }),
        "a bad shape is a precondition, not an internal error: {text}"
    );
    assert!(text.contains("\"op\""), "{text}");
    assert!(!text.contains("built-in"), "{text}");
}

#[test]
fn a_log_from_other_modules_is_refused_with_directions() {
    use moondial_log::{EventLog, FileLog, NewEvent};
    let dir = tempfile::tempdir().unwrap();
    let id = moondial_log::WorldId("w".into());
    // A world whose first event creates an entity with a component this driver never had.
    {
        let mut log = FileLog::open(dir.path()).unwrap();
        log.append(
            &id,
            NewEvent {
                at: Timestamp(0),
                kind: "store.component_set".into(),
                actor: moondial_log::Actor::Driver,
                cause: None,
                payload: json!({"op": "component_set", "id": 1, "component": "entropic.room",
                    "fields": {"name": {"kind": "plain", "value": "x"}}}),
            },
        )
        .unwrap();
    }
    let err = World::open(
        id,
        moondial_store::Store::new(super::registry()),
        crate::world::LogBacking::File(dir.path().into()),
    )
    .err()
    .expect("a foreign log must not open");
    assert!(err.contains("entropic.room"), "{err}");
    assert!(err.contains("MOONDIAL_WORLD"), "{err}");
}

/// The vocabulary's verb signatures are the registry's: every registered command is an
/// approved verb whose parameters (name, type, required, in order) the vocabulary
/// states exactly, so the checker in the browser refuses what the registry would.
#[test]
fn the_vocabulary_s_signatures_are_the_registry_s() {
    let w = open(tempfile::tempdir().unwrap().path(), Timestamp(0));
    let vocab = crate::gate::disremembered_vocabulary();
    let verbs = vocab["approved_verbs"]
        .as_object()
        .expect("approved_verbs is a map of verb to signature");
    for spec in w.registry.specs() {
        let verb = spec.verb();
        let sig = verbs
            .get(verb)
            .unwrap_or_else(|| panic!("{verb} is registered but not an approved verb"));
        let params = sig["params"].as_array().unwrap_or_else(|| {
            panic!("{verb} is registered but the vocabulary gives it no signature")
        });
        let expected: Vec<Value> = spec
            .params
            .iter()
            .map(|p| {
                let mut v = json!({"name": p.name, "required": p.required});
                let ty = serde_json::to_value(&p.ty).unwrap();
                v["type"] = ty["type"].clone();
                if let Some(values) = ty.get("values") {
                    v["values"] = values.clone();
                }
                v
            })
            .collect();
        let got: Vec<Value> = params
            .iter()
            .map(|p| {
                let mut v =
                    json!({"name": p["name"], "required": p["required"], "type": p["type"]});
                if let Some(values) = p.get("values") {
                    v["values"] = values.clone();
                }
                v
            })
            .collect();
        assert_eq!(
            got, expected,
            "signature of {verb} in clear/vocab/disremembered.yaml"
        );
    }
}

/// Players extend the world: a dug room costs a chunk, inherits its half-life, fades
/// toward a half-remembered line unless visited, keeps its name and exits regardless,
/// is the digger's to describe and name, and theirs to share by permit.
#[test]
fn players_dig_rooms_that_fade_but_stay() {
    let mut w = open(tempfile::tempdir().unwrap().path(), Timestamp(0));
    login(&mut w, "dave", "test", PASS).unwrap();
    login(&mut w, "tyler", "test", PASS).unwrap();
    // A vivid chunk: a long half-life, which the room will inherit.
    let out = admin(&mut w, "dave", "earn", json!(["hydra", 100, 1.0]));
    let chunk = format!("chunk-{}", out.result["chunk"].as_u64().unwrap());

    // No name may carry a reserved word or a glossary alias, and the chunk is spent.
    let err = call(
        &mut w,
        "dave",
        ActorKind::Citizen,
        "dig",
        json!(["down", chunk, "The Gate"]),
    )
    .unwrap_err();
    assert!(err.to_string().contains("reserves"), "{err}");
    let out = ok(
        &mut w,
        "dave",
        "dig",
        json!(["down", chunk, "The Salt Stair"]),
    );
    assert!(
        out.summary
            .starts_with("You dig down with the memory of hydra."),
        "{}",
        out.summary
    );
    let room = EntityId(out.result["room"].as_u64().unwrap());
    assert_eq!(out.result["back"], "up");
    assert_eq!(out.result["key"], format!("salt-stair-{}", room.0));
    assert!(
        w.store
            .entity(EntityId(
                chunk.trim_start_matches("chunk-").parse().unwrap()
            ))
            .is_none(),
        "the chunk is spent"
    );
    let hl = out.result["half_life_ms"].as_i64().unwrap();
    assert_eq!(
        hl,
        3 * 10 * 1000,
        "a vividity-1 chunk lasts 3x the test world's 10 s half-life"
    );

    // One dig from a seed room per citizen; the digger's own rooms are open to them.
    let out2 = admin(&mut w, "dave", "earn", json!(["wisp", 10, 0.0]));
    let chunk2 = format!("chunk-{}", out2.result["chunk"].as_u64().unwrap());
    let err = call(
        &mut w,
        "dave",
        ActorKind::Citizen,
        "dig",
        json!(["under", chunk2]),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("already dug from this room"),
        "{err}"
    );
    ok(&mut w, "dave", "travel", json!(["down"]));
    let out3 = ok(&mut w, "dave", "dig", json!(["east", chunk2]));
    assert_eq!(out3.result["name"], "An Unnamed Digging");

    // Describing and naming are the owner's; a visitor is refused until permitted.
    ok(
        &mut w,
        "dave",
        "describe",
        json!(["Steps of salt, each one damp. The air tastes of the sea that is not here."]),
    );
    let look = ok(&mut w, "dave", "look", json!([]));
    assert!(
        look.summary
            .starts_with("The Salt Stair. Steps of salt, each one damp."),
        "{}",
        look.summary
    );
    assert!(
        look.summary
            .contains("Dug by dave with the memory of hydra."),
        "{}",
        look.summary
    );
    ok(&mut w, "tyler", "travel", json!(["down"]));
    let err = call(
        &mut w,
        "tyler",
        ActorKind::Citizen,
        "describe",
        json!(["Mine now."]),
    )
    .unwrap_err();
    assert!(err.to_string().contains("permission"), "{err}");
    let err = call(
        &mut w,
        "tyler",
        ActorKind::Citizen,
        "permit",
        json!(["dave"]),
    )
    .unwrap_err();
    assert!(err.to_string().contains("owner"), "{err}");
    ok(&mut w, "dave", "permit", json!(["tyler", 1]));
    ok(&mut w, "tyler", "name", json!(["The Salt Stair, Swept"]));
    assert_eq!(
        ok(&mut w, "tyler", "look", json!([])).result["room"],
        "The Salt Stair, Swept"
    );
    // The permit expires.
    w.fixed_now = Some(w.now().plus(Duration::seconds(3601)));
    let err = call(
        &mut w,
        "tyler",
        ActorKind::Citizen,
        "describe",
        json!(["Late."]),
    )
    .unwrap_err();
    assert!(err.to_string().contains("permission"), "{err}");
    ok(&mut w, "dave", "permit", json!(["tyler"]));
    ok(
        &mut w,
        "tyler",
        "describe",
        json!(["Steps of salt. Swept now, by a friend."]),
    );
    ok(&mut w, "dave", "revoke", json!(["tyler"]));
    assert!(call(
        &mut w,
        "tyler",
        ActorKind::Citizen,
        "describe",
        json!(["Again."])
    )
    .is_err());

    // The seed rooms are the world's.
    ok(&mut w, "dave", "travel", json!(["up"]));
    let err = call(
        &mut w,
        "dave",
        ActorKind::Citizen,
        "describe",
        json!(["Mine."]),
    )
    .unwrap_err();
    assert!(err.to_string().contains("the world's"), "{err}");

    // Memory fades with nobody there: the description dims, then fades; the room stays.
    ok(&mut w, "tyler", "travel", json!(["up"]));
    let full = describe_room(&mut w, "dave", room);
    assert!(
        full.starts_with("The Salt Stair, Swept. Steps of salt. Swept now, by a friend."),
        "{full}"
    );
    w.fixed_now = Some(w.now().plus(Duration(hl * 2)));
    let dim = describe_room(&mut w, "dave", room);
    assert!(
        dim.contains("Steps of salt. The rest is hard to recall."),
        "{dim}"
    );
    w.fixed_now = Some(w.now().plus(Duration(hl * 3)));
    let faded = describe_room(&mut w, "dave", room);
    assert!(faded.contains(building::HALF_REMEMBERED), "{faded}");
    assert!(
        faded.contains("Exits: up, east."),
        "the exits survive: {faded}"
    );
    // A visit remembers it whole again.
    ok(&mut w, "dave", "travel", json!(["down"]));
    let back = ok(&mut w, "dave", "look", json!([]));
    assert!(
        back.summary
            .contains("Steps of salt. Swept now, by a friend."),
        "{}",
        back.summary
    );
    assert!(building::memory(&w, room).unwrap() > 0.99);
}

fn describe_room(w: &mut World, actor: &str, room: EntityId) -> String {
    let ctx = CallContext {
        actor: actor.into(),
        kind: ActorKind::Citizen,
    };
    rooms::describe(w, actor, &ctx, room).unwrap().summary
}

#[test]
fn append_adds_to_a_description_and_arrivals_are_counted() {
    let mut w = open(tempfile::tempdir().unwrap().path(), Timestamp(0));
    login(&mut w, "dave", "terminal", PASS).unwrap();
    let out = admin(&mut w, "dave", "earn", json!(["hydra", 100, 1.0]));
    let chunk = format!("chunk-{}", out.result["chunk"].as_u64().unwrap());
    ok(
        &mut w,
        "dave",
        "dig",
        json!(["down", chunk, "The Salt Stair"]),
    );
    ok(&mut w, "dave", "travel", json!(["down"]));
    // Appending to a fresh digging drops the placeholder; then sentences join with a space.
    let a = ok(&mut w, "dave", "append", json!(["Steps of salt"]));
    assert_eq!(a.result["description"], "Steps of salt");
    let b = ok(&mut w, "dave", "append", json!(["Each one damp."]));
    assert_eq!(b.result["description"], "Steps of salt. Each one damp.");
    let c = ok(&mut w, "dave", "append", json!(["The air tastes of salt."]));
    assert_eq!(
        c.result["description"],
        "Steps of salt. Each one damp. The air tastes of salt."
    );
    ok(&mut w, "dave", "describe", json!(["Afresh."]));
    assert_eq!(
        ok(&mut w, "dave", "append", json!(["And more."])).result["description"],
        "Afresh. And more."
    );
    let long = "x".repeat(800);
    assert!(call(&mut w, "dave", ActorKind::Citizen, "append", json!([long])).is_err());

    // Arrivals: the first login, then logins after an absence; not every request.
    login(&mut w, "dave", "http", PASS).unwrap();
    login(&mut w, "dave", "http", PASS).unwrap();
    w.fixed_now = Some(w.now().plus(Duration::seconds(11 * 60)));
    login(&mut w, "dave", "gate", PASS).unwrap();
    let arrivals: Vec<_> = w
        .log
        .read_all(&w.id, moondial_log::Cursor::START)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == crate::world::ARRIVED)
        .collect();
    assert_eq!(arrivals.len(), 2, "{arrivals:?}");
    assert_eq!(arrivals[0].payload["origin"], "terminal");
    assert_eq!(arrivals[0].payload["fresh"], true);
    assert_eq!(arrivals[1].payload["origin"], "gate");
    assert!(arrivals[1].payload["away_ms"].as_i64().unwrap() >= 11 * 60 * 1000);
    let notices = w.take_notices();
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(notices[0].contains("arrived for the first time, via terminal"));
    assert!(notices[1].contains("is back, via gate"));
    let report = crate::net::citizens_report(&w, 0);
    let dave = report.iter().find(|r| r["name"] == "dave").unwrap();
    assert_eq!(dave["arrivals"], 2);
    assert_eq!(dave["rooms_dug"], 1);
    assert_eq!(dave["origins"]["gate"], 1);
}

#[test]
fn five_wrong_passphrases_lock_a_name_for_five_minutes() {
    let mut w = open(tempfile::tempdir().unwrap().path(), Timestamp(0));
    login(&mut w, "dave", "test", PASS).unwrap();
    for _ in 0..5 {
        let Err(e) = login(&mut w, "dave", "test", "not-the-passphrase") else {
            panic!("a wrong passphrase got in")
        };
        assert!(matches!(e, LoginError::WrongPassphrase { .. }), "{e}");
    }
    // The right passphrase is refused too, until the lock lifts: no oracle for guessing.
    let Err(e) = login(&mut w, "dave", "test", PASS) else {
        panic!("the lock did not hold")
    };
    assert!(matches!(e, LoginError::Locked { minutes: 5, .. }), "{e}");
    w.fixed_now = Some(w.now().plus(Duration::seconds(5 * 60 + 1)));
    login(&mut w, "dave", "test", PASS).unwrap();
    // Success resets the count.
    assert!(login(&mut w, "dave", "test", "wrong-again-here").is_err());
    login(&mut w, "dave", "test", PASS).unwrap();
}

#[test]
fn the_public_log_hides_passphrase_hashes_and_owner_fields() {
    let mut w = open(tempfile::tempdir().unwrap().path(), Timestamp(0));
    login(&mut w, "dave", "test", PASS).unwrap();
    login(&mut w, "tyler", "test", PASS).unwrap();
    admin(&mut w, "dave", "earn", json!(["hydra", 100, 0.5]));
    let now = w.now();
    let all = w.log.read_all(&w.id, moondial_log::Cursor::START).unwrap();
    for viewer in [
        Viewer::anonymous(),
        Viewer::actor("tyler"),
        Viewer::actor("dave"),
    ] {
        let seen: Vec<_> = all
            .iter()
            .filter(|e| crate::observe::visible(&w, &viewer, now, e))
            .map(|e| crate::observe::redacted(&w, &viewer, e.clone()))
            .collect();
        let text = serde_json::to_string(&seen).unwrap();
        assert!(
            !text.contains("passphrase_hash"),
            "{:?} sees a hash",
            viewer.actor
        );
        assert!(!text.contains("$argon2"), "{:?} sees a hash", viewer.actor);
        let chunk_values = seen
            .iter()
            .filter(|e| e.kind == "store.component_set" && e.payload["component"] == CHUNK)
            .filter(|e| e.payload["fields"].get("value").is_some())
            .count();
        if viewer.actor.as_deref() == Some("dave") {
            assert!(chunk_values > 0, "the owner sees their chunk's value");
        } else {
            assert_eq!(
                chunk_values, 0,
                "{:?} sees an owner-only field",
                viewer.actor
            );
        }
    }
    // The operator sees everything.
    let whole: Vec<_> = all
        .iter()
        .map(|e| crate::observe::redacted(&w, &Viewer::admin(), e.clone()))
        .collect();
    assert!(serde_json::to_string(&whole)
        .unwrap()
        .contains("passphrase_hash"));
}

#[test]
fn an_admin_can_forget_a_passphrase_and_the_next_login_claims_it() {
    let mut w = open(tempfile::tempdir().unwrap().path(), Timestamp(0));
    login(&mut w, "dave", "test", PASS).unwrap();
    assert!(login(&mut w, "dave", "test", "a-different-one").is_err());
    let out = admin(&mut w, "tyler", "forget", json!(["dave"]));
    assert_eq!(out.result["reset"], true);
    // Too short is still refused; a good one claims the name.
    assert!(login(&mut w, "dave", "test", "short").is_err());
    login(&mut w, "dave", "test", "a-different-one").unwrap();
    assert!(login(&mut w, "dave", "test", PASS).is_err());
    // Citizens cannot reset anyone.
    let ctx = CallContext {
        actor: "dave".into(),
        kind: ActorKind::Citizen,
    };
    assert!(w.call(&ctx, "forget", &json!(["tyler"])).is_err());
}

#[test]
fn the_map_is_connected_and_new_seed_exits_reach_old_rooms() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = open(dir.path(), Timestamp(0));
    // Every room reachable from the Threshold, and every exit lands on a seeded room.
    let mut seen = std::collections::BTreeSet::new();
    let mut stack = vec![rooms::ARRIVAL];
    while let Some(k) = stack.pop() {
        if !seen.insert(k) {
            continue;
        }
        let def = rooms::ROOMS.iter().find(|r| r.key == k).unwrap();
        for (_, to) in def.exits {
            assert!(
                rooms::ROOMS.iter().any(|r| r.key == *to),
                "{k} exits to unknown {to}"
            );
            stack.push(to);
        }
    }
    assert_eq!(
        seen.len(),
        rooms::ROOMS.len(),
        "unreachable rooms: {:?}",
        rooms::ROOMS
            .iter()
            .map(|r| r.key)
            .filter(|k| !seen.contains(k))
            .collect::<Vec<_>>()
    );
    // Simulate an old log: strip the fogbank's south exit, then reboot; it comes back,
    // and a dug exit beside it survives.
    let fog = rooms::room_by_key(&w, "fogbank").unwrap();
    let mut exits = rooms::exits_of(&w, fog);
    exits.remove("south");
    let mut m = serde_json::Map::new();
    for (k, v) in &exits {
        m.insert(k.clone(), json!(v.0));
    }
    m.insert(
        "hole".into(),
        json!(rooms::room_by_key(&w, "ossuary").unwrap().0),
    );
    let mut fields = std::collections::BTreeMap::new();
    fields.insert(
        "exits".to_string(),
        moondial_store::Field::Plain(Value::Object(m)),
    );
    w.record(
        Actor::Driver,
        StoreEvent::ComponentSet {
            id: fog,
            component: EXITS.into(),
            fields,
        },
    )
    .unwrap();
    assert!(!rooms::exits_of(&w, fog).contains_key("south"));
    drop(w);
    let w = open(dir.path(), Timestamp(1));
    let exits = rooms::exits_of(&w, fog);
    assert_eq!(
        exits.get("south"),
        Some(&rooms::room_by_key(&w, "midden").unwrap())
    );
    assert!(exits.contains_key("hole"), "a dug exit was lost");
    // A dug room sitting where a seed exit goes keeps its door; the operator hears.
    let mut w = w;
    let thr = rooms::room_by_key(&w, rooms::ARRIVAL).unwrap();
    let mut m = serde_json::Map::new();
    for (k, v) in rooms::exits_of(&w, thr) {
        m.insert(k, json!(v.0));
    }
    let dug = rooms::room_by_key(&w, "midden").unwrap();
    m.insert("north".into(), json!(dug.0));
    let mut fields = std::collections::BTreeMap::new();
    fields.insert(
        "exits".to_string(),
        moondial_store::Field::Plain(Value::Object(m)),
    );
    w.record(
        Actor::Driver,
        StoreEvent::ComponentSet {
            id: thr,
            component: EXITS.into(),
            fields,
        },
    )
    .unwrap();
    drop(w);
    let mut w = open(dir.path(), Timestamp(2));
    assert_eq!(rooms::exits_of(&w, thr).get("north"), Some(&dug));
    let notices = w.take_notices();
    assert!(notices.iter().any(|n| n.contains("`north`")), "{notices:?}");
    // Every spawn names a creature that exists.
    for r in rooms::ROOMS {
        if let Some((c, _, _)) = r.spawn {
            assert!(
                combat::CREATURES.iter().any(|d| d.key == c),
                "{}: no creature {c}",
                r.key
            );
        }
    }
}

#[test]
fn remembering_a_room_costs_a_chunk_and_makes_it_last() {
    let mut w = open(tempfile::tempdir().unwrap().path(), Timestamp(0));
    login(&mut w, "dave", "test", PASS).unwrap();
    login(&mut w, "tyler", "test", PASS).unwrap();
    let out = admin(&mut w, "dave", "earn", json!(["hydra", 100, 1.0]));
    let chunk = format!("chunk-{}", out.result["chunk"].as_u64().unwrap());
    let dug = ok(
        &mut w,
        "dave",
        "dig",
        json!(["down", chunk, "The Salt Stair"]),
    );
    let room = EntityId(dug.result["room"].as_u64().unwrap());
    let hl0 = dug.result["half_life_ms"].as_i64().unwrap();

    // The world's rooms remember themselves.
    let out = admin(&mut w, "tyler", "earn", json!(["wisp", 10, 0.0]));
    let tc = format!("chunk-{}", out.result["chunk"].as_u64().unwrap());
    let err = call(&mut w, "tyler", ActorKind::Citizen, "remember", json!([tc])).unwrap_err();
    assert!(err.to_string().contains("the world's"), "{err}");

    // Tyler keeps Dave's room: his chunk is gone, the half-life grew by its own.
    ok(&mut w, "tyler", "travel", json!(["down"]));
    let level_before = level(&mut w, "tyler");
    let out = ok(&mut w, "tyler", "remember", json!([tc]));
    assert!(
        out.summary
            .starts_with("You give up the memory of wisp, and The Salt Stair keeps it."),
        "{}",
        out.summary
    );
    let hl1 = out.result["half_life_ms"].as_i64().unwrap();
    assert_eq!(hl1, hl0 + out.result["added_ms"].as_i64().unwrap());
    assert!(hl1 > hl0);
    assert!(w
        .store
        .entity(EntityId(tc.trim_start_matches("chunk-").parse().unwrap()))
        .is_none());
    assert!(
        level(&mut w, "tyler") < level_before,
        "the chunk was the cost"
    );
    let look = ok(&mut w, "tyler", "look", json!([]));
    assert!(
        look.summary.contains("Kept in memory by tyler (wisp)."),
        "{}",
        look.summary
    );

    // The record is public: an anonymous reader of the log sees who paid.
    let now = w.now();
    let seen: Vec<_> = w
        .log
        .read_all(&w.id, moondial_log::Cursor::START)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == building::REMEMBERED)
        .filter(|e| crate::observe::visible(&w, &Viewer::anonymous(), now, e))
        .collect();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].payload["by"], "tyler");
    assert_eq!(seen[0].payload["owner"], "dave");
    assert_eq!(seen[0].payload["room"], room.0);

    // Chunks must be yours and in your window or palace.
    let err = call(
        &mut w,
        "tyler",
        ActorKind::Citizen,
        "remember",
        json!([chunk]),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("not yours") || err.to_string().contains("no chunk"),
        "{err}"
    );
}

#[test]
fn a_quiet_citizen_is_disremembered_by_the_room_and_returns_as_an_arrival() {
    let t0 = Timestamp(1_000_000);
    let mut w = open(tempfile::tempdir().unwrap().path(), t0);
    login(&mut w, "dave", "test", PASS).unwrap();
    login(&mut w, "tyler", "test", PASS).unwrap();
    // Dave walks into the Shallows, where hollows hunt, and stops typing.
    ok(&mut w, "dave", "travel", json!(["east"]));
    ok(&mut w, "dave", "travel", json!(["north"]));
    let shallows = rooms::room_by_key(&w, "shallows").unwrap();
    assert!(presence::is_here(
        &w,
        find_citizen(&w, "dave").unwrap().citizen
    ));
    // Tyler, active, sees him.
    ok(&mut w, "tyler", "travel", json!(["east"]));
    ok(&mut w, "tyler", "travel", json!(["north"]));
    let look = ok(&mut w, "tyler", "look", json!([]));
    assert!(
        look.summary.contains("Also here: dave."),
        "{}",
        look.summary
    );

    // Ten minutes of silence: the sweep marks him gone, and the fight that had started ends.
    w.fixed_now = Some(t0.plus(Duration::seconds(5)));
    w.tick(); // the ambush lands on both; dave is engaged
    let dave = find_citizen(&w, "dave").unwrap().citizen;
    assert!(
        combat::engaged_with(&w, dave).is_some(),
        "the hollow should have ambushed dave"
    );
    w.fixed_now = Some(t0.plus(Duration::seconds(5)));
    ok(&mut w, "tyler", "retreat", json!([]));
    w.fixed_now = Some(t0.plus(Duration::seconds(11 * 60)));
    ok(&mut w, "tyler", "look", json!([])); // tyler stays active
    let hp_before = w
        .store
        .read_field(dave, BODY, "hp", w.now())
        .unwrap()
        .as_f64()
        .unwrap();
    w.tick();
    assert!(!presence::is_here(&w, dave), "dave went quiet");
    assert!(
        combat::engaged_with(&w, dave).is_none(),
        "the fight ended without a blow"
    );
    let departed: Vec<_> = w
        .log
        .read_all(&w.id, moondial_log::Cursor::START)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == presence::DEPARTED)
        .collect();
    assert_eq!(departed.len(), 1);
    assert_eq!(departed[0].payload["room"], shallows.0);
    let look = ok(&mut w, "tyler", "look", json!([]));
    assert!(
        look.summary
            .contains("You are alone. Remembered here: dave."),
        "{}",
        look.summary
    );
    assert_eq!(look.result["citizens"], json!([]));
    assert_eq!(look.result["remembered"][0]["name"], "dave");

    // Creatures leave the gone alone: strikes and ambushes skip him for an hour.
    for m in 12..70 {
        w.fixed_now = Some(t0.plus(Duration::seconds(m * 60)));
        ok(&mut w, "tyler", "look", json!([]));
        w.tick();
    }
    let hp_after = w
        .store
        .read_field(dave, BODY, "hp", w.now())
        .unwrap()
        .as_f64()
        .unwrap();
    assert!(
        hp_after >= hp_before,
        "a gone citizen is not struck: {hp_before} -> {hp_after}"
    );

    // The trace fades on the active half-life (10 s in tests): after an hour he is not listed.
    let look = ok(&mut w, "tyler", "look", json!([]));
    assert!(
        !look.summary.contains("Remembered here"),
        "{}",
        look.summary
    );

    // Dave types again: an arrival, here, and the hollows notice.
    let arrivals_before = w
        .log
        .read_all(&w.id, moondial_log::Cursor::START)
        .unwrap()
        .iter()
        .filter(|e| e.kind == crate::world::ARRIVED && e.payload["actor"] == "dave")
        .count();
    let look = ok(&mut w, "dave", "look", json!([]));
    assert!(
        look.summary.contains("Also here: tyler."),
        "{}",
        look.summary
    );
    assert!(presence::is_here(&w, dave));
    let arrivals_after = w
        .log
        .read_all(&w.id, moondial_log::Cursor::START)
        .unwrap()
        .iter()
        .filter(|e| e.kind == crate::world::ARRIVED && e.payload["actor"] == "dave")
        .count();
    assert_eq!(arrivals_after, arrivals_before + 1);
    let look = ok(&mut w, "tyler", "look", json!([]));
    assert!(
        look.summary.contains("Also here: dave."),
        "{}",
        look.summary
    );
    w.fixed_now = Some(t0.plus(Duration::seconds(70 * 60 + 5)));
    w.tick();
    assert!(
        combat::engaged_with(&w, dave).is_some(),
        "back in the Shallows, the hollow comes for him"
    );
}

#[test]
fn a_remembered_creature_earns_a_name_can_be_robbed_and_forgets_again() {
    let t0 = Timestamp(1_000_000);
    let mut w = open(tempfile::tempdir().unwrap().path(), t0);
    login(&mut w, "dave", "test", PASS).unwrap();
    login(&mut w, "tyler", "test", PASS).unwrap();
    // The Midden, where the ragpicker sorts. It is not aggressive.
    for who in ["dave", "tyler"] {
        ok(&mut w, who, "travel", json!(["east"]));
        ok(&mut w, who, "travel", json!(["south"]));
    }
    let midden = rooms::room_by_key(&w, "midden").unwrap();
    let rag = combat::creatures_in(&w, midden)[0].0;
    assert_eq!(mind::tier(&w, rag), 0);
    let first = ok(&mut w, "dave", "look", json!([])).summary;
    assert!(first.contains("Here: ragpicker (16.0/16.0)."), "{first}");

    // A small memory: it holds it, no name yet.
    let c = admin(&mut w, "dave", "earn", json!(["wisp", 40, 0.5]));
    let c = format!("chunk-{}", c.result["chunk"].as_u64().unwrap());
    let out = ok(&mut w, "dave", "remember", json!([c, "ragpicker"]));
    assert!(out.summary.starts_with("You give up the memory of wisp, and the ragpicker keeps it. It holds 40 now. At 60 it would be named."), "{}", out.summary);
    assert_eq!(mind::tier(&w, rag), 0);
    assert_eq!(mind::keepers(&w, rag), vec!["dave".to_string()]);
    let level_dave = level(&mut w, "dave");
    assert!(
        level_dave < 1.0,
        "the chunk left dave's window: {level_dave}"
    );

    // Over the bar: a name, and it knows who kept it.
    let c = admin(&mut w, "tyler", "earn", json!(["hollow", 60, 0.5]));
    let c = format!("chunk-{}", c.result["chunk"].as_u64().unwrap());
    let out = ok(&mut w, "tyler", "remember", json!([c, "the ragpicker"]));
    assert!(out.summary.contains("and it is"), "{}", out.summary);
    assert_eq!(mind::tier(&w, rag), 1);
    let name = mind::name(&w, rag).unwrap();
    let look = ok(&mut w, "tyler", "look", json!([]));
    assert!(
        look.summary.contains(&format!(
            "Here: {name} the ragpicker (16.0/16.0, holding 100). {name} the ragpicker knows you."
        )),
        "{}",
        look.summary
    );
    let tiers: Vec<_> = w
        .log
        .read_all(&w.id, moondial_log::Cursor::START)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == mind::TIER)
        .collect();
    assert_eq!(tiers.len(), 1);
    assert_eq!(tiers[0].payload["to"], 1);
    // A stranger is told who it knows.
    login(&mut w, "mara", "test", PASS).unwrap();
    ok(&mut w, "mara", "travel", json!(["east"]));
    ok(&mut w, "mara", "travel", json!(["south"]));
    let look = ok(&mut w, "mara", "look", json!([]));
    assert!(
        look.summary.contains(&format!(
            "{name} the ragpicker does not know you. It knows tyler, dave."
        )),
        "{}",
        look.summary
    );

    // Mara robs it: it goes down, she takes one memory, it stands up shaken.
    w.seed(7);
    let mut out = None;
    for _ in 0..40 {
        let o = ok(&mut w, "mara", "attack", json!([&name]));
        if o.result["taken"].is_u64() {
            out = Some(o);
            break;
        }
        if o.result["died"] == true {
            panic!("mara died to a ragpicker");
        }
    }
    let out = out.expect("it should have gone down");
    assert!(
        out.summary
            .contains("goes down, and you take its memory of"),
        "{}",
        out.summary
    );
    assert!(w.store.entity(rag).is_some(), "it survives");
    assert!(mind::shaken(&w, rag));
    assert!(level(&mut w, "mara") > 30.0, "mara holds what she took");
    let err = call(&mut w, "mara", ActorKind::Citizen, "attack", json!([&name])).unwrap_err();
    assert!(err.to_string().contains("not itself yet"), "{err}");
    let taken: Vec<_> = w
        .log
        .read_all(&w.id, moondial_log::Cursor::START)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == mind::TAKEN)
        .collect();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].payload["by"], "mara");

    // Hysteresis: it keeps its name above 30; below that, on a sweep, it forgets.
    let left = mind::level(&w, rag);
    assert!(left > 30.0 || mind::tier(&w, rag) == 0, "{left}");
    // The test world's chunks halve every 10 s; after a few minutes nothing is left.
    w.fixed_now = Some(t0.plus(Duration::seconds(300)));
    w.tick();
    assert_eq!(
        mind::tier(&w, rag),
        0,
        "forgotten: {}",
        mind::level(&w, rag)
    );
    assert!(mind::name(&w, rag).is_none());
    assert!(
        ok(&mut w, "dave", "look", json!([]))
            .summary
            .contains("Here: ragpicker (16.0/16.0)."),
        "scenery again"
    );
    // Remembered again, it gets the same name back.
    let c = admin(&mut w, "dave", "earn", json!(["keeper", 400, 1.0]));
    let c = format!("chunk-{}", c.result["chunk"].as_u64().unwrap());
    ok(&mut w, "dave", "remember", json!([c, "ragpicker"]));
    assert_eq!(mind::name(&w, rag).as_deref(), Some(name.as_str()));
}
