// TypeScript (napi) side of the binding benchmarks.
//
// Measures, for the @grepify/node host over the shared Rust engine:
//   * Boundary-crossing overhead:
//       - small sync call: detectCodeLanguage (cheapest napi round-trip)
//       - heavier sync call: matchCode (structural match; boundary + engine work)
//       - async mount round-trip: N sequential useMount of a trivial child
//   * Pipeline: mountEach over a generated corpus, cold run vs warm (memo-hit)
//     re-index latency.
//
// Writes a metrics JSON consumed by ../runner.py. Kept in sync with the Python
// (../python/bench.py) and Rust (../rust/src/main.rs) drivers.
//
// Imported by relative path to packages/node/index.ts (see the parity harness
// README for why the published entry does not exist in-repo).

import { readFileSync, mkdirSync, writeFileSync } from 'node:fs'
import { dirname } from 'node:path'
import { performance } from 'node:perf_hooks'
import {
  App,
  fn,
  matchCode,
  detectCodeLanguage,
  mountEach,
  splitTextRecursive,
  useMount,
  walkDir,
} from '../../../packages/node/index.ts'

const MATCH_SRC = 'def foo(a, b):\n    return a + b\n\ndef bar(x):\n    return x\n'
const MATCH_PATTERN = 'def \\NAME(\\(ARGS*\\)):'
const CHUNK_SIZE = 256
const LANGUAGE = 'markdown'

interface Args {
  corpus: string
  db: string
  metrics: string
  detectIters: number
  matchIters: number
  mountIters: number
}

function parseArgs(): Args {
  const argv = process.argv.slice(2)
  const map: Record<string, string> = {}
  for (let i = 0; i < argv.length; i += 2) map[argv[i].replace(/^--/, '')] = argv[i + 1]
  return {
    corpus: map.corpus,
    db: map.db,
    metrics: map.metrics,
    detectIters: Number(map['detect-iters'] ?? 500000),
    matchIters: Number(map['match-iters'] ?? 50000),
    mountIters: Number(map['mount-iters'] ?? 2000),
  }
}

function benchBoundarySync(detectIters: number, matchIters: number) {
  detectCodeLanguage('main.py')
  let t0 = performance.now()
  for (let i = 0; i < detectIters; i++) detectCodeLanguage('main.py')
  const detectNs = ((performance.now() - t0) / detectIters) * 1e6

  matchCode(MATCH_PATTERN, MATCH_SRC, 'python')
  t0 = performance.now()
  for (let i = 0; i < matchIters; i++) matchCode(MATCH_PATTERN, MATCH_SRC, 'python')
  const matchNs = ((performance.now() - t0) / matchIters) * 1e6

  return {
    detect_ns_per_call: Math.round(detectNs * 10) / 10,
    match_ns_per_call: Math.round(matchNs * 10) / 10,
    detect_iters: detectIters,
    match_iters: matchIters,
  }
}

const trivial = async (): Promise<number> => 0

const processFile = fn(
  async (absPath: string): Promise<number> => {
    // Memo key = logic hash + [absPath]; a warm re-run is a whole-child hit.
    const text = readFileSync(absPath, 'utf8')
    return splitTextRecursive(text, { chunkSize: CHUNK_SIZE, language: LANGUAGE }).length
  },
  { memo: true },
)

function totalStats(stats: { byComponent: Record<string, any> }) {
  const agg = { num_adds: 0, num_reprocesses: 0, num_unchanged: 0, num_deletes: 0 }
  for (const g of Object.values(stats.byComponent)) {
    agg.num_adds += g.numAdds
    agg.num_reprocesses += g.numReprocesses
    agg.num_unchanged += g.numUnchanged
    agg.num_deletes += g.numDeletes
  }
  return agg
}

async function drive(handle: { changed(): Promise<boolean> }): Promise<void> {
  while (await handle.changed()) {
    /* advance until terminated */
  }
}

async function main(): Promise<void> {
  const args = parseArgs()
  const result: Record<string, unknown> = { host: 'typescript' }

  result.boundary_sync = benchBoundarySync(args.detectIters, args.matchIters)

  // Async mount round-trip. Measured inside the update to exclude app startup.
  const rtApp = await App.open('bench_roundtrip', { dbPath: `${args.db}_rt` })
  const rtMain = async (iters: number): Promise<number> => {
    const t0 = performance.now()
    for (let i = 0; i < iters; i++) {
      await useMount<number>(`rt/${i}`, trivial)
    }
    return performance.now() - t0
  }
  const elapsed = await rtApp.update<number>(rtMain, { args: [args.mountIters] })
  result.boundary_async = {
    mount_us_per_op: Math.round((elapsed / args.mountIters) * 1000) / 1000,
    mount_iters: args.mountIters,
  }

  // Pipeline: cold vs warm.
  const pipeApp = await App.open('bench_pipeline', { dbPath: `${args.db}_pipe` })
  const pipelineMain = async (corpus: string): Promise<number> => {
    const files = walkDir(corpus, { recursive: true })
    const items: [string, string][] = files.map((f) => [f.key, f.path])
    const handle = await mountEach('process', processFile, items)
    await handle.ready()
    return items.length
  }

  let t0 = performance.now()
  const coldHandle = pipeApp.startUpdate(pipelineMain, { args: [args.corpus] })
  await drive(coldHandle)
  const coldMs = performance.now() - t0
  const coldStats = totalStats(coldHandle.stats())

  t0 = performance.now()
  const warmHandle = pipeApp.startUpdate(pipelineMain, { args: [args.corpus] })
  await drive(warmHandle)
  const warmMs = performance.now() - t0
  const warmStats = totalStats(warmHandle.stats())

  const nFiles = walkDir(args.corpus, { recursive: true }).length
  result.pipeline = {
    corpus_files: nFiles,
    cold_ms: Math.round(coldMs * 100) / 100,
    warm_ms: Math.round(warmMs * 100) / 100,
    cold_stats: coldStats,
    warm_stats: warmStats,
  }

  mkdirSync(dirname(args.metrics), { recursive: true })
  writeFileSync(args.metrics, JSON.stringify(result, null, 2), 'utf8')
}

main().then(
  () => process.exit(0),
  (err) => {
    console.error(err)
    process.exit(1)
  },
)
