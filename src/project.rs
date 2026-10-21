//! CMake/Ninja discovery. ProjectSnapshot preserves flat dependencies separately
//! from include nesting and normalizes build relocation without losing commands.
use crate::{compiler, model::*, util::*};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::Command,
};

/// Discover a configured Ninja build without changing its configuration.
pub fn discover(build_dir: &Path, targets: &[String]) -> Result<ProjectSnapshot> {
    let build = build_dir
        .canonicalize()
        .context("build directory does not exist")?;
    if !build.join("build.ninja").exists() {
        bail!("{} is not a configured Ninja build", build.display());
    }
    let cache = read_cache(&build.join("CMakeCache.txt"))?;
    let source = cache
        .get("CMAKE_HOME_DIRECTORY")
        .context("CMake source root missing from CMakeCache.txt")?;
    let source = path_string(resolve(&build, source));
    let build_string = path_string(&build);
    let native_database = checked(
        Command::new("ninja")
            .arg("-C")
            .arg(&build)
            .args(["-t", "compdb"]),
    )?
    .stdout;
    let expanded_database = checked(
        Command::new("ninja")
            .arg("-C")
            .arg(&build)
            .args(["-t", "compdb", "-x"]),
    )?
    .stdout;
    let native_entries: Vec<DatabaseEntry> = serde_json::from_slice(&native_database)?;
    let expanded_entries: Vec<DatabaseEntry> = serde_json::from_slice(&expanded_database)?;
    let commands: BTreeMap<String, String> = native_entries
        .iter()
        .filter_map(|entry| {
            Some((
                path_string(resolve(&build, entry.output.as_ref()?)),
                entry.command.clone()?,
            ))
        })
        .collect();
    let expanded_commands: BTreeMap<String, String> = expanded_entries
        .iter()
        .filter_map(|entry| {
            Some((
                path_string(resolve(&build, entry.output.as_ref()?)),
                entry.command.clone()?,
            ))
        })
        .collect();
    let mut response_files = BTreeMap::new();
    for entry in &native_entries {
        if let Some(arguments) = entry.command.as_deref().and_then(shlex::split)
            && let Ok((_, files)) =
                expand_responses(&arguments, Path::new(&entry.directory), &BTreeMap::new())
        {
            response_files.extend(files);
        }
    }
    let database_path = build.join("compile_commands.json");
    let database = if database_path.exists() {
        fs::read(&database_path)?
    } else {
        expanded_database
    };
    let entries: Vec<DatabaseEntry> =
        serde_json::from_slice(&database).context("read compilation database")?;
    let mut compilers = Vec::<CompilerInfo>::new();
    let mut compiler_ids = BTreeMap::new();
    let mut units = Vec::new();
    for entry in entries {
        let directory = path_string(resolve(&build, &entry.directory));
        let source_file = path_string(resolve(&directory, &entry.file));
        let extension = Path::new(&source_file)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let mut arguments = match entry.arguments {
            Some(args) => args,
            None => shlex::split(entry.command.as_deref().unwrap_or(""))
                .context("invalid quoting in compilation database")?,
        };
        strip_launchers(&mut arguments);
        if arguments.iter().any(|arg| arg.starts_with('@')) {
            if let Ok((expanded, _)) =
                expand_responses(&arguments, Path::new(&directory), &response_files)
            {
                arguments = expanded;
            } else if let Some(command) = entry
                .output
                .as_ref()
                .and_then(|output| expanded_commands.get(&path_string(resolve(&directory, output))))
            {
                arguments =
                    shlex::split(command).context("invalid expanded response-file command")?;
            }
        }
        strip_launchers(&mut arguments);
        if !["c", "cc", "cpp", "cxx", "C", "mm", "m", "ixx", "cppm"].contains(&extension)
            || !arguments.iter().any(|arg| arg == "-c")
        {
            continue;
        }
        let executable = arguments
            .first()
            .context("empty compilation command")?
            .clone();
        let compiler_index = if let Some(index) = compiler_ids.get(&executable) {
            *index
        } else {
            let index = compilers.len();
            compilers.push(compiler::identify(&executable, Path::new(&directory)));
            compiler_ids.insert(executable, index);
            index
        };
        let output = entry
            .output
            .or_else(|| output_argument(&arguments))
            .context("compilation has no output argument")?;
        let output = path_string(resolve(&directory, output));
        let output_key = Path::new(&output)
            .strip_prefix(&build)
            .map(path_string)
            .unwrap_or_else(|_| output.clone());
        let source_key = path_key(&source_file, &source, &build_string);
        let id = digest(format!("{source_key}\n{output_key}"));
        let command_id = semantic_command(&arguments, &source, &build_string);
        units.push(CompilationUnit {
            id: id[..16].into(),
            source: source_file,
            source_key,
            output,
            output_key,
            directory,
            arguments,
            compiler: compiler_index,
            command_id,
        });
    }
    units.sort_by(|a, b| a.id.cmp(&b.id));
    units.dedup_by(|a, b| a.id == b.id);
    let dependencies = read_dependencies(&build)?;
    let NinjaGraph {
        dependencies: graph,
        rules,
    } = read_ninja_graph(&build, targets)?;
    let link_commands: Vec<_> = expanded_commands
        .iter()
        .filter(|(output, _)| {
            rules
                .get(*output)
                .is_some_and(|rule| rule.contains("LINKER") || rule.contains("ARCHIVER"))
        })
        .map(|(output, command)| {
            (
                path_key(output, &source, &build_string),
                semantic_command(
                    &shlex::split(command).unwrap_or_default(),
                    &source,
                    &build_string,
                ),
            )
        })
        .collect();
    let project_id = project_identity(Path::new(&source));
    let configuration_id = digest(serde_json::to_vec(&(
        &project_id,
        compilers
            .iter()
            .map(|c| (&c.family, &c.version))
            .collect::<Vec<_>>(),
        units
            .iter()
            .map(|u| (&u.id, &u.command_id))
            .collect::<Vec<_>>(),
        cache.get("CMAKE_BUILD_TYPE"),
        link_commands,
    ))?);
    Ok(ProjectSnapshot {
        source_dir: source,
        build_dir: build_string,
        project_id,
        configuration_id,
        build_type: cache.get("CMAKE_BUILD_TYPE").cloned().unwrap_or_default(),
        compilers,
        units,
        dependencies,
        graph,
        rules,
        commands,
        response_files,
        cmake_cache: cache,
    })
}

/// Hash repository and CMake subdirectory identity without retaining remote credentials.
/// Projects without a remote use the canonical source path, avoiding unrelated matches.
pub fn project_identity(source: &Path) -> String {
    let git_value = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(source)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    if let (Some(root), Some(remote)) = (
        git_value(&["rev-parse", "--show-toplevel"]),
        git_value(&["config", "--get", "remote.origin.url"]),
    ) {
        let remote = remote
            .split_once("://")
            .map(|(_, tail)| tail)
            .unwrap_or(&remote);
        let remote = remote
            .rsplit_once('@')
            .map(|(_, tail)| tail)
            .unwrap_or(remote);
        let remote = remote.replace(':', "/");
        let remote = remote.trim_end_matches('/').trim_end_matches(".git");
        if let Ok(relative) = source.strip_prefix(root) {
            return digest(format!("repository:{remote}\n{}", relative.display()));
        }
    }
    digest(format!(
        "local:{}",
        source
            .canonicalize()
            .unwrap_or_else(|_| source.to_owned())
            .display()
    ))
}

/// Expand GNU-style response arguments without executing shell text, preserving exact
/// contents. Saved invocation snapshots take precedence over current filesystem data.
pub fn expand_responses(
    arguments: &[String],
    directory: &Path,
    saved: &BTreeMap<String, String>,
) -> Result<(Vec<String>, BTreeMap<String, String>)> {
    fn expand(
        arguments: &[String],
        directory: &Path,
        saved: &BTreeMap<String, String>,
        files: &mut BTreeMap<String, String>,
        depth: usize,
    ) -> Result<Vec<String>> {
        anyhow::ensure!(depth < 9, "response-file nesting exceeds eight levels");
        let mut result = Vec::new();
        for argument in arguments {
            if let Some(path) = argument.strip_prefix('@').filter(|path| !path.is_empty()) {
                let path = path_string(resolve(directory, path));
                let content = if let Some(content) = saved.get(&path) {
                    content.clone()
                } else {
                    anyhow::ensure!(
                        fs::metadata(&path)?.len() <= 8 * 1024 * 1024,
                        "response file exceeds 8 MiB"
                    );
                    fs::read_to_string(&path)
                        .with_context(|| format!("read response file {path}"))?
                };
                let nested = shlex::split(&content)
                    .with_context(|| format!("parse response file {path}"))?;
                files.insert(path, content);
                result.extend(expand(&nested, directory, saved, files, depth + 1)?);
            } else {
                result.push(argument.clone());
            }
        }
        Ok(result)
    }
    let mut files = BTreeMap::new();
    let expanded = expand(arguments, directory, saved, &mut files, 0)?;
    Ok((expanded, files))
}

/// Parse cache records while preserving types for user configuration reproduction.
pub fn read_cache(path: &Path) -> Result<BTreeMap<String, String>> {
    let text = fs::read_to_string(path).context("read CMakeCache.txt")?;
    let mut cache = BTreeMap::new();
    for line in text
        .lines()
        .filter(|line| !line.starts_with("//") && !line.starts_with('#'))
    {
        let Some((key_type, value)) = line.split_once('=') else {
            continue;
        };
        let Some((key, kind)) = key_type.rsplit_once(':') else {
            continue;
        };
        if kind != "INTERNAL" && kind != "STATIC" {
            cache.insert(format!("@type:{key}"), kind.to_owned());
            cache.insert(key.to_owned(), value.to_owned());
        } else if [
            "CMAKE_HOME_DIRECTORY",
            "CMAKE_GENERATOR",
            "CMAKE_MAKE_PROGRAM",
        ]
        .contains(&key)
        {
            cache.insert(key.to_owned(), value.to_owned());
        }
    }
    Ok(cache)
}

/// Parse Ninja's active dependency inventory; stale records are deliberately omitted.
pub fn read_dependencies(build: &Path) -> Result<BTreeMap<String, Vec<String>>> {
    let output = checked(
        Command::new("ninja")
            .arg("-C")
            .arg(build)
            .args(["-t", "deps"]),
    )?;
    Ok(parse_dependencies(
        &String::from_utf8_lossy(&output.stdout),
        build,
    ))
}

/// Decode the documented Ninja deps tool output into output-to-file relations.
pub fn parse_dependencies(text: &str, build: &Path) -> BTreeMap<String, Vec<String>> {
    let mut result = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if !line.starts_with(' ') && line.contains(": #deps ") {
            let (output, metadata) = line.split_once(": #deps ").unwrap();
            current = if metadata.contains("VALID") && !metadata.contains("STALE") {
                Some(path_string(resolve(build, output)))
            } else {
                None
            };
            if let Some(output) = &current {
                result.entry(output.clone()).or_insert_with(Vec::new);
            }
        } else if let Some(output) = &current
            && let Some(dependency) = line.strip_prefix("    ")
        {
            result
                .entry(output.clone())
                .or_default()
                .push(path_string(resolve(build, dependency)));
        }
    }
    for values in result.values_mut() {
        values.sort();
        values.dedup();
    }
    result
}

/// Read the target build graph, including command nodes with multiple outputs.
pub fn read_graph(build: &Path, targets: &[String]) -> Result<BTreeMap<String, Vec<String>>> {
    Ok(read_ninja_graph(build, targets)?.dependencies)
}

/// Collapse Ninja DOT rule vertices into file-to-prerequisite relations.
pub fn parse_graph(text: &str, build: &Path) -> BTreeMap<String, Vec<String>> {
    parse_ninja_graph(text, build).dependencies
}

/// Both views come from the same Ninja snapshot, avoiding another process and DOT parse.
struct NinjaGraph {
    dependencies: BTreeMap<String, Vec<String>>,
    rules: BTreeMap<String, String>,
}

fn read_ninja_graph(build: &Path, targets: &[String]) -> Result<NinjaGraph> {
    let output = checked(
        Command::new("ninja")
            .arg("-C")
            .arg(build)
            .args(["-t", "graph"])
            .args(targets),
    )?;
    Ok(parse_ninja_graph(
        &String::from_utf8_lossy(&output.stdout),
        build,
    ))
}

fn parse_ninja_graph(text: &str, build: &Path) -> NinjaGraph {
    let mut labels = BTreeMap::new();
    let mut rule_nodes = BTreeSet::new();
    let mut incoming = BTreeMap::<String, Vec<String>>::new();
    for line in text.lines() {
        let quoted = quoted_strings(line);
        if line.contains("->") && quoted.len() >= 2 {
            incoming
                .entry(quoted[1].clone())
                .or_default()
                .push(quoted[0].clone());
        } else if line.contains("label=") && quoted.len() >= 2 {
            labels.insert(quoted[0].clone(), quoted[1].clone());
            if line.contains("shape=ellipse") {
                rule_nodes.insert(quoted[0].clone());
            }
        }
    }
    let mut graph = BTreeMap::new();
    let mut rules = BTreeMap::new();
    for (node, label) in &labels {
        if rule_nodes.contains(node) {
            continue;
        }
        let output = path_string(resolve(build, label));
        let mut dependencies = BTreeSet::new();
        for input in incoming.get(node).into_iter().flatten() {
            if rule_nodes.contains(input) {
                if let Some(rule) = labels.get(input) {
                    rules.insert(output.clone(), rule.clone());
                }
                for prerequisite in incoming.get(input).into_iter().flatten() {
                    if let Some(name) = labels.get(prerequisite) {
                        dependencies.insert(path_string(resolve(build, name)));
                    }
                }
            } else if let Some(name) = labels.get(input) {
                dependencies.insert(path_string(resolve(build, name)));
            }
        }
        graph.insert(output, dependencies.into_iter().collect());
    }
    NinjaGraph {
        dependencies: graph,
        rules,
    }
}

/// Normalize output/dependency paths and managed instrumentation out of command identity.
pub fn semantic_command(arguments: &[String], source: &str, build: &str) -> String {
    let mut normalized = Vec::new();
    let mut skip = false;
    for arg in arguments {
        if skip {
            skip = false;
            continue;
        }
        if ["-o", "-MF", "-MT", "-MQ", "-dumpdir", "-dumpbase"].contains(&arg.as_str()) {
            skip = true;
            continue;
        }
        if arg.starts_with("-ftime-") || arg == "-H" || arg.starts_with("-fdiagnostics-format=") {
            continue;
        }
        if ["-MD", "-MMD", "-MP", "-MG"].contains(&arg.as_str()) {
            continue;
        }
        normalized.push(arg.replace(build, "<build>").replace(source, "<source>"));
    }
    digest(normalized.join("\0"))
}

/// Include all known inputs, including generated files and source files.
pub fn input_paths(project: &ProjectSnapshot) -> BTreeSet<String> {
    let compilation_outputs = project
        .units
        .iter()
        .map(|u| u.output.as_str())
        .collect::<BTreeSet<_>>();
    project
        .dependencies
        .values()
        .flatten()
        .chain(project.graph.values().flatten())
        .chain(project.units.iter().map(|u| &u.source))
        .filter(|path| {
            !compilation_outputs.contains(path.as_str())
                && !project
                    .rules
                    .get(path.as_str())
                    .is_some_and(|rule| rule.contains("LINKER") || rule.contains("ARCHIVER"))
        })
        .cloned()
        .collect()
}

/// Read metadata and content outside the measured build interval.
pub fn snapshot(paths: impl IntoIterator<Item = String>) -> BTreeMap<String, FileStamp> {
    paths
        .into_iter()
        .filter_map(|path| {
            let metadata = fs::metadata(&path).ok()?;
            if !metadata.is_file() {
                return None;
            }
            let modified_ns = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let content = (metadata.len() <= 32 * 1024 * 1024)
                .then(|| fs::read(&path).ok())
                .flatten();
            // Discard a hash if the file's observable metadata changed while reading.
            let stable = fs::metadata(&path).ok().is_some_and(|after| {
                after.len() == metadata.len() && after.modified().ok() == metadata.modified().ok()
            });
            Some((
                path,
                FileStamp {
                    modified_ns,
                    size: metadata.len(),
                    digest: content.filter(|_| stable).map(digest),
                },
            ))
        })
        .collect()
}

/// Compare dependency snapshots without attributing shared rebuilding exclusively.
pub fn changes(
    before: &BTreeMap<String, FileStamp>,
    after: &BTreeMap<String, FileStamp>,
    during: bool,
) -> Vec<InputChange> {
    let paths: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    paths
        .into_iter()
        .filter_map(|path| {
            let kind = match (before.get(path), after.get(path)) {
                (None, Some(_)) => "created",
                (Some(_), None) => "removed",
                (Some(a), Some(b))
                    if a.digest.is_some() && b.digest.is_some() && a.digest != b.digest =>
                {
                    "content"
                }
                (Some(a), Some(b)) if a.modified_ns != b.modified_ns || a.size != b.size => {
                    if a.digest.is_some() && a.digest == b.digest {
                        "timestamp"
                    } else {
                        "unknown"
                    }
                }
                _ => return None,
            };
            Some(InputChange {
                path: path.clone(),
                kind: kind.into(),
                during_build: during,
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct DatabaseEntry {
    directory: String,
    file: String,
    arguments: Option<Vec<String>>,
    command: Option<String>,
    output: Option<String>,
}

fn quoted_strings(line: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in line.chars() {
        if escaped {
            current.push(match c {
                'n' => '\n',
                _ => c,
            });
            escaped = false;
        } else if c == '\\' && quoted {
            escaped = true;
        } else if c == '"' {
            if quoted {
                strings.push(std::mem::take(&mut current));
            }
            quoted = !quoted;
        } else if quoted {
            current.push(c);
        }
    }
    strings
}

// CMake exports native compiler commands, whereas Ninja compdb includes launchers.
fn strip_launchers(arguments: &mut Vec<String>) {
    strip_buildscope_launcher(arguments);
    if let Some(index) = ccache_compiler_index(arguments) {
        arguments.drain(..index);
    }
}
