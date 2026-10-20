//! Reviewable timestamp-change observations and conservative rebuild candidates.
//! Analysis consumes saved fingerprints and Ninja evidence without reading files.
use crate::{analysis, model::*};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Fingerprints bracketing one input change; absence means the file was unobserved.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FingerprintChange {
    /// Input path in the recorded build machine's namespace.
    pub path: String,
    /// previous-build or during-build identifies the observation interval.
    pub interval: String,
    /// Earlier observation; None means missing or unreadable.
    pub before: Option<FileStamp>,
    /// Later observation; None means missing or unreadable.
    pub after: Option<FileStamp>,
}

/// A candidate retains all associated actions; costs overlap between candidates.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RebuildCandidate {
    /// Input exhibiting timestamp-only change.
    pub path: String,
    /// timestamp-only or generated-output-churn.
    pub category: String,
    /// Index of the supporting fingerprint comparison.
    pub comparison: usize,
    /// Native explanation names the input, or association only.
    pub support: String,
    /// Original Ninja diagnostic lines naming this input.
    pub reasons: Vec<String>,
    /// Successful dependent action IDs, deduplicated within this candidate.
    pub actions: Vec<String>,
    /// Generator action IDs whose outputs contain this file.
    pub generators: Vec<String>,
    /// Associated action work, not exclusive cost or predicted savings.
    pub work_ms: f64,
}

/// Portable optional evidence; old and launcher captures have no such report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RebuildReport {
    /// Evidence format version, independent of the capture schema.
    pub version: u32,
    /// Preceding compatible local capture; None means no established predecessor.
    pub previous: Option<String>,
    /// Complete pre-build snapshot retained for interrupted-capture review.
    #[serde(default)]
    pub before_inputs: BTreeMap<String, FileStamp>,
    /// Whether successful ordinary-build boundaries were observed.
    pub eligible: bool,
    /// Saved fingerprint comparisons, including concurrent content changes.
    pub comparisons: Vec<FingerprintChange>,
    /// Potentially avoidable rebuilding supported by available observations.
    pub candidates: Vec<RebuildCandidate>,
    /// Sum over the union of associated actions, never the sum of candidate costs.
    pub combined_work_ms: f64,
    /// Coverage and interpretation notes shown with the evidence.
    pub notes: Vec<String>,
}

/// Compare known fingerprints at explicit boundaries, retaining missing observations.
pub fn compare_stamps(
    before: &BTreeMap<String, FileStamp>,
    after: &BTreeMap<String, FileStamp>,
    interval: &str,
    known: impl Iterator<Item = String>,
) -> Vec<FingerprintChange> {
    known
        .filter_map(|path| {
            let a = before.get(&path);
            let b = after.get(&path);
            (a != b).then(|| FingerprintChange {
                path,
                interval: interval.into(),
                before: a.cloned(),
                after: b.cloned(),
            })
        })
        .collect()
}

/// Derive conservative candidates from saved boundaries and observed graph reach.
pub fn analyze(
    capture: &Capture,
    previous: Option<String>,
    comparisons: Vec<FingerprintChange>,
    log: &str,
) -> RebuildReport {
    let eligible = capture.kind == "record"
        && capture.status == "success"
        && capture.summary.dependency_units == capture.project.units.len();
    let reasons = analysis::trigger_evidence(log, Path::new(&capture.project.build_dir));
    let mut candidates = Vec::new();
    if eligible {
        for (index, change) in comparisons.iter().enumerate() {
            let (Some(a), Some(b)) = (&change.before, &change.after) else {
                continue;
            };
            if a.digest
                .as_ref()
                .is_none_or(|d| d.len() != 64 || !d.bytes().all(|b| b.is_ascii_hexdigit()))
                || a.modified_ns == 0
                || b.modified_ns == 0
                || a.modified_ns == b.modified_ns
                || a.size != b.size
                || a.digest.is_none()
                || a.digest != b.digest
            {
                continue;
            }
            // A file changed at another boundary cannot support an unchanged-input claim.
            if comparisons.iter().any(|other| {
                other.path == change.path
                    && other
                        .before
                        .as_ref()
                        .zip(other.after.as_ref())
                        .is_some_and(|(a, b)| {
                            a.digest.is_some() && b.digest.is_some() && a.digest != b.digest
                        })
            }) {
                continue;
            }
            let units: BTreeSet<_> = capture
                .headers
                .iter()
                .filter(|h| h.path == change.path)
                .flat_map(|h| h.units.iter().map(String::as_str))
                .collect();
            let actions: Vec<_> = capture
                .actions
                .iter()
                .filter(|a| {
                    a.success
                        && (a.unit_id.as_deref().is_some_and(|u| units.contains(u))
                            || a.unit_id.as_ref().is_some_and(|u| {
                                capture
                                    .project
                                    .units
                                    .iter()
                                    .any(|unit| &unit.id == u && unit.source == change.path)
                            }))
                })
                .collect();
            if actions.is_empty() {
                continue;
            }
            let generators: Vec<_> = capture
                .actions
                .iter()
                .filter(|a| a.success && a.kind == "generate" && a.outputs.contains(&change.path))
                .map(|a| a.id.clone())
                .collect();
            // Generated-output churn requires the generator to have completed before a dependent action.
            if !generators.is_empty()
                && !capture
                    .actions
                    .iter()
                    .filter(|a| generators.contains(&a.id))
                    .any(|g| actions.iter().any(|a| g.end_ms <= a.start_ms))
            {
                continue;
            }
            let native = reasons.get(&change.path).cloned().unwrap_or_default();
            candidates.push(RebuildCandidate {
                path: change.path.clone(),
                category: if generators.is_empty() {
                    "timestamp-only"
                } else {
                    "generated-output-churn"
                }
                .into(),
                comparison: index,
                support: if native.is_empty() {
                    "association-only"
                } else {
                    "native-input-explanation"
                }
                .into(),
                reasons: native,
                actions: actions.iter().map(|a| a.id.clone()).collect(),
                generators,
                work_ms: actions
                    .iter()
                    .map(|a| (a.end_ms - a.start_ms).max(0.0))
                    .sum(),
            });
        }
    }
    let action_ids: BTreeSet<_> = candidates.iter().flat_map(|c| c.actions.iter()).collect();
    let combined_work_ms = capture
        .actions
        .iter()
        .filter(|a| action_ids.contains(&a.id))
        .map(|a| (a.end_ms - a.start_ms).max(0.0))
        .sum();
    RebuildReport { version: 1, previous, before_inputs: BTreeMap::new(), eligible, comparisons, candidates, combined_work_ms,
        notes: vec![
            "Boundary fingerprints do not prove contents remained unchanged throughout execution. Timestamp-dependent macros and other input or command changes can make recompilation necessary.".into(),
            "Associated action work overlaps between candidates and is not predicted elapsed-time savings. Native explanations name inputs without establishing exclusive causation.".into(),
            if eligible { "Missing or unstable hashes remain unknown. Inspect other recorded changes before changing a build rule." } else { "Candidates require a successful ordinary recording with complete dependency inventory. Profiles deliberately invalidate outputs; launcher batches and interrupted builds lack complete evidence." }.into(),
        ] }
}

/// Convert candidates into the existing evidence-linked findings vocabulary.
pub fn findings(report: &RebuildReport) -> Vec<Finding> {
    report.candidates.iter().map(|c| Finding {
        category: "avoidable-rebuild".into(), title: format!("Potentially avoidable rebuild: {}", c.path),
        explanation: format!("Identical observed contents with a changed modification time; {} dependent actions executed ({:.1} ms associated work). Support: {}. Other changes may also require rebuilding.", c.actions.len(), c.work_ms, c.support),
        subject: c.path.clone(), origin: "derived".into(),
        evidence: std::iter::once(format!("Saved fingerprint comparison {} in rebuild-evidence.json; {}", c.comparison, c.category)).chain(c.reasons.clone()).collect(),
        experiment: "Inspect generator output handling and other recorded changes. Preserve timestamps for unchanged generated contents, then compare ordinary incremental recordings.".into(),
    }).collect()
}
