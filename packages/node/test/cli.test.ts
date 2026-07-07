import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { App, Environment, localfs, mountEach, runCli, type CliIo } from '../index.js'

function tmpDb(): string {
  return join(mkdtempSync(join(tmpdir(), 'grepify-db-')), 'state')
}

function captureIo(): { io: CliIo; out: string[]; err: string[] } {
  const out: string[] = []
  const err: string[] = []
  return { io: { out: (l) => out.push(l), err: (l) => err.push(l) }, out, err }
}

async function seed(dbPath: string, appName: string): Promise<void> {
  const env = await Environment.open({ dbPath })
  const app: App = await env.app(appName)
  const outDir = mkdtempSync(join(tmpdir(), 'grepify-out-'))
  await app.update(async () => {
    const target = localfs.mountDirTarget(outDir)
    const handle = await mountEach<[string, string]>(
      'write',
      async (v) => target.declareFile(v[0], v[1]),
      [['a.txt', ['a.txt', 'alpha']]],
    )
    await handle.ready()
  })
}

const ARGV = ['node', 'grepify']

describe('@grepify/node CLI', () => {
  it('ls lists persisted apps from a database', async () => {
    const db = tmpDb()
    await seed(db, 'cli-ls-app')
    const { io, out } = captureIo()
    await runCli([...ARGV, 'ls', '--db', db], io)
    expect(out.join('\n')).toContain('cli-ls-app')
  })

  it('ls reports empty database', async () => {
    const db = tmpDb()
    await Environment.open({ dbPath: db })
    const { io, out } = captureIo()
    await runCli([...ARGV, 'ls', '--db', db], io)
    expect(out.join('\n')).toContain('No persisted apps')
  })

  it('show lists stable paths for an app', async () => {
    const db = tmpDb()
    await seed(db, 'cli-show-app')
    const { io, out } = captureIo()
    await runCli([...ARGV, 'show', '--db', db, '--app-name', 'cli-show-app'], io)
    const text = out.join('\n')
    expect(text).toContain('Stable paths:')
    expect(text.split('\n').length).toBeGreaterThan(1)
  })

  it('show --long prints detail lines with target states', async () => {
    const db = tmpDb()
    await seed(db, 'cli-long-app')
    const { io, out } = captureIo()
    await runCli([...ARGV, 'show', '--db', db, '--app-name', 'cli-long-app', '--long'], io)
    const text = out.join('\n')
    expect(text).toMatch(/type:(component|directory)/)
  })

  it('update/drop are documented not-implemented stubs', async () => {
    const prevExit = process.exitCode
    const { io, err } = captureIo()
    await runCli([...ARGV, 'update', 'app.ts'], io)
    expect(err.join('\n')).toContain('not available in the TS CLI yet')
    // The stub sets a non-zero exit code; restore it so it doesn't leak to the
    // test runner's own exit status.
    process.exitCode = prevExit
  })
})
