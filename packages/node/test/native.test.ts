import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import {
  detectCodeLanguage,
  fingerprintBytes,
  matchCode,
  RateLimiter,
  splitTextRecursive,
  version,
  walkDir,
} from '../index.js'

describe('@grepify/node stateless ops', () => {
  it('exports version', () => {
    expect(version()).toMatch(/^\d+\.\d+\.\d+/)
  })

  it('detects python from extension', () => {
    expect(detectCodeLanguage('main.py')).toBe('python')
    expect(detectCodeLanguage('file.xyz')).toBeNull()
  })

  it('matches structural code patterns', () => {
    const source = 'def f(x):\n    return x\n'
    const matches = matchCode(String.raw`def \NAME(\(A*\)):`, source, 'python')
    expect(matches.length).toBeGreaterThan(0)
    expect(matches[0]!.captures['NAME']!.length).toBeGreaterThan(0)
  })

  it('splits text recursively', () => {
    const text = 'lorem ipsum dolor sit amet consectetur.\n'.repeat(400)
    const chunks = splitTextRecursive(text, {
      chunkSize: 2000,
      chunkOverlap: 200,
    })
    expect(chunks.length).toBeGreaterThan(1)
  })
})

describe('@grepify/node fingerprints', () => {
  it('is deterministic for the same bytes and differs otherwise', () => {
    const a = fingerprintBytes(Buffer.from('hello'))
    const b = fingerprintBytes(Buffer.from('hello'))
    const c = fingerprintBytes(Buffer.from('world'))
    expect(a).toBe(b)
    expect(a).not.toBe(c)
    // base64 of a 16-byte fingerprint.
    expect(a).toMatch(/^[A-Za-z0-9+/]{22}==$/)
  })
})

describe('@grepify/node walkDir', () => {
  it('walks recursively with glob patterns and exposes file entries', () => {
    const dir = mkdtempSync(join(tmpdir(), 'grepify-walk-'))
    writeFileSync(join(dir, 'a.rs'), 'fn main() {}')
    writeFileSync(join(dir, 'note.txt'), 'ignored')
    mkdirSync(join(dir, 'sub'))
    writeFileSync(join(dir, 'sub', 'b.rs'), 'mod sub;')

    const files = walkDir(dir, { recursive: true, includedPatterns: ['**/*.rs'] })
    const keys = files.map((f) => f.key).sort()
    expect(keys).toEqual(['a.rs', 'sub/b.rs'])

    const a = files.find((f) => f.key === 'a.rs')!
    expect(a.stem).toBe('a')
    expect(a.contentStr()).toBe('fn main() {}')
    expect(a.content()).toBeInstanceOf(Buffer)
  })

  it('is non-recursive by default', () => {
    const dir = mkdtempSync(join(tmpdir(), 'grepify-walk-'))
    writeFileSync(join(dir, 'root.rs'), '')
    mkdirSync(join(dir, 'nested'))
    writeFileSync(join(dir, 'nested', 'inner.rs'), '')

    const files = walkDir(dir, { includedPatterns: ['**/*.rs'] })
    expect(files.map((f) => f.key)).toEqual(['root.rs'])
  })
})

describe('@grepify/node RateLimiter', () => {
  it('acquires tokens without throwing', async () => {
    const limiter = new RateLimiter(1000, 1)
    await limiter.acquire(1)
    await limiter.acquire()
  })
})
