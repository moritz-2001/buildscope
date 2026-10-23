# Collection performance

Collection prepares trace decoding, compression, compiler stack projections, and
serialized SQL rows in the existing bounded worker pool. One ordered writer reuses
its SQLite connection and commits each unit separately. Launcher receipts still
commit only with finalized capture metadata.

Event JSON is serialized once and written to both the compressed portable archive
and a temporary row spool. Stack rows follow it in that spool. This moves CPU work
out of the writer transaction without retaining another complete serialized trace
in memory. Temporary spools are removed on success and ordinary errors. A killed
process may leave temporary files, which are never treated as completed evidence.
Active trace and stack memory is bounded by worker count, rather than by bytes.

Dependency inventory and Clang trace readers cache resolved path spellings only
for one read. Later reads resolve symlinks and generated files again.

## Reproduce CLI measurements

Build a release binary, then run the isolated synthetic launcher benchmark:

```sh
cargo build --release --locked
python3 scripts/benchmark_collect.py target/release/buildscope 128 16 8
python3 scripts/benchmark_collect.py target/release/buildscope 32 2000 16
```

Arguments after the binary are invocation count, events per invocation, and scope
depth. Python 3, Ninja, and `/usr/bin/clang++` are required. The benchmark performs
metadata discovery but executes no build or compilation. Fixture setup and checks
are outside the timer. It verifies profile/event/stack/receipt counts, spool cleanup,
and a second collection without duplicates. It uses temporary stores throughout.

On 2026-10-23, alternating three release trials per binary on this macOS workspace
gave these median full CLI collection times:

| Synthetic batch | Before | After | Time reduction |
| --- | ---: | ---: | ---: |
| 128 invocations, 16 events each, depth 8 | 0.456 s | 0.325 s | 29% |
| 32 invocations, 2,000 events each, depth 16 | 1.781 s | 1.374 s | 23% |

These fixtures measure collection overhead, not compiler execution. Performance
on actual projects depends on trace size, path repetition, storage, and available
CPUs. First-run timings vary with filesystem caches; compare alternating trials
and their medians.

A retained Protobuf Ninja dependency inventory (468 outputs, 374,092 dependency
paths) had median parsing time of 6.049 s before and 0.136 s after, across three
serial release trials. Output counts were identical. This 44.5× figure describes
dependency parsing alone; it is separate from the synthetic full CLI measurements.

`tests/dependency_paths.rs` also provides ignored repeated-header and retained
Ninja dependency benchmarks. Run them in release mode with `--ignored --nocapture`.
The retained-data benchmark accepts `BUILDSCOPE_DEPS_BENCH_TEXT` and
`BUILDSCOPE_DEPS_BENCH_BUILD` for a saved `ninja -t deps` dump and its build tree.
