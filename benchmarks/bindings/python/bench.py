"""Python (PyO3) side of the binding benchmarks.

Measures, for the Python host over the shared Rust engine:

  * Boundary-crossing overhead:
      - small sync call: detect_code_language (cheapest PyO3 round-trip)
      - heavier sync call: match_code (structural match; boundary + engine work)
      - async mount round-trip: N sequential useMount of a trivial child
  * Pipeline: mountEach over a generated corpus, cold run vs warm (memo-hit)
    re-index latency.

Writes a metrics JSON consumed by ../runner.py. Kept in sync with the TS
(../ts/bench.ts) and Rust (../rust/src/main.rs) drivers so the same workload is
measured in all three hosts.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import pathlib
import time

import grepify as coco
from grepify.connectors import localfs  # noqa: F401  (kept: mirrors real imports)
from grepify.ops.code import match_code
from grepify.ops.text import RecursiveSplitter, detect_code_language

_MATCH_SRC = "def foo(a, b):\n    return a + b\n\ndef bar(x):\n    return x\n"
_MATCH_PATTERN = r"def \NAME(\(ARGS*\)):"

_splitter = RecursiveSplitter()
_CHUNK_SIZE = 256
_LANGUAGE = "markdown"


def bench_boundary_sync(detect_iters: int, match_iters: int) -> dict:
    # Warm up (first call may lazily initialize engine state).
    detect_code_language(filename="main.py")
    t0 = time.perf_counter()
    for _ in range(detect_iters):
        detect_code_language(filename="main.py")
    detect_ns = (time.perf_counter() - t0) / detect_iters * 1e9

    match_code(_MATCH_PATTERN, _MATCH_SRC, "python")
    t0 = time.perf_counter()
    for _ in range(match_iters):
        match_code(_MATCH_PATTERN, _MATCH_SRC, "python")
    match_ns = (time.perf_counter() - t0) / match_iters * 1e9

    return {
        "detect_ns_per_call": round(detect_ns, 1),
        "match_ns_per_call": round(match_ns, 1),
        "detect_iters": detect_iters,
        "match_iters": match_iters,
    }


@coco.fn
async def _trivial() -> int:
    return 0


@coco.fn
async def _roundtrip_main(iters: int) -> float:
    t0 = time.perf_counter()
    for i in range(iters):
        await coco.use_mount(coco.component_subpath("rt", str(i)), _trivial)
    return time.perf_counter() - t0


@coco.fn(memo=True)
async def _process(abspath: str) -> int:
    # Memo key is derived from `abspath` (+ logic hash); on a warm re-run the
    # whole child is a memo hit, so the read + split is skipped entirely.
    text = pathlib.Path(abspath).read_text(encoding="utf-8")
    return len(_splitter.split(text, chunk_size=_CHUNK_SIZE, language=_LANGUAGE))


@coco.fn
async def _pipeline_main(corpus: pathlib.Path) -> int:
    files = localfs.walk_dir(corpus, recursive=True)
    items = [(key, str(corpus / key)) async for key, _f in files.items()]
    handle = await coco.mount_each(coco.component_subpath("process"), _process, items)
    await handle.ready()
    return len(items)


def _total_stats(handle) -> dict:
    stats = handle.stats()
    if stats is None:
        return {}
    t = stats.total
    return {
        "num_adds": t.num_adds,
        "num_reprocesses": t.num_reprocesses,
        "num_unchanged": t.num_unchanged,
        "num_deletes": t.num_deletes,
    }


async def _run(args: argparse.Namespace) -> dict:
    result: dict = {"host": "python"}

    result["boundary_sync"] = bench_boundary_sync(args.detect_iters, args.match_iters)

    env = coco.Environment(coco.Settings.from_env(db_path=args.db))

    # Async mount round-trip.
    rt_app = coco.App(
        coco.AppConfig(name="bench_roundtrip", environment=env),
        _roundtrip_main,
        args.mount_iters,
    )
    elapsed = await rt_app.update()
    result["boundary_async"] = {
        "mount_us_per_op": round(elapsed / args.mount_iters * 1e6, 2),
        "mount_iters": args.mount_iters,
    }

    # Pipeline: cold vs warm.
    pipe_app = coco.App(
        coco.AppConfig(name="bench_pipeline", environment=env),
        _pipeline_main,
        args.corpus,
    )

    t0 = time.perf_counter()
    h_cold = pipe_app.update()
    n_files = await h_cold.result()
    cold_ms = (time.perf_counter() - t0) * 1000.0
    cold_stats = _total_stats(h_cold)

    t0 = time.perf_counter()
    h_warm = pipe_app.update()
    await h_warm.result()
    warm_ms = (time.perf_counter() - t0) * 1000.0
    warm_stats = _total_stats(h_warm)

    result["pipeline"] = {
        "corpus_files": n_files,
        "cold_ms": round(cold_ms, 2),
        "warm_ms": round(warm_ms, 2),
        "cold_stats": cold_stats,
        "warm_stats": warm_stats,
    }
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description="Python binding benchmark")
    parser.add_argument("--corpus", type=pathlib.Path, required=True)
    parser.add_argument("--db", type=pathlib.Path, required=True)
    parser.add_argument("--metrics", type=pathlib.Path, required=True)
    parser.add_argument("--detect-iters", type=int, default=500_000)
    parser.add_argument("--match-iters", type=int, default=50_000)
    parser.add_argument("--mount-iters", type=int, default=2_000)
    args = parser.parse_args()

    result = asyncio.run(_run(args))
    args.metrics.parent.mkdir(parents=True, exist_ok=True)
    args.metrics.write_text(json.dumps(result, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
