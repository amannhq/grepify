# Grepify binding benchmarks

Performance benchmarks comparing the three hosts over the one shared Rust
engine:

- **Rust-native** (`rust/`) — calls the `grepify` SDK directly. This is the
  **zero-boundary baseline**: no FFI, so `host_time − rust_time` approximates the
  host binding + marshaling overhead.
- **Python** (`python/`) — the PyO3 host (`grepify`).
- **TypeScript** (`ts/`) — the napi host (`@grepify/node`).

It mirrors the structure of `benchmarks/file_summarization` (per-language driver
+ a Python orchestrator) rather than introducing a new framework. Each driver
runs the same workload and writes a metrics JSON; `runner.py` aggregates them,
prints a comparison table, and writes `.work/results.json` + `.work/results.md`.

## What is measured

### 1. Boundary-crossing microbench

- **`detect_code_language` (ns/call)** — the cheapest possible boundary call
  (string in, `string | null` out). Isolates raw per-call FFI overhead.
- **`match_code` (ns/call)** — a structural match; boundary crossing **plus**
  marshaling a structured result (matches → chunks → captures) back to the host.
- **child mount+create (µs/op)** — `N` sequential `useMount` (Python
  `use_mount` / Rust `ctx.scope`) of a trivial child, each at a **distinct**
  component path. See the caveat below — this is dominated by durable
  per-component commit, not pure boundary cost.

### 2. Pipeline bench (the critical code-indexer path)

`mountEach` over a generated corpus of small Markdown files, each child reading
the file and running the recursive splitter, with a memoized processor.
Measured **cold** (fresh DB — every file processed) and **warm** (unchanged DB —
every file a memo hit). The warm/memo-hit re-index latency is the number that
matters most for an incremental code indexer.

## Running

```bash
# Prereqs:
#   Python: `uv run maturin develop` at the repo root (builds grepify).
#   TS:     @grepify/node native artifact in packages/node (npm install && npm run build),
#           then `cd ts && npm install` here (installs tsx).
#   Rust:   cargo (the runner builds the release binary automatically).

python runner.py            # full run (1000-file corpus)
python runner.py --quick    # tiny/fast smoke run
python runner.py --files 5000
python runner.py --only rust,typescript
./run.sh                    # thin wrapper around runner.py
```

If a host can't run (e.g. the `.node` artifact is missing, or `tsx` isn't
installed), it is marked **PENDING** and the others still run.

## Example results

Captured on an Apple-silicon dev machine, 1000-file corpus,
`detect_iters=500k`, `match_iters=50k`, `mount_iters=1000`. Absolute numbers are
machine-specific; the **ratios** are the takeaway.

| metric | rust | python | typescript | py/rust | ts/rust |
| --- | --- | --- | --- | --- | --- |
| detect_code_language (ns/call) | ~101 | ~1220 | ~721 | 12.1x | 7.1x |
| match_code (ns/call) | ~39.6k | ~260k | ~235k | 6.6x | 6.0x |
| child mount+create (µs/op) | ~12300 | ~15700 | ~14 | — | — |
| pipeline cold (ms, 1000 files) | ~109 | ~843 | ~510 | 7.7x | 4.7x |
| pipeline warm/memo-hit (ms) | ~55 | ~360 | ~47 | 6.5x | 0.85x |

Native artifact sizes tracked each run: `grepify-node.darwin-arm64.node`
≈ 83.8 MB; rust bench binary ≈ 56.4 MB.

### Reading the numbers

- **Small-call boundary overhead is small and TS ≤ Python.** For
  `detect_code_language`, TypeScript's napi overhead (~721 ns) is **~0.6× of
  Python's** PyO3 overhead (~1220 ns) — comfortably inside the plan's "TS within
  ~1.2× of Python host overhead" target. Both are ~7–12× the 101 ns raw engine
  call, i.e. the *boundary* adds only hundreds of nanoseconds.
- **Structured results cost marshaling.** `match_code` is ~6× the rust baseline
  in both hosts — the extra cost is converting the match/chunk/capture tree into
  host objects, not the call itself. This is why the design serializes once in
  Rust and prefers batch-oriented APIs.
- **Warm re-index is the headline for a code indexer, and TS shines.** On a warm
  re-run of 1000 files, TypeScript (~47 ms) is on par with — even slightly faster
  than — rust-native (~55 ms), because `mountEach` short-circuits the whole child
  at the mount via a precomputed memo key (no boundary crossing, no file read on
  a hit). Python's warm path (~360 ms) is slower here because computing each
  file's content-based memo key still touches the file.

## Caveats / honest notes

- **`child mount+create` is not a pure boundary metric.** Each iteration mounts a
  *new, distinct* durable component, and the rust/python numbers (~12–16 ms/op)
  are dominated by the per-component LMDB commit on a sequential awaited mount;
  the TS number (~14 µs/op) reflects that its mount-callback resolves before /
  independently of that durable flush (commits coalesce). Treat this row as
  "sequential durable child-creation latency," and use the `detect`/`match` rows
  for pure boundary overhead. (A future improvement: a memoized-hit mount variant
  to isolate the async round-trip from the durable write.)
- **Warm-path memo semantics differ per host, by design.** Python and TS skip the
  *entire child* on a memo hit (the read + split never runs). The current Rust
  `mount_each` has no per-item memo key, so it re-establishes each child scope and
  memoizes at the `#[function(memo)]` level instead — hence rust's warm number
  includes 1000 child-scope setups. All three still measure a real "no-op
  re-index of N files"; the number just isn't apples-to-apples at the scope level.
- **Cross-host caches are independent** (see `tests/parity/README.md`): these are
  three separate DBs, not a shared cache.
- Timing is plain wall-clock (`perf_counter` / `performance.now` / `Instant` /
  `RunStats.elapsed`), single-shot per phase — same convention as
  `benchmarks/file_summarization`. For low-variance numbers, raise the corpus
  size and iteration counts.
