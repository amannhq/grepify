# Text Embedding

Index a folder of Markdown files for semantic search: each file is split into
overlapping chunks, every chunk is embedded with a local sentence-transformer,
and the vectors are stored in [Postgres + pgvector](https://github.com/pgvector/pgvector).
Editing one file re-embeds one file, not the whole folder.

## How it works

The whole pipeline is ordinary `async` Python and the row type is your own dataclass:

1. **Walk** Markdown files from a local directory (`live=True`, so it can watch for changes).
2. **Chunk** each file into overlapping pieces with `RecursiveSplitter`.
3. **Embed** every chunk with `all-MiniLM-L6-v2`, a small, fast model that runs locally with no API key.
4. **Store** one row per chunk in Postgres, with a pgvector index over the embedding.

`process_file` runs once per file; `memo=True` makes it incremental — if a
file's content and the function's code are unchanged, the whole file is skipped
on the next run. Read it top-to-bottom in [`main.py`](main.py):

```python
@dataclass
class DocEmbedding:
    id: int
    filename: str
    chunk_start: int
    chunk_end: int
    text: str
    embedding: Annotated[NDArray, EMBEDDER]   # dimension inferred from the embedder

@coco.fn(memo=True)
async def process_file(file: FileLike, table: postgres.TableTarget[DocEmbedding]) -> None:
    text = await file.read_text()
    chunks = _splitter.split(text, chunk_size=2000, chunk_overlap=500, language="markdown")
    id_gen = IdGenerator()
    await coco.map(process_chunk, chunks, file.file_path.path, id_gen, table)

@coco.fn
async def app_main(sourcedir: pathlib.Path) -> None:
    target_table = await postgres.mount_table_target(
        PG_DB, table_name=TABLE_NAME,
        table_schema=await postgres.TableSchema.from_class(DocEmbedding, primary_key=["id"]),
        pg_schema_name=PG_SCHEMA_NAME,
    )
    target_table.declare_vector_index(column="embedding")
    files = localfs.walk_dir(sourcedir, recursive=True,
        path_matcher=PatternFilePathMatcher(included_patterns=["**/*.md"]), live=True)
    await coco.mount_each(process_file, files.items(), target_table)
```

Each row's `id` is derived from its chunk text, so re-running upserts only the
rows that actually changed and deletes the ones whose source is gone.

## Run it

**1. Start Postgres + pgvector:**

```sh
docker compose -f ../../dev/postgres.yaml up -d
```

**2. Configure & install:**

```sh
cp .env.example .env     # set POSTGRES_URL (defaults to the local docker one)
pip install -e .
```

**3. Build the index** — the example ships a `markdown_files/` folder of sample docs:

```sh
grepify update main          # catch-up: scan, sync, exit
grepify update -L main       # live: keep watching for file changes
```

**4. Search** — embeds your query with the *same* model and returns the nearest chunks by pgvector cosine distance:

```sh
python main.py "what is self-attention?"
```
