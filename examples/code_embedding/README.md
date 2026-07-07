# Code Embedding

A live, syntax-aware vector index over a code repository — in ~100 lines of
plain async Python. Point it at a directory, search it in natural language, and
it re-embeds only what changes as you edit.

```python
query: "where do we embed chunks?"

[0.582] examples/code_embedding/main.py (L66-L83)
    @coco.fn
    async def process_chunk(chunk, filename, id_gen, table):
        embedding = await coco.use_context(EMBEDDER).embed(chunk.text)
        ...
```

## How it works

Walk a repo → detect language → split along the **syntax tree** with
Tree-sitter → embed each chunk → upsert into Postgres (pgvector). With
`live=True`, the source keeps watching and the index stays fresh as you code.

The whole indexing path is the snippet below — read it top-to-bottom in [`main.py`](main.py):

```python
@coco.fn(memo=True)
async def process_file(file: FileLike, table: postgres.TableTarget[CodeEmbedding]) -> None:
    text = await file.read_text()
    language = detect_code_language(filename=str(file.file_path.path.name))
    chunks = _splitter.split(text, chunk_size=1000, min_chunk_size=300,
                             chunk_overlap=300, language=language)   # Tree-sitter, syntax-aware
    id_gen = IdGenerator()
    await coco.map(process_chunk, chunks, file.file_path.path, id_gen, table)

@coco.fn
async def process_chunk(chunk, filename, id_gen, table) -> None:
    embedding = await coco.use_context(EMBEDDER).embed(chunk.text)
    table.declare_row(row=CodeEmbedding(
        id=await id_gen.next_id(chunk.text), filename=str(filename), code=chunk.text,
        embedding=embedding, start_line=chunk.start.line, end_line=chunk.end.line,
    ))

@coco.fn
async def app_main(sourcedir: pathlib.Path) -> None:
    table = await postgres.mount_table_target(PG_DB, table_name=TABLE_NAME, ...)
    table.declare_vector_index(column="embedding")
    files = localfs.walk_dir(sourcedir, recursive=True,
                             path_matcher=PatternFilePathMatcher(included_patterns=["**/*.py", ...]),
                             live=True)
    await coco.mount_each(process_file, files.items(), table)
```

Highlights:

- **Syntax-aware chunking.** Tree-sitter splits along real code structure — functions, classes, blocks — so retrieval returns whole units, not fragments cut mid-statement. Unknown file types fall back to plain text.
- **Incremental by default.** `@coco.fn(memo=True)` skips unchanged files and reuses embeddings for unchanged chunks; `mount_table_target` upserts only the rows that moved and deletes orphans.
- **Live updates.** `live=True` + `grepify update -L` keeps watching the filesystem and applies changes with low latency.
- **Consistent index & query.** The same embedder is shared by the indexing and query paths, so what you index is what you search.

## Run it

**1. Postgres + pgvector.** If you don't have one, start a local instance with the compose file in this repo:

```sh
docker compose -f ../../dev/postgres.yaml up -d
export POSTGRES_URL="postgres://grepify:grepify@localhost/grepify"
```

**2. Install deps:**

```sh
pip install -e .
```

**3. Build / update the index** (writes rows into Postgres) — pick one:

```sh
grepify update main       # catch-up: scan, sync changes, exit
grepify update -L main    # live: catch up, then keep watching for edits
```

**4. Query it** — semantic search from the terminal:

```sh
python main.py "embedding"
```

Each result carries `start_line`/`end_line`, so hits point straight at the
lines that matched. Query uses pgvector's `<=>` cosine distance, turned into a
similarity score.
