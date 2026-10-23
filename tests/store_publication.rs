//! Failed event validation must preserve both indexed evidence and its JSON backup.
use buildscope::{
    compiler_stacks::project,
    model::TraceEvent,
    store::{EventQuery, Store},
};
use std::fs;

#[test]
fn prepared_rows_preserve_multiline_unicode_events_and_exact_stack_projection() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let mut outer = event(0, "outer\nGrüße 🦀\r\n\"quoted\"\\path\0", Some(1));
    outer.name = "ExecuteCompiler\n日本語".into();
    outer.file = Some("include/straße\n名前.hpp".into());
    outer.duration_us = 30.0;
    let mut inner = event(1, "inner\nλ<T>\u{2028}continued", Some(2));
    inner.start_us = 3.0;
    inner.duration_us = 15.0;
    let mut crossing = event(2, "crossing\r\n雪", None);
    crossing.start_us = 10.0;
    crossing.duration_us = 15.0;
    let mut other_lane = event(3, "other lane\n🙂", Some(3));
    other_lane.thread = "worker\n二".into();
    other_lane.start_us = 5.0;
    other_lane.duration_us = 20.0;
    let events = vec![outer, inner, crossing, other_lane];
    let expected_events = serde_json::to_value(&events).unwrap();
    let expected_stacks = serde_json::to_value(project(&events)).unwrap();

    store.save_events("run", "unit", &events).unwrap();

    let directory = store.run_dir("run").unwrap().join("events");
    let decoder =
        zstd::stream::read::Decoder::new(fs::File::open(directory.join("unit.json.zst")).unwrap())
            .unwrap();
    let archived: serde_json::Value = serde_json::from_reader(decoder).unwrap();
    assert_eq!(archived, expected_events);

    let connection = rusqlite::Connection::open(store.root.join("index.sqlite3")).unwrap();
    let mut select = connection
        .prepare(
            "SELECT name,detail,file,data FROM events WHERE run='run' AND unit='unit' ORDER BY idx",
        )
        .unwrap();
    let saved: Vec<_> = select
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(saved.len(), events.len());
    for ((name, detail, file, data), expected) in saved.iter().zip(&events) {
        assert_eq!(name, &expected.name);
        assert_eq!(detail, &expected.detail);
        assert_eq!(file, &expected.file);
        assert_eq!(data, &serde_json::to_string(expected).unwrap());
    }
    let mut select = connection
        .prepare(
            "SELECT data FROM compiler_stacks WHERE run='run' AND unit='unit' ORDER BY ordinal",
        )
        .unwrap();
    let stacks: Vec<serde_json::Value> = select
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|row| serde_json::from_str(&row.unwrap()).unwrap())
        .collect();
    assert_eq!(serde_json::to_value(stacks).unwrap(), expected_stacks);
    assert_eq!(fs::read_dir(directory).unwrap().count(), 1);
}

#[test]
fn invalid_event_lines_preserve_the_existing_archive_and_database() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let original = event(0, "original", Some(1));
    store.save_events("run", "unit", &[original]).unwrap();
    let path = store.run_dir("run").unwrap().join("events/unit.json.zst");
    let original_bytes = fs::read(&path).unwrap();
    let original_page = store.events("run", "unit", EventQuery::default()).unwrap();

    // Validate every event, including invalid locations after valid replacements.
    let error = store
        .save_events(
            "run",
            "unit",
            &[
                event(0, "replacement", None),
                event(1, "invalid", Some(u64::MAX)),
            ],
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("event line exceeds SQLite integer range")
    );
    assert_eq!(fs::read(&path).unwrap(), original_bytes);
    assert_eq!(
        store.events("run", "unit", EventQuery::default()).unwrap(),
        original_page
    );
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);

    // Missing locations and the highest representable line still publish normally.
    let replacement = vec![
        event(0, "missing", None),
        event(1, "maximum", Some(i64::MAX as u64)),
    ];
    store.save_events("run", "unit", &replacement).unwrap();
    let decoder = zstd::stream::read::Decoder::new(fs::File::open(path).unwrap()).unwrap();
    let archived: serde_json::Value = serde_json::from_reader(decoder).unwrap();
    assert_eq!(archived, serde_json::to_value(&replacement).unwrap());
    let page = store.events("run", "unit", EventQuery::default()).unwrap();
    assert_eq!(page["total"], 2);
    assert!(page["events"][0]["line"].is_null());
    assert_eq!(page["events"][1]["line"], i64::MAX);
}

fn event(id: usize, detail: &str, line: Option<u64>) -> TraceEvent {
    TraceEvent {
        id,
        name: "InstantiateFunction".into(),
        detail: detail.into(),
        thread: "1".into(),
        start_us: id as f64,
        duration_us: 1.0,
        file: None,
        line,
        parent: None,
        depth: 0,
    }
}
