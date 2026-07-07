import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { App, Environment, custom, mountEach, type TargetAction } from '../index.js'

function tmpDb(): string {
  return join(mkdtempSync(join(tmpdir(), 'grepify-db-')), 'state')
}

async function openApp(name: string): Promise<App> {
  const env = await Environment.open({ dbPath: tmpDb() })
  return env.app(name)
}

// A tiny in-memory "database" a custom TS target reconciles rows into.
class MemStore {
  rows = new Map<string, unknown>()
  applied: TargetAction[] = []

  applyActions = async (actions: TargetAction[]): Promise<void> => {
    for (const a of actions) {
      this.applied.push(a)
      if (a.kind === 'delete') this.rows.delete(a.key)
      else this.rows.set(a.key, a.value)
    }
  }
}

describe('@grepify/node custom target', () => {
  it('reconciles upserts and deletes across runs', async () => {
    const app = await openApp('custom-target')
    const store = new MemStore()

    const runWith = (items: [string, { key: string; n: number }][]) =>
      app.update(async () => {
        const target = custom.mountTarget('mem/rows', store.applyActions)
        const process = async (value: { key: string; n: number }) => {
          target.declareRow(value.key, value)
        }
        const handle = await mountEach<{ key: string; n: number }>('rows', process, items)
        await handle.ready()
      })

    // Run 1: two rows created.
    await runWith([
      ['a', { key: 'a', n: 1 }],
      ['b', { key: 'b', n: 2 }],
    ])
    expect(store.rows.get('a')).toEqual({ key: 'a', n: 1 })
    expect(store.rows.get('b')).toEqual({ key: 'b', n: 2 })

    // Run 2: 'a' unchanged (skip), 'b' updated, 'c' new, and no more... keep a,b,c.
    store.applied = []
    await runWith([
      ['a', { key: 'a', n: 1 }],
      ['b', { key: 'b', n: 20 }],
      ['c', { key: 'c', n: 3 }],
    ])
    // 'a' unchanged => not re-applied; 'b' and 'c' applied.
    const appliedKeys = store.applied.map((a) => `${a.kind}:${a.key}`).sort()
    expect(appliedKeys).toEqual(['upsert:b', 'upsert:c'])
    expect(store.rows.get('b')).toEqual({ key: 'b', n: 20 })
    expect(store.rows.get('c')).toEqual({ key: 'c', n: 3 })

    // Run 3: drop 'b' and 'c' (declare only 'a') => both deleted.
    store.applied = []
    await runWith([['a', { key: 'a', n: 1 }]])
    expect(store.rows.has('b')).toBe(false)
    expect(store.rows.has('c')).toBe(false)
    expect(store.rows.has('a')).toBe(true)
    const deletes = store.applied.filter((a) => a.kind === 'delete').map((a) => a.key).sort()
    expect(deletes).toEqual(['b', 'c'])
  })
})
