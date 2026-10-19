//! Interface-level decoding and evidence contracts. These synthetic fixtures exercise
//! format variants and attribution boundaries independently of local compiler versions.
use buildscope::{
    analysis, capture, compiler,
    model::*,
    project,
    store::{CompilerWorkQuery, EventQuery, Store},
    util::*,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

#[test]
fn template_families_preserve_spellings_work_and_every_sorted_page() {
    let pairs: Vec<(String, String)> =
        serde_json::from_str(include_str!("fixtures/template-families.json")).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut capture = capture_fixture(temp.path());
    capture.project.units = vec![
        unit(temp.path(), "a.cpp", "build/CMakeFiles/app.dir/a.o"),
        unit(temp.path(), "b.cpp", "build/CMakeFiles/app.dir/b.o"),
        unit(temp.path(), "c.cpp", "build/CMakeFiles/tests.dir/c.o"),
    ];
    let store = Store::open(&temp.path().join("store")).unwrap();
    let mut expected = BTreeMap::<String, (f64, usize, f64, BTreeSet<String>)>::new();
    for (unit_index, unit) in capture.project.units.iter().enumerate() {
        let events: Vec<TraceEvent> = pairs
            .iter()
            .enumerate()
            .take(if unit_index == 2 { 8 } else { pairs.len() })
            .map(|(index, (spelling, family))| {
                let work_ms = ((index + 1) * (unit_index + 1)) as f64;
                let entry = expected.entry(family.clone()).or_default();
                entry.0 += work_ms;
                entry.1 += 1;
                entry.2 = entry.2.max(work_ms);
                entry.3.insert(unit.id.clone());
                TraceEvent {
                    id: index,
                    name: "InstantiateFunction".into(),
                    detail: spelling.clone(),
                    thread: "1".into(),
                    start_us: index as f64 * 1000.0,
                    duration_us: work_ms * 1000.0,
                    file: Some(format!("/project/header_{}.hpp", index % 2)),
                    line: Some(index as u64 + 1),
                    parent: None,
                    depth: 0,
                }
            })
            .collect();
        store.save_events(&capture.id, &unit.id, &events).unwrap();
        capture.profiles.push(CompilationProfile {
            unit_id: unit.id.clone(),
            family: "clang".into(),
            event_count: events.len(),
            granularity_us: Some(0),
            ..CompilationProfile::default()
        });
    }
    store.save(&capture).unwrap();
    let query = || CompilerWorkQuery {
        families: true,
        limit: 3,
        ..CompilerWorkQuery::default()
    };
    for order in ["work", "units", "count", "longest", "name"] {
        for descending in [true, false] {
            let mut rows = Vec::new();
            loop {
                let page = store
                    .compiler_work(
                        &capture.id,
                        CompilerWorkQuery {
                            order,
                            descending,
                            offset: rows.len(),
                            ..query()
                        },
                    )
                    .unwrap();
                assert_eq!(page["total"], expected.len());
                assert_eq!(
                    page["summary"]["work_ms"],
                    expected.values().map(|stats| stats.0).sum::<f64>()
                );
                rows.extend(page["rows"].as_array().unwrap().iter().cloned());
                if rows.len() == expected.len() {
                    break;
                }
                assert!(!page["rows"].as_array().unwrap().is_empty());
            }
            let names: BTreeSet<_> = rows
                .iter()
                .map(|row| row["name"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(
                names,
                expected.keys().cloned().collect(),
                "{order}, descending={descending}"
            );
            assert_eq!(rows.len(), names.len());
            let field = match order {
                "work" => "work_ms",
                "longest" => "max_ms",
                other => other,
            };
            for adjacent in rows.windows(2) {
                let order = if field == "name" {
                    adjacent[0][field]
                        .as_str()
                        .unwrap()
                        .cmp(adjacent[1][field].as_str().unwrap())
                } else {
                    adjacent[0][field]
                        .as_f64()
                        .unwrap()
                        .partial_cmp(&adjacent[1][field].as_f64().unwrap())
                        .unwrap()
                };
                assert!(if descending {
                    !order.is_lt()
                } else {
                    !order.is_gt()
                });
            }
            for row in rows {
                let stats = &expected[row["name"].as_str().unwrap()];
                assert_eq!(row["work_ms"], stats.0);
                assert_eq!(row["count"], stats.1);
                assert_eq!(row["max_ms"], stats.2);
                assert_eq!(row["units"], stats.3.len());
            }
        }
    }
    let families = store
        .compiler_work(
            &capture.id,
            CompilerWorkQuery {
                limit: 200,
                ..query()
            },
        )
        .unwrap();
    for family in families["rows"].as_array().unwrap() {
        let key = family["key"].as_str().unwrap();
        for breakdown in ["members", "units"] {
            let detail = store
                .compiler_work(
                    &capture.id,
                    CompilerWorkQuery {
                        entity: key,
                        breakdown,
                        limit: 200,
                        ..query()
                    },
                )
                .unwrap();
            let members = detail["rows"].as_array().unwrap();
            assert_eq!(
                members
                    .iter()
                    .map(|row| row["work_ms"].as_f64().unwrap())
                    .sum::<f64>(),
                family["work_ms"]
            );
            assert_eq!(
                members
                    .iter()
                    .map(|row| row["count"].as_u64().unwrap())
                    .sum::<u64>(),
                family["count"]
            );
            if breakdown == "units" {
                assert_eq!(members.len(), family["units"]);
            }
        }
    }
}

#[test]
fn global_compiler_work_sums_units_and_preserves_evidence_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let store = Store::open(&root.join("store")).unwrap();
    let mut capture = capture_fixture(root);
    capture.kind = "profile".into();
    capture.project.units = vec![
        unit(root, "first.cpp", "build/CMakeFiles/app.dir/first.o"),
        unit(root, "first.cpp", "build/CMakeFiles/tests.dir/first.o"),
        unit(root, "second.cpp", "build/CMakeFiles/app.dir/second.o"),
        unit(root, "alternative.cpp", "build/CMakeFiles/app.dir/alt.o"),
        unit(root, "gcc.cpp", "build/CMakeFiles/app.dir/gcc.o"),
        unit(root, "missing.cpp", "build/CMakeFiles/app.dir/missing.o"),
    ];
    let clang = capture.project.compilers[0].clone();
    capture.project.compilers.push(clang.clone());
    capture.project.compilers.push(CompilerInfo {
        family: "gcc".into(),
        version: "test gcc".into(),
        ..clang
    });
    capture.project.units[3].compiler = 1;
    capture.project.units[4].compiler = 2;
    capture.profiles = capture.project.units[..5]
        .iter()
        .enumerate()
        .map(|(index, unit)| CompilationProfile {
            unit_id: unit.id.clone(),
            family: if index == 4 { "gcc" } else { "clang" }.into(),
            event_count: if index == 4 { 0 } else { 1 },
            granularity_us: (index != 4).then_some(500),
            phases: if index == 4 {
                vec![Metric {
                    name: "constant expression evaluation".into(),
                    value: 140.0,
                    unit: "ms".into(),
                    origin: "measured".into(),
                    coverage: "complete".into(),
                }]
            } else {
                vec![]
            },
            ..CompilationProfile::default()
        })
        .collect();
    let event = |name: &str, detail: &str, duration_us: f64, line: Option<u64>| TraceEvent {
        id: 0,
        name: name.into(),
        detail: detail.into(),
        thread: "1".into(),
        start_us: 0.0,
        duration_us,
        file: line.map(|_| "/project/header.hpp".into()),
        line,
        parent: None,
        depth: 0,
    };
    let mut events = vec![
        event("InstantiateFunction", "Entity<int>", 10_000.0, Some(7)),
        event("InstantiateFunction", "Entity<int>", 20_000.0, Some(7)),
        event("InstantiateFunction", "Entity<float>", 5_000.0, Some(7)),
        event("InstantiateFunction", "Entity<int>", 2_000.0, Some(9)),
        event("EvaluateAsRValue", "", 3_000.0, None),
        event("OptFunction", "render", 4_000.0, Some(12)),
        event("CodeGen Function", "render", 1_000.0, Some(12)),
        event("isIntegerConstantExpr", "", 1_000.0, None),
    ];
    for (index, event) in events.iter_mut().enumerate() {
        event.id = index;
        event.start_us = index as f64 * 1000.0;
    }
    let first = &capture.project.units[0].id;
    store.save_events(&capture.id, first, &events).unwrap();
    for (index, duration) in [(1, 40_000.0), (2, 50_000.0), (3, 70_000.0)] {
        store
            .save_events(
                &capture.id,
                &capture.project.units[index].id,
                &[event(
                    "InstantiateFunction",
                    "Entity<int>",
                    duration,
                    Some(7),
                )],
            )
            .unwrap();
    }
    store.save(&capture).unwrap();
    let query = || CompilerWorkQuery {
        compiler: Some(0),
        ..CompilerWorkQuery::default()
    };
    let global = store.compiler_work(&capture.id, query()).unwrap();
    assert_eq!(global["total"], 3);
    assert_eq!(global["summary"]["work_ms"], 127.0);
    let top = &global["rows"][0];
    assert_eq!(top["work_ms"], 120.0);
    assert_eq!(top["units"], 3); // first.cpp compiled into app and tests counts twice.
    assert_eq!(top["count"], 4);
    assert_eq!(top["max_ms"], 50.0);
    assert_eq!(top["line"], 7);
    let key = top["key"].as_str().unwrap();
    let contributions = store
        .compiler_work(
            &capture.id,
            CompilerWorkQuery {
                entity: key,
                limit: 1,
                offset: 1,
                ..query()
            },
        )
        .unwrap();
    assert_eq!(contributions["total"], 3);
    assert_eq!(contributions["summary"]["work_ms"], 120.0);
    assert_eq!(contributions["rows"].as_array().unwrap().len(), 1);
    assert_eq!(contributions["rows"][0]["work_ms"], 40.0);
    let scopes = store
        .compiler_work(
            &capture.id,
            CompilerWorkQuery {
                entity: key,
                breakdown: "events",
                unit: first,
                ..query()
            },
        )
        .unwrap();
    assert_eq!(scopes["total"], 2);
    assert_eq!(scopes["summary"]["work_ms"], 30.0);
    assert_eq!(scopes["rows"][0]["event"]["id"], 1);
    let app = store
        .compiler_work(
            &capture.id,
            CompilerWorkQuery {
                target: "app",
                ..query()
            },
        )
        .unwrap();
    assert_eq!(app["rows"][0]["work_ms"], 80.0);
    assert_eq!(app["summary"]["work_ms"], 87.0);
    assert_eq!(app["rows"][0]["units"], 2);
    assert_eq!(app["coverage"][0]["units"], 3);
    assert_eq!(app["coverage"][0]["trace_units"], 2);
    let families = store
        .compiler_work(
            &capture.id,
            CompilerWorkQuery {
                families: true,
                ..query()
            },
        )
        .unwrap();
    assert_eq!(families["total"], 1);
    assert_eq!(families["summary"], global["summary"]);
    assert_eq!(families["rows"][0]["work_ms"], 127.0);
    assert_eq!(families["rows"][0]["units"], 3); // Distinct units, not summed specialization reach.
    assert!(families["rows"][0]["file"].is_null());
    let members = store
        .compiler_work(
            &capture.id,
            CompilerWorkQuery {
                families: true,
                entity: families["rows"][0]["key"].as_str().unwrap(),
                breakdown: "members",
                ..query()
            },
        )
        .unwrap();
    assert_eq!(members["total"], 3);
    assert_eq!(members["summary"], families["summary"]);
    let compilers = store
        .compiler_work(&capture.id, CompilerWorkQuery::default())
        .unwrap();
    assert_eq!(compilers["total"], 4);
    assert_eq!(compilers["summary"]["work_ms"], 197.0);
    assert_eq!(compilers["rows"][1]["compiler"], 1);
    let functions = store
        .compiler_work(
            &capture.id,
            CompilerWorkQuery {
                category: "functions",
                ..query()
            },
        )
        .unwrap();
    assert_eq!(functions["total"], 2);
    assert_eq!(functions["summary"]["work_ms"], 5.0);
    assert_eq!(functions["rows"][0]["activity"], "OptFunction");
    assert_eq!(functions["rows"][0]["work_ms"], 4.0);
    let codegen = store
        .compiler_work(
            &capture.id,
            CompilerWorkQuery {
                category: "functions",
                activity: "CodeGen Function",
                ..query()
            },
        )
        .unwrap();
    assert_eq!(codegen["total"], 1);
    assert_eq!(codegen["summary"]["work_ms"], 1.0);
    assert_eq!(codegen["rows"][0]["work_ms"], 1.0);
    let evaluations = store
        .compiler_work(
            &capture.id,
            CompilerWorkQuery {
                category: "evaluations",
                ..query()
            },
        )
        .unwrap();
    assert_eq!(evaluations["total"], 2);
    assert_eq!(evaluations["summary"]["work_ms"], 4.0);
    assert_eq!(evaluations["rows"][0]["name"], "");
    assert!(evaluations["timers"].as_array().unwrap().is_empty());
    let gcc = store
        .compiler_work(
            &capture.id,
            CompilerWorkQuery {
                category: "evaluations",
                compiler: Some(2),
                ..CompilerWorkQuery::default()
            },
        )
        .unwrap();
    assert_eq!(gcc["total"], 0);
    assert_eq!(gcc["summary"]["work_ms"], 0.0);
    assert_eq!(gcc["timers"][0]["work_ms"], 140.0);
    for literal in ["%", "_", "' OR 1=1 --"] {
        assert_eq!(
            store
                .compiler_work(
                    &capture.id,
                    CompilerWorkQuery {
                        text: literal,
                        ..query()
                    }
                )
                .unwrap()["total"],
            0
        );
    }
    for (text, work_ms) in [
        ("Entity<int>", 122.0),
        ("header.hpp", 127.0),
        ("absent", 0.0),
    ] {
        let result = store
            .compiler_work(&capture.id, CompilerWorkQuery { text, ..query() })
            .unwrap();
        assert_eq!(result["summary"]["work_ms"], work_ms);
    }
    let beyond_last_page = store
        .compiler_work(
            &capture.id,
            CompilerWorkQuery {
                offset: 100,
                ..query()
            },
        )
        .unwrap();
    assert!(beyond_last_page["rows"].as_array().unwrap().is_empty());
    assert_eq!(beyond_last_page["summary"], global["summary"]);
    let ascending = store
        .compiler_work(
            &capture.id,
            CompilerWorkQuery {
                descending: false,
                limit: 1,
                ..query()
            },
        )
        .unwrap();
    assert_eq!(ascending["rows"][0]["work_ms"], 2.0);
    assert_eq!(ascending["summary"], global["summary"]);
    assert!(
        store
            .compiler_work(
                &capture.id,
                CompilerWorkQuery {
                    order: "work; DROP TABLE events",
                    ..query()
                }
            )
            .is_err()
    );

    // An existing store needs no recapture. Replacing a unit's latest attempt must
    // replace its contribution; adding the global index cannot duplicate evidence.
    rusqlite::Connection::open(store.root.join("index.sqlite3"))
        .unwrap()
        .execute("DROP INDEX compiler_work_query", [])
        .unwrap();
    let reopened = Store::open(&store.root).unwrap();
    reopened
        .save_events(
            &capture.id,
            first,
            &[event(
                "InstantiateFunction",
                "Entity<int>",
                3_000.0,
                Some(7),
            )],
        )
        .unwrap();
    let replaced = reopened.compiler_work(&capture.id, query()).unwrap();
    assert_eq!(replaced["rows"][0]["work_ms"], 93.0);
    assert_eq!(replaced["summary"]["work_ms"], 93.0);
    assert_eq!(replaced["rows"][0]["count"], 3);
    let archive = root.join("global.buildscope");
    reopened.export(&capture.id, &archive, false).unwrap();
    let imported = Store::open(&root.join("imported")).unwrap();
    imported.import(&archive).unwrap();
    assert_eq!(
        imported.compiler_work(&capture.id, query()).unwrap()["rows"],
        replaced["rows"]
    );
}

#[test]
fn paired_clang_headers_keep_nested_costs_and_thread_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let trace = json!({"traceEvents":[
        {"name":"ExecuteCompiler","ph":"X","ts":0,"dur":10000,"pid":1,"tid":1},
        {"name":"Source","cat":"Source","id":0,"ph":"b","ts":1000,"pid":1,"tid":1,"args":{"detail":"root.hpp"}},
        {"name":"Source","cat":"Source","id":0,"ph":"e","ts":8000,"pid":1,"tid":1},
        {"name":"Source","cat":"Source","id":0,"ph":"b","ts":2000,"pid":1,"tid":1,"args":{"detail":"child.hpp"}},
        {"name":"Source","cat":"Source","id":0,"ph":"e","ts":4000,"pid":1,"tid":1},
        {"name":"InstantiateFunction","ph":"X","ts":5000,"dur":1500,"pid":1,"tid":1,
            "args":{"detail":"work<int>","file":"root.hpp","line":9}},
        {"name":"OtherThread","ph":"X","ts":5000,"dur":1000,"pid":1,"tid":2},
        {"name":"Total Frontend","ph":"X","ts":0,"dur":9000,"pid":1,"tid":99,"args":{"count":1}},
        {"name":"NestedBeginEnd","ph":"B","ts":6000,"pid":1,"tid":1},
        {"ph":"E","ts":6500,"pid":1,"tid":1}
    ]});
    fs::write(root.join("trace.json"), trace.to_string()).unwrap();
    fs::write(
        root.join("stderr"),
        format!(
            ". {}\n.. {}\n",
            root.join("root.hpp").display(),
            root.join("child.hpp").display()
        ),
    )
    .unwrap();
    let unit = unit(root, "main.cpp", "main.o");
    let invocation = Invocation {
        id: "probe".into(),
        directory: path_string(root),
        arguments: vec![],
        response_files: BTreeMap::new(),
        output: Some(unit.output.clone()),
        family: "clang".into(),
        exit_code: Some(0),
        elapsed_ms: 10.0,
        started_ms: Some(1000),
        trace: Some("trace.json".into()),
        stderr: "stderr".into(),
        granularity_us: Some(0),
        verbose: true,
    };
    let (profile, events) = compiler::decode(&invocation, &unit, root).unwrap();
    assert_eq!(events.len(), 6);
    assert_eq!(profile.phases[0].name, "Frontend");
    assert_eq!(profile.phases[0].value, 9.0);
    assert_eq!(profile.templates[0].name, "work<int>");
    assert_eq!(profile.templates[0].total_ms, 1.5);
    assert_eq!(profile.templates[0].line, Some(9));
    let root_cost = profile
        .header_costs
        .iter()
        .find(|h| h.path.ends_with("root.hpp"))
        .unwrap();
    assert_eq!(root_cost.inclusive_ms, 7.0);
    assert_eq!(root_cost.recorded_self_ms, 5.0);
    assert_eq!(profile.includes[1].depth, 2);
    assert!(profile.includes[1].parent.ends_with("root.hpp"));
    assert!(
        events
            .iter()
            .find(|e| e.name == "OtherThread")
            .unwrap()
            .parent
            .is_none()
    );
    let store = Store::open(&root.join("store")).unwrap();
    store.save_events("run", "unit", &events).unwrap();
    let page = store
        .events(
            "run",
            "unit",
            EventQuery {
                text: "work",
                limit: 1,
                ..EventQuery::default()
            },
        )
        .unwrap();
    assert_eq!(page["total"], 1);
    let parent = events
        .iter()
        .find(|e| e.detail.ends_with("root.hpp") && e.name == "Source")
        .unwrap();
    let scope = store.scope("run", "unit", parent.id).unwrap();
    assert!(
        scope["children"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == "InstantiateFunction")
    );
}

#[test]
fn gcc_text_and_sarif_preserve_wall_timer_semantics() {
    let linux = "Time variable                                   usr           sys          wall           GGC\n phase parsing : 0.10 (50%) 0.02 (30%) 0.15 (60%) 1234k\n template instantiation : 0.04 (20%) 0.01 (10%) 0.07 (25%) 2048k\n TOTAL : 0.20 0.05 0.25 4096k\n";
    let timers = compiler::parse_gcc_report(linux);
    assert_eq!(timers[0].value, 150.0);
    assert_eq!(timers[1].value, 70.0);
    assert_eq!(timers[2].value, 250.0);
    let mac = "Time variable                                  wall           GGC\n constant expression evaluation : 0.02 (50%) 2048k\n TOTAL : 0.04 4096k\n";
    assert_eq!(compiler::parse_gcc_report(mac)[0].value, 20.0);
    let document = json!({"runs":[{"invocations":[{"properties":{"gcc/timeReport":{"timevars":[
        {"name":"template instantiation","elapsed":{"wall":0.123456,"ggc_mem":500}}
    ]}}}]}]});
    let text = format!(". /a header.hpp\n{document}");
    let timers = compiler::parse_gcc_report(&text);
    assert_eq!(timers.len(), 1);
    assert!((timers[0].value - 123.456).abs() < 0.0001);
    assert_eq!(timers[0].origin, "measured");
}

#[test]
fn response_files_preserve_original_content_and_expand_from_the_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let response = temp.path().join("arguments.rsp");
    let text = "-DVALUE=7 -I\"include directory\" -o object.o -c main.cpp\n";
    fs::write(&response, text).unwrap();
    let args = vec!["clang++".to_owned(), "@arguments.rsp".to_owned()];
    let (expanded, saved) =
        project::expand_responses(&args, temp.path(), &BTreeMap::new()).unwrap();
    assert!(expanded.iter().any(|arg| arg == "-Iinclude directory"));
    assert_eq!(output_argument(&expanded).as_deref(), Some("object.o"));
    assert_eq!(saved[&path_string(response.canonicalize().unwrap())], text);
    fs::write(&response, "-DVALUE=999\n").unwrap();
    let (again, _) = project::expand_responses(&args, temp.path(), &saved).unwrap();
    assert_eq!(again, expanded);
    fs::write(&response, "@arguments.rsp\n").unwrap();
    assert!(project::expand_responses(&args, temp.path(), &BTreeMap::new()).is_err());
}

#[test]
fn retained_artifacts_and_source_mappings_cannot_escape_the_selected_roots() {
    let temp = tempfile::tempdir().unwrap();
    assert!(artifact_path(temp.path(), "../outside").is_err());
    assert!(artifact_path(temp.path(), "/absolute").is_err());
    let mut capture = capture_fixture(temp.path());
    capture
        .inputs
        .insert("/original/../outside".into(), FileStamp::default());
    let store = Store::open(&temp.path().join("store")).unwrap();
    store.save(&capture).unwrap();
    assert!(
        store
            .source(
                &capture,
                "/original/../outside",
                &[("/original".into(), path_string(temp.path()))]
            )
            .unwrap_err()
            .to_string()
            .contains("escapes")
    );
}

#[test]
fn dependencies_do_not_fabricate_include_paths_and_stale_inventory_is_excluded() {
    let temp = tempfile::tempdir().unwrap();
    let deps = project::parse_dependencies(
        "first.o: #deps 2, deps mtime 1 (VALID)\n    source.cpp\n    a header.hpp\n\nsecond.o: #deps 1, deps mtime 2 (STALE)\n    stale.hpp\n",
        temp.path(),
    );
    assert_eq!(deps.len(), 1);
    assert!(
        deps.values()
            .next()
            .unwrap()
            .iter()
            .any(|p| p.ends_with("a header.hpp"))
    );
    let edges = compiler::parse_includes(
        ". a header.hpp\n.. child.hpp\n. separate.hpp\n",
        "main.cpp",
        temp.path(),
    );
    assert_eq!(edges.len(), 3);
    assert!(edges[1].parent.ends_with("a header.hpp"));
    assert_eq!(edges[2].parent, "main.cpp");
}

#[test]
fn ninja_graph_collapses_rules_and_log_ignores_old_sessions_and_multioutputs() {
    let temp = tempfile::tempdir().unwrap();
    let project = project_fixture(temp.path());
    let before = "# ninja log v5\n0\t10\t100\told.o\tdead\n";
    let after = "# ninja log v5\n0\t10\t100\told.o\tdead\n0\t5\t200\tgenerated.hpp\tabc\n0\t5\t200\tgenerated.stamp\tabc\n5\t20\t300\tfirst.o\tdef\n";
    let actions = capture::parse_actions(before, after, &project, "");
    assert_eq!(actions.len(), 2);
    assert_eq!(actions[0].outputs.len(), 2);
    // Ninja can log both relative and absolute names for a custom command output.
    let duplicate_output = format!(
        "0\t5\t200\tgenerated.hpp\tabc\n0\t5\t200\t{}\tabc\n",
        temp.path().join("build/generated.hpp").display()
    );
    let duplicate_actions = capture::parse_actions("", &duplicate_output, &project, "");
    assert_eq!(duplicate_actions.len(), 1);
    assert_eq!(duplicate_actions[0].outputs.len(), 1);
    assert_eq!(
        actions[1].unit_id.as_deref(),
        Some(project.units[0].id.as_str())
    );
    let dot = r#"digraph ninja {
"source" [label="source.cpp"]
"rule" [label="CXX_COMPILER", shape=ellipse]
"output" [label="first.o"]
"source" -> "rule" [arrowhead=none]
"rule" -> "output"
}"#;
    let graph = project::parse_graph(dot, temp.path());
    assert_eq!(
        graph[&path_string(temp.path().join("first.o"))],
        [path_string(temp.path().join("source.cpp"))]
    );
}

#[test]
fn changes_and_shared_rebuilds_keep_contributors_separate_from_native_triggers() {
    let temp = tempfile::tempdir().unwrap();
    let mut run = capture_fixture(temp.path());
    let shared = path_string(temp.path().join("shared.hpp"));
    let other = path_string(temp.path().join("other.hpp"));
    run.project.dependencies.insert(
        run.project.units[0].output.clone(),
        vec![shared.clone(), other.clone()],
    );
    run.changes = vec![
        InputChange {
            path: shared.clone(),
            kind: "content".into(),
            during_build: false,
        },
        InputChange {
            path: other.clone(),
            kind: "content".into(),
            during_build: false,
        },
    ];
    run.actions = capture::parse_actions("", "0\t25\t100\tfirst.o\thash\n", &run.project, "");
    let explanation =
        format!("ninja explain: output first.o older than most recent input {shared} (100 vs 1)\n");
    analysis::analyze(&mut run, &explanation);
    let a = run.headers.iter().find(|h| h.path == shared).unwrap();
    let b = run.headers.iter().find(|h| h.path == other).unwrap();
    assert!(a.explicit_trigger && a.changed_contributor);
    assert!(!b.explicit_trigger && b.changed_contributor);
    assert_eq!(a.rebuilt_work_ms, 25.0);
    assert_eq!(b.rebuilt_work_ms, 25.0);
    assert!(a.processing_ms.is_none() && b.processing_ms.is_none());
    assert_eq!(run.summary.work_ms, 25.0);
    let same = FileStamp {
        modified_ns: 1,
        size: 1,
        digest: Some("same".into()),
    };
    let touched = FileStamp {
        modified_ns: 2,
        ..same.clone()
    };
    let a = BTreeMap::from([("header".into(), same)]);
    let b = BTreeMap::from([("header".into(), touched)]);
    assert_eq!(project::changes(&a, &b, false)[0].kind, "timestamp");
}

#[test]
fn running_checkpoints_survive_reopening_and_defer_reports_until_requested() {
    let temp = tempfile::tempdir().unwrap();
    let mut capture = capture_fixture(temp.path());
    capture.status = "running".into();
    capture.finished_ms = None;
    capture.project.dependencies.insert(
        capture.project.units[0].output.clone(),
        vec![path_string(temp.path().join("shared.hpp"))],
    );
    let root = temp.path().join("store");
    Store::open(&root).unwrap().save(&capture).unwrap();

    let store = Store::open(&root).unwrap();
    assert_eq!(
        serde_json::to_value(store.load(&capture.id).unwrap()).unwrap(),
        serde_json::to_value(&capture).unwrap()
    );
    let database = rusqlite::Connection::open(root.join("index.sqlite3")).unwrap();
    let indexed_units = || {
        database
            .query_row("SELECT count(*) FROM report_units", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    assert_eq!(indexed_units(), 0);
    database
        .execute_batch(
            "CREATE TRIGGER fail_report BEFORE INSERT ON report_units
        BEGIN SELECT RAISE(ABORT,'injected report failure'); END;",
        )
        .unwrap();
    assert!(store.overview(&capture.id).is_err());
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM report_runs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(indexed_units(), 0);
    database.execute_batch("DROP TRIGGER fail_report").unwrap();
    assert_eq!(store.overview(&capture.id).unwrap()["status"], "running");
    assert_eq!(indexed_units(), 1);
    assert!(
        store
            .unit_detail(&capture.id, &capture.project.units[0].id)
            .is_ok()
    );

    // A retry must invalidate an on-demand report of the earlier checkpoint.
    capture.project.units[0].source_key = "updated.cpp".into();
    store.save(&capture).unwrap();
    assert_eq!(indexed_units(), 0);
    assert_eq!(store.overview(&capture.id).unwrap()["status"], "running");
    assert_eq!(
        store
            .unit_detail(&capture.id, &capture.project.units[0].id)
            .unwrap()["unit"]["source_key"],
        "updated.cpp"
    );

    // Recovery consumes the durable manifest even when no compiler artifacts exist yet.
    let recovered = capture::recover(&store, &capture.id).unwrap();
    assert_eq!(recovered.status, "interrupted");
    assert_eq!(recovered.project.dependencies, capture.project.dependencies);
    assert_eq!(
        store.overview(&capture.id).unwrap()["status"],
        "interrupted"
    );
    assert!(
        store
            .header_detail(&capture.id, &path_string(temp.path().join("shared.hpp")))
            .is_ok()
    );
}

#[test]
fn large_action_inventories_keep_output_identity_and_first_match_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let mut project = project_fixture(temp.path());
    project.units = (0..2048)
        .map(|index| unit(temp.path(), "shared.cpp", &format!("build/{index}.o")))
        .collect();
    let mut duplicate = project.units[0].clone();
    duplicate.id = "duplicate-output".into();
    project.units.push(duplicate);
    let log: String = (0..2048)
        .rev()
        .map(|index| format!("0\t10\t1\t{index}.o\tcommand-{index}\n"))
        .collect();
    let actions = capture::parse_actions("", &log, &project, "");
    assert_eq!(actions.len(), 2048);
    for (action, unit) in actions.iter().zip(project.units[..2048].iter().rev()) {
        assert_eq!(action.unit_id.as_deref(), Some(unit.id.as_str()));
        assert_eq!(
            action.outputs.as_slice(),
            std::slice::from_ref(&unit.output)
        );
    }
}

#[test]
fn decoder_writer_failure_retains_evidence_and_recovery_keeps_the_latest_unit_events() {
    let temp = tempfile::tempdir().unwrap();
    let mut run = capture_fixture(temp.path());
    run.status = "running".into();
    run.finished_ms = None;
    run.project
        .units
        .push(unit(temp.path(), "second.cpp", "build/second.o"));
    let store = Store::open(&temp.path().join("store")).unwrap();
    store.save(&run).unwrap();
    let root = store.run_dir(&run.id).unwrap();
    fs::create_dir_all(root.join("raw")).unwrap();
    for (id, unit, detail, count) in [
        ("000-first", &run.project.units[0], "first", 4000),
        ("001-other", &run.project.units[1], "other", 1),
        ("002-latest", &run.project.units[0], "latest", 1),
    ] {
        let invocation = Invocation {
            id: id.into(),
            directory: path_string(temp.path()),
            arguments: unit.arguments.clone(),
            response_files: BTreeMap::new(),
            output: Some(unit.output.clone()),
            family: "clang".into(),
            exit_code: Some(0),
            elapsed_ms: 10.0,
            started_ms: Some(1000),
            trace: Some(format!("raw/{id}.json")),
            stderr: format!("raw/{id}.stderr"),
            granularity_us: Some(0),
            verbose: true,
        };
        let events: Vec<_> = (0..count)
            .map(|index| {
                json!({
                    "name":"InstantiateFunction", "ph":"X", "ts":index*10, "dur":5,
                    "pid":1, "tid":1, "args":{"detail":detail}
                })
            })
            .collect();
        write_json(
            &root.join(invocation.trace.as_ref().unwrap()),
            &json!({"traceEvents":events}),
        )
        .unwrap();
        fs::write(root.join(&invocation.stderr), "").unwrap();
        write_json(
            &root.join("invocations").join(format!("{id}.json")),
            &invocation,
        )
        .unwrap();
    }
    fs::write(root.join("invocations/003-incomplete.json"), "{").unwrap();
    let mut unknown: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("invocations/001-other.json")).unwrap())
            .unwrap();
    unknown["output"] = json!("unknown-output.o");
    write_json(&root.join("invocations/004-unknown.json"), &unknown).unwrap();
    unknown["output"] = json!(run.project.units[1].output);
    unknown["trace"] = json!("raw/missing.json");
    write_json(&root.join("invocations/005-missing-trace.json"), &unknown).unwrap();

    let database = rusqlite::Connection::open(store.root.join("index.sqlite3")).unwrap();
    database
        .execute_batch(&format!(
            "CREATE TRIGGER fail_event BEFORE INSERT ON events WHEN NEW.unit='{}'
         BEGIN SELECT RAISE(ABORT,'injected event failure'); END;",
            run.project.units[1].id
        ))
        .unwrap();
    assert!(
        capture::recover(&store, &run.id)
            .unwrap_err()
            .to_string()
            .contains("injected event failure")
    );
    assert_eq!(store.load(&run.id).unwrap().status, "running");
    assert!(fs::read_dir(root.join("events")).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
    for id in ["000-first", "001-other", "002-latest"] {
        let invocation: Invocation = serde_json::from_slice(
            &fs::read(root.join("invocations").join(format!("{id}.json"))).unwrap(),
        )
        .unwrap();
        assert!(
            root.join(invocation.trace.unwrap()).is_file(),
            "{id} must remain decodable after a writer failure"
        );
    }
    database.execute_batch("DROP TRIGGER fail_event").unwrap();

    let recovered = capture::recover(&store, &run.id).unwrap();
    assert_eq!(recovered.status, "interrupted");
    assert_eq!(recovered.profiles.len(), 2);
    assert_eq!(recovered.profiles[0].unit_id, run.project.units[0].id);
    assert_eq!(recovered.profiles[0].templates[0].name, "latest");
    assert_eq!(recovered.profiles[0].event_count, 1);
    assert_eq!(recovered.profiles[1].unit_id, run.project.units[1].id);
    assert_eq!(recovered.profiles[1].templates[0].name, "other");
    assert!(
        recovered
            .notes
            .iter()
            .any(|note| note.contains("Multiple compiler invocations"))
    );
    assert!(
        recovered
            .notes
            .iter()
            .any(|note| note.contains("Incomplete invocation manifest"))
    );
    assert!(
        recovered
            .notes
            .iter()
            .any(|note| note.contains("not in the compilation inventory"))
    );
    assert!(
        recovered
            .notes
            .iter()
            .any(|note| note.contains("Detail unavailable"))
    );
    let page = store
        .events(&run.id, &run.project.units[0].id, EventQuery::default())
        .unwrap();
    assert_eq!(page["total"], 1);
    assert_eq!(page["events"][0]["detail"], "latest");
    let bytes = zstd::stream::decode_all(
        fs::File::open(
            root.join("events")
                .join(format!("{}.json.zst", run.project.units[0].id)),
        )
        .unwrap(),
    )
    .unwrap();
    let archived: Vec<TraceEvent> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].detail, "latest");
    assert_eq!(fs::read_dir(root.join("events")).unwrap().count(), 2);
}

#[test]
fn portable_archives_preserve_evidence_and_do_not_grant_local_source_access() {
    let temp = tempfile::tempdir().unwrap();
    let mut capture = capture_fixture(temp.path());
    let file = temp.path().join("first.cpp");
    fs::write(&file, "int first() { return 1; }\n").unwrap();
    capture.inputs = project::snapshot([path_string(&file)]);
    let store = Store::open(&temp.path().join("original")).unwrap();
    store.save(&capture).unwrap();
    let event = TraceEvent {
        id: 0,
        name: "Frontend".into(),
        detail: "".into(),
        thread: "1:1".into(),
        start_us: 0.0,
        duration_us: 1000.0,
        file: None,
        line: None,
        parent: None,
        depth: 0,
    };
    store
        .save_events(&capture.id, &capture.project.units[0].id, &[event])
        .unwrap();
    let archive = temp.path().join("capture.buildscope");
    store.export(&capture.id, &archive, false).unwrap();
    let imported = Store::open(&temp.path().join("imported")).unwrap();
    let id = imported.import(&archive).unwrap();
    assert_eq!(imported.import(&archive).unwrap(), id);
    assert_eq!(imported.list().unwrap().len(), 1);
    let loaded = imported.load(&id).unwrap();
    assert_eq!(loaded.id, capture.id);
    assert!(!imported.is_local(&id).unwrap());
    assert!(imported.source(&loaded, &path_string(&file), &[]).is_err());
    assert_eq!(
        imported
            .events(
                &id,
                &capture.project.units[0].id,
                EventQuery {
                    limit: 20,
                    order: "time",
                    ..EventQuery::default()
                }
            )
            .unwrap()["total"],
        1
    );
    let with_sources = temp.path().join("sources.buildscope");
    store.export(&capture.id, &with_sources, true).unwrap();
    let with_source_store = Store::open(&temp.path().join("with-sources")).unwrap();
    with_source_store.import(&with_sources).unwrap();
    assert_eq!(
        with_source_store
            .source(&loaded, &path_string(&file), &[])
            .unwrap()["matches_capture"],
        true
    );
}

#[test]
fn archive_import_rejects_links_and_future_schemas() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&temp.path().join("store")).unwrap();
    let path = temp.path().join("link.buildscope");
    let encoder = zstd::stream::write::Encoder::new(fs::File::create(&path).unwrap(), 1).unwrap();
    let mut archive = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    header.set_mode(0o600);
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_link_name("../escape").unwrap();
    header.set_cksum();
    archive
        .append_data(&mut header, "capture/escape", std::io::empty())
        .unwrap();
    archive.into_inner().unwrap().finish().unwrap();
    assert!(
        store
            .import(&path)
            .unwrap_err()
            .to_string()
            .contains("links")
    );
    let mut future = capture_fixture(temp.path());
    future.schema_version = SCHEMA_VERSION + 1;
    assert!(store.save(&future).is_err());
}

#[test]
fn comparisons_disclose_incompatible_conditions_and_unpaired_work() {
    let temp = tempfile::tempdir().unwrap();
    let before = capture_fixture(temp.path());
    let mut after = before.clone();
    after.id = "after".into();
    after.kind = "profile".into();
    after.project.configuration_id = "other".into();
    after.machine = "different".into();
    let result = analysis::compare(&before, &after);
    assert_eq!(result["conditions_match"], false);
    assert!(result["differences"].as_array().unwrap().len() >= 3);
    assert!(result["units"].as_array().unwrap().is_empty());
}

#[test]
fn repeated_comparisons_show_distributions_and_reject_mixed_conditions() {
    let temp = tempfile::tempdir().unwrap();
    let mut before = vec![capture_fixture(temp.path()); 3];
    let mut after = before.clone();
    for (run, time) in before.iter_mut().zip([100.0, 200.0, 300.0]) {
        run.elapsed_ms = time;
        run.label = Some("before".into());
    }
    for (run, time) in after.iter_mut().zip([80.0, 90.0, 100.0]) {
        run.elapsed_ms = time;
        run.label = Some("after".into());
    }
    let result = analysis::compare_series(&before, &after).unwrap();
    assert_eq!(result["before"]["median_ms"], 200.0);
    assert_eq!(result["after"]["median_ms"], 90.0);
    assert_eq!(result["delta_median_ms"], -110.0);
    assert_eq!(result["conditions_match"], true);
    after[0].machine = "other".into();
    assert_eq!(
        analysis::compare_series(&before, &after).unwrap()["conditions_match"],
        false
    );
    assert!(analysis::compare_series(&[], &after).is_err());
}

fn unit(root: &Path, source: &str, output: &str) -> CompilationUnit {
    CompilationUnit {
        id: digest(format!("{source}{output}"))[..16].into(),
        source: path_string(root.join(source)),
        source_key: source.into(),
        output: path_string(root.join(output)),
        output_key: output.into(),
        directory: path_string(root),
        arguments: vec![
            "clang++".into(),
            "-c".into(),
            source.into(),
            "-o".into(),
            output.into(),
        ],
        compiler: 0,
        command_id: "command".into(),
    }
}

#[test]
fn ninja_regeneration_aligns_clock_segments_and_preserves_dependency_barrier() {
    let temp = tempfile::tempdir().unwrap();
    let mut run = capture_fixture(temp.path());
    let log = "# ninja log v5\n0\t60\t1\tbuild.ninja\ta\n0\t60\t1\tcmake_install.cmake\ta\n2\t17\t2\tgenerated.hpp\tb\n17\t117\t3\tfirst.o\tc\n";
    run.actions = capture::parse_actions("", log, &run.project, "");
    assert_eq!(run.actions.len(), 3);
    assert_eq!(run.actions[0].kind, "configure");
    assert_eq!(run.actions[1].start_ms, 60.0);
    assert_eq!(run.actions[2].end_ms, 175.0);
    assert_eq!(run.actions[2].clock_segment, 1);
    run.project.graph.insert(
        run.project.units[0].output.clone(),
        vec![path_string(temp.path().join("build/generated.hpp"))],
    );
    analysis::analyze(&mut run, "");
    assert_eq!(run.summary.max_concurrency, 1);
    assert_eq!(run.summary.dependency_path_ms, 175.0);
}

#[test]
fn local_project_identities_do_not_merge_unrelated_projects() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    assert_ne!(
        project::project_identity(a.path()),
        project::project_identity(b.path())
    );
    assert_eq!(
        project::project_identity(a.path()),
        project::project_identity(a.path())
    );
}

#[test]
fn older_compiler_manifests_use_legacy_trace_paths() {
    let compiler: CompilerInfo = serde_json::from_value(json!({
        "executable": "clang++", "version": "clang version 14.0.0",
        "family": "clang", "verbose_trace": false, "sarif": false
    }))
    .unwrap();
    assert!(!compiler.explicit_trace_path);
}

fn project_fixture(root: &Path) -> ProjectSnapshot {
    ProjectSnapshot {
        source_dir: path_string(root),
        build_dir: path_string(root.join("build")),
        configuration_id: "config".into(),
        project_id: "test-project".into(),
        build_type: "Release".into(),
        compilers: vec![CompilerInfo {
            executable: "clang++".into(),
            version: "test clang".into(),
            family: "clang".into(),
            explicit_trace_path: true,
            verbose_trace: true,
            sarif: false,
        }],
        units: vec![unit(root, "first.cpp", "build/first.o")],
        dependencies: BTreeMap::new(),
        graph: BTreeMap::new(),
        rules: BTreeMap::new(),
        commands: BTreeMap::new(),
        response_files: BTreeMap::new(),
        cmake_cache: BTreeMap::new(),
    }
}

fn capture_fixture(root: &Path) -> Capture {
    Capture {
        schema_version: SCHEMA_VERSION,
        id: "capture-one".into(),
        started_ms: 1000,
        finished_ms: Some(2000),
        elapsed_ms: 1000.0,
        status: "success".into(),
        kind: "record".into(),
        label: None,
        git: Default::default(),
        machine: "test-host".into(),
        environment: BTreeMap::new(),
        cache_policy: "preserved".into(),
        jobs: None,
        workload: None,
        profile_origin: None,
        targets: vec![],
        project: project_fixture(root),
        actions: vec![],
        profiles: vec![],
        headers: vec![],
        inputs: BTreeMap::new(),
        changes: vec![],
        findings: vec![],
        notes: vec![],
        summary: BuildSummary::default(),
    }
}

#[test]
fn old_capture_metadata_defaults_to_not_recorded() {
    let temp = tempfile::tempdir().unwrap();
    let mut manifest = serde_json::to_value(capture_fixture(temp.path())).unwrap();
    manifest.as_object_mut().unwrap().remove("git");
    manifest.as_object_mut().unwrap().remove("workload");
    manifest.as_object_mut().unwrap().remove("jobs");
    let capture: Capture = serde_json::from_value(manifest).unwrap();
    assert_eq!(capture.git.status, buildscope::git::GitStatus::NotRecorded);
    assert!(capture.workload.is_none());
    assert_eq!(capture.jobs, None);
    let store = Store::open(&temp.path().join("store")).unwrap();
    store.save(&capture).unwrap();
    let archive = temp.path().join("old.buildscope");
    store.export(&capture.id, &archive, false).unwrap();
    let imported = Store::open(&temp.path().join("imported")).unwrap();
    imported.import(&archive).unwrap();
    assert_eq!(
        imported.load(&capture.id).unwrap().git.status,
        buildscope::git::GitStatus::NotRecorded
    );
}
