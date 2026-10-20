//! Existing-database upgrades preserve original JSON and public query semantics.
use buildscope::store::{CompilerWorkQuery, Store};
use rusqlite::{Connection, params};
use serde_json::json;
use std::{
    path::Path,
    sync::{Arc, Barrier},
};

#[test]
fn migration_progress_uses_stderr_and_current_stores_stay_quiet() {
    let temp = tempfile::tempdir().unwrap();
    let database = legacy_database(temp.path());
    // This fixture's intentionally minimal manifest is not needed by list().
    database.execute("DELETE FROM runs", []).unwrap();
    let before = json_snapshot(&database);
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_buildscope"))
            .arg("--store")
            .arg(temp.path())
            .args(["list", "--json"])
            .output()
            .unwrap()
    };
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        json!([])
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("upgrading the entire database"), "{stderr}");
    assert!(stderr.contains("Backfilling event locations"), "{stderr}");
    assert!(stderr.contains("rows processed"), "{stderr}");
    let flush = stderr.find("Flushing database journal").unwrap();
    let complete = stderr.find("complete in").unwrap();
    assert!(flush < complete, "{stderr}");
    assert_eq!(json_snapshot(&database), before);
    let output = run();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let fresh = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_buildscope"))
        .arg("--store")
        .arg(fresh.path())
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
}

#[test]
fn legacy_rows_become_typed_queries_without_changing_json() {
    for version in [0, 1] {
        let temp = tempfile::tempdir().unwrap();
        let database = legacy_database(temp.path());
        database
            .pragma_update(None, "user_version", version)
            .unwrap();
        let before = json_snapshot(&database);
        let store = Store::open(temp.path()).unwrap();
        assert_eq!(json_snapshot(&database), before);
        assert_eq!(database_version(&database), 3);
        let target: String = database
            .query_row(
                "SELECT target FROM report_units WHERE unit='clang'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(target, "app");
        let phases: Vec<(i64, f64, String, String)> = database
            .prepare("SELECT ordinal,value,origin,coverage FROM profile_phases ORDER BY ordinal")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            phases,
            vec![
                (0, 4.0, "measured".into(), "complete".into()),
                (1, 6.0, "measured".into(), "complete".into())
            ]
        );
        let query = store
            .compiler_work("run", CompilerWorkQuery::default())
            .unwrap();
        assert_eq!(query["total"], 2);
        assert_eq!(query["summary"]["work_ms"], 3.0);
        assert_eq!(query["rows"][0]["file"], "header.hpp");
        assert_eq!(query["rows"][0]["line"], 17);
        assert!(query["rows"][1]["file"].is_null());
        assert!(query["rows"][1]["line"].is_null());
        assert_eq!(
            query["coverage"],
            json!([
                {"compiler":0,"units":3,"profile_units":2,"trace_units":2,"granularities_us":"0"},
                {"compiler":1,"units":1,"profile_units":1,"trace_units":0,"granularities_us":null}
            ])
        );
        let app = store
            .compiler_work(
                "run",
                CompilerWorkQuery {
                    target: "app",
                    text: "header.hpp",
                    ..CompilerWorkQuery::default()
                },
            )
            .unwrap();
        assert_eq!(app["total"], 1);
        assert_eq!(app["summary"]["work_ms"], 2.0);
        let key = app["rows"][0]["key"].as_str().unwrap();
        let events = store
            .compiler_work(
                "run",
                CompilerWorkQuery {
                    entity: key,
                    breakdown: "events",
                    ..CompilerWorkQuery::default()
                },
            )
            .unwrap();
        assert_eq!(events["rows"][0]["event"]["unknown"], "retained");
        let timers = store
            .compiler_work(
                "run",
                CompilerWorkQuery {
                    category: "evaluations",
                    target: "(other)",
                    ..CompilerWorkQuery::default()
                },
            )
            .unwrap();
        assert_eq!(
            timers["timers"],
            json!([
                {"compiler":1,"name":"constant expression evaluation","work_ms":10.0,"units":1}
            ])
        );
        let reopened = Store::open(temp.path()).unwrap();
        assert_eq!(
            reopened
                .compiler_work("run", CompilerWorkQuery::default())
                .unwrap(),
            query
        );
        assert_eq!(json_snapshot(&database), before);
    }
}

#[test]
fn failed_backfills_roll_back_schema_version_and_all_original_rows() {
    for corruption in ["unit", "profile", "event", "overflow"] {
        let temp = tempfile::tempdir().unwrap();
        let database = legacy_database(temp.path());
        match corruption {
            "unit" => {
                database
                    .execute("UPDATE report_units SET data='{}' WHERE unit='clang'", [])
                    .unwrap();
            }
            "profile" => {
                database
                    .execute("UPDATE profiles SET data='{}' WHERE unit='gcc'", [])
                    .unwrap();
            }
            _ => {
                // Fail beyond the first backfill batch, after valid rows were updated.
                for idx in 2..300 {
                    database.execute(
                        "INSERT INTO events VALUES('run','clang',?1,'InstantiateFunction','X',0,1,?2)",
                        params![idx, r#"{"file":"valid.hpp","line":1}"#],
                    ).unwrap();
                }
                let data = if corruption == "event" {
                    r#"{"line":"invalid"}"#
                } else {
                    r#"{"line":18446744073709551615}"#
                };
                database
                    .execute(
                        "UPDATE events SET data=?1 WHERE unit='clang' AND idx=299",
                        [data],
                    )
                    .unwrap();
            }
        }
        let before = json_snapshot(&database);
        let schema = schema_snapshot(&database);
        let error = Store::open(temp.path()).err().unwrap().to_string();
        assert!(error.contains("run/"), "{error}");
        assert_eq!(database_version(&database), 1);
        assert_eq!(json_snapshot(&database), before);
        assert_eq!(schema_snapshot(&database), schema);
        // A repaired database can retry the same migration.
        if corruption == "unit" {
            database
                .execute(
                    "UPDATE report_units SET data=?1 WHERE unit='clang'",
                    [CLANG_UNIT],
                )
                .unwrap();
        } else if corruption == "profile" {
            database
                .execute(
                    "UPDATE profiles SET data=?1 WHERE unit='gcc'",
                    [GCC_PROFILE],
                )
                .unwrap();
        } else {
            database
                .execute(
                    "UPDATE events SET data='{}' WHERE unit='clang' AND idx=299",
                    [],
                )
                .unwrap();
        }
        Store::open(temp.path()).unwrap();
        assert_eq!(database_version(&database), 3);
    }
}

#[test]
fn fresh_newer_and_unrecognized_databases_have_explicit_version_handling() {
    let temp = tempfile::tempdir().unwrap();
    Store::open(temp.path()).unwrap();
    let database = Connection::open(temp.path().join("index.sqlite3")).unwrap();
    assert_eq!(database_version(&database), 3);
    let before = schema_snapshot(&database);
    database.pragma_update(None, "user_version", 4).unwrap();
    assert!(
        Store::open(temp.path())
            .err()
            .unwrap()
            .to_string()
            .contains("newer Buildscope")
    );
    assert_eq!(database_version(&database), 4);
    assert_eq!(schema_snapshot(&database), before);
    let other = tempfile::tempdir().unwrap();
    let database = Connection::open(other.path().join("index.sqlite3")).unwrap();
    database
        .execute_batch("CREATE TABLE unrelated(data TEXT);")
        .unwrap();
    let before = schema_snapshot(&database);
    assert!(Store::open(other.path()).is_err());
    assert_eq!(database_version(&database), 0);
    assert_eq!(schema_snapshot(&database), before);
}

#[test]
fn openers_wait_beyond_the_normal_timeout_for_a_migration_writer() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        sync::mpsc,
        time::Duration,
    };

    let temp = tempfile::tempdir().unwrap();
    let database = legacy_database(temp.path());
    database.execute("DELETE FROM runs", []).unwrap();
    let original_json = json_snapshot(&database);
    // Hold the same lock used by a migrator for longer than the normal 10-second
    // connection timeout. WAL still permits the opener to read the legacy version.
    database
        .execute_batch("PRAGMA journal_mode=WAL; BEGIN IMMEDIATE;")
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_buildscope"))
        .arg("--store")
        .arg(temp.path())
        .args(["list", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (started, ready) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut messages = String::new();
        for line in BufReader::new(stderr).lines() {
            let line = line.unwrap();
            if line.contains("waiting for database write access") {
                let _ = started.send(());
            }
            messages.push_str(&line);
            messages.push('\n');
        }
        messages
    });
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_secs(11));
    let premature_exit = child.try_wait().unwrap();
    database.execute_batch("ROLLBACK").unwrap();
    let output = child.wait_with_output().unwrap();
    let messages = reader.join().unwrap();
    assert!(
        premature_exit.is_none(),
        "opener exited while migration lock was held: {messages}"
    );
    assert!(output.status.success(), "{messages}");
    assert!(
        messages.contains("elapsed"),
        "missing wait heartbeat: {messages}"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        json!([])
    );
    assert_eq!(database_version(&database), 3);
    assert_eq!(json_snapshot(&database), original_json);
}

#[test]
fn concurrent_openers_apply_the_migration_only_once() {
    let temp = tempfile::tempdir().unwrap();
    let database = legacy_database(temp.path());
    let before = json_snapshot(&database);
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let root = temp.path().to_owned();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                Store::open(&root)
                    .unwrap()
                    .compiler_work("run", CompilerWorkQuery::default())
                    .unwrap()
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results[0], results[1]);
    assert_eq!(database_version(&database), 3);
    assert_eq!(json_snapshot(&database), before);
}

fn legacy_database(root: &Path) -> Connection {
    let database = Connection::open(root.join("index.sqlite3")).unwrap();
    database
        .execute_batch(include_str!("fixtures/store-v1.sql"))
        .unwrap();
    for (unit, data) in [
        ("clang", CLANG_UNIT),
        (
            "no-location",
            r#"{"source_key":"b.cpp","output_key":"CMakeFiles/app.dir/b.o","compiler":0}"#,
        ),
        (
            "missing",
            r#"{"source_key":"missing.cpp","output_key":"CMakeFiles/nested/tool.dir/m.o","compiler":0}"#,
        ),
        (
            "gcc",
            r#"{"source_key":"gcc.cpp","output_key":"gcc.o","compiler":1}"#,
        ),
    ] {
        database
            .execute(
                "INSERT INTO report_units VALUES('run',?1,?2)",
                params![unit, data],
            )
            .unwrap();
    }
    for (unit, data) in [
        (
            "clang",
            "{\n  \"family\":\"clang\", \"event_count\":1, \"granularity_us\":0, \"phases\":[], \"unknown\": [1,2]\n}",
        ),
        (
            "no-location",
            r#"{"family":"clang","event_count":1,"phases":[]}"#,
        ),
        ("gcc", GCC_PROFILE),
    ] {
        database
            .execute(
                "INSERT INTO profiles VALUES('run',?1,?2)",
                params![unit, data],
            )
            .unwrap();
    }
    for (unit, duration, data) in [
        (
            "clang",
            2000.0,
            "{ \"file\":\"header.hpp\", \"line\":17, \"unknown\":\"retained\" }",
        ),
        ("no-location", 1000.0, "{}"),
    ] {
        database
            .execute(
                "INSERT INTO events VALUES('run',?1,0,'InstantiateFunction','Entity<int>',0,?2,?3)",
                params![unit, duration, data],
            )
            .unwrap();
    }
    database.execute("INSERT INTO runs VALUES('run',1,'success','profile','config','[]',1,' {\"original\":true} ')", []).unwrap();
    database
        .execute(
            "INSERT INTO report_runs VALUES('run',' {\"overview\":true} ')",
            [],
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO report_headers VALUES('run','header.hpp',' {\"detail\":true} ')",
            [],
        )
        .unwrap();
    database
}

fn database_version(database: &Connection) -> u32 {
    database
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn json_snapshot(database: &Connection) -> Vec<(String, Vec<String>)> {
    [
        ("report_units", "data", "run,unit"),
        ("profiles", "data", "run,unit"),
        ("events", "data", "run,unit,idx"),
        ("runs", "manifest", "id"),
        ("report_runs", "data", "id"),
        ("report_headers", "data", "run,path"),
    ]
    .into_iter()
    .map(|(table, column, order)| {
        let values = database
            .prepare(&format!("SELECT {column} FROM {table} ORDER BY {order}"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        (table.into(), values)
    })
    .collect()
}

fn schema_snapshot(database: &Connection) -> Vec<(String, Option<String>)> {
    database
        .prepare("SELECT name,sql FROM sqlite_schema ORDER BY name")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

const CLANG_UNIT: &str = "{\n \"source_key\":\"a.cpp\", \"output_key\":\"build/CMakeFiles/app.dir/a.o\", \"compiler\":0, \"unknown\":true\n}";
const GCC_PROFILE: &str = r#"{
    "family":"gcc", "event_count":0, "granularity_us":null,
    "phases":[
        {"name":"constant expression evaluation","value":4.0,"unit":"ms","origin":"measured","coverage":"complete"},
        {"name":"constant expression evaluation","value":6.0,"unit":"ms","origin":"measured","coverage":"complete"}
    ]
}"#;
