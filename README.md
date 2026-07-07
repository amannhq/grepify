# Grepify

**A lightweight code index for any harness.**

Grepify is a Python library (with a Rust engine underneath) for building
incremental data processing pipelines with declarative target states. You
declare what the output should look like as a function of the source —
`target_state = transformation(source_state)` — and Grepify keeps the target in
sync, reprocessing only what changed.

Think spreadsheets or React for data pipelines:

- **React**: declare UI as function of state → React re-renders what changed
- **Spreadsheets**: declare formulas → cells recompute when inputs change
- **Grepify**: declare target states as function of source → the engine syncs what changed

Everything is plain `async` Python with your own types — no DSL. The heavy
lifting (change detection, memoization, atomic syncing, live watching) runs in
Rust.

## Install

```sh
pip install grepify
```

## Quickstart

Watch a folder of Markdown, render each file to HTML, and keep an output folder
in sync — add, edit, or delete a source file and only that file's output moves:

```python
import pathlib

import grepify as coco
from grepify.connectors import localfs
from grepify.resources.file import FileLike, PatternFilePathMatcher
from markdown_it import MarkdownIt

_markdown_it = MarkdownIt("gfm-like")


@coco.fn(memo=True)
async def process_file(file: FileLike, target: localfs.DirTarget) -> None:
    html = _markdown_it.render(await file.read_text())
    outname = "__".join(file.file_path.path.parts) + ".html"
    target.declare_file(filename=outname, content=html)


@coco.fn
async def app_main(sourcedir: pathlib.Path, outdir: pathlib.Path) -> None:
    target = await coco.use_mount(localfs.declare_dir_target, outdir)
    files = localfs.walk_dir(
        sourcedir,
        recursive=True,
        path_matcher=PatternFilePathMatcher(included_patterns=["**/*.md"]),
    )
    await coco.mount_each(process_file, files.items(), target)


app = coco.App(
    coco.AppConfig(name="FilesTransform"),
    app_main,
    sourcedir=pathlib.Path("./docs"),
    outdir=pathlib.Path("./out"),
)
app.update_blocking(report_to_stdout=True)
```

Or run it via the CLI:

```sh
grepify update main        # catch-up: scan, sync, exit
grepify update -L main     # live: keep watching for changes
```

See [`examples/`](examples/) for complete apps: [`text_embedding`](examples/text_embedding/)
(Markdown → pgvector semantic search), [`code_embedding`](examples/code_embedding/)
(a live, Tree-sitter-chunked vector index over a repo), and
[`files_transform`](examples/files_transform/) (the minimal source → transform → target
pipeline), plus Rust ports under [`examples/rust/`](examples/rust/).

## Documentation

Docs live in-repo under [`docs/src/content/docs/`](docs/src/content/docs/) and
can be served locally:

```sh
cd docs
npm install
npm run dev
```

Start with the programming guide (`docs/src/content/docs/programming_guide/`).

## Building from source

Grepify uses [uv](https://docs.astral.sh/uv/) for Python project management and
[maturin](https://www.maturin.rs/) to build the Rust extension:

```sh
uv run maturin develop     # build Rust code and install the Python package
```

Tests and checks:

```sh
cargo test                        # Rust tests
uv run pytest python/             # Python tests
uv run mypy                       # type check
uv run ruff format . && uv run ruff check .   # format + lint
```

## License

Apache-2.0. See [LICENSE](LICENSE).
