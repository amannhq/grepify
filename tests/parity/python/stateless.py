"""Python side of the stateless-ops golden test.

Reads the shared ``fixtures/ops_cases.json``, runs each case through the shared
Rust engine via ``grepify`` (recursive splitter, structural code match, index
terms, language detection) and writes a normalized JSON result. The TypeScript
driver (``tests/parity/ts/stateless.ts``) emits the same normalized shape, and
the runner asserts the two are deeply equal.
"""

from __future__ import annotations

import argparse
import json
import pathlib

from grepify.ops.code import index_terms, match_code
from grepify.ops.text import RecursiveSplitter, detect_code_language


def _chunk_dict(source_bytes: bytes, c) -> dict:
    return {
        "startByte": c.start.byte_offset,
        "endByte": c.end.byte_offset,
        "startChar": c.start.char_offset,
        "endChar": c.end.char_offset,
        "startLine": c.start.line,
        "startColumn": c.start.column,
        "endLine": c.end.line,
        "endColumn": c.end.column,
        "text": source_bytes[c.start.byte_offset : c.end.byte_offset].decode("utf-8"),
    }


def run(fixtures: pathlib.Path, cases: dict) -> dict:
    splitter = RecursiveSplitter()
    result: dict = {
        "host": "python",
        "split": {},
        "match": {},
        "index_terms": {},
        "detect_language": {},
    }

    for case in cases.get("split_cases", []):
        text = (fixtures / case["file"]).read_text(encoding="utf-8")
        source_bytes = text.encode("utf-8")
        kwargs = {"chunk_size": case["chunk_size"]}
        if "chunk_overlap" in case:
            kwargs["chunk_overlap"] = case["chunk_overlap"]
        if "min_chunk_size" in case:
            kwargs["min_chunk_size"] = case["min_chunk_size"]
        if "language" in case:
            kwargs["language"] = case["language"]
        chunks = splitter.split(text, **kwargs)
        result["split"][case["name"]] = [_chunk_dict(source_bytes, c) for c in chunks]

    for case in cases.get("match_cases", []):
        text = (fixtures / case["file"]).read_text(encoding="utf-8")
        source_bytes = text.encode("utf-8")
        matches = match_code(case["pattern"], text, case["language"])
        result["match"][case["name"]] = [
            {
                "kind": m.kind,
                "chunks": [_chunk_dict(source_bytes, c) for c in m.chunks],
                "captures": {
                    name: [_chunk_dict(source_bytes, c) for c in chunks]
                    for name, chunks in m.captures.items()
                },
            }
            for m in matches
        ]

    for case in cases.get("index_terms_cases", []):
        text = (fixtures / case["file"]).read_text(encoding="utf-8")
        # `index_terms` returns a deduped set; its iteration order is not a
        # stable cross-host contract, so compare the sorted term set.
        result["index_terms"][case["name"]] = sorted(
            index_terms(text, case["language"], case.get("min_len", 3))
        )

    for case in cases.get("detect_language_cases", []):
        result["detect_language"][case["name"]] = detect_code_language(
            filename=case["filename"]
        )

    return result


def main() -> None:
    parser = argparse.ArgumentParser(description="Python stateless-ops golden")
    parser.add_argument("--fixtures", type=pathlib.Path, required=True)
    parser.add_argument("--cases", type=pathlib.Path, required=True)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    args = parser.parse_args()

    cases = json.loads(args.cases.read_text(encoding="utf-8"))
    result = run(args.fixtures, cases)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(
        json.dumps(result, indent=2, ensure_ascii=False), encoding="utf-8"
    )


if __name__ == "__main__":
    main()
