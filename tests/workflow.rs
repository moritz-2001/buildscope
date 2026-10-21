//! Real command-line workflows against temporary CMake/Ninja projects. No tests
//! edit the checked-in fixture, the editor application, or a user's build outputs.
use buildscope::{
    capture, compiler,
    model::{Capture, Invocation},
    store::{EventQuery, Store},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

#[test]
fn clean_noop_incremental_generator_and_failed_builds_have_distinct_evidence() {
    let Some(compiler) = clang() else {
        eprintln!("Skipping real Clang build: clang++ is unavailable");
        return;
    };
    let fixture = Fixture::new(&compiler, &[]);
    let first = fixture.run(&["record", "--label", "clean"]);
    assert!(first.status.success(), "{}", diagnostics(&first));
    let clean = fixture.latest();
    assert_eq!(clean.status, "success");
    assert_eq!(clean.summary.observed_units, 3);
    let workload = clean.workload.as_ref().unwrap();
    assert_eq!(
        (workload.compiled, workload.cache_hits, workload.unknown),
        (3, 0, 0)
    );
    assert_eq!(workload.up_to_date, Some(0));
    assert_eq!(workload.total_units, Some(3));
    assert_eq!(clean.actions.len(), 5);
    assert!(clean.actions.iter().any(|a| a.kind == "link"));
    let shared = clean
        .headers
        .iter()
        .find(|h| h.key == "shared.hpp")
        .unwrap();
    assert_eq!(shared.units.len(), 3);
    assert!(shared.processing_ms.is_none());
    assert!(
        !shared.changed_contributor,
        "newly discovered existing headers are not newly created inputs"
    );
    assert_eq!(clean.summary.dependency_units, 3);
    let noop = fixture.run(&["record", "--label", "noop"]);
    assert!(noop.status.success(), "{}", diagnostics(&noop));
    let noop = fixture.latest();
    assert!(noop.actions.is_empty());
    assert_eq!(noop.workload.as_ref().unwrap().up_to_date, Some(3));
    assert_eq!(
        noop.project.configuration_id,
        clean.project.configuration_id
    );
    assert!(
        noop.headers
            .iter()
            .all(|h| !h.explicit_trigger && !h.changed_contributor)
    );
    append(
        &fixture.source.join("shared.hpp"),
        "\n// intentional real content edit\n",
    );
    let changed = fixture.run(&["record", "--label", "header-edit"]);
    assert!(changed.status.success(), "{}", diagnostics(&changed));
    let changed = fixture.latest();
    assert_eq!(changed.summary.observed_units, 3);
    let header = changed
        .headers
        .iter()
        .find(|h| h.key == "shared.hpp")
        .unwrap();
    assert!(header.changed_contributor);
    assert!(header.explicit_trigger, "{:?}", header.evidence);
    assert_eq!(header.rebuilt_units, 3);
    assert!(
        changed
            .changes
            .iter()
            .any(|c| c.path.ends_with("shared.hpp") && c.kind == "content")
    );
    fs::write(fixture.source.join("generator_input.txt"), "19\n").unwrap();
    let generator = fixture.run(&["record", "--label", "generator-edit"]);
    assert!(generator.status.success(), "{}", diagnostics(&generator));
    let generated = fixture.latest();
    assert_eq!(generated.summary.observed_units, 1);
    assert_eq!(generated.workload.as_ref().unwrap().compiled, 1);
    assert_eq!(generated.workload.as_ref().unwrap().up_to_date, Some(2));
    assert!(generated.actions.iter().any(|a| a.kind == "generate"));
    assert!(
        generated
            .changes
            .iter()
            .any(|c| c.path.ends_with("config.hpp") && c.during_build && c.kind == "content")
    );
    append(
        &fixture.source.join("second.cpp"),
        "\nthis is not valid C++;\n",
    );
    let failed = fixture.run(&["record", "--label", "compiler-error"]);
    assert!(!failed.status.success());
    let failed_run = fixture.latest();
    assert_eq!(failed_run.status, "failed");
    assert_eq!(failed_run.workload.as_ref().unwrap().up_to_date, None);
    assert!(
        failed_run
            .notes
            .iter()
            .any(|n| n.contains("failed or interrupted actions"))
    );
    assert!(diagnostics(&failed).contains("second.cpp"));
    let history = fixture
        .store()
        .header_history(&changed, "shared.hpp")
        .unwrap();
    assert!(history.iter().any(|h| h["explicit_trigger"] == true));
}

#[test]
fn ccache_outcomes_are_build_local_and_survive_archives() {
    let Some(compiler) = clang() else {
        return;
    };
    let ccache = std::env::var("BUILDSCOPE_TEST_CCACHE").unwrap_or_else(|_| "ccache".into());
    if !Command::new(&ccache)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!("Skipping real ccache workflow: ccache is unavailable");
        return;
    }
    let fixture = Fixture::new(
        &compiler,
        &[&format!("-DCMAKE_CXX_COMPILER_LAUNCHER={ccache}")],
    );
    let shared_log = fixture.root.path().join("unrelated.stats");
    fs::write(&shared_log, "# unrelated.cpp\ndirect_cache_hit\n").unwrap();
    let cache = fixture.root.path().join("ccache");
    let record = |prefix: Option<&Path>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_buildscope"));
        command
            .arg("--store")
            .arg(&fixture.store_path)
            .args(["record", "--build-dir"])
            .arg(&fixture.build)
            .args(["--target", "app", "--jobs", "4"])
            .env("CCACHE_DIR", &cache)
            .env("CCACHE_STATSLOG", &shared_log)
            .env_remove("CCACHE_DISABLE")
            .env_remove("CCACHE_RECACHE")
            .env_remove("CCACHE_PREFIX");
        if let Some(prefix) = prefix {
            command.env("CCACHE_PREFIX", prefix);
        }
        command.output().unwrap()
    };
    let first = record(None);
    assert!(first.status.success(), "{}", diagnostics(&first));
    let miss = fixture.latest();
    let counts = miss.workload.as_ref().unwrap();
    assert_eq!(
        (counts.compiled, counts.cache_hits, counts.unknown),
        (3, 0, 0),
        "{counts:?}"
    );
    assert_eq!(miss.jobs, Some(4));
    for unit in &miss.project.units {
        fs::remove_file(&unit.output).unwrap();
    }
    let hit = record(None);
    assert!(hit.status.success(), "{}", diagnostics(&hit));
    let hit = fixture.latest();
    let counts = hit.workload.as_ref().unwrap();
    assert_eq!(
        (counts.compiled, counts.cache_hits, counts.unknown),
        (0, 3, 0),
        "{counts:?}"
    );
    assert_eq!(counts.up_to_date, Some(0));
    assert_eq!(
        fs::read_to_string(&shared_log).unwrap(),
        "# unrelated.cpp\ndirect_cache_hit\n"
    );
    let noop = record(None);
    assert!(noop.status.success(), "{}", diagnostics(&noop));
    let noop = fixture.latest().workload.unwrap();
    assert_eq!(
        (
            noop.compiled,
            noop.cache_hits,
            noop.unknown,
            noop.up_to_date
        ),
        (0, 0, 0, Some(3))
    );
    let archive = fixture.root.path().join("cached.buildscope");
    fixture.store().export(&hit.id, &archive, true).unwrap();
    let imported = Store::open(&fixture.root.path().join("imported")).unwrap();
    imported.import(&archive).unwrap();
    assert_eq!(
        imported.load(&hit.id).unwrap().workload.unwrap().cache_hits,
        3
    );
    assert_eq!(imported.list().unwrap()[0]["workload"]["cache_hits"], 3);
    // Retained logs, rather than cache counters or durations, govern recovery too.
    let stats_path = fixture
        .store()
        .run_dir(&hit.id)
        .unwrap()
        .join("ccache.stats");
    append(
        &stats_path,
        &format!("# {}\ndirect_cache_hit\n", hit.project.units[0].source),
    );
    let mut interrupted = hit.clone();
    interrupted.status = "interrupted".into();
    fixture.store().save(&interrupted).unwrap();
    let recovered = capture::recover(&fixture.store(), &hit.id).unwrap();
    let partial = recovered.workload.unwrap();
    assert_eq!(
        (partial.cache_hits, partial.unknown, partial.up_to_date),
        (2, 1, None)
    );
    fs::remove_file(stats_path).unwrap();
    let recovered = capture::recover(&fixture.store(), &hit.id).unwrap();
    let missing = recovered.workload.unwrap();
    assert_eq!(
        (missing.compiled, missing.cache_hits, missing.unknown),
        (0, 0, 3)
    );
    let prefix = fixture.root.path().join("compiler-prefix");
    fs::write(&prefix, "#!/bin/sh\nexec \"$@\"\n").unwrap();
    fs::set_permissions(&prefix, fs::Permissions::from_mode(0o755)).unwrap();
    append(
        &fixture.source.join("shared.hpp"),
        "\ninline int prefix_probe() { return 421; }\n",
    );
    let output = record(Some(&prefix));
    assert!(output.status.success(), "{}", diagnostics(&output));
    let prefixed = fixture.latest().workload.unwrap();
    assert_eq!(
        (prefixed.compiled, prefixed.cache_hits, prefixed.unknown),
        (0, 0, 3)
    );
}

#[test]
fn ccache_launcher_reuses_existing_hits_and_profiles_only_misses() {
    let Some(ccache) = ccache() else { return };
    for compiler in [clang(), gcc()].into_iter().flatten() {
        let cached_launcher = format!("{ccache};namespace=buildscope-launcher-test");
        let fixture = Fixture::new(
            &compiler,
            &[
                &format!("-DCMAKE_CXX_COMPILER_LAUNCHER={cached_launcher}"),
                "-DCMAKE_NINJA_FORCE_RESPONSE_FILE=ON",
            ],
        );
        let config = fixture.root.path().join("ccache.conf");
        fs::write(&config, "max_size = 20M\n").unwrap();
        let stats = fixture.root.path().join("ccache.stats");
        let build = || {
            let mut command = ccache_build(&fixture, &config, &stats);
            command.output().unwrap()
        };
        let output = build();
        assert!(output.status.success(), "{}", diagnostics(&output));
        assert_cache_results(&stats, 3, 0);

        // Exercise executable paths with spaces, explicit build roots, full detail,
        // inline ccache settings, response files, and Ninja compdb discovery.
        let tools = fixture.root.path().join("launcher tools");
        fs::create_dir(&tools).unwrap();
        let binary = tools.join("buildscope");
        fs::copy(env!("CARGO_BIN_EXE_buildscope"), &binary).unwrap();
        fixture.configure(&compiler, &[
            &format!("-DCMAKE_CXX_COMPILER_LAUNCHER={};launch;--detail;full;--build-dir;{};--;{cached_launcher}", binary.display(), fixture.build.display()),
            "-DCMAKE_NINJA_FORCE_RESPONSE_FILE=ON",
        ]);
        fs::remove_file(fixture.build.join("compile_commands.json")).unwrap();
        let project = buildscope::project::discover(&fixture.build, &[]).unwrap();
        assert!(project.compilers.iter().all(|c| c.family != "unknown"));
        let invalidate = || {
            for unit in &project.units {
                fs::remove_file(&unit.output).unwrap();
            }
            fs::write(&stats, "").unwrap();
        };
        let pending = fixture.build.join(".buildscope-launcher/pending");
        let pending_count = || {
            fs::read_dir(&pending)
                .map(|entries| entries.count())
                .unwrap_or(0)
        };

        invalidate();
        let output = build();
        assert!(output.status.success(), "{}", diagnostics(&output));
        assert_cache_results(&stats, 0, 3);
        assert_eq!(
            pending_count(),
            0,
            "hits must not create compiler observations"
        );
        assert!(collect_launcher(&fixture, "warm-cache").status.success());
        assert!(fixture.store().list().unwrap().is_empty());

        invalidate();
        let output = ccache_build(&fixture, &config, &stats)
            .env("CCACHE_NODIRECT", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", diagnostics(&output));
        assert_cache_results(&stats, 0, 3);
        assert_eq!(
            fs::read_to_string(&stats)
                .unwrap()
                .lines()
                .filter(|line| *line == "preprocessed_cache_hit")
                .count(),
            3
        );
        assert_eq!(pending_count(), 0);

        append(
            &fixture.source.join("first.cpp"),
            "\nint cache_miss_probe() { return 421; }\n",
        );
        invalidate();
        let output = build();
        assert!(output.status.success(), "{}", diagnostics(&output));
        assert_cache_results(&stats, 1, 2);
        assert_eq!(pending_count(), 1);
        let collected = collect_launcher(&fixture, "mixed-cache");
        assert!(collected.status.success(), "{}", diagnostics(&collected));
        let run = fixture.latest();
        assert_eq!(run.actions.len(), 1);
        assert_eq!(run.profiles.len(), 1, "{:?}", run.notes);
        assert_eq!(run.status, "success");
        let profile = &run.profiles[0];
        assert!(
            run.project
                .units
                .iter()
                .find(|u| u.id == profile.unit_id)
                .unwrap()
                .source
                .ends_with("first.cpp")
        );
        if profile.family == "clang" {
            assert!(profile.event_count > 0);
            assert_eq!(profile.granularity_us, Some(0));
            assert!(
                run.headers
                    .iter()
                    .any(|h| h.key == "shared.hpp" && h.processing_ms.is_some())
            );
        } else {
            assert_eq!(profile.family, "gcc");
            assert!(!profile.phases.is_empty());
        }

        // The trace spool has been collected and deleted. A hit must still avoid
        // invoking Buildscope, rather than replaying stale traces as fresh work.
        invalidate();
        let output = build();
        assert!(output.status.success(), "{}", diagnostics(&output));
        assert_cache_results(&stats, 0, 3);
        assert_eq!(pending_count(), 0);
        assert!(collect_launcher(&fixture, "all-hits").status.success());
        assert_eq!(fixture.store().list().unwrap().len(), 1);

        invalidate();
        let mut record = Command::new(env!("CARGO_BIN_EXE_buildscope"));
        record
            .arg("--store")
            .arg(&fixture.store_path)
            .args(["record", "--build-dir"])
            .arg(&fixture.build)
            .args(["--target", "app", "--jobs", "4"]);
        ccache_environment(&mut record, &fixture, &config, &stats);
        let output = record.output().unwrap();
        assert!(output.status.success(), "{}", diagnostics(&output));
        let workload = fixture.latest().workload.unwrap();
        assert_eq!(
            (workload.compiled, workload.cache_hits, workload.unknown),
            (0, 3, 0)
        );
        assert_eq!(pending_count(), 0);

        // Cache bypass still executes and profiles the native compiler.
        invalidate();
        let output = ccache_build(&fixture, &config, &stats)
            .env("CCACHE_DISABLE", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", diagnostics(&output));
        assert_eq!(pending_count(), 3);
        assert!(
            collect_launcher(&fixture, "cache-disabled")
                .status
                .success()
        );
        assert_eq!(fixture.latest().profiles.len(), 3);

        append(&fixture.source.join("first.cpp"), "\nnot valid C++;\n");
        let output = build();
        assert!(!output.status.success());
        assert!(diagnostics(&output).contains("first.cpp"));
        assert_eq!(pending_count(), 1);
        assert!(
            collect_launcher(&fixture, "cache-compiler-failure")
                .status
                .success()
        );
        let failed = fixture.latest();
        assert_eq!(failed.status, "failed");
        assert!(failed.actions.iter().any(|action| !action.success));
    }
}

#[test]
fn ccache_launcher_preserves_environment_config_and_inline_prefixes() {
    let (Some(ccache), Some(compiler)) = (ccache(), clang()) else {
        return;
    };
    for mode in ["environment", "config", "inline"] {
        let fixture = Fixture::new(&compiler, &[]);
        let prefix = fixture.root.path().join("compiler-prefix");
        fs::write(&prefix, "#!/bin/sh\ntest \"$1\" = prefix-argument || exit 91\nshift\ntest -z \"${BUILDSCOPE_CCACHE_CONTEXT+x}\" || exit 92\nprintf 'called\\n' >> \"$PREFIX_LOG\"\nexec \"$@\"\n").unwrap();
        fs::set_permissions(&prefix, fs::Permissions::from_mode(0o755)).unwrap();
        let prefix = format!("{} prefix-argument", prefix.display());
        let config = fixture.root.path().join("ccache.conf");
        let mut configuration = "max_size = 20M\n".to_owned();
        if mode == "config" {
            configuration.push_str(&format!("prefix_command = {prefix}\n"));
        }
        fs::write(&config, configuration).unwrap();
        let inline = if mode == "inline" {
            format!(";prefix_command={prefix}")
        } else {
            String::new()
        };
        fixture.configure(
            &compiler,
            &[&format!(
                "-DCMAKE_CXX_COMPILER_LAUNCHER={};launch;--;{ccache}{inline}",
                env!("CARGO_BIN_EXE_buildscope")
            )],
        );
        let stats = fixture.root.path().join("ccache.stats");
        let marker = fixture.root.path().join("prefix.log");
        let build = || {
            let mut command = ccache_build(&fixture, &config, &stats);
            command.env("PREFIX_LOG", &marker);
            if mode == "environment" {
                command.env("CCACHE_PREFIX", &prefix);
            }
            command.output().unwrap()
        };
        let output = build();
        assert!(output.status.success(), "{mode}: {}", diagnostics(&output));
        assert_cache_results(&stats, 3, 0);
        assert_eq!(
            fs::read_to_string(&marker).unwrap().lines().count(),
            3,
            "{mode}"
        );
        assert!(collect_launcher(&fixture, mode).status.success());
        let run = fixture.latest();
        assert_eq!(run.profiles.len(), 3, "{mode}: {:?}", run.notes);
        assert!(run.profiles.iter().all(|p| p.event_count > 0));
        let manifest = fs::read_dir(
            fixture
                .store()
                .run_dir(&run.id)
                .unwrap()
                .join("invocations"),
        )
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
        let invocation: Invocation = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
        assert!(invocation.arguments[0].ends_with("compiler-prefix"));
        assert_eq!(invocation.arguments[1], "prefix-argument");
        for unit in &run.project.units {
            fs::remove_file(&unit.output).unwrap();
        }
        fs::write(&stats, "").unwrap();
        fs::write(&marker, "").unwrap();
        let output = build();
        assert!(output.status.success(), "{mode}: {}", diagnostics(&output));
        assert_cache_results(&stats, 0, 3);
        assert!(fs::read_to_string(&marker).unwrap().is_empty());
    }
}

fn ccache() -> Option<String> {
    let executable = std::env::var("BUILDSCOPE_TEST_CCACHE").unwrap_or_else(|_| "ccache".into());
    if Command::new(&executable)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        Some(executable)
    } else {
        eprintln!("Skipping real ccache launcher workflow: ccache is unavailable");
        None
    }
}

fn ccache_build(fixture: &Fixture, config: &Path, stats: &Path) -> Command {
    let mut command = Command::new("cmake");
    command
        .arg("--build")
        .arg(&fixture.build)
        .args(["--parallel", "4"]);
    ccache_environment(&mut command, fixture, config, stats);
    command
}

fn ccache_environment(command: &mut Command, fixture: &Fixture, config: &Path, stats: &Path) {
    command
        .env("CCACHE_DIR", fixture.root.path().join("ccache"))
        .env("CCACHE_CONFIGPATH", config)
        .env("CCACHE_STATSLOG", stats)
        .env_remove("CCACHE_DISABLE")
        .env_remove("CCACHE_RECACHE")
        .env_remove("CCACHE_PREFIX")
        .env_remove("CCACHE_PREFIX_CPP");
}

fn assert_cache_results(path: &Path, misses: usize, hits: usize) {
    let stats = fs::read_to_string(path).unwrap();
    assert_eq!(
        stats.lines().filter(|line| *line == "cache_miss").count(),
        misses,
        "{stats}"
    );
    assert_eq!(
        stats
            .lines()
            .filter(|line| matches!(*line, "direct_cache_hit" | "preprocessed_cache_hit"))
            .count(),
        hits,
        "{stats}"
    );
}

#[test]
fn workload_scope_excludes_unrequested_compilation_database_entries() {
    let Some(compiler) = clang() else {
        return;
    };
    let fixture = Fixture::new(&compiler, &[]);
    fs::write(
        fixture.source.join("unused.cpp"),
        "int unused() { return 0; }\n",
    )
    .unwrap();
    append(
        &fixture.source.join("CMakeLists.txt"),
        "\nadd_library(unrequested OBJECT EXCLUDE_FROM_ALL unused.cpp)\n",
    );
    fixture.configure(&compiler, &[]);
    let output = fixture.run(&["record"]);
    assert!(output.status.success(), "{}", diagnostics(&output));
    let clean = fixture.latest();
    assert_eq!(clean.project.units.len(), 4);
    let workload = clean.workload.unwrap();
    assert_eq!(
        (workload.total_units, workload.up_to_date, workload.compiled),
        (Some(3), Some(0), 3)
    );
    let output = fixture.run(&["record"]);
    assert!(output.status.success(), "{}", diagnostics(&output));
    assert_eq!(fixture.latest().workload.unwrap().up_to_date, Some(3));
}

#[test]
fn deep_clang_profiles_are_isolated_searchable_and_portable() {
    let Some(compiler) = clang() else {
        eprintln!("Skipping real Clang profile: clang++ is unavailable");
        return;
    };
    let fixture = Fixture::new(&compiler, &[]);
    let baseline = fixture.run(&["record"]);
    assert!(baseline.status.success(), "{}", diagnostics(&baseline));
    let original_log = fs::read(fixture.build.join(".ninja_log")).unwrap();
    let profile = fixture.run(&["profile", "--detail", "full", "--label", "full-clang"]);
    assert!(profile.status.success(), "{}", diagnostics(&profile));
    let run = fixture.latest();
    assert_eq!(run.profiles.len(), 3, "{:?}", run.notes);
    assert_eq!(
        fs::read(fixture.build.join(".ninja_log")).unwrap(),
        original_log
    );
    assert_ne!(Path::new(&run.project.build_dir), fixture.build);
    assert!(
        run.profile_origin
            .as_ref()
            .is_some_and(|p| Path::new(p) == fixture.build.canonicalize().unwrap())
    );
    assert!(
        run.profiles
            .iter()
            .all(|p| p.family == "clang" && p.event_count > 0 && p.granularity_us == Some(0))
    );
    assert!(
        run.profiles
            .iter()
            .any(|p| !p.templates.is_empty() && !p.functions.is_empty())
    );
    assert!(run.profiles.iter().all(|p| {
        p.phases
            .iter()
            .any(|m| m.name == "Preprocessed output size" && m.value > 0.0)
    }));
    let shared = run.headers.iter().find(|h| h.key == "shared.hpp").unwrap();
    assert_eq!(shared.measured_units, 3);
    assert!(shared.processing_ms.unwrap() > 0.0);
    let paths = fixture
        .store()
        .include_paths(&run.id, &shared.path)
        .unwrap();
    assert!(paths["total"].as_i64().unwrap() > 0);
    let unit = &run.project.units[0];
    let events = fixture
        .store()
        .events(
            &run.id,
            &unit.id,
            EventQuery {
                activity: "InstantiateFunction",
                limit: 10,
                ..EventQuery::default()
            },
        )
        .unwrap();
    assert!(events["total"].as_i64().unwrap() > 0);
    assert!(events["events"].as_array().unwrap().len() <= 10);
    let overview = fixture.store().overview(&run.id).unwrap();
    assert!(
        overview["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["templates"].as_array().unwrap().is_empty())
    );
    let archive = fixture.root.path().join("portable.buildscope");
    fixture.store().export(&run.id, &archive, true).unwrap();
    let imported = Store::open(&fixture.root.path().join("imported")).unwrap();
    let id = imported.import(&archive).unwrap();
    assert_eq!(imported.load(&id).unwrap().summary.observed_units, 3);
    assert_eq!(imported.load(&id).unwrap().workload.unwrap().compiled, 3);
    assert_eq!(
        imported
            .events(
                &id,
                &unit.id,
                EventQuery {
                    activity: "InstantiateFunction",
                    limit: 10,
                    ..EventQuery::default()
                }
            )
            .unwrap()["total"],
        events["total"]
    );
    let focused = fixture.run(&["profile", "--unit", &unit.id, "--detail", "full"]);
    assert!(focused.status.success(), "{}", diagnostics(&focused));
    let focused = fixture.latest();
    assert_eq!(focused.summary.observed_units, 1);
    assert_eq!(focused.profiles.len(), 1);
    assert_eq!(focused.profiles[0].unit_id, unit.id);
    assert!(focused.notes.iter().any(|n| n.contains("covers 1 of 3")));
}

#[test]
fn gcc_profiles_supply_aggregate_timing_without_invented_header_costs() {
    let Some(compiler) = gcc() else {
        eprintln!("Skipping real GCC profile: GNU g++ is unavailable");
        return;
    };
    let fixture = Fixture::new(&compiler, &[]);
    let result = fixture.run(&["profile", "--label", "gcc-aggregate"]);
    assert!(result.status.success(), "{}", diagnostics(&result));
    let run = fixture.latest();
    assert_eq!(run.profiles.len(), 3, "{:?}", run.notes);
    for profile in &run.profiles {
        assert_eq!(profile.family, "gcc");
        assert_eq!(profile.event_count, 0);
        assert!(
            profile.templates.is_empty()
                && profile.functions.is_empty()
                && profile.header_costs.is_empty()
        );
        assert!(
            profile
                .phases
                .iter()
                .any(|p| p.name == "phase parsing" && p.value > 0.0),
            "{:?}",
            profile.phases
        );
        assert!(!profile.includes.is_empty());
    }
    let shared = run.headers.iter().find(|h| h.key == "shared.hpp").unwrap();
    assert_eq!(shared.units.len(), 3);
    assert!(shared.processing_ms.is_none());
    assert_eq!(shared.measured_units, 0);
}

#[test]
fn response_files_and_compilation_database_fallback_work_in_real_builds() {
    let Some(compiler) = clang() else {
        eprintln!("Skipping response-file fixture: clang++ is unavailable");
        return;
    };
    let fixture = Fixture::new(&compiler, &["-DCMAKE_NINJA_FORCE_RESPONSE_FILE=ON"]);
    fs::remove_file(fixture.build.join("compile_commands.json")).unwrap();
    let recorded = fixture.run(&["record"]);
    assert!(recorded.status.success(), "{}", diagnostics(&recorded));
    let run = fixture.latest();
    assert_eq!(run.project.units.len(), 3);
    assert!(!run.project.response_files.is_empty());
    let profiled = fixture.run(&["profile", "--detail", "full"]);
    assert!(profiled.status.success(), "{}", diagnostics(&profiled));
    let run = fixture.latest();
    assert_eq!(run.profiles.len(), 3, "{:?}", run.notes);
    assert!(!run.project.response_files.is_empty());
    assert!(run.profiles.iter().all(|profile| {
        profile
            .phases
            .iter()
            .any(|phase| phase.name == "Preprocessed output size")
    }));
    let detail = fixture
        .store()
        .unit_detail(&run.id, &run.project.units[0].id)
        .unwrap();
    assert!(
        !detail["unit"]["response_files"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn pch_and_unity_builds_keep_their_real_compilation_identity() {
    let Some(compiler) = clang() else {
        eprintln!("Skipping PCH/unity fixture: clang++ is unavailable");
        return;
    };
    let fixture = Fixture::new(&compiler, &["-DENABLE_PCH=ON"]);
    let result = fixture.run(&["profile"]);
    assert!(result.status.success(), "{}", diagnostics(&result));
    let pch = fixture.latest();
    assert!(pch.actions.iter().any(|a| a.kind == "pch"));
    assert!(pch.notes.iter().any(|n| n.contains("PCH/module")));
    let unity = Fixture::new(&compiler, &["-DENABLE_UNITY=ON"]);
    let result = unity.run(&["profile"]);
    assert!(result.status.success(), "{}", diagnostics(&result));
    let run = unity.latest();
    assert_eq!(run.project.units.len(), 1);
    assert_eq!(run.summary.observed_units, 1);
    assert!(run.project.units[0].source_key.contains("/Unity/"));
    assert!(run.notes.iter().any(|n| n.contains("Unity sources")));
}

#[test]
fn cancellation_retains_a_finalized_partial_capture() {
    let Some(compiler) = clang() else {
        eprintln!("Skipping cancellation fixture: clang++ is unavailable");
        return;
    };
    let fixture = Fixture::new(&compiler, &[]);
    append(
        &fixture.source.join("CMakeLists.txt"),
        "\nadd_custom_target(slow COMMAND \"${CMAKE_COMMAND}\" -E sleep 20)\n",
    );
    fixture.configure(&compiler, &[]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_buildscope"))
        .args([
            "--store",
            fixture.store_path.to_str().unwrap(),
            "record",
            "--build-dir",
            fixture.build.to_str().unwrap(),
            "--target",
            "slow",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "recorder did not start");
        if let Ok(run) = fixture.store().load("latest") {
            let stdout = fixture
                .store()
                .run_dir(&run.id)
                .unwrap()
                .join("build.stdout");
            if fs::metadata(stdout).is_ok_and(|m| m.len() > 0) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        if Instant::now() > deadline + Duration::from_secs(3) {
            let _ = child.kill();
            panic!("cancelled recorder did not finish");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let run = fixture.latest();
    assert_eq!(run.status, "interrupted");
    assert!(run.finished_ms.is_some());
    assert!(
        fixture
            .store()
            .run_dir(&run.id)
            .unwrap()
            .join("build.stderr")
            .exists()
    );
}

#[test]
fn managed_captures_save_git_state_without_changing_configuration_identity() {
    let Some(compiler) = clang() else {
        return;
    };
    let fixture = Fixture::new(&compiler, &[]);
    git_command(&fixture.source, &["init", "--initial-branch=main"]);
    git_command(&fixture.source, &["add", "."]);
    git_command(&fixture.source, &["commit", "-m", "initial"]);
    let commit = git_command(&fixture.source, &["rev-parse", "HEAD"]);
    let result = fixture.run(&["record"]);
    assert!(result.status.success(), "{}", diagnostics(&result));
    let recorded = fixture.latest();
    assert_eq!(recorded.git.branch.as_deref(), Some("main"));
    assert_eq!(recorded.git.commit.as_deref(), Some(commit.as_str()));
    assert_eq!(recorded.git.dirty, Some(false));
    assert!(recorded.git.observed_ms.unwrap() <= recorded.finished_ms.unwrap());

    let result = fixture.run(&["profile"]);
    assert!(result.status.success(), "{}", diagnostics(&result));
    assert_eq!(fixture.latest().git.commit, recorded.git.commit);
    assert_eq!(fixture.latest().git.dirty, Some(false));
    git_command(&fixture.source, &["checkout", "-b", "feature/new-revision"]);
    git_command(
        &fixture.source,
        &["commit", "--allow-empty", "-m", "next revision"],
    );
    fs::write(fixture.source.join("untracked.cpp"), "// local work\n").unwrap();
    let result = fixture.run(&["record"]);
    assert!(result.status.success(), "{}", diagnostics(&result));
    let after = fixture.latest();
    assert_eq!(after.git.branch.as_deref(), Some("feature/new-revision"));
    assert_ne!(after.git.commit, recorded.git.commit);
    assert_eq!(after.git.dirty, Some(true));
    assert_eq!(
        after.project.configuration_id,
        recorded.project.configuration_id
    );
    let comparison = buildscope::analysis::compare(&recorded, &after);
    assert_eq!(comparison["conditions_match"], true);
    assert_eq!(comparison["before_git"]["commit"], commit);
    assert_eq!(comparison["after_git"]["dirty"], true);
    assert_eq!(
        fixture.store().overview(&after.id).unwrap()["git"]["dirty"],
        true
    );
    assert!(
        fixture
            .store()
            .list()
            .unwrap()
            .iter()
            .any(|run| run["id"] == after.id && run["git"]["dirty"] == true)
    );

    for args in [
        vec!["inspect", "latest"],
        vec!["list"],
        vec!["compare", &recorded.id, &after.id],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_buildscope"))
            .arg("--store")
            .arg(&fixture.store_path)
            .args(args)
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", diagnostics(&result));
        assert!(String::from_utf8_lossy(&result.stdout).contains("feature/new-revision"));
    }
}

#[test]
fn launcher_retains_precompile_revisions_across_delayed_mixed_and_legacy_collection() {
    use buildscope::git::GitStatus;
    let Some(compiler) = clang() else {
        return;
    };
    let fixture = Fixture::new(&compiler, &[]);
    git_command(&fixture.source, &["init", "--initial-branch=main"]);
    git_command(&fixture.source, &["add", "."]);
    git_command(&fixture.source, &["commit", "-m", "initial"]);
    let first_commit = git_command(&fixture.source, &["rev-parse", "HEAD"]);
    let launcher = format!(
        "{};launch;--build-dir;{};--",
        env!("CARGO_BIN_EXE_buildscope"),
        fixture.build.display()
    );
    fixture.configure(
        &compiler,
        &[&format!("-DCMAKE_CXX_COMPILER_LAUNCHER={launcher}")],
    );
    assert!(normal_build(&fixture).status.success());
    git_command(&fixture.source, &["checkout", "-b", "feature/second"]);
    let first_source = fs::read_to_string(fixture.source.join("first.cpp")).unwrap();
    fs::write(
        fixture.source.join("first.cpp"),
        format!("{first_source}\n// second revision\n"),
    )
    .unwrap();
    git_command(&fixture.source, &["add", "first.cpp"]);
    git_command(&fixture.source, &["commit", "-m", "second"]);
    let second_commit = git_command(&fixture.source, &["rev-parse", "HEAD"]);

    // Collection happens on the new branch, while all compiled observations came from main.
    let result = collect_launcher(&fixture, "delayed");
    assert!(result.status.success(), "{}", diagnostics(&result));
    let delayed = fixture.latest();
    assert_eq!(delayed.git.branch.as_deref(), Some("main"));
    assert_eq!(delayed.git.commit.as_deref(), Some(first_commit.as_str()));
    assert_eq!(delayed.git.dirty, Some(false));
    assert!(normal_build(&fixture).status.success());
    git_command(&fixture.source, &["checkout", "main"]);
    assert!(normal_build(&fixture).status.success());
    let result = collect_launcher(&fixture, "mixed");
    assert!(result.status.success(), "{}", diagnostics(&result));
    let mixed = fixture.latest();
    assert_eq!(mixed.git.status, GitStatus::Mixed);
    assert_eq!(mixed.git.commit, None);
    assert_eq!(mixed.git.dirty, Some(false));
    assert!(
        mixed
            .notes
            .iter()
            .any(|note| note.contains("Git state differs"))
    );
    let store = fixture.store();
    let observations = store.run_dir(&mixed.id).unwrap().join("observations");
    let commits = fs::read_dir(&observations)
        .unwrap()
        .map(|entry| {
            let observation: serde_json::Value =
                serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap();
            assert_eq!(observation["git"]["dirty"], false);
            observation["git"]["commit"].as_str().unwrap().to_owned()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(commits, [first_commit, second_commit].into_iter().collect());
    let archive = fixture.root.path().join("git.buildscope");
    store.export(&mixed.id, &archive, false).unwrap();
    let imported = Store::open(&fixture.root.path().join("imported")).unwrap();
    imported.import(&archive).unwrap();
    assert_eq!(imported.load(&mixed.id).unwrap().git, mixed.git);
    let imported_observations = imported.run_dir(&mixed.id).unwrap().join("observations");
    assert_eq!(
        fs::read_dir(&imported_observations).unwrap().count(),
        fs::read_dir(&observations).unwrap().count()
    );
    for entry in fs::read_dir(&observations).unwrap() {
        let entry = entry.unwrap();
        assert_eq!(
            fs::read(entry.path()).unwrap(),
            fs::read(imported_observations.join(entry.file_name())).unwrap()
        );
    }

    // Old pending observations remain unknown, even when collection can see a current checkout.
    fs::write(
        fixture.source.join("first.cpp"),
        format!("{first_source}\n// legacy observation\n"),
    )
    .unwrap();
    assert!(normal_build(&fixture).status.success());
    for entry in fs::read_dir(fixture.build.join(".buildscope-launcher/pending")).unwrap() {
        let path = entry.unwrap().path().join("observation.json");
        let mut observation: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        observation.as_object_mut().unwrap().remove("git");
        fs::write(path, serde_json::to_vec(&observation).unwrap()).unwrap();
    }
    assert!(collect_launcher(&fixture, "legacy").status.success());
    assert_eq!(fixture.latest().git.status, GitStatus::NotRecorded);
}

#[test]
fn missing_git_does_not_prevent_launcher_compilation() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("build.ninja"), "").unwrap();
    fs::write(
        root.path().join("CMakeCache.txt"),
        format!("CMAKE_HOME_DIRECTORY:INTERNAL={}\n", root.path().display()),
    )
    .unwrap();
    fs::write(root.path().join("source.cpp"), "").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_buildscope"))
        .current_dir(root.path())
        .env("PATH", "/nonexistent-buildscope-test-path")
        .args(["launch", "--build-dir"])
        .arg(root.path())
        .args(["--", "/usr/bin/true", "-c", "source.cpp", "-o", "object.o"])
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", diagnostics(&result));
    let pending = fs::read_dir(root.path().join(".buildscope-launcher/pending"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let observation: serde_json::Value =
        serde_json::from_slice(&fs::read(pending.join("observation.json")).unwrap()).unwrap();
    assert_eq!(observation["git"]["status"], "unavailable");
    assert_eq!(observation["invocation"]["exit_code"], 0);
}

fn git_command(root: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=Buildscope Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", diagnostics(&result));
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}

#[test]
fn public_cmake_launcher_collects_parallel_incremental_and_failed_invocations() {
    let Some(compiler) = clang() else {
        return;
    };
    let fixture = Fixture::new(&compiler, &[]);
    let launcher = format!(
        "{};launch;--build-dir;{};--",
        env!("CARGO_BIN_EXE_buildscope"),
        fixture.build.display()
    );
    fixture.configure(
        &compiler,
        &[
            &format!("-DCMAKE_CXX_COMPILER_LAUNCHER={launcher}"),
            "-DCMAKE_NINJA_FORCE_RESPONSE_FILE=ON",
        ],
    );
    // Cover Ninja compdb fallback with a public launcher and ephemeral response files.
    fs::remove_file(fixture.build.join("compile_commands.json")).unwrap();
    let build = normal_build(&fixture);
    assert!(build.status.success(), "{}", diagnostics(&build));
    assert!(
        !fixture.store_path.exists(),
        "compiler processes must not initialize a shared database"
    );
    let pending = fixture.build.join(".buildscope-launcher/pending");
    assert_eq!(fs::read_dir(&pending).unwrap().count(), 3);
    let acknowledged = fs::read_dir(&pending)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let backup = fixture.root.path().join("completed backup");
    copy_tree(&acknowledged, &backup);
    fs::create_dir(pending.join("unfinished")).unwrap();
    let collected = collect_launcher(&fixture, "clean-launcher");
    assert!(collected.status.success(), "{}", diagnostics(&collected));
    let run = fixture.latest();
    assert_eq!(run.kind, "launcher");
    assert_eq!(run.status, "success");
    assert_eq!(run.actions.len(), 3);
    assert_eq!(run.summary.observed_units, 3);
    assert!(run.project.compilers.iter().all(|c| c.family == "clang"));
    assert_eq!(run.profiles.len(), 3, "{:?}", run.notes);
    assert!(run.profiles.iter().all(|p| p.event_count > 0));
    assert!(
        run.headers
            .iter()
            .any(|h| h.key == "shared.hpp" && h.processing_ms.is_some())
    );
    assert!(run.notes.iter().any(|n| n.contains("unfinished")));
    assert!(
        run.notes
            .iter()
            .any(|n| n.contains("not total build duration"))
    );
    assert!(run.summary.dependency_path.is_empty());
    assert!(run.inputs.keys().any(|p| p.ends_with("shared.hpp")));
    assert!(
        fs::read_dir(
            fixture
                .store()
                .run_dir(&run.id)
                .unwrap()
                .join("observations")
        )
        .unwrap()
        .count()
            == 3
    );
    let unit = fixture
        .store()
        .unit_detail(&run.id, &run.project.units[0].id)
        .unwrap();
    assert!(
        !unit["unit"]["response_files"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    // Simulate a crash after committing receipts but before deleting a completed spool.
    copy_tree(&backup, &acknowledged);
    assert!(collect_launcher(&fixture, "cleanup-retry").status.success());
    assert!(!acknowledged.exists());
    assert_eq!(fixture.store().list().unwrap().len(), 1);
    let noop = normal_build(&fixture);
    assert!(noop.status.success());
    assert!(collect_launcher(&fixture, "noop-launcher").status.success());
    assert_eq!(fixture.store().list().unwrap().len(), 1);
    append(
        &fixture.source.join("shared.hpp"),
        "\n// edited after first collection\n",
    );
    assert!(normal_build(&fixture).status.success());
    assert!(
        collect_launcher(&fixture, "changed-launcher")
            .status
            .success()
    );
    let changed = fixture.latest();
    assert_eq!(
        changed.project.configuration_id,
        run.project.configuration_id
    );
    let shared_path = run
        .inputs
        .keys()
        .find(|p| p.ends_with("shared.hpp"))
        .unwrap();
    assert_eq!(
        fixture.store().source(&run, shared_path, &[]).unwrap()["matches_capture"],
        false
    );
    assert!(
        changed
            .headers
            .iter()
            .any(|h| h.key == "shared.hpp" && h.changed_contributor && !h.explicit_trigger)
    );
    append(&fixture.source.join("second.cpp"), "\nnot valid C++;\n");
    let failed = normal_build(&fixture);
    assert!(!failed.status.success());
    assert!(diagnostics(&failed).contains("second.cpp"));
    assert!(
        collect_launcher(&fixture, "failed-launcher")
            .status
            .success()
    );
    let failed = fixture.latest();
    assert_eq!(failed.status, "failed");
    assert!(failed.actions.iter().any(|a| !a.success));
    assert!(
        fixture
            .store()
            .run_dir(&failed.id)
            .unwrap()
            .join("raw")
            .is_dir()
    );
    let archive = fixture.root.path().join("launcher.buildscope");
    fixture.store().export(&run.id, &archive, false).unwrap();
    let imported = Store::open(&fixture.root.path().join("imported")).unwrap();
    let id = imported.import(&archive).unwrap();
    assert_eq!(imported.load(&id).unwrap().kind, "launcher");
}

#[test]
fn launcher_finalization_failure_preserves_recovery_and_retries_without_duplicate_reports() {
    let Some(compiler) = clang() else {
        return;
    };
    let fixture = Fixture::new(&compiler, &[]);
    let launcher = format!(
        "{};launch;--build-dir;{};--",
        env!("CARGO_BIN_EXE_buildscope"),
        fixture.build.display()
    );
    fixture.configure(
        &compiler,
        &[&format!("-DCMAKE_CXX_COMPILER_LAUNCHER={launcher}")],
    );
    assert!(normal_build(&fixture).status.success());
    append(
        &fixture.source.join("first.cpp"),
        "\n// second invocation of the same unit\n",
    );
    assert!(normal_build(&fixture).status.success());
    let pending = fixture.build.join(".buildscope-launcher/pending");
    assert_eq!(fs::read_dir(&pending).unwrap().count(), 4);

    let store = fixture.store();
    let database = rusqlite::Connection::open(fixture.store_path.join("index.sqlite3")).unwrap();
    // Fail after one receipt insert so final report rows AND partial receipts must roll back.
    database
        .execute_batch(
            "CREATE TRIGGER fail_receipt BEFORE INSERT ON launcher_receipts
        WHEN (SELECT count(*) FROM launcher_receipts WHERE run=NEW.run)=1
        BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;",
        )
        .unwrap();

    // Trace discovery through a subprocess-local PATH; other tests retain their real Ninja.
    let native_path = std::env::var_os("PATH").unwrap();
    let ninja = std::env::split_paths(&native_path)
        .map(|directory| directory.join("ninja"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    let wrappers = fixture.root.path().join("wrappers");
    fs::create_dir(&wrappers).unwrap();
    let wrapper = wrappers.join("ninja");
    fs::write(
        &wrapper,
        r#"#!/bin/sh
for argument do
    if [ "$argument" = graph ]; then
        printf 'graph\n' >> "$BUILDSCOPE_TEST_GRAPH_LOG"
    fi
done
exec "$BUILDSCOPE_TEST_NINJA" "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let graph_log = fixture.root.path().join("graph.log");
    let path =
        std::env::join_paths(std::iter::once(wrappers).chain(std::env::split_paths(&native_path)))
            .unwrap();
    let failed = collect_launcher_command(&fixture, "retry-launcher")
        .env("PATH", path)
        .env("BUILDSCOPE_TEST_NINJA", ninja)
        .env("BUILDSCOPE_TEST_GRAPH_LOG", &graph_log)
        .output()
        .unwrap();
    assert!(!failed.status.success(), "{}", diagnostics(&failed));
    assert!(diagnostics(&failed).contains("injected receipt failure"));
    assert_eq!(fs::read_to_string(&graph_log).unwrap().lines().count(), 1);

    let checkpoint = fixture.latest();
    assert_eq!(checkpoint.status, "running");
    assert_eq!(checkpoint.actions.len(), 4);
    assert!(
        checkpoint
            .project
            .rules
            .values()
            .any(|rule| rule.contains("LINKER"))
    );
    assert!(
        checkpoint
            .project
            .rules
            .values()
            .any(|rule| rule.contains("CUSTOM_COMMAND"))
    );
    let rows = |table| {
        database
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    assert!(
        rows("events") > 0,
        "decoding completed before finalization failed"
    );
    for table in [
        "launcher_receipts",
        "report_runs",
        "report_units",
        "profiles",
        "headers",
        "report_headers",
        "include_paths",
    ] {
        assert_eq!(
            rows(table),
            0,
            "{table} must not expose a partial finalization"
        );
    }
    assert_eq!(fs::read_dir(&pending).unwrap().count(), 4);
    database.execute_batch("DROP TRIGGER fail_receipt").unwrap();

    let recovered = capture::recover(&store, &checkpoint.id).unwrap();
    assert_eq!(recovered.status, "interrupted");
    assert_eq!(recovered.profiles.len(), 3);
    assert!(
        recovered
            .profiles
            .iter()
            .all(|profile| profile.event_count > 0)
    );
    assert!(
        recovered
            .notes
            .iter()
            .any(|note| note.contains("Multiple compiler invocations"))
    );
    assert_eq!(
        rows("launcher_receipts"),
        0,
        "recovery must leave the spool unacknowledged"
    );

    let retried = collect_launcher(&fixture, "retry-launcher");
    assert!(retried.status.success(), "{}", diagnostics(&retried));
    let run = fixture.latest();
    assert_eq!(run.id, checkpoint.id);
    assert_eq!(run.status, "success");
    assert_eq!(run.actions.len(), 4);
    assert_eq!(run.profiles.len(), 3);
    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(rows("launcher_receipts"), 4);
    assert_eq!(rows("report_units"), 3);
    assert_eq!(fs::read_dir(&pending).unwrap().count(), 0);
    assert_eq!(store.overview(&run.id).unwrap()["status"], "success");
}

#[test]
fn launcher_hides_include_guard_advice_and_retains_diagnostics_and_raw_stderr() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("build.ninja"), "").unwrap();
    fs::write(root.path().join("forced header.hpp"), "").unwrap();
    let compiler = root.path().join("fake-g++");
    let stderr = concat!(
        ". /sdk/system.h\n",
        ".. ./header with spaces.hpp\n",
        ". bare.hpp\n",
        "main.cpp:1: warning: before include-guard advice\n",
        "Multiple include guards may be useful for:\n",
        "./header with spaces.hpp\n",
        "forced header.hpp\n",
        "/sdk/system.h\n",
        "bare.hpp\n",
        "/sdk/system.h:3: error: after include-guard advice\n",
        "  3 | invalid code\n",
        "    | ^~~~~~~\n",
        "compilation terminated.\n",
        "\nTime variable wall GGC\n",
        " TOTAL : 0.01 123k\n",
    );
    fs::write(
        &compiler,
        format!(
            "#!/bin/sh\n\
             case \"$1\" in\n\
             --version) echo 'GCC Free Software Foundation'; exit 0 ;;\n\
             --help) exit 0 ;;\n\
             -x) exit 1 ;;\n\
             esac\n\
             cat >&2 <<'DIAGNOSTICS'\n{stderr}DIAGNOSTICS\nexit 37\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&compiler, fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_buildscope"))
        .current_dir(root.path())
        .args(["launch", "--build-dir"])
        .arg(root.path())
        .arg("--")
        .arg(&compiler)
        .args(["-c", "main.cpp", "-o", "main.o"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(37));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        concat!(
            "main.cpp:1: warning: before include-guard advice\n",
            "/sdk/system.h:3: error: after include-guard advice\n",
            "  3 | invalid code\n",
            "    | ^~~~~~~\n",
            "compilation terminated.\n\n",
        )
    );
    let observation = fs::read_dir(root.path().join(".buildscope-launcher/pending"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let manifest = fs::read_dir(observation.join("invocations"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let invocation: Invocation = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    assert_eq!(
        fs::read_to_string(observation.join(invocation.stderr)).unwrap(),
        stderr
    );
}

#[test]
fn public_gcc_launcher_collects_native_timings_and_preserves_diagnostics() {
    let Some(compiler) = gcc() else {
        return;
    };
    let fixture = Fixture::new(&compiler, &[]);
    fs::write(
        fixture.source.join("unguarded header.hpp"),
        "// Intentionally unguarded to exercise GCC's -H advice.\n",
    )
    .unwrap();
    append(
        &fixture.source.join("second.cpp"),
        "\n#include \"unguarded header.hpp\"\n#warning visible launcher warning\n",
    );
    // Exercise inference from Ninja's working directory, without an explicit build root.
    let launcher = format!("{};launch;--", env!("CARGO_BIN_EXE_buildscope"));
    fixture.configure(
        &compiler,
        &[&format!("-DCMAKE_CXX_COMPILER_LAUNCHER={launcher}")],
    );
    let built = normal_build(&fixture);
    assert!(built.status.success(), "{}", diagnostics(&built));
    let output = diagnostics(&built);
    assert!(output.contains("warning: #warning visible launcher warning"));
    assert!(!output.contains("Multiple include guards may be useful for:"));
    assert!(!output.contains("unguarded header.hpp"));
    assert!(collect_launcher(&fixture, "gcc-launcher").status.success());
    let run = fixture.latest();
    assert_eq!(run.profiles.len(), 3, "{:?}", run.notes);
    assert!(
        run.profiles
            .iter()
            .all(|p| p.family == "gcc" && !p.phases.is_empty())
    );
    assert!(run.headers.iter().all(|h| h.processing_ms.is_none()));
    append(&fixture.source.join("second.cpp"), "\nnot valid C++;\n");
    let failed = normal_build(&fixture);
    assert!(!failed.status.success());
    let output = diagnostics(&failed);
    assert!(output.contains("second.cpp:"));
    assert!(!output.contains("Multiple include guards may be useful for:"));
    assert!(!output.contains("unguarded header.hpp"));
    assert!(
        !output.contains("\"$schema\""),
        "public launchers retain native diagnostic text"
    );
}

#[test]
fn legacy_clang_traces_preserve_sidecars_and_never_reuse_stale_evidence() {
    let Some(compiler) = clang() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let wrapper = root.path().join("legacy clang++");
    let compiler = shlex::try_quote(&compiler).unwrap();
    // Exercise a real compiler while enforcing the older driver's trace-path contract.
    fs::write(
        &wrapper,
        format!(
            r#"#!/bin/sh
if [ "$1" = "--help" ]; then
    {compiler} --help | sed '/-ftime-trace=</d'
    exit
fi
for argument in "$@"; do
    case "$argument" in
        -ftime-trace=*) echo 'explicit trace paths are unsupported' >&2; exit 1 ;;
        -DBUILDSCOPE_FAIL) echo 'forced compiler failure' >&2; exit 37 ;;
    esac
done
exec {compiler} "$@"
"#
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!compiler::identify(wrapper.to_str().unwrap(), root.path()).explicit_trace_path);

    for (name, previous, fail) in [
        ("fresh", false, false),
        ("existing", true, false),
        ("failed existing", true, true),
        ("failed fresh", false, true),
    ] {
        let build = root.path().join(name);
        fs::create_dir(&build).unwrap();
        fs::write(build.join("build.ninja"), "").unwrap();
        fs::write(
            build.join("source file.cpp"),
            "int answer() { return 42; }\n",
        )
        .unwrap();
        // Both the output path and source path are available only through a response file.
        fs::write(
            build.join("arguments.rsp"),
            "-c 'source file.cpp' -o 'object file.o'\n",
        )
        .unwrap();
        let sidecar = build.join("object file.json");
        if previous {
            fs::write(&sidecar, "previous user trace").unwrap();
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_buildscope"));
        command
            .current_dir(&build)
            .args(["launch", "--detail", "full", "--build-dir"])
            .arg(&build)
            .arg("--")
            .arg(&wrapper)
            .arg("@arguments.rsp");
        if fail {
            command.arg("-DBUILDSCOPE_FAIL");
        }
        let result = command.output().unwrap();
        assert_eq!(
            result.status.code(),
            Some(if fail { 37 } else { 0 }),
            "{}",
            diagnostics(&result)
        );
        if previous {
            assert_eq!(fs::read_to_string(&sidecar).unwrap(), "previous user trace");
        } else {
            assert!(
                !sidecar.exists(),
                "new sidecars must be retained only in the spool"
            );
        }
        assert!(!fs::read_dir(&build).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".backup")
        }));
        let pending = fs::read_dir(build.join(".buildscope-launcher/pending"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let manifest = fs::read_dir(pending.join("invocations"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let invocation: Invocation = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
        assert_eq!(invocation.exit_code, result.status.code());
        let trace = pending.join(invocation.trace.unwrap());
        if fail {
            assert!(
                !trace.exists(),
                "a failed compile must never retain a previous trace"
            );
        } else {
            let (events, _) = compiler::read_clang_trace(&trace, &build).unwrap();
            assert!(
                !events.is_empty(),
                "retain real compiler events from the sidecar"
            );
            assert!(build.join("object file.o").is_file());
        }
    }
}

#[test]
fn public_launcher_passes_through_configure_probes_and_preserves_exit_codes() {
    let root = tempfile::tempdir().unwrap();
    let scratch = root.path().join("CMakeFiles/CMakeScratch/TryCompile-test");
    fs::create_dir_all(&scratch).unwrap();
    fs::write(root.path().join("build.ninja"), "").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_buildscope"))
        .current_dir(&scratch)
        .args(["launch", "--build-dir"])
        .arg(root.path())
        .args([
            "--",
            "/bin/sh",
            "-c",
            "echo 'native stdout'; echo 'native stderr' >&2; exit 37",
        ])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(37));
    assert_eq!(String::from_utf8_lossy(&result.stdout), "native stdout\n");
    assert_eq!(String::from_utf8_lossy(&result.stderr), "native stderr\n");
    assert!(!root.path().join(".buildscope-launcher").exists());
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn normal_build(fixture: &Fixture) -> Output {
    Command::new("cmake")
        .arg("--build")
        .arg(&fixture.build)
        .args(["--parallel", "4"])
        .output()
        .unwrap()
}

fn collect_launcher(fixture: &Fixture, label: &str) -> Output {
    collect_launcher_command(fixture, label).output().unwrap()
}

fn collect_launcher_command(fixture: &Fixture, label: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_buildscope"));
    command
        .arg("--store")
        .arg(&fixture.store_path)
        .args(["collect", "--build-dir"])
        .arg(&fixture.build)
        .args(["--label", label]);
    command
}

struct Fixture {
    root: tempfile::TempDir,
    source: PathBuf,
    build: PathBuf,
    store_path: PathBuf,
}

impl Fixture {
    fn new(compiler: &str, options: &[&str]) -> Self {
        let root = tempfile::tempdir().unwrap();
        // Spaces exercise compiler argument vectors, DOT parsing, -H, archives, and CMake launchers.
        let source = root.path().join("source project");
        let build = root.path().join("build tree");
        fs::create_dir(&source).unwrap();
        let original = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project");
        for entry in fs::read_dir(original).unwrap() {
            let entry = entry.unwrap();
            fs::copy(entry.path(), source.join(entry.file_name())).unwrap();
        }
        let store_path = root.path().join("capture store");
        let fixture = Self {
            root,
            source,
            build,
            store_path,
        };
        fixture.configure(compiler, options);
        fixture
    }
    fn configure(&self, compiler: &str, options: &[&str]) {
        let result = Command::new("cmake")
            .arg("-S")
            .arg(&self.source)
            .arg("-B")
            .arg(&self.build)
            .args(["-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release"])
            .arg(format!("-DCMAKE_CXX_COMPILER={compiler}"))
            .args(options)
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", diagnostics(&result));
    }
    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_buildscope"));
        command
            .arg("--store")
            .arg(&self.store_path)
            .arg(args[0])
            .arg("--build-dir")
            .arg(&self.build)
            .args(["--target", "app"])
            .args(&args[1..]);
        command.output().unwrap()
    }
    fn store(&self) -> Store {
        Store::open(&self.store_path).unwrap()
    }
    fn latest(&self) -> Capture {
        self.store().load("latest").unwrap()
    }
}

fn clang() -> Option<String> {
    if let Ok(compiler) = std::env::var("BUILDSCOPE_TEST_CLANG") {
        return Some(compiler);
    }
    ["clang++", "clang++-20", "clang++-19", "clang++-18"]
        .into_iter()
        .find(|name| {
            Command::new(name)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        })
        .map(str::to_owned)
}
fn gcc() -> Option<String> {
    if let Ok(compiler) = std::env::var("BUILDSCOPE_TEST_GCC") {
        return Some(compiler);
    }
    ["g++-16", "g++-15", "g++-14", "g++-13", "g++"]
        .into_iter()
        .find(|name| {
            Command::new(name).arg("--version").output().is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout).contains("Free Software Foundation")
            })
        })
        .map(str::to_owned)
}
fn append(path: &Path, text: &str) {
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}
fn diagnostics(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
