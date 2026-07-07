#!/usr/bin/env node
// Launcher for the `grepify` TS CLI. Runs against the compiled package output
// (`dist/src/cli.js`), produced by the package build. During local development
// (source-only, no build), run the CLI via the programmatic `runCli` export or
// through a TS runner (e.g. `tsx src/cli.ts`).
import { runCli } from '../dist/src/cli.js'

runCli(process.argv).catch((err) => {
  console.error(err?.stack ?? String(err))
  process.exit(1)
})
