//! Dependency inventory resolution preserves path identity while reusing repeated spellings.
use buildscope::{project::parse_dependencies, util::path_string};
use std::{fs, os::unix::fs::symlink, time::Instant};

#[test]
fn repeated_dependency_paths_keep_canonical_identity_and_sorted_unique_results() {
    let temp = tempfile::tempdir().unwrap();
    let build = temp.path().join("build tree");
    let headers = temp.path().join("source headers");
    fs::create_dir(&build).unwrap();
    fs::create_dir(&headers).unwrap();
    fs::write(headers.join("a header.hpp"), "").unwrap();
    fs::write(headers.join("z.hpp"), "").unwrap();
    fs::write(build.join("actual.o"), "").unwrap();
    symlink(&headers, build.join("includes")).unwrap();
    symlink(build.join("actual.o"), build.join("output.o")).unwrap();
    let absolute = headers.join("a header.hpp");
    let text = format!(
        "output.o: #deps 6, deps mtime 1 (VALID)\n    includes/z.hpp\n    includes/a header.hpp\n    {}\n    ./includes/a header.hpp\n    generated/../missing.hpp\n    includes/z.hpp\n\nsecond.o: #deps 2, deps mtime 1 (VALID)\n    includes/a header.hpp\n    includes/z.hpp\n\nstale.o: #deps 1, deps mtime 1 (STALE)\n    stale.hpp\n",
        absolute.display()
    );
    let dependencies = parse_dependencies(&text, &build);
    let mut expected = vec![
        path_string(absolute.canonicalize().unwrap()),
        path_string(headers.join("z.hpp").canonicalize().unwrap()),
        path_string(build.join("missing.hpp")),
    ];
    expected.sort();
    assert_eq!(dependencies.len(), 2);
    assert_eq!(
        dependencies[&path_string(build.join("actual.o").canonicalize().unwrap())],
        expected
    );
    assert_eq!(
        dependencies[&path_string(build.join("second.o"))],
        expected
            .iter()
            .filter(|path| !path.ends_with("missing.hpp"))
            .cloned()
            .collect::<Vec<_>>()
    );
}

#[test]
fn dependency_resolution_is_refreshed_between_inventories() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("first.hpp"), "").unwrap();
    fs::write(temp.path().join("second.hpp"), "").unwrap();
    let alias = temp.path().join("shared.hpp");
    let text = "unit.o: #deps 2, deps mtime 1 (VALID)\n    shared.hpp\n    shared.hpp\n";
    for target in ["first.hpp", "second.hpp"] {
        if alias.exists() {
            fs::remove_file(&alias).unwrap();
        }
        symlink(target, &alias).unwrap();
        let dependencies = parse_dependencies(text, temp.path());
        assert_eq!(
            dependencies.values().next().unwrap(),
            &[path_string(
                temp.path().join(target).canonicalize().unwrap()
            )]
        );
    }
}

#[test]
#[ignore = "Synthetic repeated-header performance benchmark; run with --release --ignored --nocapture"]
fn repeated_header_inventory_benchmark() {
    let temp = tempfile::tempdir().unwrap();
    let headers = temp.path().join("sdk/include/project/detail");
    fs::create_dir_all(&headers).unwrap();
    for index in 0..256 {
        fs::write(headers.join(format!("header-{index}.hpp")), "").unwrap();
    }
    let mut text = String::new();
    for unit in 0..512 {
        text.push_str(&format!("unit-{unit}.o: #deps 256, deps mtime 1 (VALID)\n"));
        for index in (0..256).rev() {
            text.push_str(&format!(
                "    sdk/include/project/detail/header-{index}.hpp\n"
            ));
        }
        text.push('\n');
    }
    let mut times = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        let dependencies = std::hint::black_box(parse_dependencies(&text, temp.path()));
        times.push(started.elapsed());
        assert_eq!(dependencies.len(), 512);
        assert!(dependencies.values().all(|paths| paths.len() == 256));
    }
    times.sort();
    eprintln!(
        "dependency_paths=131072 unique_headers=256 parse_times={times:?} median={:?}",
        times[1]
    );
}

#[test]
#[ignore = "Set BUILDSCOPE_DEPS_BENCH_TEXT and BUILDSCOPE_DEPS_BENCH_BUILD to a retained Ninja deps dump and its build tree"]
fn retained_dependency_inventory_benchmark() {
    let text = fs::read_to_string(std::env::var("BUILDSCOPE_DEPS_BENCH_TEXT").unwrap()).unwrap();
    let build = std::path::PathBuf::from(std::env::var("BUILDSCOPE_DEPS_BENCH_BUILD").unwrap());
    for _ in 0..3 {
        let started = Instant::now();
        let dependencies = std::hint::black_box(parse_dependencies(&text, &build));
        let elapsed = started.elapsed();
        let count: usize = dependencies.values().map(Vec::len).sum();
        eprintln!(
            "outputs={} dependency_paths={count} parse={elapsed:?}",
            dependencies.len()
        );
    }
}
