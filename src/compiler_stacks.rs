//! Additive temporal attribution of recorded compiler scopes, without semantic inference.
use crate::model::TraceEvent;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Exact native identity. Compiler identity is added by the store during aggregation.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Frame {
    /// Native compiler activity, or explicit attribution bucket.
    pub name: String,
    /// Unmodified native detail spelling.
    pub detail: String,
    /// Native source spelling, when supplied.
    pub file: Option<String>,
    /// Native source line, when supplied.
    pub line: Option<u64>,
}
impl From<&TraceEvent> for Frame {
    fn from(e: &TraceEvent) -> Self {
        Self {
            name: e.name.clone(),
            detail: e.detail.clone(),
            file: e.file.clone(),
            line: e.line,
        }
    }
}
/// One node of a compact per-unit prefix trie.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stack {
    /// Parent node ordinal; none for recorded roots.
    pub parent: Option<usize>,
    /// Exact native identity.
    pub frame: Frame,
    /// Exclusive additive coverage attributed to this terminal stack.
    pub value_us: f64,
    /// A small set of native event IDs useful for evidence drilldown.
    pub examples: Vec<Example>,
}
/// Native scope sample, kept separate from attributed graph coverage.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Example {
    /// Original scope index for drilldown.
    pub event_id: usize,
    /// Inclusive native scope duration, not additive graph width.
    pub duration_us: f64,
}
/// Sweep each lane independently. Crossing scopes contribute once to an explicit
/// ambiguity bucket below their common enclosing prefix. Gaps inside an outer
/// recorded scope remain its recorded self coverage, including omitted children.
pub fn project(events: &[TraceEvent]) -> Vec<Stack> {
    let mut lanes: BTreeMap<&str, Vec<&TraceEvent>> = BTreeMap::new();
    for e in events {
        if e.start_us.is_finite()
            && e.duration_us.is_finite()
            && e.duration_us > 0.0
            && (e.start_us + e.duration_us).is_finite()
            && !e.name.starts_with("Total ")
        {
            lanes.entry(&e.thread).or_default().push(e);
        }
    }
    let mut stacks: Vec<Stack> = Vec::new();
    let mut edges: BTreeMap<(Option<usize>, Frame), usize> = BTreeMap::new();
    for lane in lanes.values() {
        let mut boundaries: Vec<(f64, bool, usize)> = lane
            .iter()
            .enumerate()
            .flat_map(|(i, e)| {
                [
                    (e.start_us, true, i),
                    (e.start_us + e.duration_us, false, i),
                ]
            })
            .collect();
        boundaries.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let mut active = std::collections::BTreeSet::new();
        let mut previous = boundaries.first().map_or(0.0, |b| b.0);
        for (time, start, index) in boundaries {
            if time > previous && !active.is_empty() {
                let mut chain: Vec<&TraceEvent> = active.iter().map(|i: &usize| lane[*i]).collect();
                chain.sort_by(|a, b| {
                    a.start_us
                        .total_cmp(&b.start_us)
                        .then((b.start_us + b.duration_us).total_cmp(&(a.start_us + a.duration_us)))
                        .then(a.id.cmp(&b.id))
                });
                // Find the first crossing, then keep only scopes enclosing every
                // active interval. Descendants of either crossing side are ambiguous.
                let crossing = chain
                    .windows(2)
                    .any(|w| w[1].start_us + w[1].duration_us > w[0].start_us + w[0].duration_us);
                let mut frames = Vec::new();
                if crossing {
                    let max_end = chain
                        .iter()
                        .map(|e| e.start_us + e.duration_us)
                        .fold(f64::NEG_INFINITY, f64::max);
                    for e in &chain {
                        if e.start_us + e.duration_us < max_end {
                            break;
                        }
                        frames.push(Frame::from(*e));
                    }
                    // The crossing sides themselves must not be represented as
                    // enclosing frames, even when the earlier one ends latest.
                    let first_cross = chain
                        .windows(2)
                        .position(|w| {
                            w[1].start_us + w[1].duration_us > w[0].start_us + w[0].duration_us
                        })
                        .unwrap();
                    frames.truncate(first_cross);
                    frames.push(Frame {
                        name: "[ambiguous overlap]".into(),
                        ..Frame::default()
                    });
                } else {
                    frames.extend(chain.iter().map(|e| Frame::from(*e)));
                }
                let mut parent = None;
                for (position, frame) in frames.into_iter().enumerate() {
                    let key = (parent, frame.clone());
                    let index = *edges.entry(key).or_insert_with(|| {
                        let index = stacks.len();
                        stacks.push(Stack {
                            parent,
                            frame,
                            value_us: 0.0,
                            examples: Vec::new(),
                        });
                        index
                    });
                    let entry = &mut stacks[index];
                    if let Some(e) = chain.get(position)
                        && entry.examples.len() < 5
                        && !entry.examples.iter().any(|sample| sample.event_id == e.id)
                    {
                        entry.examples.push(Example {
                            event_id: e.id,
                            duration_us: e.duration_us,
                        });
                    }
                    parent = Some(index);
                }
                let entry = &mut stacks[parent.unwrap()];
                entry.value_us += time - previous;
            }
            if start {
                active.insert(index);
            } else {
                active.remove(&index);
            }
            previous = time;
        }
    }
    stacks
}
