// `grepify` TS CLI — DB-backed inspection commands mirroring the read paths of
// `python/grepify/cli.py` (`ls`, `show`). The `update`/`drop` commands require
// loading a user app module (App instances live in the module, not the DB); a
// TS app-module loader is not implemented yet, so those are documented stubs.
//
// `runCli` is exported (and injected with output streams) so it is unit-testable
// without spawning a process; `bin/grepify.mjs` calls it with `process.argv`.

import { Command } from 'commander'
import { Environment } from './app.js'
import * as inspect from './inspect.js'
import type { StablePathDetail } from './inspect.js'

/** Sinks so tests can capture output instead of writing to the real console. */
export interface CliIo {
  out: (line: string) => void
  err: (line: string) => void
}

const defaultIo: CliIo = {
  out: (line) => console.log(line),
  err: (line) => console.error(line),
}

async function openEnv(dbPath: string): Promise<Environment> {
  return Environment.open({ dbPath })
}

async function cmdLs(db: string, io: CliIo): Promise<void> {
  const env = await openEnv(db)
  const names = await inspect.listAppNames(env)
  if (names.length === 0) {
    io.out('No persisted apps found in the database.')
    return
  }
  io.out(`${db}:`)
  for (const name of [...names].sort()) io.out(`  ${name}`)
}

function printOneDetail(detail: StablePathDetail, io: CliIo): void {
  const nodeType = detail.nodeType === 'Component' ? 'component' : 'directory'
  io.out(`  ${detail.path}`)
  io.out(
    `    type:${nodeType} version:${detail.version} processor:${detail.processorName || '-'}`,
  )
  io.out(
    `    has_memoization:${detail.hasMemoization ? 'true' : 'false'} target_state_count:${detail.targetStateCount}`,
  )
  if (detail.targetStateItems.length > 0) {
    io.out('    Target states:')
    for (const item of detail.targetStateItems) {
      const gen = item.providerGeneration
        ? `${item.providerGeneration.providerId}.${item.providerGeneration.providerSchemaVersion}`
        : 'None'
      const states = item.states.map((s) => `${s.version}:${s.state}`).join(', ')
      io.out(`      - path:${item.targetStatePath} key:${item.key}`)
      io.out(
        `        states:${states || '-'} schema_version:${item.providerSchemaVersion} generation:${gen}`,
      )
    }
  }
}

interface ShowOptions {
  db: string
  appName: string
  tree?: boolean
  long?: boolean
  recursive?: boolean
  parents?: boolean
  stablePath?: string
}

async function cmdShow(opts: ShowOptions, io: CliIo): Promise<void> {
  const env = await openEnv(opts.db)
  const paths = await inspect.iterStablePathsByName(env, opts.appName)

  // Targeted query: match the requested display path (or root `/`).
  if (opts.stablePath) {
    const wanted = opts.stablePath
    const pathBytes =
      wanted === '/' ? inspect.rootStablePath() : paths.find((p) => p.path === wanted)?.pathBytes
    if (!pathBytes) {
      io.err(`No stable path matching '${wanted}' in app '${opts.appName}'.`)
      return
    }
    // by-name query needs the env variant.
    const details = await inspect.queryStablePathDetailsByName(env, opts.appName, pathBytes, {
      includeChildren: opts.recursive,
      recursive: opts.recursive,
      includeParents: opts.parents,
    })
    io.out('Stable paths:')
    if (details.length === 0) io.out('  (none)')
    else for (const d of details) printOneDetail(d, io)
    return
  }

  if (paths.length === 0) {
    io.out('Stable paths:')
    io.out('  (none)')
    return
  }

  if (opts.long) {
    io.out('Stable paths:')
    for (const p of paths) {
      const detail = await inspect.getStablePathDetailByName(env, opts.appName, p.pathBytes)
      if (detail) printOneDetail(detail, io)
    }
    return
  }

  if (opts.tree) {
    io.out('Stable paths:')
    for (const p of paths) {
      // Approximate depth from the display form `/"a"/"b"` (leading empty segment).
      const segs = p.path.split('/').filter((s) => s.length > 0)
      const label = segs.length === 0 ? '/' : segs[segs.length - 1]
      const indent = '  '.repeat(Math.max(0, segs.length - 1))
      const suffix = p.nodeType === 'Component' ? ' [component]' : ''
      io.out(`${indent}- ${label}${suffix}`)
    }
    return
  }

  io.out('Stable paths:')
  for (const p of paths) io.out(`  ${p.path}`)
}

/** Build the commander program. `io` sinks make it testable. */
export function buildProgram(io: CliIo = defaultIo): Command {
  const program = new Command()
  program.name('grepify').description('CLI for Grepify (a lightweight code index for any harness)')

  program
    .command('ls')
    .description('List all persisted apps in a database.')
    .requiredOption('--db <path>', 'Path to the LMDB database directory.')
    .action(async (opts: { db: string }) => {
      await cmdLs(opts.db, io)
    })

  program
    .command('show')
    .description("Show an app's stable paths (from its database).")
    .requiredOption('--db <path>', 'Path to the LMDB database directory.')
    .requiredOption('--app-name <name>', 'App name to inspect.')
    .option('--tree', 'Display stable paths as an indented tree.')
    .option('-l, --long', 'Display detailed multi-line info per path.')
    .option('-r, --recursive', 'Show all children recursively (with a stable_path).')
    .option('-p, --parents', 'Show all parent paths (with a stable_path).')
    .argument('[stable_path]', 'A specific stable path (display form) to query.')
    .action(
      async (
        stablePath: string | undefined,
        opts: {
          db: string
          appName: string
          tree?: boolean
          long?: boolean
          recursive?: boolean
          parents?: boolean
        },
      ) => {
        if ((opts.recursive || opts.parents) && !stablePath) {
          io.err('-r/--recursive and -p/--parents require a stable_path argument.')
          return
        }
        await cmdShow({ ...opts, stablePath }, io)
      },
    )

  // Module-dependent commands: App instances live in a user TS module, not the
  // DB, and a TS app-module loader (the analogue of Python's user_app_loader)
  // is not implemented yet.
  const notImplemented = (name: string) =>
    program
      .command(name)
      .description(`(not implemented) ${name} requires loading a user app module.`)
      .allowUnknownOption(true)
      .action(() => {
        io.err(
          `'grepify ${name}' is not available in the TS CLI yet: it needs a TS app-module ` +
            `loader (analogue of Python's user_app_loader). Use the programmatic API ` +
            `(App.update()/App.dropState()) for now.`,
        )
        process.exitCode = 2
      })
  notImplemented('update')
  notImplemented('drop')

  return program
}

/** Parse and run the CLI. `argv` is full argv (including node + script). */
export async function runCli(argv: string[], io: CliIo = defaultIo): Promise<void> {
  const program = buildProgram(io)
  await program.parseAsync(argv)
}
