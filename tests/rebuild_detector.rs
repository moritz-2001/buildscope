//! Boundary evidence and attribution contracts for the optional rebuild detector.
use buildscope::{analysis, model::*, rebuild, store::Store, util::*};
use std::{collections::BTreeMap, fs, path::Path};
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
fn stamp(mtime: u128, text: &str) -> FileStamp {
    FileStamp {
        modified_ns: mtime,
        size: text.len() as u64,
        digest: Some(digest(text)),
    }
}
fn change(path: &str, before: FileStamp, after: FileStamp) -> rebuild::FingerprintChange {
    rebuild::FingerprintChange {
        path: path.into(),
        interval: "during-build".into(),
        before: Some(before),
        after: Some(after),
    }
}
fn observed(root: &Path) -> Capture {
    let mut run = capture_fixture(root);
    let source = run.project.units[0].source.clone();
    run.project
        .dependencies
        .insert(run.project.units[0].output.clone(), vec![source]);
    run.actions = vec![
        BuildAction {
            id: "generator".into(),
            outputs: vec![run.project.units[0].source.clone()],
            unit_id: None,
            kind: "generate".into(),
            clock_segment: 0,
            start_ms: 0.0,
            end_ms: 5.0,
            command_hash: "g".into(),
            reasons: vec![],
            success: true,
        },
        BuildAction {
            id: "compile".into(),
            outputs: vec![run.project.units[0].output.clone()],
            unit_id: Some(run.project.units[0].id.clone()),
            kind: "compile".into(),
            clock_segment: 0,
            start_ms: 5.0,
            end_ms: 25.0,
            command_hash: "c".into(),
            reasons: vec![],
            success: true,
        },
    ];
    analysis::analyze(&mut run, "");
    run
}
#[test]
fn unchanged_generated_overwrite_has_candidate_and_overlap_counts_union() {
    let temp = tempfile::tempdir().unwrap();
    let run = observed(temp.path());
    let path = &run.project.units[0].source;
    let comparisons = vec![
        change(path, stamp(1, "same"), stamp(2, "same")),
        change(path, stamp(2, "same"), stamp(3, "same")),
    ];
    let report = rebuild::analyze(&run, None, comparisons, "");
    assert_eq!(report.candidates.len(), 2);
    assert_eq!(report.candidates[0].category, "generated-output-churn");
    assert_eq!(report.candidates[0].generators, vec!["generator"]);
    assert_eq!(report.combined_work_ms, 20.0);
    assert_eq!(
        report.candidates.iter().map(|c| c.work_ms).sum::<f64>(),
        40.0
    );
}
#[test]
fn unknown_content_edits_partial_and_instrumented_captures_do_not_assert_candidates() {
    let temp = tempfile::tempdir().unwrap();
    let mut run = observed(temp.path());
    let path = run.project.units[0].source.clone();
    for after in [
        stamp(2, "edit"),
        FileStamp {
            digest: None,
            ..stamp(2, "same")
        },
        stamp(0, "same"),
        stamp(1, "same"),
    ] {
        assert!(
            rebuild::analyze(&run, None, vec![change(&path, stamp(1, "same"), after)], "")
                .candidates
                .is_empty()
        );
    }
    let comparisons = vec![change(&path, stamp(1, "same"), stamp(2, "same"))];
    run.summary.dependency_units = 0;
    assert!(!rebuild::analyze(&run, None, comparisons.clone(), "").eligible);
    run.summary.dependency_units = run.project.units.len();
    for (kind, status) in [
        ("profile", "success"),
        ("launcher", "success"),
        ("record", "failed"),
        ("record", "interrupted"),
    ] {
        run.kind = kind.into();
        run.status = status.into();
        let report = rebuild::analyze(&run, None, comparisons.clone(), "");
        assert!(!report.eligible);
        assert!(report.candidates.is_empty());
    }
}
#[test]
fn stable_copy_has_no_comparison_and_content_edits_at_another_boundary_suppress_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let run = observed(temp.path());
    let path = &run.project.units[0].source;
    let map = BTreeMap::from([(path.clone(), stamp(1, "same"))]);
    assert!(rebuild::compare_stamps(&map, &map, "during-build", map.keys().cloned()).is_empty());
    let comparisons = vec![
        change(path, stamp(1, "same"), stamp(2, "same")),
        change(path, stamp(2, "same"), stamp(3, "edit")),
    ];
    assert!(
        rebuild::analyze(&run, None, comparisons, "")
            .candidates
            .is_empty()
    );
}
#[test]
fn raw_evidence_survives_archive_without_sources() {
    let temp = tempfile::tempdir().unwrap();
    let run = observed(temp.path());
    let store = Store::open(&temp.path().join("store")).unwrap();
    store.save(&run).unwrap();
    let timestamp = 1_792_518_489_384_825_301;
    let mut report = rebuild::analyze(
        &run,
        None,
        vec![change(
            "precise.hpp",
            stamp(timestamp, "past"),
            stamp(timestamp + 1, "past"),
        )],
        "",
    );
    report
        .before_inputs
        .insert("missing-now".into(), stamp(1, "past"));
    write_json(
        &store
            .run_dir(&run.id)
            .unwrap()
            .join("rebuild-evidence.json"),
        &report,
    )
    .unwrap();
    let view = store.rebuild_analysis(&run.id).unwrap();
    assert!(view.get("before_inputs").is_none());
    assert_eq!(
        view["comparisons"][0]["before"]["modified_ns"],
        timestamp.to_string()
    );
    assert_eq!(
        view["comparisons"][0]["after"]["modified_ns"],
        (timestamp + 1).to_string()
    );
    let archive = temp.path().join("run.tar.zst");
    store.export(&run.id, &archive, false).unwrap();
    let imported = Store::open(&temp.path().join("imported")).unwrap();
    imported.import(&archive).unwrap();
    let bytes = fs::read(
        imported
            .run_dir(&run.id)
            .unwrap()
            .join("rebuild-evidence.json"),
    )
    .unwrap();
    let saved: rebuild::RebuildReport = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        saved.before_inputs["missing-now"].digest,
        Some(digest("past"))
    );
}

#[test]
fn real_ninja_unchanged_generator_and_copy_if_different_have_distinct_results() {
    use buildscope::capture::{self, RecordRequest};
    use std::{process::Command, time::Duration};
    for tool in ["cmake", "ninja", "clang++"] {
        if Command::new(tool).arg("--version").output().is_err() {
            eprintln!("Skipping real rebuild fixture: {tool} unavailable");
            return;
        }
    }
    for preserve in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let build = temp.path().join("build");
        fs::create_dir(&source).unwrap();
        fs::write(
            source.join("main.cpp"),
            "#include \"generated.hpp\"\nint main(){return VALUE;}\n",
        )
        .unwrap();
        fs::write(source.join("trigger.txt"), "first").unwrap();
        fs::write(source.join("template.hpp"), "#define VALUE 0\n").unwrap();
        let generator = if preserve {
            "copy_if_different"
        } else {
            "copy"
        };
        fs::write(source.join("CMakeLists.txt"), format!(r#"cmake_minimum_required(VERSION 3.20)
project(RebuildFixture CXX)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)
add_custom_command(OUTPUT "${{CMAKE_BINARY_DIR}}/generated.hpp"
 COMMAND "${{CMAKE_COMMAND}}" -E {generator} "${{CMAKE_SOURCE_DIR}}/template.hpp" "${{CMAKE_BINARY_DIR}}/generated.hpp"
 DEPENDS "${{CMAKE_SOURCE_DIR}}/trigger.txt" "${{CMAKE_SOURCE_DIR}}/template.hpp")
add_executable(app main.cpp "${{CMAKE_BINARY_DIR}}/generated.hpp")
target_include_directories(app PRIVATE "${{CMAKE_BINARY_DIR}}")
"#)).unwrap();
        assert!(
            Command::new("cmake")
                .args(["-G", "Ninja", "-DCMAKE_CXX_COMPILER=clang++", "-S"])
                .arg(&source)
                .arg("-B")
                .arg(&build)
                .output()
                .unwrap()
                .status
                .success()
        );
        let store = Store::open(&temp.path().join("store")).unwrap();
        let request = RecordRequest {
            build_dir: build.clone(),
            targets: vec![],
            label: None,
            jobs: Some(1),
            full_detail: false,
            preprocess: false,
        };
        let (first, code) = capture::record(&store, &request).unwrap();
        assert_eq!(code, 0);
        // A newer capture from another tree must not replace the local predecessor.
        let mut other_tree = first.clone();
        other_tree.id = format!("{}-other-tree", first.id);
        other_tree.started_ms += 1;
        other_tree.project.build_dir = path_string(temp.path().join("different-tree"));
        store.save(&other_tree).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        fs::write(source.join("trigger.txt"), "second").unwrap();
        let (second, code) = capture::record(&store, &request).unwrap();
        assert_eq!(code, 0);
        let report: rebuild::RebuildReport = serde_json::from_slice(
            &fs::read(
                store
                    .run_dir(&second.id)
                    .unwrap()
                    .join("rebuild-evidence.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(report.previous, Some(first.id));
        let generated = report
            .candidates
            .iter()
            .find(|c| c.path.ends_with("/generated.hpp"));
        assert_eq!(
            generated.is_some(),
            !preserve,
            "preserve={preserve}; report={report:?}"
        );
        if let Some(candidate) = generated {
            assert_eq!(candidate.category, "generated-output-churn");
            assert!(!candidate.generators.is_empty());
        }
        assert!(
            report
                .comparisons
                .iter()
                .any(|c| c.path.ends_with("/trigger.txt")),
            "other content changes retained"
        );
        // An interrupted recovery must retain the pre-build evidence instead of reading later inputs.
        let mut interrupted = second;
        interrupted.status = "running".into();
        store.save(&interrupted).unwrap();
        fs::write(build.join("generated.hpp"), "later unrelated contents").unwrap();
        capture::recover(&store, &interrupted.id).unwrap();
        let recovered: rebuild::RebuildReport = serde_json::from_slice(
            &fs::read(
                store
                    .run_dir(&interrupted.id)
                    .unwrap()
                    .join("rebuild-evidence.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(!recovered.eligible);
        assert!(recovered.candidates.is_empty());
        assert_eq!(recovered.before_inputs, report.before_inputs);
        // Reject stale lineage across an intervening interrupted same-tree capture.
        fs::write(build.join("generated.hpp"), "#define VALUE 0\n").unwrap();
        let (next, code) = capture::record(&store, &request).unwrap();
        assert_eq!(code, 0);
        let next_report: rebuild::RebuildReport = serde_json::from_slice(
            &fs::read(
                store
                    .run_dir(&next.id)
                    .unwrap()
                    .join("rebuild-evidence.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(next_report.previous, None);
    }
}
