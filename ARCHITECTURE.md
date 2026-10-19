# Buildscope architecture

Buildscope is a standalone Rust application. The Rust library owns collection,
normalization, analysis, persistence, and report queries. The binary is a thin
CLI entry point. Browser assets are embedded in the binary.

## Main interfaces

| Boundary | Implementation | Responsibility |
| --- | --- | --- |
| User commands | [cli.rs](src/cli.rs) | Record, profile, launch, collect, inspect, compare, compare-series, report, export/import, recover |
| Project inventory | [project.rs](src/project.rs) | CMake compilation database, Ninja dependencies/rules/graph, command identities, input snapshots |
| Process effects | [capture.rs](src/capture.rs) | Ordinary builds, owned profiling trees, launcher artifacts, cancellation |
| Public compiler launcher | [launcher.rs](src/launcher.rs) | Independent per-invocation spool, compiler identity cache, explicit batch collection |
| Compiler decoding | [compiler.rs](src/compiler.rs), [decoder.rs](src/decoder.rs) | Capability probes, Clang scopes, GCC timers, bounded invocation decoding and compression |
| Shared vocabulary | [model.rs](src/model.rs) | Versioned captures and explicitly scoped observations |
| Pure analysis | [analysis.rs](src/analysis.rs) | Header joins, findings, concurrency/dependency work, comparisons |
| Persistence | [store.rs](src/store.rs) | SQLite indexes, compressed evidence, archive exchange, source preview |
| Presentation | [report.rs](src/report.rs), [web](src/web) | Read-only loopback API and interactive report |

Project discovery produces ProjectSnapshot. Recording produces Capture and
per-process Invocation manifests. Compiler decoding produces
CompilationProfile plus normalized TraceEvent records. Analysis derives
HeaderSummary, Finding, and BuildSummary. The CLI and report consume the same
model and analyses.

[Project discovery](src/project.rs) executes Ninja's graph tool once and derives
both prerequisite relations and native rule names from that DOT snapshot. Raw and
response-expanded compilation databases remain separate because they preserve
different command evidence.

Collection owns mutable process/filesystem effects. Analyses operate on explicit
observations and do not execute build commands. Compiler-specific vocabulary is
preserved alongside common metrics.

## Identity and configuration

A CompilationUnit identifies a source/output pair, not merely a filename.
source_key and output_key relocate with the source/build roots; their hash forms
the stable unit ID. command_id separately hashes normalized semantic arguments.
Output/depfile arguments and collector instrumentation are excluded from that
semantic identity.

The project configuration ID combines project identity, commands, compiler
identities/versions, and build type. Project identity hashes the repository remote
and CMake source subdirectory; a project without a remote uses its canonical local
source root. A Capture also records host identity, requested targets, selected
compiler-affecting environment, mode, and cache policy. Comparisons disclose
differences rather than treating unlike runs as equivalent.

Absolute filenames retain source navigation context. Portable keys distinguish
source-relative, generated, and external files. External SDK paths are not
silently merged across toolchains.

## Git provenance

[git.rs](src/git.rs) owns the `GitSnapshot` model, best-effort local observation,
formatting, and pure batch summarization. `Capture.git` defaults to `NotRecorded`
for existing schema-1 captures; the schema and SQLite layout remain unchanged.
Available snapshots retain observation time, local branch, full HEAD object ID,
and repository-wide dirtiness. Detached HEAD has no branch; an unborn branch has
no commit. Explicit statuses distinguish non-repositories, discovery failures,
missing historical metadata, and mixed batches. No remote URLs or credentials
are added to the capture.

Managed [capture::execute](src/capture.rs) observes `ProjectSnapshot.source_dir`
before persisting the running capture and spawning Ninja. The public
[launcher](src/launcher.rs) reads `CMAKE_HOME_DIRECTORY` directly from CMake's
cache and observes that source checkout before `run_compiler`, without full Ninja
discovery or SQLite access. Missing cache context yields unavailable provenance;
generated source locations and the launcher's working directory are not used as
repository fallbacks.

Each completed launcher observation retains its snapshot. Collection summarizes
only these saved values, ignoring timestamps for branch/commit agreement and
combining dirty state conservatively: any true is true, all false is false,
otherwise unknown. Branch, commit, or discovery-outcome disagreement produces a
mixed summary with no single branch/commit. Individual snapshots survive in the
retained `observations/*.json` files and archives. Old spool entries default to
not recorded and are never backfilled from the current checkout.

Observation uses Git's porcelain-v2 NUL-delimited status with branch headers,
explicit untracked/submodule handling, and optional locks disabled. Explicit
Git directory/worktree/index environment overrides are removed so the query uses
the configured source directory. Filenames are handled as bytes; rename source
paths cannot be mistaken for branch headers. Discovery is outside measured build
and compiler time, though per-invocation scans add launcher overhead. An observation
does not guarantee the working tree remained unchanged during execution.

[store.rs](src/store.rs) includes Git summaries in history and report data.
[analysis.rs](src/analysis.rs) includes both revisions in comparisons and summarizes
series provenance without changing configuration identity or compatibility.
The CLI and embedded report render saved metadata, including unknown states;
report commit buttons copy the full object ID. Reports and imports do not refresh
revision metadata from the current checkout.

## Recording lifecycle

1. Discover the configured project and preceding compatible capture.
2. Save dependency fingerprints and a baseline Ninja log outside measured time.
3. Persist a running capture before spawning Ninja.
4. Drain stdout/stderr concurrently into artifacts while preserving diagnostics.
5. Forward cancellation to the build's process group.
6. Read the new Ninja log, refreshing dependencies and graph after execution.
7. Decode invocation artifacts and run optional preprocessing probes.
8. Analyze observations, persist the final capture, and return the build status.

The recorder uses a monotonic clock for total Ninja duration. Native Ninja
timestamps supply action intervals. Compiler traces keep their own local time
origin; the UI does not pretend they align exactly with the build clock.

CMake regeneration can restart Ninja's clock. Later clock segments are aligned
at prior completion, preserving native action durations. The capture explicitly
labels this derived alignment. Configuration actions provide barriers in the
dependency-chain calculation.

New Ninja log rows are compared against the pre-run rows, and outputs sharing
one command execution are grouped into one BuildAction. A failed action need
not have a successful-log row; retained diagnostics and partial-coverage notes
make this explicit.

Input fingerprints include known graph inputs and previous dependency records.
Newly discovered dependencies are not labelled created during execution unless
their path was in the pre-build inventory. Content hashes distinguish a real edit
from a timestamp-only update when available.

## Managed profiling

profile creates a separate, marked CMake build tree. The marker binds it to the
original source/build pair. A nonempty unowned directory is rejected. Before
profiling, only selected compilation outputs under the owned tree are removed.
Existing developer outputs are not invalidated.

The CMake launcher invokes the same Buildscope executable's internal __launch
entry point. Without capture environment variables, the launcher transparently
runs the compiler, allowing CMake checks and ordinary later builds.

Each captured invocation writes a unique manifest and artifact set. Parallel
compilers never append to one shared manifest or SQLite connection. The parent
imports their observations after execution. Caches are bypassed in the managed
profile configuration; standard captures preserve the original build's behavior.

Source/preset/toolchain options and explicit overrides determine the new tree.
Differences from the original semantic configuration are recorded. Alternative
compiler profiles retain their actual compiler identity.

## Public launcher and batch collection

`launch` passes explicit context to [capture::run_compiler](src/capture.rs), without
modifying the process environment or opening SQLite. Each process creates a
unique directory under the configured build's `.buildscope-launcher/pending`.
Compiler identity probes are cached by executable path, modification time, and
size. Arguments remain vectors; response files are read before the compiler can
remove them. Configuration probes pass through without creating observations.

An atomic observation.json is the completion marker. It retains the compiler's
wall-clock placement, monotonic duration, original expanded arguments, actual
instrumented invocation, selected environment, and post-compilation input
fingerprints. Source fingerprints never get replaced with collection-time
contents. The public launcher preserves GCC's native diagnostic format.

`collect` acquires a kernel lock shared only by collectors for that build. It
reads completed observations, queries the current Ninja inventory, and constructs
a `launcher` capture. Unfinished observations remain untouched. Actual invocation
commands replace matching inventory commands, with mismatches disclosed. Native
Ninja compdb discovery strips Buildscope launcher arguments before identifying
the compiler. Named data uses the existing compiler decoders and event indexes.

Capture finalization and invocation receipts commit in one SQLite transaction.
Spool cleanup follows that commit; a collector interrupted during cleanup skips
receipted observations on retry. Deterministic batch IDs also allow retries of
indexing interrupted before finalization. Raw observations are copied into the
capture so archives retain their provenance.

[Store persistence](src/store.rs) commits the complete running manifest for crash
recovery without building report indexes. It invalidates derived rows from any
earlier attempt; finalization rebuilds them in the same transaction as receipts.
An explicit overview request can build indexes for a running checkpoint on demand.
That lazy indexing holds a SQLite writer transaction and reloads the manifest under
the lock, so it cannot publish a stale checkpoint over a completed capture or leave
partial report rows after interruption. Recovery always reads the manifest,
independently of those disposable indexes.

JSON manifests and imported event streams use buffered readers. Temporary hash
maps in [collection](src/launcher.rs) and [decoding](src/decoder.rs) index outputs
and unit IDs while the vectors retain their deterministic order. Output indexes
preserve first-match semantics; repeated invocation profiles replace the existing
unit's profile in place, with all raw attempts retained.

[Invocation decoding](src/decoder.rs) uses an [ordered worker pool](src/worker_pool.rs)
shared by collection, profiling, and recovery. It uses at most four workers, limited
by available CPUs and invocation count. Each worker owns one outstanding invocation;
its next job is submitted only after the calling thread consumes its previous result.
That bounds active decoders plus retained event vectors, even when the first trace
is slow. The bound is by invocation count, not an absolute byte limit; one large
trace can still need substantial memory.

Workers decode and compress invocation-local traces and prepare normalized events
in unique temporary files. Only the calling thread publishes per-unit event archives
and writes SQLite, in sorted manifest order. This preserves profile order and the
existing latest-decoded-invocation behavior when several invocations share a unit.
On failure all workers are joined and unpublished event files are removed. Temporary
files left by a process kill are never used as completed evidence. Raw compression
publishes its updated invocation manifest before removing the original trace, so
recovery can read either the old raw trace or its completed compressed replacement.
Launcher receipts remain part of finalization, after decoding and indexing succeed.

Launcher batches have no observed full-build boundary or dependency-chain work.
The report labels their first-to-last compiler span separately from Ninja elapsed
time. Link/generator timing, native rebuild explanations, requested targets, and
no-op builds cannot be recovered from compiler calls. A successful batch means
its observed compilers exited successfully. Comparison conditions and history
keep launcher batches separate from ordinary builds and managed profiles.

## Compiler observations

Compiler discovery checks whether Clang advertises an explicit time-trace path.
Modern drivers write directly into each invocation's raw artifacts. Older drivers
use `-ftime-trace`, then [capture.rs](src/capture.rs) copies the output-derived
JSON sidecar into the same artifact layout. Response-file outputs are expanded
before resolving that path. Any existing sidecar is moved aside and restored
after compilation, including failures, so stale traces are never collected.
This shared execution path serves managed profiles and the public launcher.

Clang decoding handles complete X scopes, B/E scopes, and asynchronous b/e
Source spans. Native Total events become metrics rather than duplicate trace
scopes. Time containment is normalized independently within each process/thread
lane. It is distinct from semantic template-instantiation dependencies.

Header inclusive processing and recorded-child-exclusive processing are
calculated from Source intervals. Exclusive work removes recorded nested header
intervals, not arbitrary template/backend work. Missing children from filtered
traces limit this measurement. Named symbols aggregate inclusive recorded scopes
and retain their original spelling and available source metadata.

GCC text decoding handles wall-only and user/system/wall layouts. SARIF decoding
uses gcc/timeReport when present. GCC timers can be nested or overlapping;
individual header and specialization costs remain absent.

-H provides actual include nesting for both compilers. Flat Ninja dependency
lists establish reach only. Skipped include directives and reused PCH/module
contents are not manufactured as observed textual processing.

## Evidence and analysis

Header summaries join distinct units, observed include relationships, textual
processing samples, changed inputs, and Ninja explanations.

explicit_trigger requires a native explanation naming the input and observed
rebuilt dependents. changed_contributor records co-occurrence of a changed
dependency and rebuilt dependents. Neither means exclusive causal attribution.
Native wording remains available as evidence.

Header reach can include dependency records from earlier successful compilations.
Compiler-detail coverage describes only invocations in this capture. Missing
timings are optional values, never numeric zero. Metrics carry units, provenance,
and coverage.

BuildSummary separates accumulated action work, maximum action concurrency, and
the longest observed prerequisite-chain work. Graph traversal passes through
phony nodes. The chain ignores resource scheduling and is not an elapsed-savings
estimate. Deterministic findings propose an inspection or measurement, preserving
these attribution limits.

Comparison joins units by stable identity and headers by portable key. Repeated
label comparisons use successful captures, report distribution statistics, and
check recorded condition fingerprints across all samples.

## Storage and reporting

SQLite stores run manifests plus indexes for events, headers, include chains,
unit details, and report summaries. Raw Clang traces and normalized events are
Zstandard-compressed in per-run directories. Optional preprocessing output is
also compressed. JSON manifests are replaced atomically.

Report summaries omit raw events, named-symbol arrays, full command arguments,
and dependency matrices. Unit/header detail is loaded through indexed queries.
Trace search is paginated and capped, with a parent index supporting nested-scope
inspection. Browser tables page independently of the complete capture.

Global compiler work uses [store/compiler_work.rs](src/store/compiler_work.rs).
`Store::compiler_work` takes a `CompilerWorkQuery` and serves bounded pages from
`/api/compiler-work`. A partial index over relevant normalized `events` supports
existing captures without re-profiling or a manifest-schema change. Compiler,
target, activity, and literal spelling/path filters precede aggregation. The key
preserves compiler index, native activity, exact detail, and supplied file/line;
missing metadata stays missing. Family grouping uses a deterministic approximate
SQLite display function and never becomes semantic specialization identity. The
family scanner preserves angle-operator tokens, quoted literals, and bracketed
expressions; incomplete argument lists retain their exact spelling. Shared
spelling fixtures exercise the Rust query and JavaScript unit/tour views, while
paged aggregation tests verify sorting, work/count conservation, and distinct
unit reach after grouping.

The response keeps `total` as the matching row count and adds `summary.work_ms`
as the sum of all matching inclusive work before sorting and pagination. It is
computed with the count on the same read snapshot as the page and coverage;
family grouping preserves work while changing row count. The browser displays
this summary above the ranking, clearing it during search and ignoring stale
responses. A zero sum is shown as unavailable when selected inventory coverage
contains no named trace evidence. Native GCC timers are excluded from this sum.

Ranking, exact-family members, contributing units, and matching events use the
same query boundary. Durations/counts sum; unit reach is distinct; longest scope
uses the maximum. Native GCC phase timers remain separate. Coverage derives from
the filtered inventory and available profiles, independent of spelling searches.
[compiler-work.js](src/web/compiler-work.js) retains one page at a time, guards
asynchronous responses against changed selections, and reuses existing source,
unit, and scope investigations. The guided tour supplies equivalent queries over
its synthetic normalized events without reaching the real store.

Archives contain a checked capture prefix and regular files/directories. Import
rejects links and unsafe paths, enforces size/schema limits, and rebuilds indexes.
Identical imports are idempotent; conflicting IDs are rejected. Imported runs
cannot authorize local source access. Source preview requires a bundled
snapshot, a locally recorded capture, or an explicit source-root mapping.

The HTTP API is GET-only and loopback-bound. It serves embedded assets and
indexed report data; it does not execute captured commands. Source content and
compiler diagnostics are escaped in the browser.

The guided tour in [tour.js](src/web/tour.js) drives the same report views and
detail interfaces as real captures. While active, the browser API boundary
dispatches exclusively to synthetic in-memory fixtures, including source previews,
trace queries, and comparisons. No example requests reach the store or filesystem.
The tour snapshots the current capture, filters, detail selection, URL, scroll,
and focus; exit restores them and rebuilds the report indexes. Its controls move
into the native detail dialog's top layer so they remain accessible during detail
steps. `report --tour` permits an empty store and starts the walkthrough from the
URL. The normal report command still resolves a real capture before serving.

## Verification boundaries

[observations.rs](tests/observations.rs) tests decoder formats, trace nesting,
attribution, comparison statistics, archive behavior, and event queries through
the library interfaces.

[workflow.rs](tests/workflow.rs) runs the CLI against real CMake/Ninja builds
copied into temporary paths containing spaces. It verifies that ordinary builds,
isolated profiles, cancellation, failures, generated headers, PCH, unity sources,
and archive exchange preserve their intended public behavior.
