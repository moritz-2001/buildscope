#!/usr/bin/env python3
"""Benchmark actual `collect` against isolated synthetic launcher observations.

Usage: python3 scripts/benchmark_collect.py /absolute/path/to/buildscope UNITS EVENTS DEPTH
Fixture creation and verification are outside the measured subprocess duration.
No build or compilation runs; collect performs its normal read-only discovery,
including Ninja metadata tools and compiler capability/version probes.
"""

import argparse
import json
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import time


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False), encoding="utf-8")


def prepare(root, units, events, depth):
    source = root / "src"
    build = root / "build"
    source.mkdir()
    build.mkdir()
    (build / "CMakeFiles/app.dir").mkdir(parents=True)
    (build / "CMakeCache.txt").write_text(
        f"CMAKE_HOME_DIRECTORY:INTERNAL={source}\n"
        "CMAKE_GENERATOR:INTERNAL=Ninja\nCMAKE_BUILD_TYPE:STRING=Release\n",
        encoding="utf-8",
    )
    compiler = {
        "executable": "/usr/bin/clang++", "version": "synthetic benchmark observation",
        "family": "clang", "explicit_trace_path": True,
        "verbose_trace": True, "sarif": False,
    }
    trace = json.dumps({"traceEvents": [
        {
            "name": "ExecuteCompiler" if index % depth == 0 else "InstantiateFunction",
            "ph": "X", "ts": index // depth * depth * 3 + index % depth,
            "dur": (depth - index % depth) * 2, "pid": 1, "tid": 1,
            "args": {
                "detail": f"work_{index % 257}<std::vector<benchmark_type>>",
                "file": "benchmark.hpp", "line": index % 1000 + 1,
            },
        }
        for index in range(events)
    ]}, separators=(",", ":")).encode("utf-8")
    database = []
    ninja = ["rule CXX_COMPILER", "  command = /usr/bin/clang++ -c $in -o $out"]
    outputs = []
    for index in range(units):
        identity = f"{index:06}"
        cpp = source / f"unit-{index}.cpp"
        cpp.write_text("// Synthetic collection fixture; never compiled.\n", encoding="utf-8")
        output_key = f"CMakeFiles/app.dir/unit-{index}.o"
        output = build / output_key
        arguments = ["/usr/bin/clang++", "-c", str(cpp), "-o", str(output)]
        database.append({"directory": str(build), "file": str(cpp),
                         "output": str(output), "arguments": arguments})
        ninja.append(f"build {output_key}: CXX_COMPILER ../src/unit-{index}.cpp")
        outputs.append(output_key)
        pending = build / ".buildscope-launcher/pending" / identity
        invocation = {
            "id": identity, "directory": str(build), "arguments": arguments,
            "response_files": {}, "output": str(output), "family": "clang",
            "exit_code": 0, "started_ms": 1000 + index, "elapsed_ms": 10.0,
            "trace": f"raw/{identity}.json", "stderr": f"raw/{identity}.stderr",
            "granularity_us": 0, "verbose": True,
        }
        write_json(pending / "invocations" / f"{identity}.json", invocation)
        (pending / "raw").mkdir()
        (pending / invocation["trace"]).write_bytes(trace)
        (pending / invocation["stderr"]).write_text("", encoding="utf-8")
        # The completion marker follows its durable raw artifacts and manifest.
        write_json(pending / "observation.json", {
            "schema_version": 1, "id": identity, "build_dir": str(build),
            "compiler": compiler, "arguments": arguments, "source": str(cpp),
            "environment": {}, "machine": "benchmark", "inputs": {},
            "invocation": invocation,
        })
    ninja.append("build all: phony " + " ".join(outputs))
    ninja.append("default all")
    (build / "build.ninja").write_text("\n".join(ninja) + "\n", encoding="utf-8")
    write_json(build / "compile_commands.json", database)
    return build


def snapshot(store, units, events):
    with sqlite3.connect(store / "index.sqlite3") as connection:
        rows = connection.execute("SELECT id,status,manifest FROM runs").fetchall()
        assert len(rows) == 1, rows
        identity, status, manifest = rows[0]
        run = json.loads(manifest)
        assert status == "success" and run["finished_ms"] is not None, run
        assert len(run["profiles"]) == units, len(run["profiles"])
        assert all(profile["event_count"] == events for profile in run["profiles"])
        counts = {
            table: connection.execute(f"SELECT count(*) FROM {table}").fetchone()[0]
            for table in ("profiles", "events", "compiler_stack_units", "compiler_stacks",
                          "launcher_receipts")
        }
        assert counts["profiles"] == units, counts
        assert counts["events"] == units * events, counts
        assert counts["compiler_stack_units"] == units, counts
        assert counts["compiler_stacks"] >= units, counts
        assert counts["launcher_receipts"] == units, counts
    event_files = list((store / "runs" / identity / "events").iterdir())
    assert len(event_files) == units, event_files
    assert all(path.name.endswith(".json.zst") for path in event_files), event_files
    return identity, counts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=lambda value: Path(value).resolve(strict=True))
    parser.add_argument("units", type=int)
    parser.add_argument("events", type=int)
    parser.add_argument("depth", type=int)
    args = parser.parse_args()
    if min(args.units, args.events, args.depth) <= 0:
        parser.error("units, events, and depth must be positive")
    with tempfile.TemporaryDirectory(prefix="buildscope-cli-benchmark-") as temporary:
        root = Path(temporary).resolve()
        build = prepare(root, args.units, args.events, args.depth)
        store = root / "store"
        command = [str(args.binary), "--store", str(store), "collect", "--build-dir", str(build)]
        started = time.perf_counter()
        first = subprocess.run(command, capture_output=True, text=True)
        elapsed = time.perf_counter() - started
        assert first.returncode == 0, (first.stdout, first.stderr)
        initial = snapshot(store, args.units, args.events)
        pending = build / ".buildscope-launcher/pending"
        assert not list(pending.iterdir()), "completed spool remains"
        second = subprocess.run(command, capture_output=True, text=True)
        assert second.returncode == 0, (second.stdout, second.stderr)
        assert "No completed, uncollected compiler invocations" in second.stdout, second.stdout
        assert snapshot(store, args.units, args.events) == initial, "second collect changed data"
        print(json.dumps({
            "binary": str(args.binary), "units": args.units, "events_per_unit": args.events,
            "depth": args.depth, "elapsed_seconds": elapsed, "counts": initial[1],
            "second_collect_unchanged": True,
        }, sort_keys=True))


if __name__ == "__main__":
    main()
