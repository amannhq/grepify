# Files Transform

The smallest complete **source → transform → target** pipeline: watch a folder
of Markdown, render each file to HTML with `markdown-it-py`, and write the
`.html` outputs to a folder that stays in sync with the source. No database, no
embeddings, no API keys — files in, files out.

## How it works

The whole pipeline is about 25 lines. `process_file` reads the Markdown,
renders it to HTML, derives a flat output name from the source path, and
declares the output file as a target state; `app_main` walks the source folder
for `*.md` and mounts one component per file. Read all of [`main.py`](main.py):

```python
_markdown_it = MarkdownIt("gfm-like")

@coco.fn(memo=True)
async def process_file(file: FileLike, outdir: pathlib.Path) -> None:
    html = _markdown_it.render(await file.read_text())
    outname = "__".join(file.file_path.path.parts) + ".html"
    localfs.declare_file(outdir / outname, html, create_parent_dirs=True)

@coco.fn
async def app_main(sourcedir: pathlib.Path, outdir: pathlib.Path) -> None:
    files = localfs.walk_dir(
        sourcedir,
        path_matcher=PatternFilePathMatcher(included_patterns=["**/*.md"]),
        live=True,
    )
    await coco.mount_each(process_file, files.items(), outdir)
```

The transform itself is just two lines: read the text, render it. The output
name joins the source path parts with `__`, so `subdir/file.md` becomes
`subdir__file.html` — a flat, collision-free name. `localfs.declare_file`
describes the file you *want to exist*; Grepify writes it, overwrites it on
change, and deletes it when the source Markdown is gone.

## Run it

**1. Install** (no external services required):

```sh
pip install -e .
```

**2. Add some Markdown** — the example ships a `data/` folder of sample files, or drop your own in. The `.env` sets `GREPIFY_DB=./grepify.db` for internal state.

**3. Build the output folder** — catch-up (scan, sync, exit) or live (catch up, then keep watching):

```sh
grepify update main        # catch-up
grepify update -L main     # live: keep watching for file changes
```

The converted files appear in `./output_html/`, one `.html` per source `.md`
(named by the source path parts joined with `__`, e.g. `subdir__file.html`).

**4. Try incremental updates** — add, edit, or delete a `.md` in `data/` and re-run: only the changed file is re-rendered, and a removed source's `.html` is deleted automatically.
