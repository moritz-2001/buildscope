//! Compilation workload accounting from saved Ninja actions and capture-local ccache
//! evidence. Missing, ambiguous, failed, and unsupported outcomes remain unknown.
use crate::{model::*, util::*};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
    process::Command,
};

/// Freeze command classification and ccache prefix configuration before execution.
/// Recovery consumes this evidence instead of probing executables from saved commands.
pub(crate) fn prepare(capture: &Capture, root: &Path) -> Result<()> {
    let mut evidence = BTreeMap::new();
    let mut prefixes = BTreeMap::new();
    let mut compilers = BTreeMap::new();
    for unit in &capture.project.units {
        let Some(command) = capture.project.commands.get(&unit.output) else {
            continue;
        };
        let Some(arguments) = shlex::split(command) else {
            continue;
        };
        let is_direct = |arg: &String, compilers: &mut BTreeMap<_, _>| {
            *compilers
                .entry((arg.clone(), unit.directory.clone()))
                .or_insert_with(|| direct_compiler(arg, &unit.directory))
        };
        let kind = if arguments
            .first()
            .is_some_and(|arg| is_direct(arg, &mut compilers))
        {
            CommandKind::Direct
        } else if arguments
            .first()
            .is_some_and(|arg| executable_name(arg) == "ccache")
            && arguments
                .get(1)
                .is_some_and(|arg| is_direct(arg, &mut compilers))
        {
            let plain = *prefixes
                .entry((arguments[0].clone(), unit.directory.clone()))
                .or_insert_with(|| {
                    ["prefix_command", "prefix_command_cpp"].iter().all(|key| {
                        Command::new(&arguments[0])
                            .args(["--get-config", key])
                            .current_dir(&unit.directory)
                            .output()
                            .is_ok_and(|output| {
                                output.status.success()
                                    && output.stdout.iter().all(u8::is_ascii_whitespace)
                            })
                    })
                });
            CommandKind::Ccache { plain }
        } else {
            CommandKind::Unknown
        };
        evidence.insert(
            unit.id.clone(),
            CommandEvidence {
                command: command.clone(),
                kind,
            },
        );
    }
    write_json(&root.join("workload.commands.json"), &evidence)
}

// Kept private: this is retained collector evidence, not an independently editable policy.
#[derive(Serialize, Deserialize)]
struct CommandEvidence {
    command: String,
    kind: CommandKind,
}

#[derive(Serialize, Deserialize)]
enum CommandKind {
    Direct,
    Ccache { plain: bool },
    Unknown,
}

/// Collect counts while the capture's original commands and requested graph are available.
/// Only successful complete Ninja captures can establish that unexecuted units are current.
pub(crate) fn collect(capture: &Capture, root: &Path) -> CompilationWorkload {
    let mut actions = BTreeMap::<&str, Vec<&BuildAction>>::new();
    for action in &capture.actions {
        if let Some(unit) = action.unit_id.as_deref() {
            actions.entry(unit).or_default().push(action);
        }
    }
    let mut result = CompilationWorkload {
        executed_units: actions.keys().map(|id| (*id).to_owned()).collect(),
        notes: vec!["Counts are compilation units (source/output pairs), not unique source files. Cache hits require capture-local evidence; action duration never determines an outcome.".into()],
        ..CompilationWorkload::default()
    };
    let scoped: BTreeSet<_> = capture
        .project
        .units
        .iter()
        .filter(|unit| capture.project.graph.contains_key(&unit.output))
        .map(|unit| unit.id.as_str())
        .collect();
    if capture.kind != "launcher" && !capture.project.graph.is_empty() {
        result.total_units = Some(scoped.len());
        if capture.status == "success"
            && !capture
                .notes
                .iter()
                .any(|note| note.starts_with("Post-build inventory is partial"))
            && actions.keys().all(|id| scoped.contains(id))
        {
            result.up_to_date = Some(scoped.len().saturating_sub(actions.len()));
        }
    }
    let evidence: BTreeMap<String, CommandEvidence> = fs::read(root.join("workload.commands.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let mut ccache_units = BTreeMap::<&str, Vec<(&CompilationUnit, bool)>>::new();
    let mut sources = BTreeSet::new();
    for unit in &capture.project.units {
        let Some(executions) = actions.get(unit.id.as_str()) else {
            continue;
        };
        sources.insert(&unit.source);
        if executions.len() != 1 || !executions[0].success {
            result.unknown += 1;
            continue;
        }
        // A cache may restore trace artifacts too. Compiler scopes alone therefore do
        // not prove execution; only the managed profile explicitly bypasses caches.
        if capture.kind == "profile"
            && capture.cache_policy == "bypassed"
            && capture
                .profiles
                .iter()
                .any(|p| p.unit_id == unit.id && p.event_count > 0)
        {
            result.compiled += 1;
            continue;
        }
        let observation = evidence
            .get(&unit.id)
            .filter(|saved| capture.project.commands.get(&unit.output) == Some(&saved.command));
        match observation.map(|saved| &saved.kind) {
            Some(CommandKind::Direct) => result.compiled += 1,
            Some(CommandKind::Ccache { plain }) => ccache_units
                .entry(&unit.source)
                .or_default()
                .push((unit, *plain)),
            _ => result.unknown += 1,
        }
    }
    result.source_files = sources.len();
    let records = parse_ccache(&read_optional(&root.join("ccache.stats")));
    for (source, units) in ccache_units {
        let matches: Vec<_> = records
            .iter()
            .filter(|(path, _)| {
                units
                    .iter()
                    .any(|(unit, _)| resolve(&unit.directory, path) == Path::new(source))
            })
            .map(|(_, outcome)| *outcome)
            .collect();
        // A source can be compiled into several targets. Aggregate only when every
        // successful action has one record; never assign a record to an arbitrary output.
        if matches.len() == units.len() {
            for outcome in matches {
                match outcome {
                    Outcome::Hit => result.cache_hits += 1,
                    Outcome::Compiled if units.iter().all(|(_, plain)| *plain) => {
                        result.compiled += 1
                    }
                    Outcome::Compiled => result.unknown += 1,
                    Outcome::Unknown => result.unknown += 1,
                }
            }
        } else {
            result.unknown += units.len();
        }
    }
    // An action without an inventory match cannot silently disappear from accounting.
    result.unknown += actions
        .keys()
        .filter(|id| !capture.project.units.iter().any(|unit| unit.id == **id))
        .count();
    if capture.status != "success" {
        result.notes.push("Partial capture: Ninja logs completed successful actions only. Failed or unfinished compilations can be absent; counts are lower bounds.".into());
    }
    if result.unknown > 0 {
        result.notes.push("Unknown includes unsupported wrappers/caches, missing or ambiguous ccache logs, and partial executions. It is not zero compilation cost.".into());
    }
    if result.up_to_date.is_none() {
        result.notes.push("Up-to-date counts are unavailable for partial builds, missing scope, and launcher batches.".into());
    }
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Hit,
    Compiled,
    Unknown,
}

// ccache writes one locked block per invocation: '# source' followed by result IDs.
// Storage hit counters are deliberately ignored: only final direct/preprocessed results
// prove a compiler-cache hit. A miss is accepted only for a successful Ninja action.
fn parse_ccache(text: &str) -> Vec<(String, Outcome)> {
    let mut records = Vec::new();
    let mut source = None;
    let mut counters = BTreeSet::new();
    let finish = |source: Option<String>, counters: &BTreeSet<&str>, records: &mut Vec<_>| {
        if let Some(source) = source {
            let hit = counters.contains("direct_cache_hit")
                || counters.contains("preprocessed_cache_hit");
            let miss = counters.contains("cache_miss");
            let outcome = match (hit, miss) {
                (true, false) => Outcome::Hit,
                (false, true) => Outcome::Compiled,
                _ => Outcome::Unknown,
            };
            records.push((source, outcome));
        }
    };
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("# ") {
            finish(source.take(), &counters, &mut records);
            counters.clear();
            source = Some(path.to_owned());
        } else if !line.is_empty() {
            counters.insert(line);
        }
    }
    finish(source, &counters, &mut records);
    records
}

fn executable_name(executable: &str) -> &str {
    Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
}

fn direct_compiler(executable: &str, directory: &str) -> bool {
    let path = if Path::new(executable).components().count() > 1 {
        resolve(directory, executable)
    } else {
        let Some(path) = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|directory| directory.join(executable))
                .find(|path| path.is_file())
        }) else {
            return false;
        };
        path
    };
    // Follow masquerading ccache symlinks before accepting a compiler command. Shell
    // scripts and unknown launchers remain unknown even if named like a compiler.
    let Ok(path) = fs::canonicalize(path) else {
        return false;
    };
    let name = executable_name(path.to_str().unwrap_or(""));
    let compiler_name = ["clang", "clang++", "gcc", "g++", "cc", "c++"]
        .iter()
        .any(|base| {
            name == *base
                || name
                    .strip_prefix(&format!("{base}-"))
                    .is_some_and(|version| version.starts_with(|c: char| c.is_ascii_digit()))
        });
    let mut magic = [0; 4];
    compiler_name
        && fs::File::open(path)
            .and_then(|mut file| file.read_exact(&mut magic))
            .is_ok()
        && [
            *b"\x7fELF",
            [0xcf, 0xfa, 0xed, 0xfe],
            [0xca, 0xfe, 0xba, 0xbe],
            [0xce, 0xfa, 0xed, 0xfe],
        ]
        .contains(&magic)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_final_cache_results_establish_hits_and_misses() {
        let parsed = parse_ccache(
            "# a.cpp\nlocal_storage_hit\ncache_miss\n# b.cpp\ndirect_cache_hit\n# c.cpp\npreprocessed_cache_hit\n# d.cpp\nlocal_storage_hit\n# e.cpp\ncache_miss\ndirect_cache_hit\n",
        );
        assert_eq!(
            parsed
                .iter()
                .map(|(_, outcome)| *outcome)
                .collect::<Vec<_>>(),
            [
                Outcome::Compiled,
                Outcome::Hit,
                Outcome::Hit,
                Outcome::Unknown,
                Outcome::Unknown
            ]
        );
    }
}
