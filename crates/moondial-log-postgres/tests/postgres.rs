//! Runs against a real PostgreSQL when `MOONDIAL_TEST_PG` names it; otherwise skips.
//! CI provides one as a service; locally: `MOONDIAL_TEST_PG=postgres://user@host/db`.

use moondial_log::{Actor, Cursor, EventLog, FileLog, LogError, NewEvent, Seq, WorldId};
use moondial_log_postgres::{copy_world, PostgresLog};
use moondial_time::Timestamp;
use serde_json::json;

fn url() -> Option<String> {
    std::env::var("MOONDIAL_TEST_PG")
        .ok()
        .filter(|s| !s.is_empty())
}

fn fresh_world() -> WorldId {
    WorldId(format!(
        "test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn ev(at: i64, kind: &str, n: u64) -> NewEvent {
    NewEvent {
        at: Timestamp(at),
        kind: kind.into(),
        actor: Actor::Citizen("dave".into()),
        cause: None,
        payload: json!({"n": n, "text": "héllo \"quoted\""}),
    }
}

#[test]
fn appends_read_back_in_order_with_dense_sequences() {
    let Some(url) = url() else {
        eprintln!("MOONDIAL_TEST_PG not set; skipping");
        return;
    };
    let mut log = PostgresLog::open(&url)
        .unwrap_or_else(|e| panic!("cannot open the test database at {url}: {e}"));
    let w = fresh_world();
    assert_eq!(log.head(&w).unwrap(), None);
    for i in 1..=5 {
        let e = log.append(&w, ev(1000 * i as i64, "t.k", i)).unwrap();
        assert_eq!(e.seq, Seq(i));
        assert_eq!(e.world, w);
    }
    assert_eq!(log.head(&w).unwrap(), Some(Seq(5)));
    let all = log.read_all(&w, Cursor::START).unwrap();
    assert_eq!(all.len(), 5);
    assert_eq!(all[2].payload["n"], 3);
    assert_eq!(all[2].payload["text"], "héllo \"quoted\"");
    assert_eq!(all[2].actor, Actor::Citizen("dave".into()));
    let tail = log.read(&w, Cursor { after: 3 }, 10).unwrap();
    assert_eq!(tail.iter().map(|e| e.seq.0).collect::<Vec<_>>(), vec![4, 5]);
    let page = log.read(&w, Cursor::START, 2).unwrap();
    assert_eq!(page.len(), 2);

    // Clocks only move forward.
    let err = log.append(&w, ev(10, "t.k", 9)).unwrap_err();
    assert!(matches!(err, LogError::OutOfOrder { .. }), "{err}");

    // Another world is another sequence.
    let w2 = fresh_world();
    assert_eq!(log.append(&w2, ev(1, "t.k", 1)).unwrap().seq, Seq(1));
    assert_eq!(log.head(&w).unwrap(), Some(Seq(5)));

    // A second connection sees the same log: it is the database, not the process.
    let log2 = PostgresLog::open(&url).unwrap();
    assert_eq!(log2.head(&w).unwrap(), Some(Seq(5)));
}

#[test]
fn a_file_log_copies_in_once() {
    let Some(url) = url() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut file = FileLog::open(dir.path()).unwrap();
    let w = fresh_world();
    for i in 1..=3 {
        file.append(&w, ev(i as i64 * 7, "store.x", i)).unwrap();
    }
    let mut pg = PostgresLog::open(&url)
        .unwrap_or_else(|e| panic!("cannot open the test database at {url}: {e}"));
    assert_eq!(copy_world(&file, &mut pg, &w).unwrap(), 3);
    assert_eq!(
        pg.read_all(&w, Cursor::START).unwrap(),
        file.read_all(&w, Cursor::START).unwrap()
    );
    let err = copy_world(&file, &mut pg, &w).unwrap_err();
    assert!(err.to_string().contains("already has events"), "{err}");
}
