// TypeScript side of the stateless-ops golden test.
//
// Mirror of tests/parity/python/stateless.py: read the shared
// fixtures/ops_cases.json, run each case through the shared Rust engine via
// @grepify/node (recursive splitter, structural code match, index terms,
// language detection) and write a normalized JSON result. The runner asserts
// the Python and TypeScript results are deeply equal.

import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import {
  type Chunk,
  detectCodeLanguage,
  indexTerms,
  matchCode,
  splitTextRecursive,
} from '../../../packages/node/index.ts'

interface Args {
  fixtures: string
  cases: string
  out: string
}

function parseArgs(): Args {
  const argv = process.argv.slice(2)
  const map: Record<string, string> = {}
  for (let i = 0; i < argv.length; i += 2) {
    map[argv[i].replace(/^--/, '')] = argv[i + 1]
  }
  return { fixtures: map.fixtures, cases: map.cases, out: map.out }
}

function chunkDict(sourceBytes: Buffer, c: Chunk): Record<string, unknown> {
  return {
    startByte: c.startByte,
    endByte: c.endByte,
    startChar: c.startCharOffset,
    endChar: c.endCharOffset,
    startLine: c.startLine,
    startColumn: c.startColumn,
    endLine: c.endLine,
    endColumn: c.endColumn,
    text: sourceBytes.subarray(c.startByte, c.endByte).toString('utf8'),
  }
}

function main(): void {
  const args = parseArgs()
  const cases = JSON.parse(readFileSync(args.cases, 'utf8'))

  const result: {
    host: string
    split: Record<string, unknown[]>
    match: Record<string, unknown[]>
    index_terms: Record<string, string[]>
    detect_language: Record<string, string | null>
  } = {
    host: 'typescript',
    split: {},
    match: {},
    index_terms: {},
    detect_language: {},
  }

  for (const c of cases.split_cases ?? []) {
    const text = readFileSync(join(args.fixtures, c.file), 'utf8')
    const sourceBytes = Buffer.from(text, 'utf8')
    const config: Record<string, unknown> = { chunkSize: c.chunk_size }
    if ('chunk_overlap' in c) config.chunkOverlap = c.chunk_overlap
    if ('min_chunk_size' in c) config.minChunkSize = c.min_chunk_size
    if ('language' in c) config.language = c.language
    const chunks = splitTextRecursive(text, config as any)
    result.split[c.name] = chunks.map((ch) => chunkDict(sourceBytes, ch))
  }

  for (const c of cases.match_cases ?? []) {
    const text = readFileSync(join(args.fixtures, c.file), 'utf8')
    const sourceBytes = Buffer.from(text, 'utf8')
    const matches = matchCode(c.pattern, text, c.language)
    result.match[c.name] = matches.map((m) => ({
      kind: m.kind,
      chunks: m.chunks.map((ch) => chunkDict(sourceBytes, ch)),
      captures: Object.fromEntries(
        Object.entries(m.captures).map(([name, chunks]) => [
          name,
          chunks.map((ch) => chunkDict(sourceBytes, ch)),
        ]),
      ),
    }))
  }

  for (const c of cases.index_terms_cases ?? []) {
    const text = readFileSync(join(args.fixtures, c.file), 'utf8')
    // `indexTerms` returns a deduped set; its iteration order is not a stable
    // cross-host contract, so compare the sorted term set.
    result.index_terms[c.name] = indexTerms(text, c.language, c.min_len ?? 3).sort()
  }

  for (const c of cases.detect_language_cases ?? []) {
    result.detect_language[c.name] = detectCodeLanguage(c.filename)
  }

  mkdirSync(dirname(args.out), { recursive: true })
  writeFileSync(args.out, JSON.stringify(result, null, 2), 'utf8')
}

main()
