# AGENTS.md — Grepify examples

Guidance for AI coding agents (Claude Code, Cursor, etc.) working in this `examples/`
directory. Each top-level Python subfolder is a self-contained, runnable
Grepify **v1** app; `rust/` contains Rust ports with per-example READMEs.

## Before you write Grepify code: install the skill

Grepify v1 is a fundamental redesign from v0. Without context, LLMs tend to
hallucinate the v0 flow-builder DSL and deprecated decorators. Install the
skill first — it teaches the correct v1 API. It lives in-repo at
[`skills/grepify/`](../skills/grepify) (SKILL.md + `references/`):

```sh
mkdir -p .agents/skills && cp -r <repo>/skills/grepify .agents/skills/
```

For Claude Code's native project path, use `.claude/skills/grepify` instead
of `.agents/skills/grepify`; the skill format is the same. For Cursor, copy
`SKILL.md` into `.cursor/rules/`.

## The v1 mental model

`target_state = transform(source_state)`. You declare what the target should look
like; the Rust engine keeps it in sync, reprocessing only what changed (state is
tracked in a local LMDB store — **no database is required for the engine itself**,
only when an example writes to one). Key APIs: `@coco.fn`, `mount` / `use_mount` /
`mount_each`, `ContextKey`, target-state declarations. See the skill for details.

## Running examples

Each Python example is a standalone project with its own `pyproject.toml`:

```sh
cd <example_dir>
cp .env.example .env          # if present — fill in the blanks (see below)
pip install -e .              # or: uv pip install -e .
grepify update main           # catch-up: scan sources, sync, exit
grepify update -L main        # live mode: catch up, then watch for changes (where supported)
```

Use the example's README as the source of truth. The Rust ports under
`rust/<example>` use `cargo run -- index` for indexing and
`cargo run -- query "..."` for search; see each README.

Some examples expose a query/CLI demo via `python main.py "<query>"`; check the
example's `README.md`.

## Environment / credentials

When an example needs credentials or service configuration, required env vars
are templated in that example's **`.env.example`** — `cp` it to `.env` and fill
in the blanks; both `python main.py` and the `grepify` CLI load `.env`
automatically. Common ones:

- `POSTGRES_URL` — for Postgres/pgvector targets. Local instance:
  `docker compose -f ../../dev/postgres.yaml up -d` from inside an example
  directory.

Examples with no `.env.example` (e.g. `files_transform`) run fully locally with
no credentials.

**Never commit secrets.** The `.env` files tracked in this repo hold only
non-secret defaults (`GREPIFY_DB`, local service URLs); keep API keys and
credentials in your local `.env` edits and out of commits.

## The examples

- `text_embedding` — Markdown → pgvector; the simplest end-to-end index.
- `code_embedding` — repo → Tree-sitter chunks → pgvector; query code in English.
- `files_transform` — watch Markdown files → HTML, live mode (local, no services).
- `rust/` — Rust ports of the above, using the Grepify Rust API.

## Conventions for edits

- Keep each Python example self-contained: its own `pyproject.toml` and
  `README.md`; add `.env.example` when credentials or configurable services are
  required.
- Match the surrounding code's low comment density.
- Don't commit generated artifacts (`grepify.db`, `__pycache__`, build output) —
  they're already git-ignored.
