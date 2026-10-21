# Compilation time history

Open **Build history** in the local report. Duration points and workload bars share
capture positions. Captures are evenly spaced in capture order, with dates and
times below each position; the spacing does not represent time between builds.

Select a point or table row to see exact counts, open the capture, set its duration
as the baseline, or compare it with the baseline. The baseline defaults to the first
successful capture in the visible series. Scenario labels and date ranges narrow
that series. Date ranges end at its latest capture, including for archived builds.

## What the counts mean

- **Compiled**: a successful source/output compilation unit with an observed direct
  compiler command, a cache-bypassing managed profile, or a matched ccache miss
  with no configured compiler prefix.
- **Cache hits**: successful units matched to ccache's final `direct_cache_hit` or
  `preprocessed_cache_hit` outcome. Storage lookups alone are not cache hits.
- **Unknown**: an executed unit without sufficient outcome evidence. Unsupported
  cache wrappers, missing logs, and ambiguous repeated executions remain unknown.
- **Up to date**: requested units that did not execute during a successful complete
  Ninja recording. These are separate from compiler-cache hits. This count is
  unavailable for failed/interrupted captures and compiler-only launcher batches.

Counts describe compilation units, not deduplicated source filenames. Compiling
one source for two target outputs counts twice. The selected capture also shows
the number of distinct executed source paths. Partial captures can omit failed or
unfinished actions because Ninja only logs successful completed actions.

Ordinary recordings set `CCACHE_STATSLOG` to `ccache.stats` inside the capture's
artifact directory. Ccache writes locked per-invocation records there, isolating
parallel compiler processes and unrelated builds without resetting shared cache
statistics. This overrides a caller's stats-log destination for that build only;
it does not change cache contents or disable cache reuse. The original log is
retained and included in full-evidence archives. Logs are matched by source path
only when the successful unit count matches the record count for that source.

The supported cached command is `ccache [KEY=VALUE ...] COMPILER ...` with a direct
native Clang or GCC compiler, optionally preceded by `buildscope launch ... --`.
Custom wrappers, masquerading cache symlinks, nested cache chains,
and sccache remain unknown. Ccache misses with a configured compiler prefix also
remain unknown because another cache may have served them. Trace artifacts can be
restored by a cache and do not independently prove compiler execution.
Buildscope does not infer hits from short durations or use global cache counters.
Public launcher batches cannot observe hits served by a cache outside the launcher,
and are never treated as complete builds. Legacy captures keep unknown workload
coverage rather than reconstructing outcomes from current machine state.

## Comparing measurements

A series matches project, configuration, host, capture mode, requested target set,
compiler-affecting environment, cache policy, instrumentation, and requested Ninja
parallelism. Select another capture to anchor another configuration. The requested
parallelism is saved for new recordings; a default job limit does not establish
actual CPU availability. Cache state, system load, and no-op versus rebuild scope
can still vary even within a series.

The report says **Workload changed** when counts, requested scope, executed unit
identities, or action counts differ. Missing outcomes produce **Workload coverage
incomplete**. Neither state interprets a shorter elapsed duration as faster
compilation. Failed/interrupted captures appear as separate markers and break the
duration line. They cannot be baselines or contribute to elapsed-difference metrics.
