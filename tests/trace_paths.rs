//! Trace-local path reuse preserves native scope locations and refreshes between decodes.
use buildscope::{compiler::read_clang_trace, util::path_string};
use serde_json::json;
use std::{fs, os::unix::fs::symlink};

#[test]
fn repeated_trace_paths_preserve_locations_and_complete_and_paired_scopes() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source tree");
    let headers = temp.path().join("actual headers");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&headers).unwrap();
    let header = headers.join("a header.hpp");
    fs::write(&header, "").unwrap();
    symlink(&headers, source.join("includes")).unwrap();
    let trace = temp.path().join("trace.json");
    fs::write(
        &trace,
        json!({"traceEvents":[
            {"name":"Source","ph":"X","ts":0,"dur":100,"pid":1,"tid":1,
             "args":{"detail":"includes/a header.hpp","file":"includes/a header.hpp","line":12}},
            {"name":"InstantiateFunction","ph":"B","ts":10,"pid":1,"tid":1,
             "args":{"detail":"render<int>","file":"includes/a header.hpp","line":13}},
            {"ph":"E","ts":20,"pid":1,"tid":1},
            {"name":"Source","cat":"Source","id":"header","ph":"b","ts":30,"pid":1,"tid":1,
             "args":{"detail":"includes/a header.hpp","file":header}},
            {"name":"Source","cat":"Source","id":"header","ph":"e","ts":50,"pid":1,"tid":1},
            {"name":"Source","ph":"X","ts":60,"dur":5,"pid":1,"tid":1,
             "args":{"detail":"<built-in>"}},
            {"name":"Source","ph":"X","ts":80,"dur":5,"pid":1,"tid":1,
             "args":{"detail":"missing/../future.hpp","file":"missing/../future.hpp"}},
            {"name":"InstantiateClass","ph":"X","ts":90,"dur":5,"pid":1,"tid":1,
             "args":{"detail":"Template<args>","file":"<built-in>"}}
        ]})
        .to_string(),
    )
    .unwrap();
    let (events, phases) = read_clang_trace(&trace, &source).unwrap();
    assert!(phases.is_empty());
    assert_eq!(events.len(), 6);
    let expected = path_string(header.canonicalize().unwrap());
    assert_eq!(events[0].detail, expected);
    assert_eq!(events[0].file.as_deref(), Some(expected.as_str()));
    assert_eq!(events[0].line, Some(12));
    assert_eq!(events[1].detail, "render<int>");
    assert_eq!(events[1].file.as_deref(), Some(expected.as_str()));
    assert_eq!(events[1].duration_us, 10.0);
    assert_eq!(events[1].line, Some(13));
    assert_eq!(events[2].detail, expected);
    assert_eq!(events[2].file.as_deref(), Some(expected.as_str()));
    assert_eq!(events[2].duration_us, 20.0);
    assert_eq!(events[3].detail, "<built-in>");
    assert_eq!(events[3].file, None);
    let missing = path_string(source.join("future.hpp"));
    assert_eq!(events[4].detail, missing);
    assert_eq!(events[4].file.as_deref(), Some(missing.as_str()));
    assert_eq!(events[5].detail, "Template<args>");
    assert_eq!(
        events[5].file.as_deref(),
        Some(path_string(source.join("<built-in>")).as_str())
    );
    for (id, event) in events.iter().enumerate() {
        assert_eq!(event.id, id);
    }
    for event in &events[1..] {
        assert_eq!(event.parent, Some(0));
        assert_eq!(event.depth, 1);
    }
}

#[test]
fn trace_path_resolution_is_refreshed_for_new_traces_and_directories() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("first.hpp"), "").unwrap();
    fs::write(temp.path().join("second.hpp"), "").unwrap();
    let trace = temp.path().join("trace.json");
    fs::write(
        &trace,
        json!({"traceEvents":[
            {"name":"Source","ph":"X","ts":0,"dur":10,
             "args":{"detail":"shared.hpp","file":"shared.hpp"}}
        ]})
        .to_string(),
    )
    .unwrap();
    let alias = temp.path().join("shared.hpp");
    for target in ["first.hpp", "second.hpp"] {
        if alias.exists() {
            fs::remove_file(&alias).unwrap();
        }
        symlink(target, &alias).unwrap();
        let (events, _) = read_clang_trace(&trace, temp.path()).unwrap();
        let expected = path_string(temp.path().join(target).canonicalize().unwrap());
        assert_eq!(events[0].detail, expected);
        assert_eq!(events[0].file.as_deref(), Some(expected.as_str()));
    }
    let other = temp.path().join("other directory");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("shared.hpp"), "").unwrap();
    let (events, _) = read_clang_trace(&trace, &other).unwrap();
    let expected = path_string(other.join("shared.hpp").canonicalize().unwrap());
    assert_eq!(events[0].detail, expected);
    assert_eq!(events[0].file.as_deref(), Some(expected.as_str()));
}
