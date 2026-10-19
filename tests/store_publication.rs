//! Failed event validation must preserve both indexed evidence and its JSON backup.
use buildscope::{
    model::TraceEvent,
    store::{EventQuery, Store},
};
use std::fs;

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
