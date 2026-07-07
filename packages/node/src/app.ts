// Environment / App lifecycle — the ergonomic TS layer over the native
// `EnvironmentJs` / `AppJs` (see `rust/node/src/app.rs`).
//
// `App.update(main)` wraps the user's root function so it runs inside the
// pipeline's `AsyncLocalStorage` store (seeding context) and msgpack-encodes its
// result for transport back across the native boundary.

import {
  AppJs,
  EnvironmentJs,
  type CtxJs,
  type EnvironmentOptions,
  type UpdateHandleJs,
} from '../binding.js'
import { componentStorage } from './context.js'
import type { ContextProvider } from './context.js'
import type { AnyAsyncFn } from './fn.js'
import { decode, encode } from './serde.js'

export type { EnvironmentOptions } from '../binding.js'

/** Options for a single `App.update` / `App.startUpdate` run. */
export interface UpdateOptions {
  /** Context values made available via `useContext` during the run. */
  context?: ContextProvider
  /** Positional arguments passed to the root `main` function. */
  args?: unknown[]
}

function rootProcessor(
  main: AnyAsyncFn,
  opts: UpdateOptions,
): (ctx: CtxJs) => Promise<Buffer> {
  const contextMap = opts.context?.snapshot() ?? new Map<symbol, unknown>()
  const args = opts.args ?? []
  return (rootCtx: CtxJs) =>
    componentStorage.run({ ctx: rootCtx, contextMap }, async () => {
      const result = await main(...args)
      return encode(result === undefined ? null : result)
    })
}

/** A runnable Grepify app. Wraps the native `AppJs`. */
export class App {
  constructor(readonly native: AppJs) {}

  static async open(name: string, options?: EnvironmentOptions): Promise<App> {
    return new App(await AppJs.open(name, options))
  }

  get name(): string {
    return this.native.name
  }

  /** Run `main` to completion and return its (decoded) result. */
  async update<R = unknown>(main: AnyAsyncFn, opts: UpdateOptions = {}): Promise<R> {
    const buf = await this.native.update(rootProcessor(main, opts))
    return decode<R>(buf)
  }

  /**
   * Start an update and return the native handle for polling `stats()` /
   * awaiting `result()`. The raw handle exposes msgpack `Buffer` results.
   */
  startUpdate(main: AnyAsyncFn, opts: UpdateOptions = {}): UpdateHandleJs {
    return this.native.startUpdate(rootProcessor(main, opts))
  }

  /** Delete all persisted state for this app. Irreversible. */
  async dropState(): Promise<void> {
    await this.native.dropState()
  }
}

/** A Grepify environment (an LMDB store shared by apps). Wraps `EnvironmentJs`. */
export class Environment {
  constructor(readonly native: EnvironmentJs) {}

  static async open(options?: EnvironmentOptions): Promise<Environment> {
    return new Environment(await EnvironmentJs.open(options))
  }

  async app(name: string): Promise<App> {
    return new App(await this.native.app(name))
  }
}
