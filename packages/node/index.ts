// Public entry point for @grepify/node.
//
// Stateless ops + native classes come straight from the auto-generated napi
// bindings (`binding.cjs`); the stateful pipeline API (App/Environment, mount,
// fn, context, state) is the TS orchestration layer in `src/`.

// ---- stateless ops + native primitives (Phase 0/1) ----
export {
  version,
  matchCode,
  indexTerms,
  detectCodeLanguageJs as detectCodeLanguage,
  splitTextRecursive,
  walkDirJs as walkDir,
  fingerprintBytes,
  RateLimiter,
  FileEntryJs as FileEntry,
  initRuntime,
  cancelAllJs as cancelAll,
  resetGlobalCancellationJs as resetGlobalCancellation,
} from './binding.js'

export type {
  ChunkJs as Chunk,
  CodeMatchJs as CodeMatch,
  RecursiveChunkConfigJs as RecursiveChunkConfig,
  TextPositionJs as TextPosition,
  WalkOptions,
  ComponentStatsJs as ComponentStats,
  UpdateStatsJs as UpdateStats,
  UpdateHandleJs,
  DropHandleJs,
  CtxJs,
} from './binding.js'

// ---- lifecycle (Phase 2) ----
export { App, Environment } from './src/app.js'
export type { EnvironmentOptions, UpdateOptions } from './src/app.js'

// ---- orchestration + fn + context + state (Phase 3) ----
export { fn } from './src/fn.js'
export type { FnOptions, GrepifyFn } from './src/fn.js'
export {
  useMount,
  mount,
  mountEach,
  map,
  componentSubpath,
  ComponentMountHandle,
} from './src/mount.js'
export { useState, StateHandle } from './src/state.js'
export { ContextKey, ContextProvider, useContext } from './src/context.js'

// ---- inspect (read-only) ----
export * as inspect from './src/inspect.js'
export type { StablePathInfo, StablePathDetail, QueryOptions } from './src/inspect.js'

// ---- CLI (programmatic entry) ----
export { runCli, buildProgram } from './src/cli.js'
export type { CliIo } from './src/cli.js'

// ---- GPU runner (stub; see src/gpu.ts) ----
export * as gpu from './src/gpu.js'

// ---- connectors (Phase 4) ----
export * as localfs from './src/localfs.js'
export * as custom from './src/connectors/custom.js'
export { CustomTarget, mountTarget } from './src/connectors/custom.js'
export type { TargetAction } from './src/connectors/custom.js'
