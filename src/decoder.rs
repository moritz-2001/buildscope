//! Invocation decoding and compression run in a bounded pool. The calling thread
//! publishes results and writes SQLite in manifest order, preserving retry semantics.
use crate::{
    model::*,
    project,
    store::{PreparedEvents, Store},
    util::*,
    worker_pool,
};
use anyhow::{Result, ensure};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufReader, Write},
    path::Path,
    process::Command,
    thread,
};

/// Decode retained invocations without acknowledging the launcher spool. At most four
/// invocations are active or waiting for publication; completed event vectors never
/// accumulate behind an arbitrarily slow earlier invocation.
pub(crate) fn decode_invocations(
    store: &Store,
    capture: &mut Capture,
    preprocess: bool,
) -> Result<()> {
    let root = store.run_dir(&capture.id)?;
    let directory = root.join("invocations");
    if !directory.exists() {
        return Ok(());
    }
    let mut entries = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries
        .retain(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"));
    entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    capture.profiles.clear();
    if entries.is_empty() {
        return Ok(());
    }
    let mut units_by_output = HashMap::with_capacity(capture.project.units.len());
    for unit in &capture.project.units {
        units_by_output.entry(unit.output.as_str()).or_insert(unit);
    }
    // Indices remain stable: repeated units replace profiles in place; new units append.
    let mut profiles_by_unit = HashMap::<String, usize>::new();
    let workers = thread::available_parallelism()
        .map_or(1, |count| count.get())
        .min(4);
    let run = &capture.id;
    let mut writer = store.event_writer()?;
    worker_pool::process_ordered(
        &entries,
        workers,
        |manifest| prepare_invocation(store, run, manifest, &root, &units_by_output, preprocess),
        |decoded| {
            match decoded {
                DecodedInvocation::Note(note) => capture.notes.push(note),
                DecodedInvocation::Profile(decoded) => {
                    let DecodedProfile {
                        profile,
                        events,
                        artifact,
                        source_key,
                    } = *decoded;
                    // Only this thread replaces artifacts and reuses the SQLite writer.
                    writer.save(artifact, &events)?;
                    if let Some(&index) = profiles_by_unit.get(&profile.unit_id) {
                        capture.notes.push(format!("Multiple compiler invocations for {source_key}; named detail uses the latest decoded invocation. Raw evidence for all attempts is retained."));
                        capture.profiles[index] = profile;
                    } else {
                        profiles_by_unit.insert(profile.unit_id.clone(), capture.profiles.len());
                        capture.profiles.push(profile);
                    }
                }
            }
            Ok(())
        },
    )
}

enum DecodedInvocation {
    Note(String),
    Profile(Box<DecodedProfile>),
}

/// One worker result. The temporary artifact is owned until the serial writer
/// publishes it; dropping any unconsumed result removes its unpublished event file.
struct DecodedProfile {
    profile: CompilationProfile,
    events: Vec<TraceEvent>,
    artifact: PreparedEvents,
    source_key: String,
}

fn prepare_invocation(
    store: &Store,
    run: &str,
    manifest: &Path,
    root: &Path,
    units_by_output: &HashMap<&str, &CompilationUnit>,
    preprocess: bool,
) -> Result<DecodedInvocation> {
    let mut invocation: Invocation =
        match serde_json::from_reader(BufReader::new(File::open(manifest)?)) {
            Ok(value) => value,
            Err(error) => {
                return Ok(DecodedInvocation::Note(format!(
                    "Incomplete invocation manifest: {error}"
                )));
            }
        };
    let Some(unit) = invocation
        .output
        .as_deref()
        .and_then(|output| units_by_output.get(output).copied())
    else {
        return Ok(DecodedInvocation::Note(format!(
            "Instrumented output was not in the compilation inventory: {:?}",
            invocation.output
        )));
    };
    let (mut profile, events) = match crate::compiler::decode(&invocation, unit, root) {
        Ok(decoded) => decoded,
        Err(error) => {
            return Ok(DecodedInvocation::Note(format!(
                "Detail unavailable for {}: {error:#}",
                unit.source_key
            )));
        }
    };
    if let Some(trace) = invocation
        .trace
        .clone()
        .filter(|path| !path.ends_with(".zst"))
        && let Err(error) = compress_trace(manifest, root, &trace, &mut invocation, &mut profile)
    {
        profile
            .notes
            .push(format!("Raw trace compression unavailable: {error:#}"));
    }
    if preprocess && invocation.exit_code == Some(0) {
        match preprocessing_metrics(&invocation, root) {
            Ok(metrics) => profile.phases.extend(metrics),
            Err(error) => profile
                .notes
                .push(format!("Preprocessing probe unavailable: {error:#}")),
        }
    }
    let artifact = store.prepare_events(run, &unit.id, &events)?;
    Ok(DecodedInvocation::Profile(Box::new(DecodedProfile {
        profile,
        events,
        artifact,
        source_key: unit.source_key.clone(),
    })))
}

fn compress_trace(
    manifest: &Path,
    root: &Path,
    trace: &str,
    invocation: &mut Invocation,
    profile: &mut CompilationProfile,
) -> Result<()> {
    let source = artifact_path(root, trace)?;
    let compressed = format!("{trace}.zst");
    let destination = artifact_path(root, &compressed)?;
    let temporary = destination.with_extension(format!("{}.tmp", unique_id()));
    let result = (|| -> Result<()> {
        let mut input = File::open(&source)?;
        let mut encoder = zstd::stream::write::Encoder::new(File::create(&temporary)?, 3)?;
        std::io::copy(&mut input, &mut encoder)?;
        encoder.finish()?;
        fs::rename(&temporary, &destination)?;
        // Publish the new manifest before deleting its old trace. Either manifest
        // version therefore retains a readable trace if collection is interrupted.
        invocation.trace = Some(compressed.clone());
        write_json(manifest, invocation)?;
        profile.artifact = compressed;
        fs::remove_file(source)?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}

fn preprocessing_metrics(invocation: &Invocation, root: &Path) -> Result<Vec<Metric>> {
    let output = root.join("raw").join(format!("{}.ii", invocation.id));
    let mut arguments = Vec::new();
    let mut skip = false;
    let (expanded, _) = project::expand_responses(
        &invocation.arguments,
        Path::new(&invocation.directory),
        &invocation.response_files,
    )?;
    for arg in &expanded[1..] {
        if skip {
            skip = false;
            continue;
        }
        if ["-o", "-MF", "-MT", "-MQ"].contains(&arg.as_str()) {
            skip = true;
            continue;
        }
        if ["-c", "-MD", "-MMD", "-MP", "-H"].contains(&arg.as_str())
            || arg.starts_with("-ftime-")
            || arg.starts_with("-fdiagnostics-format=")
            || arg.starts_with("-fmodule-output=")
        {
            continue;
        }
        arguments.push(arg.clone());
    }
    let result = Command::new(&invocation.arguments[0])
        .args(arguments)
        .args(["-E", "-o"])
        .arg(&output)
        .current_dir(&invocation.directory)
        .output()?;
    ensure!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let bytes = fs::read(&output)?;
    let count = bytes.iter().filter(|&&b| b == b'\n').count();
    let mut compressed =
        zstd::stream::write::Encoder::new(File::create(output.with_extension("ii.zst"))?, 3)?;
    compressed.write_all(&bytes)?;
    compressed.finish()?;
    fs::remove_file(output)?;
    Ok(vec![
        Metric {
            name: "Preprocessed output size".into(),
            value: bytes.len() as f64,
            unit: "bytes".into(),
            origin: "measured".into(),
            coverage: "separate preprocessing run; size is not parsing difficulty".into(),
        },
        Metric {
            name: "Preprocessed output lines".into(),
            value: count as f64,
            unit: "count".into(),
            origin: "measured".into(),
            coverage: "includes line directives; not a token count".into(),
        },
    ])
}
