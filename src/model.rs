//! Versioned capture model shared by collectors, pure analyses, storage, and reports.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Current capture and archive schema; readers reject newer versions.
pub const SCHEMA_VERSION: u32 = 1;

/// One durable ordinary build, managed profile, or collected compiler batch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capture {
    /// Archive schema governing all nested data.
    pub schema_version: u32,
    /// Unique run identifier used by CLI and archives.
    pub id: String,
    /// Unix milliseconds used for history ordering.
    pub started_ms: u64,
    /// Completion time; None means not finalized.
    pub finished_ms: Option<u64>,
    /// Ninja execution milliseconds, or observed compiler span for launcher batches.
    pub elapsed_ms: f64,
    /// running, success, failed, or interrupted.
    pub status: String,
    /// record, profile, or launcher capture mode.
    pub kind: String,
    /// User scenario or experiment name; None means unnamed.
    pub label: Option<String>,
    /// Source checkout observed before execution; launcher batches summarize their
    /// retained per-invocation observations. Missing historical data is NotRecorded.
    #[serde(default)]
    pub git: crate::git::GitSnapshot,
    /// Host identity used to flag measurements from different machines.
    pub machine: String,
    /// Selected compiler-affecting environment values, excluding arbitrary environment data.
    pub environment: BTreeMap<String, String>,
    /// Cache treatment; ordinary recordings preserve caches and report observed outcomes separately.
    pub cache_policy: String,
    /// Explicit Ninja job limit; None means the default or an older capture without job metadata.
    #[serde(default)]
    pub jobs: Option<usize>,
    /// Measured compilation workload; None means this older capture did not collect outcomes.
    #[serde(default)]
    pub workload: Option<CompilationWorkload>,
    /// Original build behind a managed profile tree; None when the build is not managed.
    pub profile_origin: Option<String>,
    /// Requested targets; empty means Ninja defaults.
    pub targets: Vec<String>,
    /// Configuration and dependency inventory for this run.
    pub project: ProjectSnapshot,
    /// Executed actions from this run, excluding historical log entries.
    pub actions: Vec<BuildAction>,
    /// Detail from compiler invocations that actually ran.
    pub profiles: Vec<CompilationProfile>,
    /// Joined header reach, timing, and invalidation evidence.
    pub headers: Vec<HeaderSummary>,
    /// Dependency fingerprints after execution, keyed by absolute path.
    pub inputs: BTreeMap<String, FileStamp>,
    /// Observed changes since the last compatible run or during this build.
    pub changes: Vec<InputChange>,
    /// Deterministic evidence-linked diagnostics.
    pub findings: Vec<Finding>,
    /// Coverage limits and collection problems.
    pub notes: Vec<String>,
    /// Metrics derived from this run's action set.
    pub summary: BuildSummary,
}

/// Counts successful compilation units separately from cache reuse and unobserved outcomes.
/// A source compiled for multiple outputs counts once per output. Repeated executions of
/// one unit remain unknown because cache logs cannot reliably associate them with a unit.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CompilationWorkload {
    /// Successful units known to have run the compiler, from direct commands or ccache misses.
    pub compiled: usize,
    /// Successful units served by a confirmed ccache direct or preprocessed cache hit.
    pub cache_hits: usize,
    /// Executed units without an unambiguous successful compiler/cache outcome.
    pub unknown: usize,
    /// Requested units skipped by a successful Ninja build; None for partial or launcher captures.
    pub up_to_date: Option<usize>,
    /// Compilation inventory within the requested Ninja graph; None when scope is unavailable.
    pub total_units: Option<usize>,
    /// Distinct source paths among executed units; multiple target outputs can share one source.
    pub source_files: usize,
    /// Sorted executed unit identities used to detect workload changes beyond the count alone.
    pub executed_units: Vec<String>,
    /// Evidence sources and coverage limits retained with the counts for reports and archives.
    pub notes: Vec<String>,
}

/// Configured CMake and Ninja inventory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    /// Absolute source root used to identify project files.
    pub source_dir: String,
    /// Absolute build root used to resolve outputs.
    pub build_dir: String,
    /// Repository identity plus source subdirectory, or conservative local source identity.
    #[serde(default)]
    pub project_id: String,
    /// Hash of normalized compiler identities and semantic commands.
    pub configuration_id: String,
    /// CMake build type; empty means unspecified.
    pub build_type: String,
    /// Distinct compilers referenced by compilation units.
    pub compilers: Vec<CompilerInfo>,
    /// Source and output pairs, including multiple configurations of a source.
    pub units: Vec<CompilationUnit>,
    /// Output to active file dependencies reported by Ninja.
    pub dependencies: BTreeMap<String, Vec<String>>,
    /// Output to prerequisite files or generated outputs.
    pub graph: BTreeMap<String, Vec<String>>,
    /// Native Ninja rule names used to distinguish linking from custom generators.
    #[serde(default)]
    pub rules: BTreeMap<String, String>,
    /// Native Ninja commands keyed by output; compiler argument vectors remain separate.
    #[serde(default)]
    pub commands: BTreeMap<String, String>,
    /// Exact response-file contents retained after execution, keyed by absolute filename.
    #[serde(default)]
    pub response_files: BTreeMap<String, String>,
    /// User configuration values used when creating a profile tree.
    pub cmake_cache: BTreeMap<String, String>,
}

/// Compiler identity and detected capabilities.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompilerInfo {
    /// Actual toolchain executable.
    pub executable: String,
    /// Full version output for comparisons.
    pub version: String,
    /// clang, gcc, or unknown.
    pub family: String,
    /// Whether Clang advertises an explicit trace destination; false uses its output sidecar.
    #[serde(default)]
    pub explicit_trace_path: bool,
    /// Whether the executable advertises verbose trace metadata.
    pub verbose_trace: bool,
    /// Whether a probe successfully produced structured diagnostics.
    pub sarif: bool,
}

/// One source compiled into one output.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompilationUnit {
    /// Stable source and output identity independent of profile relocation.
    pub id: String,
    /// Absolute source filename, including generated unity sources.
    pub source: String,
    /// Portable source identity relative to source or build root.
    pub source_key: String,
    /// Absolute object, PCH, or module output.
    pub output: String,
    /// Output identity relative to the build tree.
    pub output_key: String,
    /// Working directory required by the original command.
    pub directory: String,
    /// Argument vector without shell interpretation.
    pub arguments: Vec<String>,
    /// Index into ProjectSnapshot.compilers.
    pub compiler: usize,
    /// Hash of normalized semantic arguments.
    pub command_id: String,
}

/// One executed Ninja command or observed public-launcher compiler invocation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildAction {
    /// Action identity local to this capture.
    pub id: String,
    /// All outputs emitted by the same command.
    pub outputs: Vec<String>,
    /// Compilation identity; None means no inventory match.
    pub unit_id: Option<String>,
    /// compile, pch, module, link, configure, or generate.
    pub kind: String,
    /// Ninja clock segment; regeneration may restart its native execution clock. Launcher actions use zero.
    #[serde(default)]
    pub clock_segment: usize,
    /// Start relative to capture execution, with later clock segments aligned by completion.
    pub start_ms: f64,
    /// End relative to the same origin.
    pub end_ms: f64,
    /// Native Ninja command hash; empty when only a launcher invocation was observed.
    pub command_hash: String,
    /// Native invalidation explanations relevant to these outputs.
    pub reasons: Vec<String>,
    /// Whether the action completed successfully.
    pub success: bool,
}

/// Dependency fingerprint used to distinguish edits from timestamp changes.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FileStamp {
    /// Unix nanoseconds from filesystem metadata; zero means unavailable.
    pub modified_ns: u128,
    /// File length in bytes.
    pub size: u64,
    /// SHA256 content hash; None means unreadable or beyond the hashing limit.
    pub digest: Option<String>,
}

/// Observed input change without exclusive causal attribution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputChange {
    /// Absolute changed dependency filename.
    pub path: String,
    /// content, timestamp, created, removed, or unknown.
    pub kind: String,
    /// Whether the before and after snapshots bracket this change.
    pub during_build: bool,
}

/// Decoded detail from one compiler invocation.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CompilationProfile {
    /// Compilation unit responsible for this profile.
    pub unit_id: String,
    /// Compiler family that actually produced these measurements.
    pub family: String,
    /// Relative original trace or stderr artifact.
    pub artifact: String,
    /// Decoded nonsummary trace-event count.
    pub event_count: usize,
    /// Clang threshold; None means an aggregate GCC report.
    pub granularity_us: Option<u64>,
    /// Whether richer trace metadata was requested.
    pub verbose: bool,
    /// Native compiler timers with explicit scope.
    pub phases: Vec<Metric>,
    /// Named recorded template-instantiation work.
    pub templates: Vec<SymbolCost>,
    /// Named recorded optimization and code-generation work.
    pub functions: Vec<SymbolCost>,
    /// Recorded compile-time evaluation activity.
    pub evaluations: Vec<SymbolCost>,
    /// Textual header processing measurements for this unit.
    pub header_costs: Vec<HeaderCost>,
    /// Preprocessor include relationships actually encountered.
    pub includes: Vec<IncludeEdge>,
    /// Measurement limits or unsupported attribution.
    pub notes: Vec<String>,
}

/// Measurement with explicit unit, provenance, and coverage.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metric {
    /// Native compiler timer or derived metric name.
    pub name: String,
    /// Numeric magnitude interpreted with unit.
    pub value: f64,
    /// Explicit unit such as ms, bytes, or count.
    pub unit: String,
    /// measured, derived, or estimated.
    pub origin: String,
    /// complete or recorded-events, describing available evidence.
    pub coverage: String,
}

/// Aggregate recorded work for a named compiler entity.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SymbolCost {
    /// Original compiler spelling retained for evidence.
    pub name: String,
    /// Accumulated inclusive recorded scope duration.
    pub total_ms: f64,
    /// Recorded scope count, not an unfiltered semantic count.
    pub count: usize,
    /// Longest recorded scope.
    pub max_ms: f64,
    /// Definition or source filename; None means not supplied.
    pub file: Option<String>,
    /// One-based source line; None means not supplied.
    pub line: Option<u64>,
}

/// Measured textual header processing in one compilation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeaderCost {
    /// Absolute resolved header filename.
    pub path: String,
    /// Duration including nested header processing.
    pub inclusive_ms: f64,
    /// Duration excluding the union of recorded nested headers.
    pub recorded_self_ms: f64,
    /// Number of recorded header-processing intervals.
    pub count: usize,
}

/// An observed include relationship, never inferred from a flat depfile.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IncludeEdge {
    /// Absolute including source or header.
    pub parent: String,
    /// Absolute included header.
    pub child: String,
    /// Include depth, with one meaning directly included by the source.
    pub depth: usize,
}

/// Header reach and work within one capture.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeaderSummary {
    /// Absolute path joining dependency and timing observations.
    pub path: String,
    /// Portable path for presentation and comparisons.
    pub key: String,
    /// Distinct dependent units in the available inventory.
    pub units: Vec<String>,
    /// Units with a recorded direct include.
    pub direct_units: Vec<String>,
    /// Dependent units actually rebuilt during this run.
    pub rebuilt_units: usize,
    /// Accumulated dependent compilation duration, not elapsed-time savings.
    pub rebuilt_work_ms: f64,
    /// Inclusive textual processing work; None means unmeasured.
    pub processing_ms: Option<f64>,
    /// Work excluding recorded nested headers; None means unmeasured.
    pub recorded_self_ms: Option<f64>,
    /// Per-unit inclusive measurements for distributions.
    pub samples_ms: Vec<f64>,
    /// Number of units with actual header timing.
    pub measured_units: usize,
    /// Whether Ninja explicitly names this header as a dirty input.
    pub explicit_trigger: bool,
    /// Whether a changed dependency coincides with rebuilt dependents.
    pub changed_contributor: bool,
    /// Native invalidation explanations supporting explicit_trigger.
    pub evidence: Vec<String>,
}

/// Evidence-linked explanation and a concrete follow-up experiment.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Finding {
    /// Stable diagnostic category.
    pub category: String,
    /// Short explanation for report lists.
    pub title: String,
    /// What observations establish and their attribution limits.
    pub explanation: String,
    /// Unit ID or header path to open in the report.
    pub subject: String,
    /// measured, derived, or estimated support.
    pub origin: String,
    /// Native measurements or explanations supporting the finding.
    pub evidence: Vec<String>,
    /// Specific source inspection or follow-up measurement.
    pub experiment: String,
}

/// Build metrics with explicit dependency-path semantics.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct BuildSummary {
    /// Sum of action durations, potentially greater than elapsed time.
    pub work_ms: f64,
    /// Maximum simultaneous observed actions, not CPU utilization.
    pub max_concurrency: usize,
    /// Longest dependency-chain work, ignoring resource constraints.
    pub dependency_path_ms: f64,
    /// Action IDs on that chain in prerequisite order.
    pub dependency_path: Vec<String>,
    /// Inventory units with usable Ninja dependency records.
    pub dependency_units: usize,
    /// Distinct units observed executing during this run.
    pub observed_units: usize,
}

/// One normalized compiler scope; summary events are separate metrics.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraceEvent {
    /// Index local to this unit after chronological normalization.
    pub id: usize,
    /// Compiler activity name.
    pub name: String,
    /// Native symbol, file, or pass spelling.
    pub detail: String,
    /// Compiler process and thread lane; containment never crosses lanes.
    pub thread: String,
    /// Start relative to the compiler's own trace origin.
    pub start_us: f64,
    /// Scope duration in microseconds.
    pub duration_us: f64,
    /// Source filename; None means not supplied.
    pub file: Option<String>,
    /// One-based source line; None means not supplied.
    pub line: Option<u64>,
    /// Containing recorded scope; None means root or crossing scope.
    pub parent: Option<usize>,
    /// Depth in the recorded time-containment tree.
    pub depth: usize,
}

/// Per-process launcher manifest written atomically before and after execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Invocation {
    /// Unique artifact prefix for concurrent invocations.
    pub id: String,
    /// Working directory for source and output resolution.
    pub directory: String,
    /// Actual instrumented command, without shell expansion.
    pub arguments: Vec<String>,
    /// Response contents read immediately before this compiler ran.
    #[serde(default)]
    pub response_files: BTreeMap<String, String>,
    /// Recognized absolute output path; None means not identified.
    pub output: Option<String>,
    /// Detected compiler family.
    pub family: String,
    /// Completion code; None means not finalized.
    pub exit_code: Option<i32>,
    /// Wall-clock anchor for action placement; None in older or unstarted invocations.
    #[serde(default)]
    pub started_ms: Option<u64>,
    /// Launcher duration including compiler startup.
    pub elapsed_ms: f64,
    /// Relative trace artifact; None for aggregate collection.
    pub trace: Option<String>,
    /// Relative artifact containing includes, diagnostics, and timings.
    pub stderr: String,
    /// Clang event cutoff; None for GCC.
    pub granularity_us: Option<u64>,
    /// Whether verbose Clang metadata was requested.
    pub verbose: bool,
}
