# Grepify cross-language parity harness

This harness runs the **same declarative pipeline** and the **same stateless-ops
cases** in two hosts over the one shared Rust engine — the Python SDK
(`grepify`) and the TypeScript SDK (`@grepify/node`) — and asserts their
**outputs** match.

## What "parity" means here (and what it does *not*)

Parity is about **equal outputs, not shared caches.**

The two hosts each keep their own memoization state under their own `db_path`.
The memo/logic fingerprints canonicalize *host-language* objects (a Python
`FileLike`, a JS value), so a cache entry written by Python is **not** expected
to be reusable by TypeScript, and vice versa. Cross-language memo-cache *reuse*
is explicitly a non-goal (see Part C of the plan). What must be identical is the
**result** each host produces from the same input:

- the reconciled `localfs` DirTarget files (byte-for-byte), and
- the stateless-ops results (chunk boundaries, structural-match captures, index
  terms, language detection).

Incremental behaviour must also match: editing one source file re-processes
exactly one component in each host, independently.

## Layout

```
tests/parity/
├── README.md
├── Makefile                 # make setup / parity / keep / clean
├── run_parity.py            # orchestrator: runs both hosts, compares, asserts
├── fixtures/
│   ├── ops_cases.json       # single source of truth for the stateless cases
│   ├── corpus/              # Markdown fixtures for the pipeline (walk → chunk → DirTarget)
│   │   ├── alpha.md
│   │   ├── beta.md
│   │   └── nested/gamma.md
│   └── code/sample.py       # source fixture for match_code / index_terms
├── python/                  # Python host (uv project, editable grepify)
│   ├── pyproject.toml
│   ├── pipeline.py          # walk → RecursiveSplitter → localfs DirTarget
│   └── stateless.py         # ops_cases.json → normalized golden JSON
└── ts/                      # TypeScript host (@grepify/node via tsx)
    ├── package.json
    ├── tsconfig.json
    ├── pipeline.ts          # walk → splitTextRecursive → localfs DirTarget
    └── stateless.ts         # ops_cases.json → normalized golden JSON
```

The two `pipeline.*` drivers construct their output-file bytes with an
identical, explicitly-specified format (a header line, a per-chunk boundary
line, the chunk's source bytes sliced by byte offset, and a `0x1e` record
separator), so the DirTargets are directly byte-comparable. The two
`stateless.*` drivers emit the same normalized JSON shape.

## The pipeline (defined twice)

Walk a corpus of Markdown files → split each with the shared recursive splitter
→ write one deterministic `<relpath>.chunks` file per source into a `localfs`
DirTarget. Chunk config (`chunkSize=200`, `chunkOverlap=40`, `language=markdown`)
is kept in sync between `python/pipeline.py` and `ts/pipeline.ts`.

## Checks performed by `run_parity.py`

1. **DirTarget outputs identical (cold)** — every output file exists in both and
   is byte-for-byte equal.
2. **Cold processing signature** (per host) — all files `added`, none reused.
3. **Stateless ops identical** — deep-equality of the normalized golden JSON for
   `split` (recursive splitter), `match` (`match_code` kind + chunk offsets +
   capture text), `index_terms` (compared as a sorted set — the API returns a
   deduped set whose order is not a cross-host contract), and
   `detect_language`.
4. **Incremental signature** (per host) — after editing exactly one source file,
   `UpdateStats` shows exactly `1` reprocess and `N-1` unchanged (memo hits),
   `0` adds/deletes.
5. **Only the edited file's output changed** (per host, authoritative) — hashing
   every output file before/after the re-run, only the edited source's
   `.chunks` output changes on disk.
6. **DirTarget outputs identical after edit** — cross-host byte parity still
   holds post-edit.

### Notes on the incremental / counter check

Both SDKs expose per-component `UpdateStats` counters
(`numAdds`/`numReprocesses`/`numUnchanged`/`numDeletes`), but with different key
shapes: the Python SDK aggregates a processor group under its function name
(`process_file`), while `@grepify/node` keys per stable path
(`mount:process/<relpath>`). The runner aggregates every component whose key
contains `process`, which isolates the file-processing work in both hosts. The
counter check is backed up by the authoritative output-hash diff (check 5),
which needs no counter support at all.

## Running

Prerequisites:

- **Python side:** build the Rust core once at the repo root:
  `uv run maturin develop`. The `python/` driver is a `uv` project with an
  editable `grepify` dependency; `uv run` provisions its venv on first use.
- **TS side:** the `@grepify/node` native artifact must exist in
  `packages/node/` (`cd packages/node && npm install && npm run build`). Then
  install this driver's `tsx`:

```bash
make setup          # cd ts && npm install
```

Run the harness:

```bash
make parity         # runs everything; exits non-zero if any check fails
make keep           # same, but keeps .work/ (db + out dirs) for inspection
```

`run_parity.py` writes a machine-readable summary to `.work/parity_results.json`
(all checks + the raw `UpdateStats` snapshots for both hosts).

## Scope of assertions today

This harness asserts against what the TS SDK supports today: the
mount/`useMount`/`mountEach` pipeline, the `localfs` DirTarget
(create/update/delete reconcile), and the stateless ops
(`matchCode`/`indexTerms`/`splitTextRecursive`/`detectCodeLanguage`). It is
structured to extend easily — add cases to `fixtures/ops_cases.json`, or add new
target comparisons — as more connectors, live components, and the inspect API
land in the TS SDK.
