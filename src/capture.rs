//! Ordinary Ninja recording, isolated profiling, and shared compiler execution.
//! Managed builds persist captures before execution; public launchers own their spool.
pub(crate) use crate::decoder::decode_invocations;
use crate::{analysis, compiler, model::*, project, store::Store, util::*};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::{BufReader, Read, Write},
    os::unix::process::{CommandExt, ExitStatusExt},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Options shared by ordinary and instrumented build executions.
#[derive(Clone, Debug)]
pub struct RecordRequest {
    /// Existing configured CMake/Ninja directory.
    pub build_dir: PathBuf,
    /// Requested targets; empty means Ninja defaults.
    pub targets: Vec<String>,
    /// Optional scenario or experiment label.
    pub label: Option<String>,
    /// Explicit Ninja job limit; None preserves Ninja's default.
    pub jobs: Option<usize>,
    /// Whether compiler traces should use zero threshold and verbose metadata.
    pub full_detail: bool,
    /// Whether to run a separate preprocessing measurement after Ninja completes.
    pub preprocess: bool,
}

/// Configuration for a separate, tool-owned profiling build.
#[derive(Clone, Debug)]
pub struct ProfileRequest {
    /// Original build whose source and CMake settings should be reproduced.
    pub original: RecordRequest,
    /// Managed profile output directory; None selects a stable directory in the store.
    pub profile_dir: Option<PathBuf>,
    /// Unique unit ID/prefix or source key to investigate; None profiles requested targets.
    pub unit: Option<String>,
    /// CMake preset to apply before explicit settings; None uses the original cache.
    pub preset: Option<String>,
    /// Additional CMake arguments for project-specific configuration.
    pub cmake_args: Vec<String>,
    /// Explicit alternative compiler for a separately labelled investigation.
    pub compiler: Option<String>,
}

/// Record an ordinary build without injecting compiler instrumentation.
pub fn record(store: &Store, request: &RecordRequest) -> Result<(Capture, i32)> {
    execute(store, request, false, Vec::new())
}

/// Configure and profile a dedicated tree, preserving the original build and outputs.
pub fn profile(store: &Store, request: &ProfileRequest) -> Result<(Capture, i32)> {
    let original = project::discover(&request.original.build_dir, &request.original.targets)?;
    ensure!(
        original
            .cmake_cache
            .get("CMAKE_GENERATOR")
            .is_some_and(|generator| generator == "Ninja"),
        "profiling requires a single-config Ninja build; configure one from the same source or CMake preset"
    );
    let variant = digest(serde_json::to_vec(&(
        &original.configuration_id,
        &request.compiler,
        &request.cmake_args,
        &request.preset,
    ))?);
    let profile_dir = request
        .profile_dir
        .clone()
        .unwrap_or_else(|| store.root.join("profiles").join(&variant[..16]));
    let profile_dir = resolve(std::env::current_dir()?, profile_dir);
    ensure!(
        !profile_dir.starts_with(&original.build_dir)
            && !Path::new(&original.build_dir).starts_with(&profile_dir),
        "profiling directory must be separate from the original build"
    );
    ensure!(
        profile_dir != Path::new(&original.source_dir),
        "profile directory cannot be the source root"
    );
    let owner = profile_dir.join(".buildscope-profile.json");
    if profile_dir.exists() && !owner.exists() {
        ensure!(
            fs::read_dir(&profile_dir)?.next().is_none(),
            "profile directory is not owned by Buildscope"
        );
    }
    fs::create_dir_all(&profile_dir)?;
    let expected_owner =
        serde_json::json!({"source":original.source_dir,"original_build":original.build_dir});
    if owner.exists() {
        let existing: serde_json::Value =
            serde_json::from_reader(BufReader::new(File::open(&owner)?))?;
        ensure!(
            existing == expected_owner,
            "profile directory belongs to a different project/build"
        );
    } else {
        write_json(&owner, &expected_owner)?;
    }
    let mut configure = Command::new("cmake");
    configure.current_dir(&original.source_dir);
    if let Some(preset) = &request.preset {
        configure.args(["--preset", preset]);
    }
    configure
        .arg("-S")
        .arg(&original.source_dir)
        .arg("-B")
        .arg(&profile_dir)
        .args(["-G", "Ninja"]);
    for (key, value) in &original.cmake_cache {
        if !copy_cache_key(key, &original.cmake_cache) {
            continue;
        }
        let value = value.replace(&original.build_dir, &path_string(&profile_dir));
        configure.arg(format!("-D{key}={value}"));
    }
    if let Some(compiler) = &request.compiler {
        configure.arg(format!("-DCMAKE_CXX_COMPILER={compiler}"));
    }
    configure.args(&request.cmake_args);
    let launcher = format!("{};__launch", std::env::current_exe()?.display());
    configure
        .arg(format!("-DCMAKE_CXX_COMPILER_LAUNCHER={launcher}"))
        .arg(format!("-DCMAKE_C_COMPILER_LAUNCHER={launcher}"))
        .arg("-DCMAKE_EXPORT_COMPILE_COMMANDS=ON");
    eprintln!(
        "Configuring isolated profile tree: {}",
        profile_dir.display()
    );
    let result = configure.status().context("configure profile tree")?;
    ensure!(
        result.success(),
        "profile configuration failed; pass project-specific options with --cmake-arg"
    );
    let configured = project::discover(&profile_dir, &request.original.targets)?;
    ensure!(
        !configured.units.is_empty(),
        "no supported C/C++ compilation units were discovered"
    );
    ensure!(
        configured
            .compilers
            .iter()
            .all(|compiler| ["clang", "gcc"].contains(&compiler.family.as_str())),
        "compiler profiling supports Clang and GCC; use record for other compilers"
    );
    let mut notes = vec![
        format!(
            "Instrumented build in {}. Original build outputs were preserved.",
            profile_dir.display()
        ),
        "Compiler caches are bypassed in this dedicated profile configuration.".into(),
        "Profile action durations include compiler instrumentation overhead.".into(),
    ];
    if original.configuration_id != configured.configuration_id {
        notes.push("Profile configuration differs from the original (for example build-path arguments, compiler, or custom options). Compare commands before treating it as equivalent.".into());
    }
    if request.compiler.is_some() {
        notes.push(
            "Explicit alternative compiler investigation; these are measurements of that compiler."
                .into(),
        );
    }
    let selected: Vec<_> = if let Some(selector) = &request.unit {
        let matches: Vec<_> = configured
            .units
            .iter()
            .filter(|u| u.id.starts_with(selector) || u.source_key == *selector)
            .collect();
        ensure!(
            matches.len() == 1,
            "unit selector must match exactly one ID/prefix or source key; inspect units in the report"
        );
        matches
    } else {
        configured.units.iter().collect()
    };
    // Only owned compilation outputs are removed, so profiling always produces fresh
    // evidence even when an earlier profile tree is already up to date.
    for unit in &selected {
        let output = resolve(&profile_dir, &unit.output);
        ensure!(
            output.starts_with(&profile_dir),
            "refusing to invalidate output outside the managed profile tree"
        );
        if output.is_file() {
            fs::remove_file(output)?;
        }
    }
    let mut recording = request.original.clone();
    recording.build_dir = profile_dir;
    if request.unit.is_some() {
        recording.targets = vec![selected[0].output_key.clone()];
    }
    recording.preprocess |= recording.full_detail;
    execute(store, &recording, true, notes)
}

/// Transparent compiler launcher used only by the dedicated CMake configuration.
pub fn launch(arguments: Vec<String>) -> Result<i32> {
    ensure!(
        !arguments.is_empty(),
        "compiler launcher requires an executable"
    );
    let Some(root) = std::env::var_os("BUILDSCOPE_RUN_DIR").map(PathBuf::from) else {
        let status = Command::new(&arguments[0]).args(&arguments[1..]).status()?;
        return Ok(exit_code(status));
    };
    let compilers: Vec<CompilerInfo> = serde_json::from_str(
        &std::env::var("BUILDSCOPE_COMPILERS").unwrap_or_else(|_| "[]".into()),
    )?;
    let full = std::env::var("BUILDSCOPE_DETAIL").as_deref() == Ok("full");
    Ok(run_compiler(arguments, &root, full, &compilers, false)?
        .exit_code
        .unwrap_or(1))
}

/// Run one compiler with an explicit artifact destination. Public launchers preserve
/// native diagnostics; managed profiles may use structured GCC diagnostics instead.
pub(crate) fn run_compiler(
    arguments: Vec<String>,
    root: &Path,
    full: bool,
    compilers: &[CompilerInfo],
    native_diagnostics: bool,
) -> Result<Invocation> {
    let directory = std::env::current_dir()?;
    let (expanded, response_files) =
        project::expand_responses(&arguments, &directory, &BTreeMap::new())
            .unwrap_or_else(|_| (arguments.clone(), BTreeMap::new()));
    let info = compilers
        .iter()
        .find(|c| resolve(&directory, &c.executable) == resolve(&directory, &arguments[0]))
        .cloned()
        .unwrap_or_else(|| compiler::identify(&arguments[0], &directory));
    let id = unique_id();
    fs::create_dir_all(root.join("raw"))?;
    fs::create_dir_all(root.join("invocations"))?;
    let stderr = format!("raw/{id}.stderr");
    let stdout = format!("raw/{id}.stdout");
    let output = output_argument(&expanded).map(|p| path_string(resolve(&directory, p)));
    let mut actual = arguments;
    let mut trace = None;
    let mut legacy_trace = None;
    let granularity_us = (info.family == "clang").then_some(if full { 0 } else { 500 });
    if info.family == "clang" {
        let path = format!("raw/{id}.trace.json");
        if info.explicit_trace_path {
            actual.push(format!("-ftime-trace={}", root.join(&path).display()));
        } else {
            // Older Clang writes beside -o, including outputs expanded from response files.
            let output = output
                .as_deref()
                .context("Clang tracing requires an -o output")?;
            legacy_trace = Some(LegacyTrace::prepare(Path::new(output), &id)?);
            actual.push("-ftime-trace".into());
        }
        actual.push(format!(
            "-ftime-trace-granularity={}",
            granularity_us.unwrap()
        ));
        if full && info.verbose_trace {
            actual.push("-ftime-trace-verbose".into());
        }
        actual.push("-H".into());
        trace = Some(path);
    } else if info.family == "gcc" {
        actual.extend([
            "-ftime-report".into(),
            "-ftime-report-details".into(),
            "-H".into(),
        ]);
        if info.sarif && !native_diagnostics {
            actual.push("-fdiagnostics-format=sarif-stderr".into());
        }
    }
    let mut invocation = Invocation {
        id: id.clone(),
        directory: path_string(&directory),
        arguments: actual.clone(),
        response_files,
        output,
        family: info.family,
        exit_code: None,
        elapsed_ms: 0.0,
        started_ms: None,
        trace,
        stderr: stderr.clone(),
        granularity_us,
        verbose: full && info.verbose_trace,
    };
    let manifest = root.join("invocations").join(format!("{id}.json"));
    write_json(&manifest, &invocation)?;
    let mut command = Command::new(&actual[0]);
    command.args(&actual[1..]);
    invocation.started_ms = Some(now_ms());
    let start = Instant::now();
    let status = run_process(
        &mut command,
        &root.join(&stdout),
        &root.join(&stderr),
        false,
    )?;
    invocation.elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    invocation.exit_code = Some(exit_code(status));
    if let Some(mut sidecar) = legacy_trace {
        sidecar.retain(&root.join(invocation.trace.as_ref().unwrap()))?;
    }
    write_json(&manifest, &invocation)?;
    if let Ok(bytes) = fs::read(root.join(stdout)) {
        let _ = std::io::stdout().write_all(&bytes);
    }
    display_compiler_diagnostics(&read_optional(&root.join(stderr)));
    Ok(invocation)
}

/// Finalize a killed recorder using only retained artifacts, without running the project.
pub fn recover(store: &Store, id: &str) -> Result<Capture> {
    let mut capture = store.load(id)?;
    ensure!(
        capture.status == "running" || capture.status == "interrupted",
        "capture is already finalized"
    );
    let root = store.run_dir(&capture.id)?;
    let before = read_optional(&root.join("ninja.before.log"));
    let after = read_optional(&root.join("ninja.after.log"));
    let explanation = format!(
        "{}\n{}",
        read_optional(&root.join("build.stdout")),
        read_optional(&root.join("build.stderr"))
    );
    if !after.is_empty() {
        capture.actions = parse_actions(&before, &after, &capture.project, &explanation);
    }
    capture.status = "interrupted".into();
    capture.finished_ms = Some(now_ms());
    capture.notes.push("Recovered partial artifacts; absent timing or dependency data was not inferred from subsequent builds.".into());
    decode_invocations(store, &mut capture, false)?;
    capture.workload = Some(crate::workload::collect(&capture, &root));
    analysis::analyze(&mut capture, &explanation);
    store.save(&capture)?;
    Ok(capture)
}

/// Parse only new log rows and deduplicate command outputs belonging to one execution.
pub fn parse_actions(
    before: &str,
    after: &str,
    project: &ProjectSnapshot,
    explanation: &str,
) -> Vec<BuildAction> {
    let mut previous = HashMap::<&str, usize>::new();
    for line in before.lines() {
        *previous.entry(line).or_default() += 1;
    }
    let mut units_by_output = HashMap::with_capacity(project.units.len());
    for unit in &project.units {
        units_by_output.entry(unit.output.as_str()).or_insert(unit);
    }
    let mut actions = Vec::<BuildAction>::new();
    let build = Path::new(&project.build_dir);
    for line in after.lines().filter(|line| !line.starts_with('#')) {
        if let Some(count) = previous.get_mut(line).filter(|count| **count > 0) {
            *count -= 1;
            continue;
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() < 5 {
            continue;
        }
        let (Ok(start), Ok(end)) = (fields[0].parse::<u64>(), fields[1].parse::<u64>()) else {
            continue;
        };
        let output = path_string(resolve(build, fields[3]));
        let unit = units_by_output.get(output.as_str()).copied();
        let same_execution = actions.last().is_some_and(|action| {
            action.start_ms == start as f64
                && action.end_ms == end as f64
                && action.command_hash == fields[4]
        });
        if !same_execution {
            actions.push(BuildAction {
                id: String::new(),
                outputs: Vec::new(),
                unit_id: unit.map(|u| u.id.clone()),
                kind: classify_action(&output, unit, project.rules.get(&output)),
                clock_segment: 0,
                start_ms: start as f64,
                end_ms: end as f64,
                command_hash: fields[4].into(),
                reasons: Vec::new(),
                success: true,
            });
        }
        let action = actions.last_mut().unwrap();
        if output == path_string(build.join("build.ninja")) {
            action.kind = "configure".into();
        } else if action.unit_id.is_none() && unit.is_some() {
            action.unit_id = unit.map(|u| u.id.clone());
            action.kind = classify_action(&output, unit, project.rules.get(&output));
        }
        action.outputs.push(output);
    }
    // Ninja appends completed commands in order. A successful regeneration is a
    // barrier, after which Ninja may restart its elapsed clock. Align each segment
    // at prior completion without changing any native action duration.
    let mut segment = 0;
    for action in &mut actions {
        action.clock_segment = segment;
        if action.kind == "configure" {
            segment += 1;
        }
    }
    let mut completed = 0.0f64;
    for current in 0..=segment {
        let origin = actions
            .iter()
            .filter(|a| a.clock_segment == current)
            .map(|a| a.start_ms)
            .min_by(f64::total_cmp)
            .unwrap_or(0.0);
        let shift = if current == 0 {
            0.0
        } else {
            completed - origin
        };
        for action in actions.iter_mut().filter(|a| a.clock_segment == current) {
            action.start_ms += shift;
            action.end_ms += shift;
            completed = completed.max(action.end_ms);
        }
    }
    actions.sort_by(|a, b| {
        a.start_ms
            .total_cmp(&b.start_ms)
            .then(a.end_ms.total_cmp(&b.end_ms))
    });
    for (index, action) in actions.iter_mut().enumerate() {
        action.id = format!("action-{index}");
        action.outputs.sort();
        action.outputs.dedup();
        action.reasons = explanation
            .lines()
            .filter(|line| {
                line.starts_with("ninja explain: ")
                    && action.outputs.iter().any(|output| {
                        line.contains(output)
                            || Path::new(output)
                                .strip_prefix(build)
                                .ok()
                                .is_some_and(|p| line.contains(&path_string(p)))
                    })
            })
            .map(str::to_owned)
            .collect();
    }
    actions
}

fn execute(
    store: &Store,
    request: &RecordRequest,
    instrumented: bool,
    notes: Vec<String>,
) -> Result<(Capture, i32)> {
    let project = project::discover(&request.build_dir, &request.targets)?;
    let kind = if instrumented { "profile" } else { "record" };
    let previous = store.previous(&project.configuration_id, kind, &request.targets)?;
    let root_id = unique_id();
    let root = store.run_dir(&root_id)?;
    fs::create_dir_all(&root)?;
    let before_log = read_optional(&Path::new(&project.build_dir).join(".ninja_log"));
    fs::write(root.join("ninja.before.log"), &before_log)?;
    let mut before_paths = project::input_paths(&project);
    if let Some(previous) = &previous {
        before_paths.extend(previous.inputs.keys().cloned());
    }
    let before_inputs = project::snapshot(before_paths.iter().cloned());
    let mut capture = Capture {
        schema_version: SCHEMA_VERSION,
        id: root_id,
        started_ms: now_ms(),
        finished_ms: None,
        elapsed_ms: 0.0,
        status: "running".into(),
        kind: kind.into(),
        label: request.label.clone(),
        git: crate::git::observe(Path::new(&project.source_dir)),
        machine: machine_identity(),
        environment: compiler_environment(),
        cache_policy: if instrumented {
            "bypassed"
        } else {
            "preserved; outcomes recorded when supported"
        }
        .into(),
        profile_origin: fs::read(Path::new(&project.build_dir).join(".buildscope-profile.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| {
                value
                    .get("original_build")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            }),
        targets: request.targets.clone(),
        jobs: request.jobs,
        workload: None,
        project,
        actions: Vec::new(),
        profiles: Vec::new(),
        headers: Vec::new(),
        inputs: before_inputs.clone(),
        changes: Vec::new(),
        findings: Vec::new(),
        notes,
        summary: BuildSummary::default(),
    };
    if let Some(previous) = &previous {
        capture.changes = project::changes(&previous.inputs, &before_inputs, false);
    }
    if let Err(error) = crate::workload::prepare(&capture, &root) {
        capture
            .notes
            .push(format!("Workload command evidence unavailable: {error:#}"));
    }
    store.save(&capture)?;
    eprintln!("Buildscope {}: {}", kind, capture.id);
    let mut command = Command::new("ninja");
    command
        .arg("-C")
        .arg(&capture.project.build_dir)
        .args(["-d", "explain", "-d", "keeprsp", "-v"]);
    if let Some(jobs) = request.jobs {
        command.arg("-j").arg(jobs.to_string());
    }
    command.args(&request.targets);
    // ccache locks its per-build log, so parallel compilers can safely append without
    // sampling shared cache counters or resetting another build's statistics.
    command.env("CCACHE_STATSLOG", root.join("ccache.stats"));
    if instrumented {
        command
            .env("BUILDSCOPE_RUN_DIR", &root)
            .env(
                "BUILDSCOPE_DETAIL",
                if request.full_detail {
                    "full"
                } else {
                    "standard"
                },
            )
            .env(
                "BUILDSCOPE_COMPILERS",
                serde_json::to_string(&capture.project.compilers)?,
            )
            .env("CCACHE_DISABLE", "1")
            .env("SCCACHE_DISABLE", "1");
    }
    let start = Instant::now();
    let result = run_process(
        &mut command,
        &root.join("build.stdout"),
        &root.join("build.stderr"),
        true,
    );
    capture.elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    let code = match result {
        Ok(status) => exit_code(status),
        Err(error) => {
            capture
                .notes
                .push(format!("Build execution error: {error:#}"));
            1
        }
    };
    capture.status = if interrupted().load(Ordering::SeqCst) || code == 130 {
        "interrupted"
    } else if code == 0 {
        "success"
    } else {
        "failed"
    }
    .into();
    capture.finished_ms = Some(now_ms());
    let after_log = read_optional(&Path::new(&capture.project.build_dir).join(".ninja_log"));
    fs::write(root.join("ninja.after.log"), &after_log)?;
    let explanation = format!(
        "{}\n{}",
        read_optional(&root.join("build.stdout")),
        read_optional(&root.join("build.stderr"))
    );
    match project::discover(Path::new(&capture.project.build_dir), &request.targets) {
        Ok(project) => capture.project = project,
        Err(error) => capture
            .notes
            .push(format!("Post-build inventory is partial: {error:#}")),
    }
    capture.actions = parse_actions(&before_log, &after_log, &capture.project, &explanation);
    if capture.actions.iter().any(|a| a.clock_segment > 0) {
        capture.notes.push("Timeline segments after CMake regeneration are aligned at prior completion. Alignment is derived; native action durations are unchanged.".into());
    }
    let paths = project::input_paths(&capture.project)
        .into_iter()
        .chain(before_inputs.keys().cloned());
    capture.inputs = project::snapshot(paths);
    // A dependency first discovered after compilation was not necessarily created
    // during the build. Only paths in the pre-build inventory can support that claim.
    capture.changes.extend(
        project::changes(&before_inputs, &capture.inputs, true)
            .into_iter()
            .filter(|change| before_paths.contains(&change.path)),
    );
    if code != 0 {
        capture.notes.push("Ninja logs successful actions only; failed or interrupted actions can be absent from the timeline. Raw diagnostics are retained.".into());
    }
    if let Err(error) = decode_invocations(store, &mut capture, request.preprocess) {
        capture
            .notes
            .push(format!("Compiler detail is partial: {error:#}"));
    }
    let observed_paths = capture
        .profiles
        .iter()
        .flat_map(|profile| {
            profile
                .includes
                .iter()
                .map(|edge| edge.child.clone())
                .chain(profile.header_costs.iter().map(|cost| cost.path.clone()))
        })
        .filter(|path| !capture.inputs.contains_key(path))
        .collect::<std::collections::BTreeSet<_>>();
    capture.inputs.extend(project::snapshot(observed_paths));
    capture.workload = Some(crate::workload::collect(&capture, &root));
    analysis::analyze(&mut capture, &explanation);
    if capture
        .project
        .units
        .iter()
        .any(|u| u.source_key.contains("/Unity/"))
    {
        capture.notes.push("Unity sources are generated compilation units; original .cpp files can appear as textually included inputs in the header view.".into());
    }
    if capture
        .actions
        .iter()
        .any(|a| a.kind == "pch" || a.kind == "module")
    {
        capture.notes.push("PCH/module creation is a separate action. Reusing an artifact does not reparse every underlying header; per-header timing covers textual processing only.".into());
    }
    if capture.summary.dependency_units < capture.project.units.len() {
        capture.notes.push(format!("Dependency inventory covers {} of {} units. Counts describe available records, not a complete project inventory.",
            capture.summary.dependency_units, capture.project.units.len()));
    } else {
        capture.notes.push("Dependency reach comes from Ninja's available inventory; direct include paths are known only for instrumented invocations.".into());
    }
    if instrumented && capture.profiles.len() < capture.project.units.len() {
        capture.notes.push(format!("Compiler detail covers {} of {} inventory units; older dependency records can still describe additional units.",
            capture.profiles.len(),capture.project.units.len()));
    }
    if capture.summary.observed_units < capture.project.units.len() {
        capture.notes.push("Dependency records for unexecuted units can originate from earlier compilations and have not been revalidated against their current source in this capture.".into());
    }
    store.save(&capture)?;
    eprintln!(
        "Saved {}: {} actions, {} headers, {:.3}s elapsed. Open with: buildscope report {}",
        capture.id,
        capture.actions.len(),
        capture.headers.len(),
        capture.elapsed_ms / 1000.0,
        capture.id
    );
    Ok((capture, code))
}

fn copy_cache_key(key: &str, cache: &BTreeMap<String, String>) -> bool {
    if key.starts_with("@type:")
        || key.ends_with("_BINARY_DIR")
        || key.ends_with("_SOURCE_DIR")
        || key.ends_with("_IS_TOP_LEVEL")
        || key.ends_with("_COMPILER_LAUNCHER")
    {
        return false;
    }
    if cache.get(&format!("@type:{key}")).is_none() {
        return false;
    }
    if !key.starts_with("CMAKE_") {
        return true;
    }
    key.starts_with("CMAKE_CXX_FLAGS")
        || key.starts_with("CMAKE_C_FLAGS")
        || key.starts_with("CMAKE_EXE_LINKER_FLAGS")
        || key.starts_with("CMAKE_SHARED_LINKER_FLAGS")
        || key.starts_with("CMAKE_MODULE_LINKER_FLAGS")
        || key.starts_with("CMAKE_STATIC_LINKER_FLAGS")
        || key.starts_with("CMAKE_OSX_")
        || [
            "CMAKE_BUILD_TYPE",
            "CMAKE_CXX_COMPILER",
            "CMAKE_C_COMPILER",
            "CMAKE_TOOLCHAIN_FILE",
            "CMAKE_SYSROOT",
            "CMAKE_PREFIX_PATH",
            "CMAKE_FIND_ROOT_PATH",
            "CMAKE_POSITION_INDEPENDENT_CODE",
            "CMAKE_INTERPROCEDURAL_OPTIMIZATION",
            "CMAKE_CXX_STANDARD",
            "CMAKE_CXX_EXTENSIONS",
            "CMAKE_EXPORT_COMPILE_COMMANDS",
            "CMAKE_NINJA_FORCE_RESPONSE_FILE",
        ]
        .contains(&key)
}

fn classify_action(output: &str, unit: Option<&CompilationUnit>, rule: Option<&String>) -> String {
    if output.ends_with(".pch") || output.ends_with(".gch") {
        "pch"
    } else if output.ends_with(".pcm") || output.ends_with(".gcm") {
        "module"
    } else if unit.is_some() {
        "compile"
    } else if rule.is_some_and(|r| r.contains("LINKER") || r.contains("ARCHIVER")) {
        "link"
    } else {
        "generate"
    }
    .into()
}

pub(crate) fn compiler_environment() -> BTreeMap<String, String> {
    [
        "LANG",
        "LC_ALL",
        "SDKROOT",
        "CPATH",
        "CPLUS_INCLUDE_PATH",
        "C_INCLUDE_PATH",
        "MACOSX_DEPLOYMENT_TARGET",
        "CCACHE_DISABLE",
        "SCCACHE_DISABLE",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok().map(|value| (key.into(), value)))
    .collect()
}

pub(crate) fn machine_identity() -> String {
    let hostname = Command::new("uname")
        .arg("-n")
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    format!(
        "{}/{}/{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        &digest(hostname)[..12]
    )
}

fn interrupted() -> &'static Arc<AtomicBool> {
    static SIGNAL: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    SIGNAL.get_or_init(|| {
        let flag = Arc::new(AtomicBool::new(false));
        let handler = Arc::clone(&flag);
        let _ = ctrlc::set_handler(move || {
            handler.store(true, Ordering::SeqCst);
        });
        flag
    })
}

/// Own the temporary sidecar location required by older Clang drivers. A previous
/// file is moved aside before compilation so failed builds cannot reuse stale data.
struct LegacyTrace {
    /// Compiler-derived JSON path beside the original object or PCH output.
    path: PathBuf,
    /// Previous sidecar held in the same directory; None means no file existed.
    backup: Option<PathBuf>,
    /// Set after restoring the previous filesystem state, preventing repeated cleanup.
    restored: bool,
}

impl LegacyTrace {
    fn prepare(output: &Path, id: &str) -> Result<Self> {
        let path = output.with_extension("json");
        ensure!(
            path != output,
            "Clang trace would overwrite its compilation output"
        );
        let backup = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file() || metadata.is_symlink(),
                    "Clang trace path is not a file"
                );
                let backup = path.with_extension(format!("json.buildscope-{id}.backup"));
                fs::rename(&path, &backup).context("preserve existing Clang trace sidecar")?;
                Some(backup)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            path,
            backup,
            restored: false,
        })
    }

    fn retain(&mut self, destination: &Path) -> Result<()> {
        // Copy rather than rename: the store and build tree may use different filesystems.
        match fs::copy(&self.path, destination) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("retain Clang trace sidecar"),
        }
        self.restore()
    }

    fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        if let Some(backup) = &self.backup {
            fs::rename(backup, &self.path).context("restore previous Clang trace sidecar")?;
        } else {
            match fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.restored = true;
        Ok(())
    }
}

impl Drop for LegacyTrace {
    fn drop(&mut self) {
        // Also restore a previous sidecar if spawning or collecting the compiler failed.
        let _ = self.restore();
    }
}

fn run_process(
    command: &mut Command,
    stdout: &Path,
    stderr: &Path,
    display: bool,
) -> Result<ExitStatus> {
    interrupted().store(false, Ordering::SeqCst);
    let output_file = File::create(stdout)?;
    let error_file = File::create(stderr)?;
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command
        .spawn()
        .with_context(|| format!("start {command:?}"))?;
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let output_thread = std::thread::spawn(move || drain(out, output_file, false, display));
    let error_thread = std::thread::spawn(move || drain(err, error_file, true, display));
    let mut forwarded = None;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if interrupted().load(Ordering::SeqCst) {
            let first = forwarded.get_or_insert_with(|| {
                // The child owns its process group, so cancellation also reaches its descendants.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGINT);
                }
                Instant::now()
            });
            if first.elapsed() > Duration::from_secs(3) {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    output_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stdout collector panicked"))??;
    error_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stderr collector panicked"))??;
    Ok(status)
}

fn drain(mut input: impl Read, mut file: File, stderr: bool, display: bool) -> std::io::Result<()> {
    let mut bytes = [0; 8192];
    loop {
        let count = input.read(&mut bytes)?;
        if count == 0 {
            return Ok(());
        }
        file.write_all(&bytes[..count])?;
        if display {
            if stderr {
                let _ = std::io::stderr().write_all(&bytes[..count]);
            } else {
                let _ = std::io::stdout().write_all(&bytes[..count]);
            }
        }
    }
}

fn display_compiler_diagnostics(text: &str) {
    if let Some(document) = compiler::sarif_document(text) {
        if let Some(results) = document
            .pointer("/runs/0/results")
            .and_then(serde_json::Value::as_array)
        {
            for result in results {
                eprintln!(
                    "{}: {}",
                    result
                        .get("level")
                        .and_then(|v| v.as_str())
                        .unwrap_or("diagnostic"),
                    result
                        .pointer("/message/text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                );
            }
        }
    } else {
        for line in text.lines() {
            if line.contains("Time variable") {
                break;
            }
            let depth = line.chars().take_while(|c| *c == '.').count();
            if depth > 0 && line[depth..].starts_with(' ') {
                continue;
            }
            eprintln!("{line}");
        }
    }
}

fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
}
