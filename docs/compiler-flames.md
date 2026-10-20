# Recorded compiler-work flame graphs

Open a unit's Flame graph tab or switch project compiler work from Table to Flame graph. This view uses Clang's recorded temporal scopes. GCC's aggregate timers cannot supply temporal nesting and are unavailable here. Select exact target and compiler identities, reverse to bottom up, focus a frame, return to its parent, or highlight native activity/detail/source text. Highlighting never removes frames or changes widths. Scope examples link to native evidence; source buttons use the existing local-source permission boundary.

Widths are **attributed recorded coverage**, in microseconds, summed across process/thread lanes and selected units. Each covered interval on each lane contributes once. Nested intervals contribute to their complete recorded stack. Crossing intervals contribute to the common enclosing prefix plus `[ambiguous overlap]`; they are never silently double counted or assigned invented nesting. Lane sums can exceed wall time. `self_us` is coverage without a recorded child, including work omitted by Clang's trace threshold. It is not an estimate of intrinsic compiler self time. Native sample `inclusive_us` retains the original scope duration and is displayed separately. Temporal nesting does not assert semantic template dependencies.

Exact native activity, detail, file spelling and line form a frame identity. Compiler indices remain separate project roots. Each unit index is a compact prefix trie with parent ordinals, exclusive attributed mass and at most five native samples per node. Event replacement (including an empty trace) replaces the projection in the same SQLite transaction. Import uses the same event publication path. Inclusive compiler-work tables are unchanged.

Database version 3 adds two empty projection tables without scanning historical traces at startup. A selected historical unit without a marker yields `missing-index` with an explicit command:

```
buildscope reindex RUN --compiler-stacks
```

Reindex reports progress on stderr before the first unit, every ten units and the last unit, preserving CLI stdout. It resolves IDs/prefixes/latest and processes one unit at a time and uses retained native event rows or compressed unit archives. It never opens original compiler capture paths. New traces and imports build the index during publication. Empty indexed traces are distinguishable from missing indexes.

`GET /api/compiler-flamegraph` accepts `run`, `unit`, `target`, `compiler` (compiler index), `direction` (`top-down` or `bottom-up`), opaque `focus`, `depth` (1–32), `offset`, and `limit` (2–5000 nodes). Empty filters select all. The response contains `status`, `total_us`, `selected_units`, `missing_units`, `tree` and coverage notes. Each node supplies `value_us`, `self_us`, exact native fields, `children`, `has_children`, a focus token, `contributing_units`, and bounded scope examples. The visible tree preserves mass with explicit expandable Other buckets; hidden siblings retain their widths. Offset is applied at the focused root; the Previous siblings control returns to offset zero. Focus/parent/reset refine without downloading raw traces.

Canvas renders bounded geometry and has keyboard navigation (arrows, Enter to focus, Escape to reset), plus a native DOM select listing every visible scope. DOM controls and details are accessible without interpreting canvas pixels. Search only affects highlights. Request generations reject stale responses and resize redraws cached geometry. No inline styles or raw-trace browser processing are required.

Validation: `cargo test --test compiler_flames` and `node --test tests/flamegraph.test.cjs`. An ignored real-capture benchmark accepts `BUILDSCOPE_FLAME_BENCH_TRACE` and reads only the compressed event archive:

```
BUILDSCOPE_FLAME_BENCH_TRACE=/path/unit.json.zst cargo test --release --test compiler_flames real_trace_projection_benchmark -- --ignored --nocapture
```

Measured locally in release mode on an existing read-only archive containing **3,131,740 events**: JSON/zstd loading **5.05 s**, attribution/trie construction **6.63 s**, **1,426,401 trie nodes**, **1,600,831,488 bytes** peak process RSS (includes the fully decoded native input and output trie). Attributed coverage was **8,072,372 µs**. A prior run measured 6.17 s loading and 9.07 s projection. These are projection benchmarks, not end-to-end database publication or project query latency. The browser response remains capped; server aggregation memory follows selected distinct stack nodes, and rebuilding a very large trace still requires its decoded event vector. Aggregation samples stay typed with numeric unit references until bounded response serialization, avoiding per-hidden-node JSON detail objects.

Coverage is explicit for every filter selection: `inventory_units` counts all matching compilation units, including units without profiles; `trace_units` counts selected Clang profiles; `indexed_units` counts available Clang projections. `unprofiled_units` and `unsupported_units` distinguish absent measurements from aggregate compiler profiles that cannot provide temporal stacks. `granularities_us` lists distinct native recorded Clang thresholds (including zero), while `unknown_granularity_units` counts profiles without threshold metadata. `coverage_status` is `complete`, `partial`, or `unavailable`, independently of the index status. The graph total always includes only observed indexed Clang coverage. Missing profiles or indexes and unsupported compiler timers never imply zero compiler work. The UI presents the inventory denominator, threshold list and coverage gaps alongside the graph.
