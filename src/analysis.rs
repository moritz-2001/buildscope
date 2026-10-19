//! Pure joins and evidence-based diagnostics. Header costs, change contributors,
//! recorded triggers, dependency work, and elapsed time remain separate quantities.
use crate::{model::*, util::*};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::Path,
};

/// Derive header summaries, build metrics, and deterministic findings from observations.
pub fn analyze(capture: &mut Capture, explanation_log: &str) {
    let project = &capture.project;
    let known_sources: BTreeSet<_> = project.units.iter().map(|u| u.source.as_str()).collect();
    let changed: BTreeSet<_> = capture.changes.iter().map(|c| c.path.as_str()).collect();
    let mut headers = BTreeMap::<String, HeaderSummary>::new();
    for unit in &project.units {
        for path in project.dependencies.get(&unit.output).into_iter().flatten() {
            if !known_sources.contains(path.as_str()) {
                let header = headers
                    .entry(path.clone())
                    .or_insert_with(|| empty_header(path, project));
                header.units.push(unit.id.clone());
            }
        }
    }
    for profile in &capture.profiles {
        for edge in &profile.includes {
            let header = headers
                .entry(edge.child.clone())
                .or_insert_with(|| empty_header(&edge.child, project));
            header.units.push(profile.unit_id.clone());
            if edge.depth == 1 {
                header.direct_units.push(profile.unit_id.clone());
            }
        }
        for cost in &profile.header_costs {
            let header = headers
                .entry(cost.path.clone())
                .or_insert_with(|| empty_header(&cost.path, project));
            header.units.push(profile.unit_id.clone());
            *header.processing_ms.get_or_insert(0.0) += cost.inclusive_ms;
            *header.recorded_self_ms.get_or_insert(0.0) += cost.recorded_self_ms;
            header.samples_ms.push(cost.inclusive_ms);
            header.measured_units += 1;
        }
    }
    let reasons = trigger_evidence(explanation_log, Path::new(&project.build_dir));
    let mut work_by_unit = HashMap::<&str, f64>::new();
    for action in &capture.actions {
        if let Some(unit) = &action.unit_id {
            *work_by_unit.entry(unit).or_default() += (action.end_ms - action.start_ms).max(0.0);
        }
    }
    for header in headers.values_mut() {
        header.units.sort();
        header.units.dedup();
        header.direct_units.sort();
        header.direct_units.dedup();
        for unit in &header.units {
            if let Some(work) = work_by_unit.get(unit.as_str()) {
                header.rebuilt_units += 1;
                header.rebuilt_work_ms += work;
            }
        }
        header.evidence = reasons.get(&header.path).cloned().unwrap_or_default();
        header.explicit_trigger = !header.evidence.is_empty() && header.rebuilt_units > 0;
        header.changed_contributor =
            changed.contains(header.path.as_str()) && header.rebuilt_units > 0;
        header.samples_ms.sort_by(f64::total_cmp);
    }
    capture.headers = headers.into_values().collect();
    capture.headers.sort_by(|a, b| {
        b.processing_ms
            .unwrap_or(0.0)
            .total_cmp(&a.processing_ms.unwrap_or(0.0))
            .then(b.units.len().cmp(&a.units.len()))
            .then(a.key.cmp(&b.key))
    });
    capture.summary = summarize(capture);
    capture.findings = findings(capture);
}

/// Attach only explicit native invalidation input evidence, retaining its original wording.
pub fn trigger_evidence(text: &str, build: &Path) -> BTreeMap<String, Vec<String>> {
    let mut reasons = BTreeMap::<String, Vec<String>>::new();
    for line in text.lines() {
        let Some(reason) = line.strip_prefix("ninja explain: ") else {
            continue;
        };
        let input = if let Some((_, input)) = reason.split_once("most recent input ") {
            Some(
                input
                    .rsplit_once(" (")
                    .map(|(path, _)| path)
                    .unwrap_or(input),
            )
        } else {
            reason.strip_suffix(" is dirty")
        };
        if let Some(input) = input.filter(|p| !p.is_empty()) {
            reasons
                .entry(path_string(resolve(build, input)))
                .or_default()
                .push(line.into());
        }
    }
    reasons
}

/// Compare matched units/headers and disclose configuration differences before interpreting time.
pub fn compare(before: &Capture, after: &Capture) -> Value {
    let mut differences = Vec::new();
    if before.project.project_id != after.project.project_id {
        differences.push("Measurements belong to different projects.");
    }
    if before.project.configuration_id != after.project.configuration_id {
        differences.push("Compiler versions or semantic compilation configuration differ.");
    }
    if before.machine != after.machine {
        differences.push("Measurements came from different hosts.");
    }
    if before.kind != after.kind {
        differences
            .push("Capture modes differ in instrumentation, cache policy, or timing coverage.");
    }
    if before.targets != after.targets {
        differences.push("Requested target sets differ.");
    }
    if before.environment != after.environment {
        differences.push("Recorded compiler-affecting environment differs.");
    }
    if before.cache_policy != after.cache_policy {
        differences.push("Recorded cache policies differ.");
    }
    let detail = |run: &Capture| {
        run.profiles
            .iter()
            .map(|p| (p.granularity_us, p.verbose))
            .collect::<BTreeSet<_>>()
    };
    if before.kind != "record" && detail(before) != detail(after) {
        differences.push("Compiler instrumentation detail differs.");
    }
    if before.status != "success" || after.status != "success" {
        differences.push("At least one capture is unsuccessful or incomplete.");
    }
    let unit_work = |run: &Capture| {
        let mut map = BTreeMap::<String, f64>::new();
        for action in &run.actions {
            if let Some(id) = &action.unit_id {
                *map.entry(id.clone()).or_default() += action.end_ms - action.start_ms;
            }
        }
        map
    };
    let a = unit_work(before);
    let b = unit_work(after);
    let units: Vec<_> = before
        .project
        .units
        .iter()
        .filter(|_| before.project.project_id == after.project.project_id)
        .filter_map(|unit| {
            Some(
                json!({"id":unit.id,"source":unit.source_key,"before_ms":a.get(&unit.id)?,
            "after_ms":b.get(&unit.id)?,"delta_ms":b[&unit.id]-a[&unit.id]}),
            )
        })
        .collect();
    let old_headers: BTreeMap<_, _> = before.headers.iter().map(|h| (&h.key, h)).collect();
    let headers: Vec<_> = after.headers.iter().filter(|_| before.project.project_id == after.project.project_id).filter_map(|header| {
        let old = old_headers.get(&header.key)?;
        Some(json!({"key":header.key,"before_units":old.units.len(),"after_units":header.units.len(),
            "before_processing_ms":old.processing_ms,"after_processing_ms":header.processing_ms,
            "delta_processing_ms":old.processing_ms.zip(header.processing_ms).map(|(a,b)|b-a),
            "before_rebuilt_units":old.rebuilt_units,"after_rebuilt_units":header.rebuilt_units}))
    }).collect();
    json!({"before_timing_scope":timing_scope(before),"after_timing_scope":timing_scope(after),
        "before":before.id,"after":after.id,"before_git":before.git,"after_git":after.git,
        "conditions_match":differences.is_empty(),
        "differences":differences,"limitations":["Single-run differences are observations, not proof of repeatable savings.",
        "Ordinary recordings preserve compiler caches; individual cache hit outcomes are unknown.",
        "Only units executed in both captures have paired action timings.",
        "Launcher durations describe a collected compiler span, possibly across builds, and exclude links and generators."],
        "before_elapsed_ms":before.elapsed_ms,"after_elapsed_ms":after.elapsed_ms,
        "delta_elapsed_ms":after.elapsed_ms-before.elapsed_ms,"units":units,"headers":headers})
}

/// Compare repeated successful captures by label using distributions, while checking
/// that all samples share the same recorded measurement conditions.
pub fn compare_series(before: &[Capture], after: &[Capture]) -> anyhow::Result<Value> {
    anyhow::ensure!(
        !before.is_empty() && !after.is_empty(),
        "each label needs at least one successful capture"
    );
    let conditions = |run: &Capture| {
        let detail = run
            .profiles
            .iter()
            .map(|p| (p.granularity_us, p.verbose))
            .collect::<BTreeSet<_>>();
        serde_json::to_string(&(
            &run.project.configuration_id,
            &run.machine,
            &run.kind,
            &run.targets,
            &run.environment,
            &run.cache_policy,
            detail,
        ))
        .unwrap()
    };
    let keys = before
        .iter()
        .chain(after)
        .map(conditions)
        .collect::<BTreeSet<_>>();
    let distribution = |runs: &[Capture]| {
        let mut values = runs.iter().map(|r| r.elapsed_ms).collect::<Vec<_>>();
        values.sort_by(f64::total_cmp);
        let percentile = |p: f64| {
            let location = p * (values.len() - 1) as f64;
            let lower = location.floor() as usize;
            let upper = location.ceil() as usize;
            values[lower] + (values[upper] - values[lower]) * (location - lower as f64)
        };
        json!({"label":runs[0].label,"run_ids":runs.iter().map(|r|&r.id).collect::<Vec<_>>(),
            "git":crate::git::summarize(runs.iter().map(|run| &run.git)),
            "count":values.len(),"median_ms":percentile(0.5),"p10_ms":percentile(0.1),
            "p90_ms":percentile(0.9),"min_ms":values[0],"max_ms":values[values.len()-1]})
    };
    let before = distribution(before);
    let after = distribution(after);
    let delta = after["median_ms"].as_f64().unwrap() - before["median_ms"].as_f64().unwrap();
    Ok(
        json!({"before":before,"after":after,"conditions_match":keys.len()==1,
        "delta_median_ms":delta,"limitations":[
            "Samples include successful captures only. Ordinary compiler-cache hit outcomes remain unknown.",
            "Launcher samples describe collected compiler spans, not whole builds; batch boundaries must be controlled separately.",
            "Different machines, configurations, modes, targets, environments, or instrumentation make the groups incompatible.",
            "Distributions describe the collected samples; they do not establish exclusive causation for a code change."]}),
    )
}

fn timing_scope(run: &Capture) -> &'static str {
    if run.kind == "launcher" {
        "Observed compiler span"
    } else {
        "Ninja execution"
    }
}

fn empty_header(path: &str, project: &ProjectSnapshot) -> HeaderSummary {
    HeaderSummary {
        path: path.into(),
        key: path_key(path, &project.source_dir, &project.build_dir),
        units: Vec::new(),
        direct_units: Vec::new(),
        rebuilt_units: 0,
        rebuilt_work_ms: 0.0,
        processing_ms: None,
        recorded_self_ms: None,
        samples_ms: Vec::new(),
        measured_units: 0,
        explicit_trigger: false,
        changed_contributor: false,
        evidence: Vec::new(),
    }
}

fn summarize(capture: &Capture) -> BuildSummary {
    let actions = &capture.actions;
    let mut sweep = Vec::new();
    let mut output_actions = HashMap::<&str, usize>::new();
    let mut work_ms = 0.0;
    for (index, action) in actions.iter().enumerate() {
        let duration = (action.end_ms - action.start_ms).max(0.0);
        work_ms += duration;
        if duration > 0.0 {
            sweep.push((action.start_ms, 1i32));
            sweep.push((action.end_ms, -1));
        }
        for output in &action.outputs {
            output_actions.insert(output, index);
        }
    }
    sweep.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut active = 0i32;
    let mut max_concurrency = 0;
    for (_, change) in sweep {
        active += change;
        max_concurrency = max_concurrency.max(active.max(0) as usize);
    }
    let mut order: Vec<_> = (0..actions.len()).collect();
    order.sort_by(|&a, &b| actions[a].end_ms.total_cmp(&actions[b].end_ms));
    let mut weights = vec![0.0f64; actions.len()];
    let mut predecessor = vec![None; actions.len()];
    for index in order {
        let action = &actions[index];
        if let Some(prior) = (0..actions.len())
            .filter(|&prior| {
                actions[prior].kind == "configure"
                    && actions[prior].clock_segment < action.clock_segment
                    && actions[prior].end_ms <= action.start_ms + 1.0
            })
            .max_by(|&a, &b| actions[a].end_ms.total_cmp(&actions[b].end_ms))
        {
            weights[index] = weights[prior];
            predecessor[index] = Some(prior);
        }
        let mut queue = Vec::new();
        for output in &action.outputs {
            queue.extend(
                capture
                    .project
                    .graph
                    .get(output)
                    .into_iter()
                    .flatten()
                    .map(String::as_str),
            );
            queue.extend(
                capture
                    .project
                    .dependencies
                    .get(output)
                    .into_iter()
                    .flatten()
                    .map(String::as_str),
            );
        }
        let mut visited = HashSet::new();
        while let Some(path) = queue.pop() {
            if !visited.insert(path) {
                continue;
            }
            if let Some(&prior) = output_actions.get(path) {
                if prior != index
                    && actions[prior].end_ms <= action.start_ms + 1.0
                    && weights[prior] > weights[index]
                {
                    weights[index] = weights[prior];
                    predecessor[index] = Some(prior);
                }
            } else {
                queue.extend(
                    capture
                        .project
                        .graph
                        .get(path)
                        .into_iter()
                        .flatten()
                        .map(String::as_str),
                );
            }
        }
        weights[index] += (action.end_ms - action.start_ms).max(0.0);
    }
    let last = (0..weights.len()).max_by(|&a, &b| weights[a].total_cmp(&weights[b]));
    let dependency_path_ms = last.map(|i| weights[i]).unwrap_or_default();
    let mut dependency_path = Vec::new();
    let mut current = last;
    while let Some(index) = current {
        dependency_path.push(actions[index].id.clone());
        current = predecessor[index];
    }
    dependency_path.reverse();
    BuildSummary {
        work_ms,
        max_concurrency,
        dependency_path_ms,
        dependency_path,
        dependency_units: capture
            .project
            .units
            .iter()
            .filter(|u| capture.project.dependencies.contains_key(&u.output))
            .count(),
        observed_units: actions
            .iter()
            .filter_map(|a| a.unit_id.as_ref())
            .collect::<BTreeSet<_>>()
            .len(),
    }
}

fn findings(capture: &Capture) -> Vec<Finding> {
    let mut result = Vec::new();
    for header in capture
        .headers
        .iter()
        .filter(|h| h.processing_ms.unwrap_or_default() >= 10.0)
        .take(5)
    {
        result.push(Finding { category: "header-processing".into(),
            title: format!("{} contributes repeated header-processing work", header.key),
            explanation: format!("{:.1} ms inclusive work in {} measured units; {} dependent units in the available inventory. Nested header costs overlap.",
                header.processing_ms.unwrap(), header.measured_units, header.units.len()),
            subject: header.path.clone(), origin: "measured".into(),
            evidence: vec![format!("{} timing samples; recorded-child-exclusive work {:.1} ms",
                header.samples_ms.len(), header.recorded_self_ms.unwrap_or_default())],
            experiment: "Inspect the include paths and public API boundary, then compare compatible captures after a dependency change.".into() });
    }
    for header in capture
        .headers
        .iter()
        .filter(|h| h.explicit_trigger || h.changed_contributor)
        .take(10)
    {
        result.push(Finding { category: "header-rebuild".into(),
            title: format!("{} is associated with rebuilt dependents", header.key),
            explanation: format!("{} dependent units executed, accumulating {:.1} ms. Shared rebuild work is not attributed exclusively to this header.",
                header.rebuilt_units, header.rebuilt_work_ms),
            subject: header.path.clone(), origin: "derived".into(), evidence: header.evidence.clone(),
            experiment: "Compare repeated incremental captures after narrowing this header's dependency reach.".into() });
    }
    let mut repeated = BTreeMap::<&str, (BTreeSet<&str>, f64)>::new();
    for profile in &capture.profiles {
        if let Some(template) = profile.templates.first().filter(|t| t.total_ms > 1.0) {
            result.push(Finding { category: "expensive-template".into(), title: format!("Expensive recorded template: {}", template.name),
                explanation: format!("{:.1} ms inclusive across {} recorded scopes; the longest is {:.1} ms.",
                    template.total_ms, template.count, template.max_ms),
                subject: profile.unit_id.clone(), origin: "measured".into(), evidence: vec![profile.artifact.clone()],
                experiment: "Inspect the template scopes and run a focused full-detail capture. Evaluate explicit instantiation or simpler type usage where appropriate.".into() });
        }
        if let Some(timer) = profile
            .phases
            .iter()
            .find(|p| p.name == "template instantiation" && p.value >= 10.0)
        {
            result.push(Finding { category: "gcc-template-time".into(), title: "GCC reports substantial template-instantiation activity".into(),
                explanation: format!("{:.1} ms in GCC's aggregate timer. Individual specialization costs are unavailable.", timer.value),
                subject: profile.unit_id.clone(), origin: "measured".into(), evidence: vec![profile.artifact.clone()],
                experiment: "Inspect template usage, or create a separately labelled Clang investigation using an equivalent configuration.".into() });
        }
        if let Some(function) = profile.functions.first().filter(|f| f.total_ms > 5.0) {
            result.push(Finding { category: "expensive-function".into(), title: format!("Recorded backend work in {}", function.name),
                explanation: format!("{:.1} ms inclusive optimization/code-generation work.", function.total_ms),
                subject: profile.unit_id.clone(), origin: "measured".into(), evidence: vec![profile.artifact.clone()],
                experiment: "Inspect the surrounding compiler passes and compare an intentional optimization-setting experiment separately.".into() });
        }
        for template in &profile.templates {
            let item = repeated.entry(&template.name).or_default();
            item.0.insert(&profile.unit_id);
            item.1 += template.total_ms;
        }
    }
    let mut repeated: Vec<_> = repeated
        .into_iter()
        .filter(|(_, (units, time))| units.len() > 1 && *time > 5.0)
        .collect();
    repeated.sort_by(|a, b| b.1.1.total_cmp(&a.1.1));
    for (name, (units, time)) in repeated.into_iter().take(5) {
        result.push(Finding { category: "repeated-template".into(), title: format!("Repeated recorded specialization: {name}"),
            explanation: format!("{time:.1} ms inclusive across {} compilation units. Matching is by exact compiler spelling.", units.len()),
            subject: units.first().unwrap().to_string(), origin: "derived".into(),
            evidence: units.into_iter().map(str::to_owned).collect(),
            experiment: "Inspect whether explicit instantiation or moving implementation out of headers can reduce repeated work.".into() });
    }
    result
}
