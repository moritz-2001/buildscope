//! Interface-level decoding and evidence contracts. These synthetic fixtures exercise
//! format variants and attribution boundaries independently of local compiler versions.
use buildscope::{
    analysis, capture, compiler,
    model::*,
    project,
    store::{EventQuery, Store},
    util::*,
};
use serde_json::json;
use std::{collections::BTreeMap, fs, path::Path};

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
        machine: "test-host".into(),
        environment: BTreeMap::new(),
        cache_policy: "preserved".into(),
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
