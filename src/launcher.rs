//! Public CMake compiler launcher and explicit batch collection. Compiler processes
//! write independent spool directories; only collection discovers Ninja and indexes data.
use crate::{analysis, capture, compiler, model::*, project, store::Store, util::*};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    os::{fd::AsRawFd, unix::process::ExitStatusExt},
    path::{Path, PathBuf},
    process::Command,
};

/// Options installed in CMake's C/C++ compiler launcher list.
pub struct LauncherRequest {
    /// Configured root build directory; None searches the invocation's ancestors.
    pub build_dir: Option<PathBuf>,
    /// Zero-threshold Clang detail; false retains the standard trace cutoff.
    pub full_detail: bool,
    /// Actual compiler followed by its original arguments, without shell interpretation.
    pub arguments: Vec<String>,
}

/// Collect one compiler invocation during an ordinary build, preserving its exit code.
/// Configure probes outside a finished Ninja tree pass through without recording.
pub fn launch(request: &LauncherRequest) -> Result<i32> {
    ensure!(
        !request.arguments.is_empty(),
        "launch requires -- COMPILER [ARGUMENTS...]"
    );
    let directory = std::env::current_dir()?;
    let scratch = directory
        .components()
        .any(|part| matches!(part.as_os_str().to_str(), Some("CMakeScratch" | "CMakeTmp")));
    let build = request
        .build_dir
        .as_ref()
        .map(|path| resolve(&directory, path))
        .or_else(|| {
            directory
                .ancestors()
                .find(|p| p.join("build.ninja").is_file())
                .map(Path::to_owned)
        });
    let Some(build) = build.filter(|path| !scratch && path.join("build.ninja").is_file()) else {
        return forward(&request.arguments);
    };
    let (arguments, _) =
        project::expand_responses(&request.arguments, &directory, &BTreeMap::new())?;
    if !arguments.iter().any(|arg| arg == "-c") || output_argument(&arguments).is_none() {
        return forward(&request.arguments);
    }
    let spool = build.join(".buildscope-launcher");
    fs::create_dir_all(spool.join("pending"))?;
    let info = cached_compiler(&arguments[0], &directory, &spool)?;
    let id = unique_id();
    let root = spool.join("pending").join(&id);
    fs::create_dir(&root)?;
    let invocation = capture::run_compiler(
        request.arguments.clone(),
        &root,
        request.full_detail,
        std::slice::from_ref(&info),
        true,
    )?;
    let source = source_argument(&arguments).map(|path| path_string(resolve(&directory, path)));
    let includes = compiler::parse_includes(
        &read_optional(&root.join(&invocation.stderr)),
        source.as_deref().unwrap_or(""),
        &directory,
    );
    let paths = source
        .iter()
        .cloned()
        .chain(includes.into_iter().map(|edge| edge.child));
    let inputs = project::snapshot(paths);
    let observation = Observation {
        schema_version: SCHEMA_VERSION,
        id,
        build_dir: path_string(&build),
        compiler: info,
        arguments,
        source,
        environment: capture::compiler_environment(),
        machine: capture::machine_identity(),
        inputs,
        invocation,
    };
    // This is the completion marker: collectors never read a live compiler's artifacts.
    write_json(&root.join("observation.json"), &observation)?;
    Ok(observation.invocation.exit_code.unwrap_or(1))
}

/// Assemble completed, uncollected launcher observations without executing a build.
/// A batch can span several builds; no complete build duration or native cause is inferred.
pub fn collect(store: &Store, build_dir: &Path, label: Option<String>) -> Result<Option<Capture>> {
    let build = build_dir
        .canonicalize()
        .context("build directory does not exist")?;
    let spool = build.join(".buildscope-launcher");
    let pending = spool.join("pending");
    if !pending.exists() {
        return Ok(None);
    }
    let _lock = collection_lock(&spool)?;
    let build_key = path_string(&build);
    let receipts = store.launcher_receipts(&build_key)?;
    let mut observations = Vec::new();
    let mut incomplete = 0;
    for entry in fs::read_dir(&pending)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        if receipts.contains(&id) {
            // A prior collector committed but was interrupted before spool cleanup.
            fs::remove_dir_all(entry.path())?;
            continue;
        }
        let marker = entry.path().join("observation.json");
        if !marker.is_file() {
            incomplete += 1;
            continue;
        }
        let observation: Observation = serde_json::from_reader(File::open(marker)?)?;
        ensure!(
            observation.schema_version == SCHEMA_VERSION,
            "unsupported launcher schema"
        );
        ensure!(
            observation.id == id && observation.build_dir == build_key,
            "launcher observation identity mismatch"
        );
        observations.push((entry.path(), observation));
    }
    if observations.is_empty() {
        if incomplete > 0 {
            eprintln!(
                "{incomplete} unfinished launcher invocations retained; collect again after the build finishes."
            );
        }
        return Ok(None);
    }
    observations.sort_by(|a, b| {
        a.1.invocation
            .started_ms
            .cmp(&b.1.invocation.started_ms)
            .then(a.1.id.cmp(&b.1.id))
    });
    let ids: Vec<_> = observations.iter().map(|(_, o)| o.id.clone()).collect();
    let mut inventory = project::discover(&build, &[])?;
    let first = &observations[0].1;
    let started = observations
        .iter()
        .filter_map(|(_, o)| o.invocation.started_ms)
        .min()
        .unwrap_or_else(now_ms);
    let mut run = Capture {
        schema_version: SCHEMA_VERSION,
        id: format!("launcher-{}", &digest(serde_json::to_vec(&ids)?)[..32]),
        started_ms: started, finished_ms: None, elapsed_ms: 0.0, status: "running".into(), kind: "launcher".into(),
        label, machine: first.machine.clone(), environment: first.environment.clone(),
        cache_policy: "preserved; individual outcomes unknown".into(), profile_origin: None, targets: Vec::new(),
        project: inventory.clone(), actions: Vec::new(), profiles: Vec::new(), headers: Vec::new(),
        inputs: BTreeMap::new(), changes: Vec::new(), findings: Vec::new(), summary: BuildSummary::default(),
        notes: vec![
            "Launcher batch: includes completed compiler invocations since the last collection, possibly from several builds. Collect after each build for separate history entries.".into(),
            "Elapsed time is the observed compiler span, not total build duration. Links, generators, no-op builds, requested targets, and native Ninja invalidation explanations are unavailable.".into(),
            "Compiler instrumentation adds overhead. Existing caches and outputs are preserved; cache hit outcomes and skipped invocations are unknown.".into(),
            "Batch status describes observed compiler exits only; a later link or build step may have failed without appearing here.".into(),
            "Dependency inventory is read at collection time and can contain older records. Source fingerprints were taken after each compiler invocation, not at collection time.".into(),
        ],
    };
    if incomplete > 0 {
        run.notes.push(format!(
            "{incomplete} unfinished invocations remain in the spool; this batch excludes them."
        ));
    }
    let root = store.run_dir(&run.id)?;
    fs::create_dir_all(&root)?;
    let mut mismatches = BTreeSet::new();
    let mut environments = BTreeSet::new();
    let mut machines = BTreeSet::new();
    for (directory, observation) in &observations {
        let invocation = &observation.invocation;
        inventory
            .response_files
            .extend(invocation.response_files.clone());
        environments.insert(serde_json::to_string(&observation.environment)?);
        machines.insert(observation.machine.clone());
        let output = invocation
            .output
            .as_ref()
            .context("launcher output is missing")?;
        let source = observation
            .source
            .as_ref()
            .or_else(|| {
                inventory
                    .units
                    .iter()
                    .find(|u| &u.output == output)
                    .map(|u| &u.source)
            })
            .context("launcher source is missing")?
            .clone();
        let command_id = project::semantic_command(
            &observation.arguments,
            &inventory.source_dir,
            &inventory.build_dir,
        );
        let source_key = path_key(&source, &inventory.source_dir, &inventory.build_dir);
        let output_key = Path::new(output)
            .strip_prefix(&build)
            .map(path_string)
            .unwrap_or_else(|_| output.clone());
        let unit_index = inventory.units.iter().position(|u| &u.output == output);
        if unit_index.is_none_or(|index| inventory.units[index].command_id != command_id) {
            mismatches.insert(format!("{output}:{command_id}"));
        }
        let compiler_index = inventory
            .compilers
            .iter()
            .position(|c| {
                c.executable == observation.compiler.executable
                    && c.version == observation.compiler.version
            })
            .unwrap_or_else(|| {
                inventory.compilers.push(observation.compiler.clone());
                inventory.compilers.len() - 1
            });
        let unit = CompilationUnit {
            id: digest(format!("{source_key}\n{output_key}"))[..16].into(),
            source,
            source_key,
            output: output.clone(),
            output_key,
            directory: invocation.directory.clone(),
            arguments: observation.arguments.clone(),
            compiler: compiler_index,
            command_id,
        };
        let unit_id = unit.id.clone();
        if let Some(index) = unit_index {
            inventory.units[index] = unit;
        } else {
            inventory.units.push(unit);
        }
        let start = invocation
            .started_ms
            .unwrap_or(started)
            .saturating_sub(started) as f64;
        let end = start + invocation.elapsed_ms;
        run.elapsed_ms = run.elapsed_ms.max(end);
        run.actions.push(BuildAction {
            id: observation.id.clone(),
            outputs: vec![output.clone()],
            unit_id: Some(unit_id),
            kind: if output.ends_with(".pch") || output.ends_with(".gch") {
                "pch"
            } else if output.ends_with(".pcm") {
                "module"
            } else {
                "compile"
            }
            .into(),
            clock_segment: 0,
            start_ms: start,
            end_ms: end,
            command_hash: String::new(),
            reasons: Vec::new(),
            success: invocation.exit_code == Some(0),
        });
        for (path, stamp) in &observation.inputs {
            if run.inputs.get(path).is_some_and(|prior| prior != stamp) {
                run.notes.push(format!("Input changed between recorded invocations: {path}. Source preview uses the latest fingerprint in this batch."));
            }
            run.inputs.insert(path.clone(), stamp.clone());
        }
        copy_artifacts(directory, &root)?;
        write_json(
            &root
                .join("observations")
                .join(format!("{}.json", observation.id)),
            observation,
        )?;
    }
    inventory.units.sort_by(|a, b| a.id.cmp(&b.id));
    if !mismatches.is_empty() {
        run.notes.push("Recorded commands differ from the current compilation inventory. Actual invocation commands are retained; collect before reconfiguring the build to avoid mixed configurations.".into());
        inventory.configuration_id =
            digest(format!("{}\n{mismatches:?}", inventory.configuration_id));
    }
    if environments.len() > 1 || machines.len() > 1 {
        run.notes.push("This batch mixes recorded environments or hosts; comparisons must not treat it as one controlled measurement.".into());
        inventory.configuration_id = digest(format!(
            "{}\n{environments:?}\n{machines:?}",
            inventory.configuration_id
        ));
    }
    run.project = inventory;
    let previous = store.previous(&run.project.configuration_id, "launcher", &[])?;
    if let Some(previous) = previous {
        // Missing paths in a focused batch are unobserved, not removed inputs.
        run.changes = project::changes(&previous.inputs, &run.inputs, false)
            .into_iter()
            .filter(|change| {
                previous.inputs.contains_key(&change.path) && run.inputs.contains_key(&change.path)
            })
            .collect();
    }
    store.save(&run)?;
    capture::decode_invocations(store, &mut run, false)?;
    run.finished_ms = Some(started.saturating_add(run.elapsed_ms.ceil() as u64));
    run.status = if observations
        .iter()
        .any(|(_, o)| o.invocation.exit_code == Some(130))
    {
        "interrupted"
    } else if observations
        .iter()
        .any(|(_, o)| o.invocation.exit_code != Some(0))
    {
        "failed"
    } else {
        "success"
    }
    .into();
    analysis::analyze(&mut run, "");
    // A compiler-only batch has no observed generator/link dependency chain.
    run.summary.dependency_path.clear();
    run.summary.dependency_path_ms = 0.0;
    store.save_launcher(&run, &build_key, &ids)?;
    for (directory, _) in &observations {
        fs::remove_dir_all(directory)?;
    }
    Ok(Some(run))
}

#[derive(Serialize, Deserialize)]
struct Observation {
    schema_version: u32,
    id: String,
    build_dir: String,
    compiler: CompilerInfo,
    arguments: Vec<String>,
    source: Option<String>,
    environment: BTreeMap<String, String>,
    machine: String,
    inputs: BTreeMap<String, FileStamp>,
    invocation: Invocation,
}

fn forward(arguments: &[String]) -> Result<i32> {
    let status = Command::new(&arguments[0]).args(&arguments[1..]).status()?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
}

fn source_argument(arguments: &[String]) -> Option<&str> {
    let after_compile = arguments
        .iter()
        .position(|arg| arg == "-c")
        .and_then(|index| arguments.get(index + 1));
    if let Some(source) = after_compile.filter(|arg| !arg.starts_with('-')) {
        return Some(source);
    }
    arguments
        .iter()
        .rev()
        .find(|arg| {
            !arg.starts_with('-')
                && Path::new(arg)
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| {
                        [
                            "c", "cc", "cpp", "cxx", "C", "mm", "m", "ixx", "cppm", "h", "hxx",
                            "hpp",
                        ]
                        .contains(&s)
                    })
        })
        .map(String::as_str)
}

fn cached_compiler(executable: &str, directory: &Path, spool: &Path) -> Result<CompilerInfo> {
    let executable_path = if executable.contains('/') {
        resolve(directory, executable)
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|p| p.join(executable))
            .find(|p| p.is_file())
            .unwrap_or_else(|| directory.join(executable))
    };
    let metadata = fs::metadata(&executable_path).ok();
    let key = digest(format!(
        "{}:{:?}:{:?}",
        executable_path.display(),
        metadata.as_ref().and_then(|m| m.modified().ok()),
        metadata.as_ref().map(|m| m.len())
    ));
    let directory_path = spool.join("compilers");
    fs::create_dir_all(&directory_path)?;
    let path = directory_path.join(format!("{key}.json"));
    if let Ok(file) = File::open(&path)
        && let Ok(info) = serde_json::from_reader(file)
    {
        return Ok(info);
    }
    let info = compiler::identify(executable, directory);
    write_json(&path, &info)?;
    Ok(info)
}

fn collection_lock(spool: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(spool.join("collect.lock"))?;
    // Kernel locks release on crashes; launchers do not acquire this collector-only lock.
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "another collection is active for this build directory"
    );
    Ok(file)
}

fn copy_artifacts(source: &Path, destination: &Path) -> Result<()> {
    for child in ["raw", "invocations"] {
        let from = source.join(child);
        if !from.exists() {
            continue;
        }
        fs::create_dir_all(destination.join(child))?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            ensure!(
                entry.file_type()?.is_file(),
                "launcher artifact is not a regular file"
            );
            fs::copy(
                entry.path(),
                destination.join(child).join(entry.file_name()),
            )?;
        }
    }
    Ok(())
}
