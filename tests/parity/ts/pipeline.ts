// TypeScript side of the cross-language parity pipeline.
//
// Mirror of tests/parity/python/pipeline.py: walk a corpus of Markdown files,
// split each with the shared Rust recursive splitter, and write one
// deterministic `*.chunks` file per source into a localfs DirTarget. The
// output bytes are constructed identically to the Python driver so the two
// DirTargets compare byte-for-byte.
//
// Run with tsx (see package.json). We import the SDK by relative path to
// packages/node/index.ts because the published `@grepify/node` entry
// (index.js) is only generated at publish time; the native `.node` artifact and
// the TS sources are what exist in-repo today.

import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname } from 'node:path'
import {
  App,
  type Chunk,
  fn,
  localfs,
  mountEach,
  splitTextRecursive,
  walkDir,
} from '../../../packages/node/index.ts'

// Chunking configuration shared with the Python driver. Keep in sync with
// tests/parity/python/pipeline.py.
const CHUNK_SIZE = 200
const CHUNK_OVERLAP = 40
const LANGUAGE = 'markdown'

const RECORD_SEP = Buffer.from([0x1e])

interface Args {
  source: string
  out: string
  db: string
  metrics: string
}

function parseArgs(): Args {
  const argv = process.argv.slice(2)
  const map: Record<string, string> = {}
  for (let i = 0; i < argv.length; i += 2) {
    map[argv[i].replace(/^--/, '')] = argv[i + 1]
  }
  return { source: map.source, out: map.out, db: map.db, metrics: map.metrics }
}

/** Build the canonical, cross-language output bytes for one source file. */
function renderChunks(relativePath: string, sourceBytes: Buffer, chunks: Chunk[]): Buffer {
  const parts: Buffer[] = []
  parts.push(Buffer.from(`${relativePath}\n`, 'utf8'))
  parts.push(Buffer.from(`${chunks.length}\n`, 'utf8'))
  for (const c of chunks) {
    const header =
      `${c.startByte} ${c.endByte} ` +
      `${c.startCharOffset} ${c.endCharOffset} ` +
      `${c.startLine} ${c.startColumn} ` +
      `${c.endLine} ${c.endColumn}\n`
    parts.push(Buffer.from(header, 'utf8'))
    parts.push(sourceBytes.subarray(c.startByte, c.endByte))
    parts.push(RECORD_SEP)
  }
  return Buffer.concat(parts)
}

interface FileItem {
  rel: string
  text: string
}

async function main(): Promise<void> {
  const args = parseArgs()

  const app = await App.open('parity_pipeline', { dbPath: args.db })

  // The processor closes over `target` (a DirTarget is not serializable, so it
  // cannot be a mountEach extra-arg without disabling memoization). The memo key
  // is derived from the fn logic hash + the item value, so a content change on
  // one file invalidates only that child.
  const appMain = async (sourceDir: string, outDir: string): Promise<void> => {
    // Create the target in the root component scope, then use it from the
    // `process/*` children via closure (mirrors packages/node/test/target.test.ts).
    const target = localfs.mountDirTarget(outDir)

    const files = walkDir(sourceDir, {
      recursive: true,
      includedPatterns: ['**/*.md'],
    })
    const items: [string, FileItem][] = files.map((f) => [
      f.relativePath,
      { rel: f.relativePath, text: f.contentStr() },
    ])

    const processFile = fn(
      async (item: FileItem): Promise<void> => {
        const sourceBytes = Buffer.from(item.text, 'utf8')
        const chunks = splitTextRecursive(item.text, {
          chunkSize: CHUNK_SIZE,
          chunkOverlap: CHUNK_OVERLAP,
          language: LANGUAGE,
        })
        const outName = item.rel.replaceAll('/', '__') + '.chunks'
        target.declareFile(outName, renderChunks(item.rel, sourceBytes, chunks))
      },
      { memo: true },
    )

    const handle = await mountEach('process', processFile, items)
    await handle.ready()
  }

  const updateHandle = app.startUpdate(appMain, { args: [args.source, args.out] })
  // Drive to completion with `changed()` (which restores the handle each poll)
  // rather than `result()` (which consumes it), so `stats()` stays readable.
  while (await updateHandle.changed()) {
    /* keep advancing until the update terminates */
  }
  const stats = updateHandle.stats()

  const metrics = {
    host: 'typescript',
    out_dir: args.out,
    db_path: args.db,
    stats: {
      ready: stats.ready,
      by_component: Object.fromEntries(
        Object.entries(stats.byComponent).map(([k, g]) => [
          k,
          {
            num_execution_starts: g.numExecutionStarts,
            num_unchanged: g.numUnchanged,
            num_adds: g.numAdds,
            num_deletes: g.numDeletes,
            num_reprocesses: g.numReprocesses,
            num_errors: g.numErrors,
            num_processed:
              g.numUnchanged + g.numAdds + g.numDeletes + g.numReprocesses,
          },
        ]),
      ),
    },
  }

  mkdirSync(dirname(args.metrics), { recursive: true })
  writeFileSync(args.metrics, JSON.stringify(metrics, null, 2), 'utf8')
}

main().then(
  () => process.exit(0),
  (err) => {
    console.error(err)
    process.exit(1)
  },
)
