// Read-only inspect API — ergonomic TS wrappers over the native inspect
// bindings (see `rust/node/src/inspect.rs`). Mirrors Python's inspect helpers.
//
// Stable paths are identified by an opaque `pathBytes` (msgpack of the engine's
// `StablePath`). You get them from `iterStablePaths`/`iterStablePathsByName` and
// pass them back into `getStablePathDetail`/`queryStablePathDetails`. Use
// `rootStablePath()` to start a query from the LMDB root (`/`).

import {
  getStablePathDetail as nativeGetDetail,
  getStablePathDetailByName as nativeGetDetailByName,
  iterStablePaths as nativeIterPaths,
  iterStablePathsByName as nativeIterPathsByName,
  listAppNames as nativeListAppNames,
  queryStablePathDetails as nativeQueryDetails,
  queryStablePathDetailsByName as nativeQueryDetailsByName,
  rootStablePath as nativeRootStablePath,
  type StablePathDetailJs,
  type StablePathInfoJs,
} from '../binding.js'
import type { App, Environment } from './app.js'

export type StablePathInfo = StablePathInfoJs
export type StablePathDetail = StablePathDetailJs

/** The msgpack encoding of the LMDB root path (`/`). */
export function rootStablePath(): Buffer {
  return nativeRootStablePath()
}

/** Options for {@link queryStablePathDetails}. */
export interface QueryOptions {
  includeChildren?: boolean
  recursive?: boolean
  includeParents?: boolean
}

// --- App-scoped ---

/** All stable paths for `app`, with node-type metadata. */
export function iterStablePaths(app: App): Promise<StablePathInfo[]> {
  return nativeIterPaths(app.native)
}

/** Detailed info for one stable path (its `pathBytes` from {@link iterStablePaths}). */
export function getStablePathDetail(
  app: App,
  pathBytes: Buffer,
): Promise<StablePathDetail | null> {
  return nativeGetDetail(app.native, pathBytes)
}

/** Query detail for a path and (optionally) its children/parents. */
export function queryStablePathDetails(
  app: App,
  pathBytes: Buffer,
  opts: QueryOptions = {},
): Promise<StablePathDetail[]> {
  return nativeQueryDetails(
    app.native,
    pathBytes,
    opts.includeChildren ?? false,
    opts.recursive ?? false,
    opts.includeParents ?? false,
  )
}

// --- Environment-scoped (by name) ---

/** Names of all apps with persisted state in `env`. */
export function listAppNames(env: Environment): Promise<string[]> {
  return nativeListAppNames(env.native)
}

/** Stable paths (with metadata) for an app by name; empty if it doesn't exist. */
export function iterStablePathsByName(
  env: Environment,
  appName: string,
): Promise<StablePathInfo[]> {
  return nativeIterPathsByName(env.native, appName)
}

/** Detailed info for one path of an app by name. */
export function getStablePathDetailByName(
  env: Environment,
  appName: string,
  pathBytes: Buffer,
): Promise<StablePathDetail | null> {
  return nativeGetDetailByName(env.native, appName, pathBytes)
}

/** Query detail for a path (and optionally children/parents) of an app by name. */
export function queryStablePathDetailsByName(
  env: Environment,
  appName: string,
  pathBytes: Buffer,
  opts: QueryOptions = {},
): Promise<StablePathDetail[]> {
  return nativeQueryDetailsByName(
    env.native,
    appName,
    pathBytes,
    opts.includeChildren ?? false,
    opts.recursive ?? false,
    opts.includeParents ?? false,
  )
}
