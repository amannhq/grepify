#!/usr/bin/env python3
"""Binding-benchmark orchestrator for Grepify.

Generates a shared corpus, then runs the SAME workload in three hosts over the
one Rust engine — Rust-native (direct SDK, the zero-boundary baseline), Python
(PyO3), and TypeScript (napi) — and prints a comparison table plus a
JSON/Markdown report. Also records the native artifact sizes (.node, Rust
binary).

Mirrors the structure of benchmarks/file_summarization (per-language driver +
Python orchestrator), rather than introducing a new framework.

Usage:
    python runner.py                 # full run (1000-file corpus)
    python runner.py --quick         # small/fast run for smoke-testing
    python runner.py --files 5000    # custom corpus size
    python runner.py --only rust,python
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
WORK = HERE / ".work"
CORPUS = WORK / "corpus"

PY_DIR = HERE / "python"
TS_DIR = HERE / "ts"
RUST_DIR = HERE / "rust"
RUST_MANIFEST = RUST_DIR / "Cargo.toml"
TSX = TS_DIR / "node_modules" / ".bin" / "tsx"
PACKAGES_NODE = REPO_ROOT / "packages" / "node"

HOSTS = ("rust", "python", "typescript")


def rust_binary_path() -> Path | None:
    """Locate the built binary via `cargo metadata` (the target dir may be
    redirected by CARGO_TARGET_DIR, e.g. under a sandbox cache)."""
    proc = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=RUST_DIR,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        return None
    target_dir = Path(json.loads(proc.stdout)["target_directory"])
    binp = target_dir / "release" / "bindings_bench"
    return binp if binp.exists() else None


def generate_corpus(n_files: int) -> None:
    if CORPUS.exists():
        shutil.rmtree(CORPUS)
    CORPUS.mkdir(parents=True)
    para = (
        "Grepify is a lightweight code index for any harness. This paragraph is "
        "deterministic filler so the recursive splitter produces a couple of "
        "chunks per file. The same corpus is processed by every host.\n\n"
    )
    for i in range(n_files):
        # Vary content slightly per file so their memo keys differ.
        body = f"# Document {i:05d}\n\n" + para + f"\nUnique tail for file {i:05d}.\n"
        (CORPUS / f"file_{i:05d}.md").write_text(body, encoding="utf-8")


def build_rust() -> bool:
    print("Building Rust benchmark (release)...")
    proc = subprocess.run(
        ["cargo", "build", "--release", "--manifest-path", str(RUST_MANIFEST)],
        cwd=RUST_DIR,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stdout + proc.stderr)
        print("  Rust build FAILED — Rust benches will be marked pending.")
        return False
    return True


def _run(cmd: list[str], *, cwd: Path) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)


def run_host(host: str, args: argparse.Namespace) -> dict | None:
    metrics = WORK / f"{host}_metrics.json"
    db = WORK / f"{host}_db"
    # Wipe any prior DB state so the "cold" run is genuinely cold (each host
    # uses db / db_rt / db_pipe variants).
    for stale in WORK.glob(f"{host}_db*"):
        shutil.rmtree(stale, ignore_errors=True)
    common = [
        "--corpus",
        str(CORPUS),
        "--db",
        str(db),
        "--metrics",
        str(metrics),
        "--detect-iters",
        str(args.detect_iters),
        "--match-iters",
        str(args.match_iters),
        "--mount-iters",
        str(args.mount_iters),
    ]
    if host == "python":
        cmd = [
            "uv",
            "run",
            "--project",
            str(PY_DIR),
            "python",
            str(PY_DIR / "bench.py"),
        ] + common
        cwd = REPO_ROOT
    elif host == "typescript":
        if not TSX.exists():
            print(
                f"  [typescript] PENDING — tsx not installed. Run `npm install` in {TS_DIR}."
            )
            return None
        cmd = [str(TSX), str(TS_DIR / "bench.ts")] + common
        cwd = HERE
    elif host == "rust":
        binp = rust_binary_path()
        if binp is None:
            print("  [rust] PENDING — binary not built.")
            return None
        cmd = [str(binp)] + common
        cwd = HERE
    else:
        raise ValueError(host)

    print(f"  running {host}...")
    t0 = time.perf_counter()
    proc = _run(cmd, cwd=cwd)
    wall = time.perf_counter() - t0
    if proc.returncode != 0:
        sys.stderr.write(proc.stdout + proc.stderr)
        print(f"  [{host}] FAILED (exit {proc.returncode}) — marked pending.")
        return None
    data = json.loads(metrics.read_text())
    data["_driver_wall_s"] = round(wall, 2)
    return data


def artifact_sizes() -> dict:
    sizes: dict[str, dict[str, int] | int | None] = {}
    node_bins = sorted(PACKAGES_NODE.glob("*.node"))
    sizes["node_addon"] = (
        {p.name: p.stat().st_size for p in node_bins} if node_bins else None
    )
    binp = rust_binary_path()
    sizes["rust_binary"] = binp.stat().st_size if binp else None
    return sizes


def _fmt(v: object) -> str:
    return "—" if v is None else str(v)


def _get(results: dict, host: str, *path: str) -> Any:
    cur: Any = results.get(host)
    for p in path:
        if not isinstance(cur, dict):
            return None
        cur = cur.get(p)
    return cur


def print_table(results: dict, sizes: dict) -> None:
    rows = [
        ("detect_code_language (ns/call)", ("boundary_sync", "detect_ns_per_call")),
        ("match_code (ns/call)", ("boundary_sync", "match_ns_per_call")),
        ("child mount+create (us/op)", ("boundary_async", "mount_us_per_op")),
        ("pipeline cold (ms)", ("pipeline", "cold_ms")),
        ("pipeline warm/memo-hit (ms)", ("pipeline", "warm_ms")),
        ("corpus files", ("pipeline", "corpus_files")),
    ]
    hosts = [h for h in HOSTS]
    w0 = max(len(r[0]) for r in rows) + 2
    header = "metric".ljust(w0) + "".join(h.ljust(16) for h in hosts)
    print("\n" + header)
    print("-" * len(header))
    for label, path in rows:
        line = label.ljust(w0)
        for h in hosts:
            line += _fmt(_get(results, h, *path)).ljust(16)
        print(line)

    # Overhead ratios vs the Rust baseline.
    print("\nOverhead vs Rust-native baseline (host_time / rust_time):")
    ratio_rows = [
        ("detect_code_language", ("boundary_sync", "detect_ns_per_call")),
        ("match_code", ("boundary_sync", "match_ns_per_call")),
        ("child mount+create", ("boundary_async", "mount_us_per_op")),
        ("pipeline cold", ("pipeline", "cold_ms")),
        ("pipeline warm", ("pipeline", "warm_ms")),
    ]
    for label, path in ratio_rows:
        base = _get(results, "rust", *path)
        line = "  " + label.ljust(w0)
        for h in ("python", "typescript"):
            v = _get(results, h, *path)
            if base and v:
                line += f"{v / base:.2f}x".ljust(16)
            else:
                line += "—".ljust(16)
        print(line)

    print("\nNative artifact sizes:")
    if sizes["node_addon"]:
        for name, sz in sizes["node_addon"].items():
            print(f"  {name}: {sz / 1e6:.1f} MB")
    else:
        print("  .node addon: not found")
    if sizes["rust_binary"]:
        print(f"  rust bench binary: {sizes['rust_binary'] / 1e6:.1f} MB")


def write_markdown(
    path: Path, results: dict, sizes: dict, args: argparse.Namespace
) -> None:
    lines = ["# Grepify binding benchmark results", ""]
    lines.append(f"- corpus files: {args.files}")
    lines.append(
        f"- iters: detect={args.detect_iters}, match={args.match_iters}, mount={args.mount_iters}"
    )
    lines.append("")
    lines.append("| metric | rust | python | typescript |")
    lines.append("| --- | --- | --- | --- |")
    rows = [
        ("detect_code_language (ns/call)", ("boundary_sync", "detect_ns_per_call")),
        ("match_code (ns/call)", ("boundary_sync", "match_ns_per_call")),
        ("child mount+create (us/op)", ("boundary_async", "mount_us_per_op")),
        ("pipeline cold (ms)", ("pipeline", "cold_ms")),
        ("pipeline warm/memo-hit (ms)", ("pipeline", "warm_ms")),
    ]
    for label, p in rows:
        lines.append(
            f"| {label} | {_fmt(_get(results, 'rust', *p))} | "
            f"{_fmt(_get(results, 'python', *p))} | {_fmt(_get(results, 'typescript', *p))} |"
        )
    lines.append("")
    lines.append("## Native artifact sizes")
    if sizes["node_addon"]:
        for name, sz in sizes["node_addon"].items():
            lines.append(f"- `{name}`: {sz / 1e6:.1f} MB")
    if sizes["rust_binary"]:
        lines.append(f"- rust bench binary: {sizes['rust_binary'] / 1e6:.1f} MB")
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description="Grepify binding benchmark runner")
    parser.add_argument("--files", type=int, default=1000)
    parser.add_argument("--detect-iters", type=int, default=500_000)
    parser.add_argument("--match-iters", type=int, default=50_000)
    parser.add_argument("--mount-iters", type=int, default=1_000)
    parser.add_argument("--quick", action="store_true", help="tiny/fast smoke run")
    parser.add_argument(
        "--only", type=str, default="", help="comma list: rust,python,typescript"
    )
    args = parser.parse_args()

    if args.quick:
        args.files = 100
        args.detect_iters = 50_000
        args.match_iters = 5_000
        args.mount_iters = 200

    only = {h.strip() for h in args.only.split(",") if h.strip()}
    hosts = [h for h in HOSTS if not only or h in only]

    WORK.mkdir(parents=True, exist_ok=True)
    print(f"Generating corpus of {args.files} files at {CORPUS} ...")
    generate_corpus(args.files)

    if "rust" in hosts:
        build_rust()

    results: dict = {}
    print("\nRunning hosts:")
    for host in hosts:
        data = run_host(host, args)
        if data is not None:
            results[host] = data

    sizes = artifact_sizes()
    print_table(results, sizes)

    out_json = WORK / "results.json"
    out_md = WORK / "results.md"
    out_json.write_text(
        json.dumps(
            {"config": vars(args), "results": results, "sizes": sizes}, indent=2
        ),
        encoding="utf-8",
    )
    write_markdown(out_md, results, sizes, args)
    print(f"\nResults written to {out_json} and {out_md}")

    ran = set(results)
    pending = [h for h in hosts if h not in ran]
    if pending:
        print(f"\nPENDING hosts (did not run): {pending}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
