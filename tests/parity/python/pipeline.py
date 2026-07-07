"""Python side of the cross-language parity pipeline.

Walks a corpus of Markdown files, splits each with the shared Rust recursive
splitter, and writes one deterministic ``*.chunks`` file per source into a
``localfs`` DirTarget. The output-file bytes are constructed identically to the
TypeScript driver (``tests/parity/ts/pipeline.ts``) so the two DirTargets can be
compared byte-for-byte.

Also emits a metrics JSON capturing the per-component ``UpdateStats`` counters,
which the runner uses to assert incremental behaviour (edit one file, re-run,
expect exactly one component to reprocess and the rest to stay unchanged).
"""

from __future__ import annotations

import argparse
import asyncio
import json
import pathlib

import grepify as coco
from grepify.connectors import localfs
from grepify.resources.file import FileLike, PatternFilePathMatcher

# Chunking configuration shared with the TS driver. Keep in sync with
# tests/parity/ts/pipeline.ts.
CHUNK_SIZE = 200
CHUNK_OVERLAP = 40
LANGUAGE = "markdown"

_RECORD_SEP = b"\x1e"


def _render_chunks(relative_path: str, source_bytes: bytes, chunks: list) -> bytes:
    """Build the canonical, cross-language output bytes for one source file.

    Format (identical in Python and TypeScript):
        <relative_path>\n
        <n_chunks>\n
        for each chunk:
            "<sb> <eb> <sc> <ec> <sl> <scol> <el> <ecol>\n"
            <chunk bytes sliced from source by byte offset>
            <0x1e record separator>
    """
    out = bytearray()
    out += relative_path.encode("utf-8") + b"\n"
    out += str(len(chunks)).encode("utf-8") + b"\n"
    for c in chunks:
        header = (
            f"{c.start.byte_offset} {c.end.byte_offset} "
            f"{c.start.char_offset} {c.end.char_offset} "
            f"{c.start.line} {c.start.column} "
            f"{c.end.line} {c.end.column}\n"
        )
        out += header.encode("utf-8")
        out += source_bytes[c.start.byte_offset : c.end.byte_offset]
        out += _RECORD_SEP
    return bytes(out)


_splitter = None


def _get_splitter():
    global _splitter
    if _splitter is None:
        from grepify.ops.text import RecursiveSplitter

        _splitter = RecursiveSplitter()
    return _splitter


@coco.fn(memo=True)
async def process_file(item: tuple[str, FileLike], target: localfs.DirTarget) -> None:
    # `item` is (walk-relative path, file). The relative path (forward slashes,
    # relative to the walk root) is used for the output name and the content
    # header so it matches the TS driver's `FileEntry.relativePath`.
    relpath, file = item
    text = await file.read_text()
    source_bytes = text.encode("utf-8")
    chunks = _get_splitter().split(
        text,
        chunk_size=CHUNK_SIZE,
        chunk_overlap=CHUNK_OVERLAP,
        language=LANGUAGE,
    )
    outname = relpath.replace("/", "__") + ".chunks"
    target.declare_file(
        filename=outname,
        content=_render_chunks(relpath, source_bytes, chunks),
    )


@coco.fn
async def app_main(sourcedir: pathlib.Path, outdir: pathlib.Path) -> None:
    target = await coco.use_mount(localfs.declare_dir_target, outdir)
    files = localfs.walk_dir(
        sourcedir,
        recursive=True,
        path_matcher=PatternFilePathMatcher(included_patterns=["**/*.md"]),
    )
    items = [(key, (key, file)) async for key, file in files.items()]
    handle = await coco.mount_each(
        coco.component_subpath("process"), process_file, items, target
    )
    await handle.ready()


def _stats_to_dict(stats: coco.UpdateStats | None) -> dict:
    if stats is None:
        return {"total": {}, "by_component": {}}

    def group(g) -> dict:
        return {
            "num_execution_starts": g.num_execution_starts,
            "num_unchanged": g.num_unchanged,
            "num_adds": g.num_adds,
            "num_deletes": g.num_deletes,
            "num_reprocesses": g.num_reprocesses,
            "num_errors": g.num_errors,
            "num_processed": g.num_processed,
        }

    return {
        "total": group(stats.total),
        "by_component": {name: group(g) for name, g in stats.by_component.items()},
    }


async def _run(args: argparse.Namespace) -> None:
    env = coco.Environment(coco.Settings.from_env(db_path=args.db))
    app = coco.App(
        coco.AppConfig(name="parity_pipeline", environment=env),
        app_main,
        sourcedir=args.source,
        outdir=args.out,
    )
    handle = app.update()
    await handle.result()
    stats = handle.stats()

    metrics = {
        "host": "python",
        "out_dir": str(args.out),
        "db_path": str(args.db),
        "stats": _stats_to_dict(stats),
    }
    args.metrics.parent.mkdir(parents=True, exist_ok=True)
    args.metrics.write_text(json.dumps(metrics, indent=2), encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description="Python parity pipeline")
    parser.add_argument("--source", type=pathlib.Path, required=True)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    parser.add_argument("--db", type=pathlib.Path, required=True)
    parser.add_argument("--metrics", type=pathlib.Path, required=True)
    asyncio.run(_run(parser.parse_args()))


if __name__ == "__main__":
    main()
