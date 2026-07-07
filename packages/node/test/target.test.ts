import { existsSync, mkdtempSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { App, Environment, localfs, mountEach } from '../index.js'

function tmpDir(prefix: string): string {
  return mkdtempSync(join(tmpdir(), prefix))
}

async function openApp(name: string): Promise<App> {
  const env = await Environment.open({ dbPath: join(tmpDir('grepify-db-'), 'state') })
  return env.app(name)
}

describe('@grepify/node localfs DirTarget reconcile', () => {
  it('creates, updates, and deletes files to match declarations', async () => {
    const app = await openApp('dir-target')
    const outDir = tmpDir('grepify-out-')

    // Run 1: declare two files.
    await app.update(async () => {
      const target = localfs.mountDirTarget(outDir)
      const process = async (value: [string, string]) => {
        target.declareFile(value[0], value[1])
      }
      const handle = await mountEach<[string, string]>('write', process, [
        ['a.txt', ['a.txt', 'alpha']],
        ['b.txt', ['b.txt', 'beta']],
      ])
      await handle.ready()
    })
    expect(readFileSync(join(outDir, 'a.txt'), 'utf8')).toBe('alpha')
    expect(readFileSync(join(outDir, 'b.txt'), 'utf8')).toBe('beta')

    // Run 2: update a.txt, drop b.txt (should be deleted), keep only a.
    await app.update(async () => {
      const target = localfs.mountDirTarget(outDir)
      const process = async (value: [string, string]) => {
        target.declareFile(value[0], value[1])
      }
      const handle = await mountEach<[string, string]>('write', process, [
        ['a.txt', ['a.txt', 'alpha-2']],
      ])
      await handle.ready()
    })
    expect(readFileSync(join(outDir, 'a.txt'), 'utf8')).toBe('alpha-2')
    expect(existsSync(join(outDir, 'b.txt'))).toBe(false)
  })
})
