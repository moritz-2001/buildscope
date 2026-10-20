//! Saved Git frequency and capture-cost contracts.
use buildscope::{
    header_history::{collect, hotspots},
    model::*,
    util::*,
};
use std::{collections::BTreeMap, fs, path::Path, process::Command};
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

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim_end_matches('\n')
        .into()
}
fn commit(root: &Path) {
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "change"]);
}
fn header_capture(root: &Path, key: &str) -> Capture {
    let mut c = capture_fixture(root);
    c.git = buildscope::git::observe(root);
    c.git.observed_ms = Some(now_ms() + 2000);
    let header:HeaderSummary=serde_json::from_value(serde_json::json!({
        "path":root.join(key),"key":key,"units":[c.project.units[0].id],"direct_units":[],
        "rebuilt_units":0,"rebuilt_work_ms":0.,"processing_ms":null,"recorded_self_ms":null,
        "samples_ms":[],"measured_units":0,"explicit_trigger":false,"changed_contributor":false,"evidence":[]
    })).unwrap();
    c.headers.push(header);
    c
}
#[test]
fn subdirectory_weird_paths_rename_and_anchor_are_preserved() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    git(root, &["init", "-b", "main"]);
    fs::create_dir(root.join("src")).unwrap();
    let sub = root.join("src");
    let old = "old\t name.hpp";
    let new = "new\n name.hpp";
    fs::write(
        sub.join(old),
        (0..100)
            .map(|i| format!("int field{i};\n"))
            .collect::<String>(),
    )
    .unwrap();
    commit(root);
    git(&sub, &["mv", old, new]);
    commit(root);
    let mut content = fs::read_to_string(sub.join(new)).unwrap();
    content.push_str("int extra;\n");
    fs::write(sub.join(new), content).unwrap();
    commit(root);
    let c = header_capture(&sub, new);
    let saved = collect(&c, 90).unwrap();
    assert!(saved.complete);
    assert_eq!(saved.files.len(), 1);
    assert_eq!(saved.files[0].edits.len(), 2);
    assert_eq!(saved.files[0].renames.len(), 1);
    fs::write(sub.join(new), "later content").unwrap();
    commit(root);
    let again = collect(&c, 90).unwrap();
    assert_eq!(again.files[0].edits.len(), 2);
    assert_eq!(saved.anchor, again.anchor);
}
#[test]
fn first_parent_merge_counts_once_and_edited_rename_follows_history() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    git(r, &["init", "-b", "main"]);
    fs::write(
        r.join("a.hpp"),
        (0..100)
            .map(|i| format!("int field{i};\n"))
            .collect::<String>(),
    )
    .unwrap();
    commit(r);
    git(r, &["checkout", "-b", "feature"]);
    let content = fs::read_to_string(r.join("a.hpp")).unwrap() + "int feature;\n";
    fs::write(r.join("a.hpp"), content).unwrap();
    commit(r);
    git(r, &["checkout", "main"]);
    git(r, &["merge", "--no-ff", "feature", "-m", "merge"]);
    git(r, &["mv", "a.hpp", "b.hpp"]);
    let content = fs::read_to_string(r.join("b.hpp")).unwrap() + "int renamed;\n";
    fs::write(r.join("b.hpp"), content).unwrap();
    commit(r);
    let saved = collect(&header_capture(r, "b.hpp"), 90).unwrap();
    assert_eq!(saved.files[0].edits.len(), 3);
    assert_eq!(saved.files[0].renames.len(), 1);
}
#[test]
fn ranking_requires_complete_managed_costs_and_applies_filters() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    git(r, &["init", "-b", "main"]);
    fs::write(r.join("a.hpp"), "int a;\n").unwrap();
    commit(r);
    let mut c = header_capture(r, "a.hpp");
    let saved = collect(&c, 90).unwrap();
    c.actions.push(BuildAction {
        id: "1".into(),
        outputs: vec![],
        unit_id: Some(c.project.units[0].id.clone()),
        kind: "compile".into(),
        clock_segment: 0,
        start_ms: 0.,
        end_ms: 125.,
        command_hash: String::new(),
        reasons: vec![],
        success: true,
    });
    let result = hotspots(&c, &saved, 90, "", "", "");
    assert!(result["rows"][0]["priority"].is_null());
    assert_eq!(result["rows"][0]["work_ms"], 125.);
    c.kind = "profile".into();
    c.profile_origin = Some("managed".into());
    assert_eq!(
        hotspots(&c, &saved, 90, "", "", "")["rows"][0]["priority"],
        125.
    );
    assert!(hotspots(&c, &saved, 180, "", "", "")["rows"][0]["priority"].is_null());
    assert_eq!(
        hotspots(&c, &saved, 90, "missing", "", "")["rows"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        hotspots(&c, &saved, 90, "", "missing", "")["rows"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        hotspots(&c, &saved, 90, "", "", &c.headers[0].path)["rows"][0]["edits"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    c.actions[0].success = false;
    assert!(hotspots(&c, &saved, 90, "", "", "")["rows"][0]["priority"].is_null());
}
#[test]
fn shallow_history_is_disclosed_and_missing_git_is_an_error() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path().join("repo");
    fs::create_dir(&r).unwrap();
    git(&r, &["init", "-b", "main"]);
    fs::write(r.join("a.hpp"), "one").unwrap();
    commit(&r);
    fs::write(r.join("a.hpp"), "two").unwrap();
    commit(&r);
    let dest = t.path().join("shallow");
    git(
        t.path(),
        &[
            "clone",
            "--depth=1",
            &format!("file://{}", r.display()),
            dest.to_str().unwrap(),
        ],
    );
    let c = header_capture(&dest, "a.hpp");
    let saved = collect(&c, 90).unwrap();
    assert!(!saved.complete);
    assert_eq!(saved.files[0].edits.len(), 1);
    assert!(saved.notes.iter().any(|n| n.contains("shallow")));
    let c = capture_fixture(t.path());
    assert!(collect(&c, 90).is_err());
}

#[test]
fn store_cli_cache_pages_and_offline_archives_preserve_saved_evidence() {
    use buildscope::store::{HeaderHotspotQuery, Store};
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    for name in ["a.hpp", "b.hpp", "c.hpp"] {
        fs::write(repo.join(name), "int field;\n").unwrap();
    }
    commit(&repo);
    let mut capture = header_capture(&repo, "a.hpp");
    for key in ["b.hpp", "c.hpp"] {
        let mut header = capture.headers[0].clone();
        header.key = key.into();
        header.path = path_string(repo.join(key));
        capture.headers.push(header);
    }
    let root = temp.path().join("store");
    let store = Store::open(&root).unwrap();
    store.save(&capture).unwrap();
    assert!(
        !store
            .header_hotspots(&capture.id, HeaderHotspotQuery::default())
            .unwrap()["available"]
            .as_bool()
            .unwrap()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_buildscope"))
        .args([
            "--store",
            root.to_str().unwrap(),
            "analyze-history",
            &capture.id,
            "--days",
            "90",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved: buildscope::header_history::HistorySnapshot =
        serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(saved.files.len(), 3);
    let artifact = store
        .run_dir(&capture.id)
        .unwrap()
        .join("header-history.json");
    let before = fs::read(&artifact).unwrap();
    fs::write(repo.join("a.hpp"), "int later;\n").unwrap();
    commit(&repo);
    assert_eq!(collect(&capture, 90).unwrap().files[0].edits.len(), 1);
    assert_eq!(
        store.analyze_history(&capture.id, 30).unwrap().analyzed_ms,
        saved.analyzed_ms
    );
    assert_eq!(fs::read(&artifact).unwrap(), before);
    let page = store
        .header_hotspots(
            &capture.id,
            HeaderHotspotQuery {
                offset: 1,
                limit: 1,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(page["total"], 3);
    assert_eq!(page["rows"].as_array().unwrap().len(), 1);
    assert_eq!(page["rows"][0]["key"], "b.hpp");
    assert_eq!(page["rows"][0]["edits"].as_array().unwrap().len(), 0);
    let longer = store
        .header_hotspots(
            &capture.id,
            HeaderHotspotQuery {
                days: 180,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(longer["window_truncated"], true);
    assert_eq!(longer["complete"], false);
    let archive = temp.path().join("capture.buildscope");
    store.export(&capture.id, &archive, false).unwrap();
    // Remove Git entirely: all report reads must continue to consume saved artifacts.
    fs::remove_dir_all(&repo).unwrap();
    let imported = Store::open(&temp.path().join("imported")).unwrap();
    let imported_id = imported.import(&archive).unwrap();
    let offline = imported
        .header_hotspots(
            &imported_id,
            HeaderHotspotQuery {
                offset: 1,
                limit: 1,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(offline["rows"], page["rows"]);
    assert!(imported.analyze_history(&imported_id, 90).is_err());
    assert_eq!(
        store.analyze_history(&capture.id, 90).unwrap().anchor,
        saved.anchor
    );
}
