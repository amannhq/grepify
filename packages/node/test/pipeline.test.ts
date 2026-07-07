import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import {
  App,
  ContextKey,
  ContextProvider,
  Environment,
  fn,
  map,
  mountEach,
  useContext,
  useMount,
  useState,
} from '../index.js'

function tmpDb(): string {
  return join(mkdtempSync(join(tmpdir(), 'grepify-db-')), 'state')
}

async function openApp(name: string): Promise<App> {
  const env = await Environment.open({ dbPath: tmpDb() })
  return env.app(name)
}

describe('@grepify/node pipeline', () => {
  it('runs a root update and returns its result', async () => {
    const app = await openApp('root')
    const result = await app.update<number>(async () => 42)
    expect(result).toBe(42)
  })

  it('mounts a dependent child and returns its value', async () => {
    const app = await openApp('use-mount')
    const double = async (n: number) => n * 2
    const result = await app.update<number>(async () => {
      return useMount<number>('double', double, 21)
    })
    expect(result).toBe(42)
  })

  it('memoizes an unchanged child across runs (hit skips re-execution)', async () => {
    const app = await openApp('memo')
    let executions = 0
    const compute = fn(
      async (n: number) => {
        executions += 1
        return n + 1
      },
      { memo: true },
    )

    const first = await app.update<number>(async () => useMount<number>('compute', compute, 10))
    const second = await app.update<number>(async () => useMount<number>('compute', compute, 10))

    expect(first).toBe(11)
    expect(second).toBe(11)
    // Second run is a memo hit: the processor body does not re-run.
    expect(executions).toBe(1)
  })

  it('re-executes a memoized child when inputs change (miss)', async () => {
    const app = await openApp('memo-miss')
    let executions = 0
    const compute = fn(
      async (n: number) => {
        executions += 1
        return n + 1
      },
      { memo: true },
    )

    await app.update<number>(async () => useMount<number>('compute', compute, 10))
    await app.update<number>(async () => useMount<number>('compute', compute, 20))

    expect(executions).toBe(2)
  })

  it('persists per-component state across runs', async () => {
    const app = await openApp('state')
    const seen: number[] = []
    const main = async () => {
      const counter = useState<number>('counter', 0)
      seen.push(counter.value)
      counter.value = counter.value + 1
    }
    await app.update(main)
    await app.update(main)
    await app.update(main)
    expect(seen).toEqual([0, 1, 2])
  })

  it('mounts one child per item with mountEach', async () => {
    const app = await openApp('mount-each')
    const processed: string[] = []
    const process = async (value: string) => {
      processed.push(value)
      return value.toUpperCase()
    }
    await app.update(async () => {
      const handle = await mountEach('process', process, [
        ['a', 'alpha'],
        ['b', 'beta'],
      ] as [string, string][])
      await handle.ready()
    })
    expect(processed.sort()).toEqual(['alpha', 'beta'])
  })

  it('exposes context values via useContext', async () => {
    const app = await openApp('context')
    const GREETING = new ContextKey<string>('greeting')
    const provider = new ContextProvider().provide(GREETING, 'hello')
    const child = async () => useContext(GREETING)
    const result = await app.update<string>(
      async () => useMount<string>('child', child),
      { context: provider },
    )
    expect(result).toBe('hello')
  })

  it('runs map concurrently within a component (no child components)', async () => {
    const app = await openApp('map')
    const result = await app.update<number[]>(async () => {
      return map(async (n: number) => n * 10, [1, 2, 3])
    })
    expect(result).toEqual([10, 20, 30])
  })
})
