#!/usr/bin/env python3
"""Cross-language parity runner for Grepify (Python SDK vs @grepify/node).

Runs the SAME declarative pipeline and the SAME stateless-ops cases in both
hosts, into separate output dirs and separate LMDB db_paths, then asserts:

  1. DirTarget outputs are byte-for-byte identical between hosts (cold run).
  2. Stateless-ops golden outputs (chunk boundaries, match captures, index
     terms, language detection) are deeply equal between hosts.
  3. Incremental behaviour: edit exactly one source file, re-run both hosts, and
     assert only the component owning that file reprocessed (via UpdateStats
     counters) and only that one output file changed on disk (authoritative
     output-hash check). Every other component is a memo hit / unchanged.

Parity is about equal OUTPUTS, not shared caches: the two hosts keep independent
memo state under independent db_paths. See README.md.

Usage:
    python run_parity.py            # run everything, exit non-zero on any failure
    python run_parity.py --keep     # keep the .work scratch dir for inspection
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
FIXTURES = HERE / "fixtures"
CORPUS = FIXTURES / "corpus"
OPS_CASES = FIXTURES / "ops_cases.json"
WORK = HERE / ".work"

PY_DIR = HERE / "python"
TS_DIR = HERE / "ts"
TSX = TS_DIR / "node_modules" / ".bin" / "tsx"

# The source file the incremental test edits (relative to the corpus root).
EDIT_TARGET = "nested/gamma.md"
EXPECTED_FILE_COUNT = 3  # alpha.md, beta.md, nested/gamma.md


class ParityError(AssertionError):
    pass


def _run(cmd: list[str], *, cwd: Path) -> None:
    print(f"  $ {' '.join(str(c) for c in cmd)}")
    proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    if proc.returncode != 0:
        sys.stdout.write(proc.stdout)
        sys.stderr.write(proc.stderr)
        raise ParityError(
            f"command failed ({proc.returncode}): {' '.join(map(str, cmd))}"
        )


def run_python_pipeline(src: Path, out: Path, db: Path, metrics: Path) -> dict:
    _run(
        [
            "uv",
            "run",
            "--project",
            str(PY_DIR),
            "python",
            str(PY_DIR / "pipeline.py"),
            "--source",
            str(src),
            "--out",
            str(out),
            "--db",
            str(db),
            "--metrics",
            str(metrics),
        ],
        cwd=REPO_ROOT,
    )
    return json.loads(metrics.read_text())


def run_ts_pipeline(src: Path, out: Path, db: Path, metrics: Path) -> dict:
    _run(
        [
            str(TSX),
            str(TS_DIR / "pipeline.ts"),
            "--source",
            str(src),
            "--out",
            str(out),
            "--db",
            str(db),
            "--metrics",
            str(metrics),
        ],
        cwd=HERE,
    )
    return json.loads(metrics.read_text())


def run_python_stateless(out: Path) -> dict:
    _run(
        [
            "uv",
            "run",
            "--project",
            str(PY_DIR),
            "python",
            str(PY_DIR / "stateless.py"),
            "--fixtures",
            str(FIXTURES),
            "--cases",
            str(OPS_CASES),
            "--out",
            str(out),
        ],
        cwd=REPO_ROOT,
    )
    return json.loads(out.read_text())


def run_ts_stateless(out: Path) -> dict:
    _run(
        [
            str(TSX),
            str(TS_DIR / "stateless.ts"),
            "--fixtures",
            str(FIXTURES),
            "--cases",
            str(OPS_CASES),
            "--out",
            str(out),
        ],
        cwd=HERE,
    )
    return json.loads(out.read_text())


# --- comparison helpers -----------------------------------------------------


def dir_hashes(d: Path) -> dict[str, str]:
    """Map each file (relative path) under `d` to a sha256 of its bytes."""
    out: dict[str, str] = {}
    for p in sorted(d.rglob("*")):
        if p.is_file():
            out[str(p.relative_to(d))] = hashlib.sha256(p.read_bytes()).hexdigest()
    return out


def assert_dirs_identical(a: Path, b: Path, label: str) -> None:
    ha, hb = dir_hashes(a), dir_hashes(b)
    if set(ha) != set(hb):
        only_a = sorted(set(ha) - set(hb))
        only_b = sorted(set(hb) - set(ha))
        raise ParityError(
            f"[{label}] file set differs. only in {a.name}: {only_a}; "
            f"only in {b.name}: {only_b}"
        )
    diffs = [name for name in ha if ha[name] != hb[name]]
    if diffs:
        # Show a byte-level preview of the first mismatch to aid debugging.
        name = diffs[0]
        preview = (a / name).read_bytes()[:200]
        raise ParityError(
            f"[{label}] {len(diffs)} file(s) differ byte-for-byte: {diffs}. "
            f"first-mismatch preview ({a.name}/{name}): {preview!r}"
        )


def processing_signature(metrics: dict) -> dict[str, int]:
    """Aggregate the counters of the file-processing components.

    Works across both host key formats: Python keys the group by processor name
    (`process_file`); TS keys per stable path (`mount:process/<relpath>`). Both
    contain the substring `process`, while the root/setup components do not.
    """
    agg = {"num_adds": 0, "num_reprocesses": 0, "num_unchanged": 0, "num_deletes": 0}
    by_component = metrics["stats"]["by_component"]
    matched = 0
    for key, group in by_component.items():
        if "process" not in key:
            continue
        matched += 1
        for field in agg:
            agg[field] += group[field]
    if matched == 0:
        raise ParityError(
            f"no processing components found in stats: {list(by_component)}"
        )
    return agg


def deep_equal_report(a: object, b: object, path: str = "") -> list[str]:
    """Return a list of human-readable difference locations (empty if equal)."""
    if type(a) is not type(b):
        return [f"{path or '<root>'}: type {type(a).__name__} != {type(b).__name__}"]
    if isinstance(a, dict):
        diffs: list[str] = []
        if set(a) != set(b):
            diffs.append(f"{path or '<root>'}: keys {sorted(a)} != {sorted(b)}")
        for k in a:
            if k in b:
                diffs += deep_equal_report(
                    a[k], b[k], f"{path}.{k}" if path else str(k)
                )
        return diffs
    if isinstance(a, list):
        if len(a) != len(b):
            return [f"{path}: length {len(a)} != {len(b)}"]
        diffs = []
        for i, (x, y) in enumerate(zip(a, b)):
            diffs += deep_equal_report(x, y, f"{path}[{i}]")
        return diffs
    if a != b:
        return [f"{path}: {a!r} != {b!r}"]
    return []


# --- main -------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description="Grepify cross-language parity runner")
    parser.add_argument(
        "--keep", action="store_true", help="keep the .work scratch dir"
    )
    parser.add_argument(
        "--results",
        type=Path,
        default=WORK / "parity_results.json",
        help="where to write the machine-readable results",
    )
    args = parser.parse_args()

    if not TSX.exists():
        print(
            f"ERROR: tsx not found at {TSX}. Run `npm install` in {TS_DIR} first "
            "(the TS side needs @grepify/node's prebuilt .node artifact too).",
            file=sys.stderr,
        )
        return 2

    if WORK.exists():
        shutil.rmtree(WORK)
    WORK.mkdir(parents=True)

    # A mutable copy of the corpus so the incremental step can edit a file.
    src = WORK / "corpus_src"
    shutil.copytree(CORPUS, src)

    checks: list[tuple[str, bool, str]] = []

    def record(name: str, ok: bool, detail: str = "") -> None:
        status = "PASS" if ok else "FAIL"
        print(f"[{status}] {name}" + (f" — {detail}" if detail else ""))
        checks.append((name, ok, detail))

    # 1. Cold run of both pipelines.
    print("\n== Pipeline: cold run ==")
    py_out1 = WORK / "py_out"
    ts_out1 = WORK / "ts_out"
    py_db = WORK / "py_db"
    ts_db = WORK / "ts_db"
    py_m1 = run_python_pipeline(src, py_out1, py_db, WORK / "py_metrics1.json")
    ts_m1 = run_ts_pipeline(src, ts_out1, ts_db, WORK / "ts_metrics1.json")

    # 2. Byte-for-byte output parity.
    print("\n== Compare: DirTarget outputs (cold) ==")
    try:
        assert_dirs_identical(py_out1, ts_out1, "pipeline-cold")
        record(
            "pipeline outputs identical (byte-for-byte)",
            True,
            f"{len(dir_hashes(py_out1))} files",
        )
    except ParityError as e:
        record("pipeline outputs identical (byte-for-byte)", False, str(e))

    # 3. Cold processing signature per host.
    print("\n== Assert: cold processing signature ==")
    for host, m in (("python", py_m1), ("ts", ts_m1)):
        try:
            sig = processing_signature(m)
            ok = (
                sig["num_adds"] == EXPECTED_FILE_COUNT
                and sig["num_reprocesses"] == 0
                and sig["num_unchanged"] == 0
                and sig["num_deletes"] == 0
            )
            record(f"[{host}] cold: all files added, none reused", ok, json.dumps(sig))
        except ParityError as e:
            record(f"[{host}] cold: all files added, none reused", False, str(e))

    # 4. Stateless-ops golden parity.
    print("\n== Stateless ops golden ==")
    py_ops = run_python_stateless(WORK / "py_ops.json")
    ts_ops = run_ts_stateless(WORK / "ts_ops.json")
    # Drop the host label before comparing.
    py_cmp = {k: v for k, v in py_ops.items() if k != "host"}
    ts_cmp = {k: v for k, v in ts_ops.items() if k != "host"}
    diffs = deep_equal_report(py_cmp, ts_cmp)
    record(
        "stateless ops identical (split / match / index_terms / detect_language)",
        not diffs,
        "equal" if not diffs else f"{len(diffs)} diff(s): {diffs[:5]}",
    )

    # 5. Incremental: edit one file, re-run both, assert single-component reprocess.
    print("\n== Incremental: edit one file, re-run ==")
    edited = src / EDIT_TARGET
    edited.write_text(
        edited.read_text() + "\n\nAn extra paragraph appended by the parity runner "
        "to change this file's content and force exactly one reprocess.\n",
        encoding="utf-8",
    )

    before_py = dir_hashes(py_out1)
    before_ts = dir_hashes(ts_out1)
    py_m2 = run_python_pipeline(src, py_out1, py_db, WORK / "py_metrics2.json")
    ts_m2 = run_ts_pipeline(src, ts_out1, ts_db, WORK / "ts_metrics2.json")
    after_py = dir_hashes(py_out1)
    after_ts = dir_hashes(ts_out1)

    edited_out_name = EDIT_TARGET.replace("/", "__") + ".chunks"

    # 5a. Counter-based: exactly one reprocess, rest unchanged, per host.
    print("\n== Assert: incremental processing signature ==")
    for host, m in (("python", py_m2), ("ts", ts_m2)):
        try:
            sig = processing_signature(m)
            ok = (
                sig["num_reprocesses"] == 1
                and sig["num_unchanged"] == EXPECTED_FILE_COUNT - 1
                and sig["num_adds"] == 0
                and sig["num_deletes"] == 0
            )
            record(
                f"[{host}] incremental: exactly 1 reprocess, rest unchanged",
                ok,
                json.dumps(sig),
            )
        except ParityError as e:
            record(
                f"[{host}] incremental: exactly 1 reprocess, rest unchanged",
                False,
                str(e),
            )

    # 5b. Output-hash-based (authoritative): only the edited file's output changed.
    print("\n== Assert: only the edited file's output changed ==")
    for host, before, after in (
        ("python", before_py, after_py),
        ("ts", before_ts, after_ts),
    ):
        changed = sorted(n for n in after if before.get(n) != after[n])
        ok = changed == [edited_out_name]
        record(
            f"[{host}] only {edited_out_name} rewrote on re-index",
            ok,
            f"changed={changed}",
        )

    # 5c. Cross-host output parity still holds after the edit.
    try:
        assert_dirs_identical(py_out1, ts_out1, "pipeline-incremental")
        record("pipeline outputs identical after edit (byte-for-byte)", True)
    except ParityError as e:
        record("pipeline outputs identical after edit (byte-for-byte)", False, str(e))

    # Results file.
    args.results.parent.mkdir(parents=True, exist_ok=True)
    args.results.write_text(
        json.dumps(
            {
                "checks": [{"name": n, "ok": ok, "detail": d} for n, ok, d in checks],
                "python_cold": py_m1["stats"],
                "ts_cold": ts_m1["stats"],
                "python_incremental": py_m2["stats"],
                "ts_incremental": ts_m2["stats"],
            },
            indent=2,
        ),
        encoding="utf-8",
    )

    passed = sum(1 for _, ok, _ in checks if ok)
    total = len(checks)
    print(f"\n=== Parity: {passed}/{total} checks passed ===")
    print(f"Results written to {args.results}")

    if not args.keep:
        # Keep results + metrics but drop the bulky db/out dirs.
        for name in ("py_db", "ts_db", "corpus_src"):
            shutil.rmtree(WORK / name, ignore_errors=True)

    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
