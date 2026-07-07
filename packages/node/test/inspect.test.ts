import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { App, Environment, inspect, localfs, mountEach } from '../index.js'

function tmpDir(prefix: string): string {
  return mkdtempSync(join(tmpdir(), prefix))
}

describe('@grepify/node inspect', () => {
  it('lists app names and stable paths after a run, and round-trips path detail', async () => {
    const dbPath = join(tmpDir('grepify-db-'), 'state')
    const env = await Environment.open({ dbPath })
    const app = await env.app('inspect-app')
    const outDir = tmpDir('grepify-out-')

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

    // App is discoverable by name at the environment level.
    const names = await inspect.listAppNames(env)
    expect(names).toContain('inspect-app')

    // Stable paths exist and expose a display string + opaque pathBytes.
    const paths = await inspect.iterStablePaths(app)
    expect(paths.length).toBeGreaterThan(0)
    for (const p of paths) {
      expect(typeof p.path).toBe('string')
      expect(Buffer.isBuffer(p.pathBytes)).toBe(true)
      expect(['Directory', 'Component']).toContain(p.nodeType)
    }

    // Root query, recursive: should surface the same set of paths.
    const root = inspect.rootStablePath()
    const details = await inspect.queryStablePathDetails(app, root, {
      includeChildren: true,
      recursive: true,
    })
    expect(details.length).toBeGreaterThan(0)

    // The 'write' fan-out declares files as target states; at least one detail
    // should carry target-state items.
    const withTargets = details.filter((d) => d.targetStateItems.length > 0)
    expect(withTargets.length).toBeGreaterThan(0)

    // pathBytes round-trips: fetch one detail directly by its bytes.
    const one = paths[0]
    const detail = await inspect.getStablePathDetail(app, one.pathBytes)
    expect(detail).not.toBeNull()
    expect(detail?.path).toBe(one.path)
  })

  it('by-name inspect matches app-scoped inspect', async () => {
    const dbPath = join(tmpDir('grepify-db-'), 'state')
    const env = await Environment.open({ dbPath })
    const app = await env.app('by-name')
    await app.update(async () => 1)

    const direct = await inspect.iterStablePaths(app)
    const byName = await inspect.iterStablePathsByName(env, 'by-name')
    expect(byName.map((p) => p.path).sort()).toEqual(direct.map((p) => p.path).sort())

    // Unknown app => empty (no throw).
    const missing = await inspect.iterStablePathsByName(env, 'does-not-exist')
    expect(missing).toEqual([])
  })
})
