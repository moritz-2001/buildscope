//! Compiler capability probes and version-tolerant decoders. Detailed Clang scopes
//! remain distinct from GCC aggregate timers, and missing attribution stays missing.
use crate::{model::*, util::*};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{BufReader, Read, Write},
    path::Path,
    process::{Command, Stdio},
};

/// Identify an executable using its own output rather than its filename alone.
pub fn identify(executable: &str, directory: &Path) -> CompilerInfo {
    let version = Command::new(executable)
        .arg("--version")
        .current_dir(directory)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_else(|e| e.to_string());
    let family = if version.to_lowercase().contains("clang") {
        "clang"
    } else if version.contains("Free Software Foundation") || version.contains("GCC") {
        "gcc"
    } else {
        "unknown"
    };
    let help = Command::new(executable)
        .arg("--help")
        .current_dir(directory)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let sarif = family == "gcc" && probe_sarif(executable, directory);
    CompilerInfo {
        executable: executable.into(),
        version,
        family: family.into(),
        explicit_trace_path: help.contains("-ftime-trace=<"),
        verbose_trace: help.contains("-ftime-trace-verbose"),
        sarif,
    }
}

/// Decode the artifacts of one actual invocation into common observations.
pub fn decode(
    invocation: &Invocation,
    unit: &CompilationUnit,
    root: &Path,
) -> Result<(CompilationProfile, Vec<TraceEvent>)> {
    let stderr = read_optional(&artifact_path(root, &invocation.stderr)?);
    let includes = parse_includes(&stderr, &unit.source, Path::new(&invocation.directory));
    let mut profile = CompilationProfile {
        unit_id: unit.id.clone(),
        family: invocation.family.clone(),
        artifact: invocation
            .trace
            .clone()
            .unwrap_or_else(|| invocation.stderr.clone()),
        granularity_us: invocation.granularity_us,
        verbose: invocation.verbose,
        includes,
        ..CompilationProfile::default()
    };
    let events = if let Some(trace) = &invocation.trace {
        let (events, phases) = read_clang_trace(
            &artifact_path(root, trace)?,
            Path::new(&invocation.directory),
        )?;
        profile.phases = phases;
        profile.templates = aggregate_symbols(&events, |name| {
            matches!(name, "InstantiateClass" | "InstantiateFunction")
        });
        profile.functions = aggregate_symbols(&events, |name| {
            matches!(name, "OptFunction" | "CodeGen Function")
        });
        profile.evaluations = aggregate_symbols(&events, |name| {
            name.starts_with("Evaluate") || name == "isIntegerConstantExpr"
        });
        profile.header_costs = header_costs(&events);
        profile.notes.push(
            "Named costs are inclusive recorded scopes. Overlapping categories must not be added."
                .into(),
        );
        if invocation.granularity_us.unwrap_or(500) > 0 {
            profile.notes.push(
                "Short events are filtered; named counts describe recorded events only.".into(),
            );
        }
        profile.notes.push("Source locations are supplied metadata, usually definitions; trace nesting is not a semantic instantiation graph.".into());
        events
    } else if invocation.family == "gcc" {
        profile.phases = parse_gcc_report(&stderr);
        profile.notes.push("GCC supplies aggregate timers. Individual header and specialization timings are unavailable.".into());
        profile
            .notes
            .push("Native phase and subphase timers can overlap; do not sum all rows.".into());
        Vec::new()
    } else {
        profile.notes.push(
            "Unsupported compiler or launcher chain: command forwarded without profiling flags."
                .into(),
        );
        Vec::new()
    };
    if invocation.exit_code != Some(0) {
        profile.notes.push(
            "This compiler invocation failed or was interrupted; its measurements are partial."
                .into(),
        );
    }
    profile.event_count = events.len();
    Ok((profile, events))
}

/// Decode complete and paired Clang events, including asynchronous Source spans.
pub fn read_clang_trace(path: &Path, directory: &Path) -> Result<(Vec<TraceEvent>, Vec<Metric>)> {
    let input: Box<dyn Read> = if path.extension().is_some_and(|extension| extension == "zst") {
        Box::new(zstd::stream::read::Decoder::new(File::open(path)?)?)
    } else {
        Box::new(File::open(path)?)
    };
    let raw: RawTrace =
        serde_json::from_reader(BufReader::new(input)).context("decode Clang trace")?;
    let mut events = Vec::new();
    let mut phases = Vec::new();
    let mut pending: HashMap<String, Vec<RawEvent>> = HashMap::new();
    let mut paths = HashMap::new();
    for event in raw.events {
        if let Some(name) = event.name.strip_prefix("Total ") {
            if let Some(duration) = event.dur {
                phases.push(Metric {
                    name: name.into(),
                    value: duration / 1000.0,
                    unit: "ms".into(),
                    origin: "measured".into(),
                    coverage: "compiler-reported".into(),
                });
            }
            continue;
        }
        match event.ph.as_str() {
            "X" => {
                if let Some(duration) = event.dur {
                    events.push(normalize(event, duration, directory, &mut paths));
                }
            }
            "B" | "b" => {
                pending.entry(event.pair_key()).or_default().push(event);
            }
            "E" | "e" => {
                if let Some(begin) = pending.get_mut(&event.pair_key()).and_then(Vec::pop) {
                    let duration = (event.ts - begin.ts).max(0.0);
                    events.push(normalize(begin, duration, directory, &mut paths));
                }
            }
            _ => {}
        }
    }
    events.sort_by(|a, b| {
        a.start_us
            .total_cmp(&b.start_us)
            .then(b.duration_us.total_cmp(&a.duration_us))
    });
    let mut lanes: HashMap<String, Vec<usize>> = HashMap::new();
    for index in 0..events.len() {
        let stack = lanes.entry(events[index].thread.clone()).or_default();
        while let Some(&parent) = stack.last() {
            let end = events[parent].start_us + events[parent].duration_us;
            if end <= events[index].start_us
                || end + 0.001 < events[index].start_us + events[index].duration_us
            {
                stack.pop();
            } else {
                break;
            }
        }
        events[index].id = index;
        events[index].parent = stack.last().copied();
        events[index].depth = stack.len();
        stack.push(index);
    }
    Ok((events, phases))
}

/// Parse native GCC text reports or the compiler's structured SARIF timeReport.
pub fn parse_gcc_report(text: &str) -> Vec<Metric> {
    if let Some(value) = sarif_document(text) {
        let timers = value
            .pointer("/runs/0/invocations/0/properties/gcc~1timeReport/timevars")
            .and_then(Value::as_array);
        if let Some(timers) = timers {
            return timers
                .iter()
                .filter_map(|timer| {
                    Some(metric(
                        timer.get("name")?.as_str()?,
                        timer.pointer("/elapsed/wall")?.as_f64()? * 1000.0,
                    ))
                })
                .collect();
        }
    }
    let mut metrics = Vec::new();
    let mut timing = false;
    for line in text.lines() {
        if line.contains("Time variable") {
            timing = true;
            continue;
        }
        if !timing {
            continue;
        }
        let Some((name, values)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let mut outside = String::new();
        let mut depth = 0;
        for c in values.chars() {
            if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
            } else if depth == 0 {
                outside.push(c);
            }
        }
        let times: Vec<f64> = outside
            .split_whitespace()
            .take_while(|s| s.parse::<f64>().is_ok())
            .filter_map(|s| s.parse().ok())
            .collect();
        let wall = if times.len() >= 3 {
            times[2]
        } else {
            *times.first().unwrap_or(&0.0)
        };
        if !times.is_empty() {
            metrics.push(metric(name, wall * 1000.0));
        }
    }
    metrics
}

/// Extract a SARIF document embedded after include diagnostics, if present.
pub fn sarif_document(text: &str) -> Option<Value> {
    for (index, _) in text.match_indices('{').take(128) {
        if let Some(Ok(value)) = serde_json::Deserializer::from_str(&text[index..])
            .into_iter::<Value>()
            .next()
            && value.get("runs").is_some()
        {
            return Some(value);
        }
    }
    None
}

/// Reconstruct actual preprocessor nesting from -H output for either compiler.
pub fn parse_includes(text: &str, source: &str, directory: &Path) -> Vec<IncludeEdge> {
    let mut stack = vec![source.to_owned()];
    let mut includes = Vec::new();
    for line in text.lines() {
        let depth = line.chars().take_while(|c| *c == '.').count();
        if depth == 0 || !line[depth..].starts_with(' ') {
            continue;
        }
        let child = line[depth..].trim();
        if child.is_empty() || child.starts_with('<') {
            continue;
        }
        let child = path_string(resolve(directory, child));
        if depth > stack.len() {
            continue;
        }
        stack.truncate(depth);
        let parent = stack.last().cloned().unwrap_or_else(|| source.to_owned());
        includes.push(IncludeEdge {
            parent,
            child: child.clone(),
            depth,
        });
        stack.push(child);
    }
    includes
}

/// Approximate display grouping; original spellings and measurements remain intact.
pub fn template_family(name: &str) -> String {
    let mut result = String::new();
    let mut depth = 0usize;
    for c in name.chars() {
        match c {
            '<' => {
                if depth == 0 {
                    result.push_str("<…>");
                }
                depth += 1;
            }
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => result.push(c),
            _ => {}
        }
    }
    if depth == 0 { result } else { name.to_owned() }
}

fn probe_sarif(executable: &str, directory: &Path) -> bool {
    let child = Command::new(executable)
        .current_dir(directory)
        .args([
            "-x",
            "c++",
            "-fsyntax-only",
            "-fdiagnostics-format=sarif-stderr",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"int buildscope_probe;\n");
    }
    child.wait_with_output().is_ok_and(|o| {
        o.status.success() && sarif_document(&String::from_utf8_lossy(&o.stderr)).is_some()
    })
}

fn metric(name: &str, value: f64) -> Metric {
    Metric {
        name: name.into(),
        value,
        unit: "ms".into(),
        origin: "measured".into(),
        coverage: "compiler-reported".into(),
    }
}

#[derive(Deserialize)]
struct RawTrace {
    #[serde(rename = "traceEvents")]
    events: Vec<RawEvent>,
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(default)]
    name: String,
    #[serde(default)]
    ph: String,
    #[serde(default)]
    ts: f64,
    dur: Option<f64>,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    pid: Value,
    #[serde(default)]
    tid: Value,
    #[serde(default)]
    id: Value,
    #[serde(default)]
    cat: String,
}

impl RawEvent {
    fn thread(&self) -> String {
        format!("{}:{}", self.pid, self.tid)
    }
    fn pair_key(&self) -> String {
        if self.ph == "B" || self.ph == "E" {
            self.thread()
        } else {
            format!("{}:{}:{}", self.thread(), self.cat, self.id)
        }
    }
}

fn normalize(
    raw: RawEvent,
    duration: f64,
    directory: &Path,
    paths: &mut HashMap<String, String>,
) -> TraceEvent {
    let thread = raw.thread();
    let mut detail = raw
        .args
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if raw.name == "Source" && !detail.starts_with('<') {
        detail = trace_path(directory, &detail, paths);
    }
    TraceEvent {
        id: 0,
        name: raw.name,
        detail,
        thread,
        start_us: raw.ts,
        duration_us: duration,
        file: raw
            .args
            .get("file")
            .and_then(Value::as_str)
            .map(|path| trace_path(directory, path, paths)),
        line: raw.args.get("line").and_then(Value::as_u64),
        parent: None,
        depth: 0,
    }
}

// A trace can repeat one header location in millions of scopes. Cache only within
// this decode so later traces still observe current paths and symlink targets.
fn trace_path(directory: &Path, path: &str, paths: &mut HashMap<String, String>) -> String {
    if let Some(resolved) = paths.get(path) {
        return resolved.clone();
    }
    let resolved = path_string(resolve(directory, path));
    paths.insert(path.into(), resolved.clone());
    resolved
}

fn aggregate_symbols(events: &[TraceEvent], include: impl Fn(&str) -> bool) -> Vec<SymbolCost> {
    let mut costs = BTreeMap::<String, SymbolCost>::new();
    for event in events.iter().filter(|e| include(&e.name)) {
        let name = if event.detail.is_empty() {
            event.name.clone()
        } else {
            event.detail.clone()
        };
        let item = costs.entry(name.clone()).or_insert_with(|| SymbolCost {
            name,
            total_ms: 0.0,
            count: 0,
            max_ms: 0.0,
            file: event.file.clone(),
            line: event.line,
        });
        item.count += 1;
        item.total_ms += event.duration_us / 1000.0;
        item.max_ms = item.max_ms.max(event.duration_us / 1000.0);
    }
    let mut costs: Vec<_> = costs.into_values().collect();
    costs.sort_by(|a, b| b.total_ms.total_cmp(&a.total_ms));
    costs
}

fn header_costs(events: &[TraceEvent]) -> Vec<HeaderCost> {
    let mut costs = BTreeMap::<String, HeaderCost>::new();
    let mut self_times: HashMap<usize, f64> = HashMap::new();
    let mut lanes = HashMap::<String, Vec<usize>>::new();
    for event in events
        .iter()
        .filter(|e| e.name == "Source" && !e.detail.is_empty())
    {
        self_times.insert(event.id, event.duration_us);
        let stack = lanes.entry(event.thread.clone()).or_default();
        while let Some(&parent) = stack.last() {
            if events[parent].start_us + events[parent].duration_us + 0.001
                < event.start_us + event.duration_us
                || events[parent].start_us + events[parent].duration_us <= event.start_us
            {
                stack.pop();
            } else {
                break;
            }
        }
        if let Some(parent) = stack.last() {
            *self_times.entry(*parent).or_default() -= event.duration_us;
        }
        stack.push(event.id);
    }
    for event in events
        .iter()
        .filter(|e| e.name == "Source" && !e.detail.is_empty())
    {
        let cost = costs
            .entry(event.detail.clone())
            .or_insert_with(|| HeaderCost {
                path: event.detail.clone(),
                inclusive_ms: 0.0,
                recorded_self_ms: 0.0,
                count: 0,
            });
        cost.count += 1;
        cost.inclusive_ms += event.duration_us / 1000.0;
        cost.recorded_self_ms += self_times
            .get(&event.id)
            .copied()
            .unwrap_or_default()
            .max(0.0)
            / 1000.0;
    }
    costs.into_values().collect()
}
