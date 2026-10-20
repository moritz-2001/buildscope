//! Bounded compiler stack queries over incremental per-unit projections.
use super::Store;
use crate::{
    compiler_stacks::{Example, Frame, Stack, project},
    model::TraceEvent,
};
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::BufReader,
};

/// Filters and bounded expansion for an additive compiler-work flame graph.
pub struct FlamegraphQuery<'a> {
    /// Exact compilation unit; empty selects the project.
    pub unit: &'a str,
    /// Exact build target; empty selects all targets.
    pub target: &'a str,
    /// Compiler index; empty selects all, preserving separate compiler roots.
    pub compiler: &'a str,
    /// `bottom-up` reverses recorded temporal stacks; otherwise top-down.
    pub direction: &'a str,
    /// Opaque JSON path returned by a node's focus field.
    pub focus: &'a str,
    /// Visible depth, clamped to 1..32.
    pub depth: usize,
    /// Skip children of the focused root, for bounded sibling pagination.
    pub offset: usize,
    /// Response node budget, clamped to 2..5000.
    pub limit: usize,
}
impl Default for FlamegraphQuery<'_> {
    fn default() -> Self {
        Self {
            unit: "",
            target: "",
            compiler: "",
            direction: "top-down",
            focus: "",
            depth: 12,
            offset: 0,
            limit: 5000,
        }
    }
}
/// Replace a unit projection inside the same transaction as its native events.
pub(crate) fn index_events(
    connection: &Connection,
    run: &str,
    unit: &str,
    events: &[TraceEvent],
) -> Result<()> {
    connection.execute(
        "DELETE FROM compiler_stacks WHERE run=?1 AND unit=?2",
        params![run, unit],
    )?;
    connection.execute(
        "INSERT OR REPLACE INTO compiler_stack_units(run,unit) VALUES(?1,?2)",
        params![run, unit],
    )?;
    let mut insert = connection
        .prepare("INSERT INTO compiler_stacks(run,unit,ordinal,data) VALUES(?1,?2,?3,?4)")?;
    for (i, stack) in project(events).into_iter().enumerate() {
        insert.execute(params![run, unit, i as i64, serde_json::to_string(&stack)?])?;
    }
    Ok(())
}
#[derive(Default)]
struct Node {
    value: f64,
    own: f64,
    children: BTreeMap<Frame, Node>,
    examples: Vec<(usize, Example)>,
    units: BTreeSet<usize>,
}
impl Node {
    fn add(
        &mut self,
        path: &[Frame],
        value: f64,
        examples: &[Example],
        unit: usize,
        sample_depth: Option<usize>,
    ) {
        self.units.insert(unit);
        self.value += value;
        if sample_depth == Some(0) {
            for e in examples {
                if self.examples.len() < 5
                    && !self
                        .examples
                        .iter()
                        .any(|(u, s)| *u == unit && s.event_id == e.event_id)
                {
                    self.examples.push((unit, e.clone()));
                }
            }
        }
        if let Some((frame, tail)) = path.split_first() {
            self.children.entry(frame.clone()).or_default().add(
                tail,
                value,
                examples,
                unit,
                sample_depth.and_then(|depth| depth.checked_sub(1)),
            );
        } else {
            self.own += value;
        }
    }
    fn render(
        &self,
        frame: &Frame,
        path: &mut Vec<Frame>,
        depth: usize,
        budget: &mut usize,
        offset: usize,
        unit_info: &[(String, String)],
    ) -> Value {
        *budget = budget.saturating_sub(1);
        let mut ordered: Vec<_> = self.children.iter().collect();
        ordered.sort_by(|(a, x), (b, y)| y.value.total_cmp(&x.value).then(a.cmp(b)));
        let mut children = Vec::new();
        let mut hidden = 0.0;
        let mut next_offset = None;
        for (index, (child, node)) in ordered.iter().enumerate() {
            if index < offset {
                hidden += node.value;
                continue;
            }
            if depth == 0 || *budget <= 1 {
                hidden += node.value;
                next_offset.get_or_insert(index);
                continue;
            }
            path.push((*child).clone());
            *budget -= 1; // Reserve one slot for this parent's omitted mass.
            children.push(node.render(child, path, depth - 1, budget, 0, unit_info));
            *budget += 1;
            path.pop();
        }
        if hidden > 0.0 && *budget > 0 {
            *budget -= 1;
            children.push(json!({"name":"[Other recorded stacks]","detail":"Expand or page to inspect remaining coverage","value_us":hidden,"self_us":0,"children":[],"has_children":true,"focus":serde_json::to_string(path).unwrap(),"offset":next_offset.unwrap_or(0),"other":true}));
        }
        let examples:Vec<Value>=self.examples.iter().map(|(index,sample)|{
            let (unit,source)=&unit_info[*index];json!({"unit_id":unit,"event_id":sample.event_id,"inclusive_us":sample.duration_us,"source":source})
        }).collect();
        json!({"name":frame.name,"detail":frame.detail,"file":frame.file,"line":frame.line,"value_us":self.value,"self_us":self.own,"children":children,"has_children":!self.children.is_empty(),"focus":serde_json::to_string(path).unwrap(),"examples":examples,"contributing_units":self.units.len()})
    }
}
impl Store {
    /// Explicitly backfill old captures, one unit at a time, without startup scans.
    pub fn reindex_compiler_stacks(&self, run: &str) -> Result<usize> {
        let run = self.resolve_id(run)?;
        let run = run.as_str();
        let mut connection = self.connection()?;
        let units: Vec<String> = {
            let mut s = connection.prepare(
                "SELECT unit FROM profiles WHERE run=?1 UNION SELECT unit FROM events WHERE run=?1",
            )?;
            s.query_map([run], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (position, unit) in units.iter().enumerate() {
            if position == 0 || position % 10 == 0 || position + 1 == units.len() {
                eprintln!(
                    "buildscope: indexing compiler stacks {}/{}: {}",
                    position + 1,
                    units.len(),
                    unit
                );
            }
            let mut events: Vec<TraceEvent> = {
                let mut s = connection
                    .prepare("SELECT data FROM events WHERE run=?1 AND unit=?2 ORDER BY idx")?;
                let mut rows = s.query(params![run, unit])?;
                let mut events = Vec::new();
                while let Some(row) = rows.next()? {
                    let data: String = row.get(0)?;
                    events.push(serde_json::from_str(&data)?);
                }
                events
            };
            if events.is_empty() {
                let path = self
                    .root
                    .join("runs")
                    .join(run)
                    .join("events")
                    .join(format!("{unit}.json.zst"));
                if path.exists() {
                    events = serde_json::from_reader(BufReader::new(
                        zstd::stream::read::Decoder::new(File::open(path)?)?,
                    ))?;
                }
            }
            let tx = connection.transaction()?;
            index_events(&tx, run, unit, &events)?;
            tx.commit()?;
        }
        eprintln!(
            "buildscope: compiler stack indexing complete ({} units)",
            units.len()
        );
        Ok(units.len())
    }
    /// Merge compact selected projections and return at most 5000 visible nodes.
    pub fn compiler_flamegraph(&self, run: &str, q: FlamegraphQuery<'_>) -> Result<Value> {
        let run = self.resolve_id(run)?;
        let run = run.as_str();
        let connection = self.connection()?;
        let mut root = Node::default();
        let mut missing = Vec::new();
        let mut selected = 0;
        let mut unit_info = Vec::new();
        let (inventory_units, unprofiled_units, unsupported_units):(i64,i64,i64)=connection.query_row(
            "SELECT count(*),coalesce(sum(p.unit IS NULL),0),coalesce(sum(p.unit IS NOT NULL AND p.family!='clang'),0) FROM report_units u LEFT JOIN profiles p ON p.run=u.run AND p.unit=u.unit WHERE u.run=?1 AND (?2='' OR u.unit=?2) AND (?3='' OR u.target=?3) AND (?4='' OR CAST(u.compiler AS TEXT)=?4)",
            params![run,q.unit,q.target,q.compiler],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let mut indexed_units = 0;
        let mut granularities_us = BTreeSet::new();
        let mut unknown_granularity_units = 0;
        let mut units=connection.prepare("SELECT p.unit,u.compiler,u.source_key,EXISTS(SELECT 1 FROM compiler_stack_units i WHERE i.run=p.run AND i.unit=p.unit),p.granularity_us FROM profiles p JOIN report_units u ON u.run=p.run AND u.unit=p.unit WHERE p.run=?1 AND p.family='clang' AND (?2='' OR p.unit=?2) AND (?3='' OR u.target=?3) AND (?4='' OR CAST(u.compiler AS TEXT)=?4) ORDER BY p.unit")?;
        let rows = units.query_map(params![run, q.unit, q.target, q.compiler], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })?;
        let mut select = connection.prepare(
            "SELECT data FROM compiler_stacks WHERE run=?1 AND unit=?2 ORDER BY ordinal",
        )?;
        for row in rows {
            let (unit, compiler, source, indexed, granularity) = row?;
            if let Some(value) = granularity {
                granularities_us.insert(value);
            } else {
                unknown_granularity_units += 1;
            }
            selected += 1;
            if !indexed {
                missing.push(unit);
                continue;
            }
            indexed_units += 1;
            let unit_index = unit_info.len();
            unit_info.push((unit.clone(), source));
            let stacks = select.query_map(params![run, unit], |r| r.get::<_, String>(0))?;
            let mut prefix: Vec<(Option<usize>, Frame)> = Vec::new();
            for data in stacks {
                let stack: Stack = serde_json::from_str(&data?)?;
                let mut frames = vec![stack.frame.clone()];
                let mut parent = stack.parent;
                while let Some(index) = parent {
                    let (next, frame) = &prefix[index];
                    frames.push(frame.clone());
                    parent = *next;
                }
                frames.reverse();
                prefix.push((stack.parent, stack.frame));

                if q.direction == "bottom-up" {
                    frames.reverse();
                }
                frames.insert(
                    0,
                    Frame {
                        name: format!("Compiler {compiler}"),
                        ..Frame::default()
                    },
                );
                let sample_depth = if q.direction == "bottom-up" {
                    2
                } else {
                    frames.len()
                };
                root.add(
                    &frames,
                    stack.value_us,
                    &stack.examples,
                    unit_index,
                    Some(sample_depth),
                );
            }
        }
        let path: Vec<Frame> = if q.focus.is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(q.focus)?
        };
        let total = root.value;
        let mut node = &root;
        for frame in &path {
            let Some(child) = node.children.get(frame) else {
                return Ok(json!({"status":"invalid-focus","total_us":total}));
            };
            node = child;
        }
        let frame = path.last().cloned().unwrap_or(Frame {
            name: "Recorded compiler work".into(),
            ..Frame::default()
        });
        let mut budget = q.limit.clamp(2, 5000);
        let tree = node.render(
            &frame,
            &mut path.clone(),
            q.depth.clamp(1, 32),
            &mut budget,
            q.offset,
            &unit_info,
        );
        Ok(
            json!({"status":if selected==0{"unavailable"}else if missing.is_empty(){"ready"}else{"missing-index"},"total_us":total,"tree":tree,"selected_units":selected,"inventory_units":inventory_units,"trace_units":selected,"indexed_units":indexed_units,"unprofiled_units":unprofiled_units,"unsupported_units":unsupported_units,"granularities_us":granularities_us,"unknown_granularity_units":unknown_granularity_units,"coverage_status":if selected==0{"unavailable"}else if indexed_units<inventory_units{"partial"}else{"complete"},"missing_units":missing,"index_command":format!("buildscope reindex {run} --compiler-stacks"),"direction":q.direction,"notes":["Totals include observed indexed Clang scopes only; missing profiles, unsupported compiler timers and missing indexes do not imply zero work.","Widths attribute each covered interval once per compiler lane; summed lanes can exceed wall time.","Self means recorded coverage without recorded children; threshold-omitted work remains here.","Temporal nesting is not a semantic template call graph. Native inclusive durations remain available in the scope explorer."]}),
        )
    }
}
