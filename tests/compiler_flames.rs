use buildscope::{
    compiler_stacks::{Stack, project},
    model::TraceEvent,
    store::{FlamegraphQuery, Store},
};
use rusqlite::{Connection, params};
use serde_json::Value;
fn event(id: usize, start: f64, duration: f64, name: &str) -> TraceEvent {
    TraceEvent {
        id,
        name: name.into(),
        detail: format!("{name}<int>"),
        thread: "1".into(),
        start_us: start,
        duration_us: duration,
        file: Some("a.hpp".into()),
        line: Some(12),
        parent: None,
        depth: 0,
    }
}
fn total(stacks: &[Stack]) -> f64 {
    stacks.iter().map(|s| s.value_us).sum()
}
#[test]
fn temporal_attribution_is_additive_and_preserves_native_identity() {
    let mut events = vec![
        event(0, 0., 100., "Outer"),
        event(1, 10., 20., "Inner"),
        event(2, 20., 30., "Crossing"),
    ];
    let mut other = event(3, 0., 40., "Other lane");
    other.thread = "2".into();
    events.push(other);
    let stacks = project(&events);
    assert_eq!(total(&stacks), 140.);
    assert_eq!(
        stacks
            .iter()
            .find(|s| s.frame.name == "[ambiguous overlap]")
            .unwrap()
            .value_us,
        10.
    );
    assert_eq!(
        stacks
            .iter()
            .find(|s| s.frame.name == "Outer")
            .unwrap()
            .value_us,
        60.
    );
    assert!(
        stacks
            .iter()
            .any(|s| s.frame.file.as_deref() == Some("a.hpp") && s.frame.line == Some(12))
    );
    assert!(project(&[]).is_empty());
    assert_eq!(
        total(&project(&[
            event(0, 0., 100., "Outer"),
            event(1, 10., 20., "Inner")
        ])),
        100.
    );
}
fn fixture() -> (tempfile::TempDir, Store, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let db = Connection::open(dir.path().join("index.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO runs VALUES('run',1,'success','profile','config','[]',1,'{}')",
        [],
    )
    .unwrap();
    for (unit, compiler, target, family) in [
        ("a", 0, "app", "clang"),
        ("b", 1, "lib", "clang"),
        ("gcc", 2, "lib", "gcc"),
    ] {
        db.execute(
            "INSERT INTO report_units VALUES('run',?1,'{}',?1,'object',?2,?3)",
            params![unit, compiler, target],
        )
        .unwrap();
        db.execute(
            "INSERT INTO profiles VALUES('run',?1,'{}',?2,0,NULL)",
            params![unit, family],
        )
        .unwrap();
    }
    (dir, store, db)
}
fn mass(node: &Value) {
    let value = node["value_us"].as_f64().unwrap();
    let children = node["children"].as_array().unwrap();
    if !children.is_empty() {
        let own = node["self_us"].as_f64().unwrap_or(0.);
        let sum: f64 = children
            .iter()
            .map(|n| n["value_us"].as_f64().unwrap())
            .sum();
        assert!((value - own - sum).abs() < 0.001, "{node}");
    }
    for child in children {
        mass(child);
    }
}
fn count(node: &Value) -> usize {
    1 + node["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(count)
        .sum::<usize>()
}
#[test]
fn filters_replacement_missing_indexes_and_bounded_refinement() {
    let (_dir, store, db) = fixture();
    let events: Vec<_> = (0..100)
        .map(|i| event(i, i as f64, 1., &format!("Activity {i}")))
        .collect();
    store.save_events("run", "a", &events).unwrap();
    store
        .save_events("run", "b", &[event(0, 0., 50., "Second")])
        .unwrap();
    for direction in ["top-down", "bottom-up"] {
        for depth in [1, 2, 12] {
            for offset in [0, 1, 20] {
                let result = store
                    .compiler_flamegraph(
                        "latest",
                        FlamegraphQuery {
                            direction,
                            depth,
                            offset,
                            limit: 10,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                assert_eq!(result["status"], "ready");
                assert_eq!(result["total_us"], 150.);
                assert!(count(&result["tree"]) <= 10);
                mass(&result["tree"]);
            }
        }
    }
    assert_eq!(
        store
            .compiler_flamegraph(
                "run",
                FlamegraphQuery {
                    target: "app",
                    ..Default::default()
                }
            )
            .unwrap()["total_us"],
        100.
    );
    assert_eq!(
        store
            .compiler_flamegraph(
                "run",
                FlamegraphQuery {
                    compiler: "1",
                    ..Default::default()
                }
            )
            .unwrap()["total_us"],
        50.
    );
    store.save_events("run", "a", &[]).unwrap();
    assert_eq!(
        store
            .compiler_flamegraph("run", FlamegraphQuery::default())
            .unwrap()["total_us"],
        50.
    );
    db.execute("DELETE FROM compiler_stack_units WHERE unit='b'", [])
        .unwrap();
    assert_eq!(
        store
            .compiler_flamegraph("run", FlamegraphQuery::default())
            .unwrap()["status"],
        "missing-index"
    );
    db.execute("DELETE FROM events WHERE unit='b'", []).unwrap(); // recover compressed archive
    assert_eq!(store.reindex_compiler_stacks("latest").unwrap(), 3);
    assert_eq!(
        store
            .compiler_flamegraph("run", FlamegraphQuery::default())
            .unwrap()["total_us"],
        50.
    );
    assert!(
        store
            .compiler_flamegraph("unknown", FlamegraphQuery::default())
            .is_err()
    );
}
#[test]
fn source_line_variants_and_compiler_identity_do_not_merge() {
    let (_dir, store, _db) = fixture();
    let mut first = event(0, 0., 10., "InstantiateFunction");
    let mut second = event(1, 10., 10., "InstantiateFunction");
    second.line = Some(13);
    store
        .save_events("run", "a", &[first.clone(), second])
        .unwrap();
    first.id = 2;
    store.save_events("run", "b", &[first]).unwrap();
    let result = store
        .compiler_flamegraph("run", FlamegraphQuery::default())
        .unwrap();
    assert_eq!(result["tree"]["children"].as_array().unwrap().len(), 2);
    let compiler = &result["tree"]["children"][0];
    assert_eq!(compiler["children"].as_array().unwrap().len(), 2);
    let focus = compiler["focus"].as_str().unwrap();
    let focused = store
        .compiler_flamegraph(
            "run",
            FlamegraphQuery {
                focus,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(focused["tree"]["value_us"], 20.);
}
#[test]
#[ignore = "Set BUILDSCOPE_FLAME_BENCH_TRACE to a read-only compressed native event archive"]
fn real_trace_projection_benchmark() {
    let path = std::env::var("BUILDSCOPE_FLAME_BENCH_TRACE").unwrap();
    let begin = std::time::Instant::now();
    let events: Vec<TraceEvent> = serde_json::from_reader(std::io::BufReader::new(
        zstd::stream::read::Decoder::new(std::fs::File::open(path).unwrap()).unwrap(),
    ))
    .unwrap();
    let loaded = begin.elapsed();
    let result = project(&events);
    let elapsed = begin.elapsed() - loaded;
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    let peak = unsafe {
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) == 0 {
            usage.assume_init().ru_maxrss as u64
        } else {
            0
        }
    };
    let peak_bytes = if cfg!(target_os = "macos") {
        peak
    } else {
        peak * 1024
    };
    eprintln!("peak_process_rss_bytes={peak_bytes}");
    eprintln!(
        "events={} trie_nodes={} load={loaded:?} project={elapsed:?} attributed_us={}",
        events.len(),
        result.len(),
        total(&result)
    );
}

#[test]
fn coverage_exposes_inventory_missing_profiles_and_native_thresholds() {
    let (_dir, store, db) = fixture();
    store
        .save_events("run", "a", &[event(0, 0., 10., "Observed")])
        .unwrap();
    db.execute("UPDATE profiles SET granularity_us=500 WHERE unit='a'", [])
        .unwrap();
    db.execute("UPDATE profiles SET granularity_us=0 WHERE unit='b'", [])
        .unwrap();
    db.execute(
        "INSERT INTO report_units VALUES('run','unprofiled','{}','missing.cpp','object',0,'app')",
        [],
    )
    .unwrap();
    let graph = store
        .compiler_flamegraph("run", FlamegraphQuery::default())
        .unwrap();
    assert_eq!(graph["inventory_units"], 4);
    assert_eq!(graph["trace_units"], 2);
    assert_eq!(graph["indexed_units"], 1);
    assert_eq!(graph["unprofiled_units"], 1);
    assert_eq!(graph["unsupported_units"], 1);
    assert_eq!(graph["granularities_us"], serde_json::json!([0, 500]));
    assert_eq!(graph["coverage_status"], "partial");
    assert_eq!(graph["status"], "missing-index");
    assert_eq!(graph["total_us"], 10.);
    let app = store
        .compiler_flamegraph(
            "run",
            FlamegraphQuery {
                target: "app",
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(app["inventory_units"], 2);
    assert_eq!(app["trace_units"], 1);
    assert_eq!(app["status"], "ready");
    assert_eq!(app["coverage_status"], "partial");
    assert_eq!(app["total_us"], 10.);
    let unsupported = store
        .compiler_flamegraph(
            "run",
            FlamegraphQuery {
                unit: "gcc",
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(unsupported["status"], "unavailable");
    assert_eq!(unsupported["unsupported_units"], 1);
    assert_eq!(unsupported["trace_units"], 0);
    let absent = store
        .compiler_flamegraph(
            "run",
            FlamegraphQuery {
                unit: "unprofiled",
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(absent["status"], "unavailable");
    assert_eq!(absent["unprofiled_units"], 1);
    assert_eq!(absent["inventory_units"], 1);
}
