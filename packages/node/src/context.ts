// Component context propagation — the TS analogue of
// `python/grepify/_internal/component_ctx.py` + `context_keys.py`.
//
// A pipeline runs across the JS <-> Rust boundary: the engine invokes each
// component's JS processor via a napi ThreadsafeFunction, which starts a *fresh*
// async context, so `AsyncLocalStorage` does NOT propagate automatically into a
// processor. We work around this by capturing the parent store synchronously
// when a mount closure is created (while ALS is still active) and re-entering
// it with `als.run(...)` inside the processor (see `mount.ts`).

import { AsyncLocalStorage } from 'node:async_hooks'
import type { CtxJs } from '../binding.js'

/** The ambient state carried through a pipeline run. */
export interface ComponentStore {
  /** The current component's native context (root or child). */
  ctx: CtxJs
  /** Context values keyed by `ContextKey`, inherited by all descendants. */
  contextMap: Map<symbol, unknown>
}

export const componentStorage = new AsyncLocalStorage<ComponentStore>()

/** Get the current component store, throwing if called outside a pipeline. */
export function currentStore(): ComponentStore {
  const store = componentStorage.getStore()
  if (store === undefined) {
    throw new Error(
      'No active component context. This API must be called inside an ' +
        'App.update(main) pipeline (mount / useMount / useState / useContext).',
    )
  }
  return store
}

/** The native context for the current component. */
export function currentCtx(): CtxJs {
  return currentStore().ctx
}

/**
 * A typed key for a context value provided via {@link ContextProvider} and read
 * via {@link useContext}. Mirrors Python's `ContextKey`.
 */
export class ContextKey<T> {
  /** Phantom marker so `T` is used (keeps the type parameter meaningful). */
  declare readonly __type: T
  readonly symbol: symbol
  constructor(name: string) {
    this.symbol = Symbol(name)
  }
}

/**
 * A mutable bag of context values, built before an update and seeded into the
 * pipeline's root store. Mirrors Python's `ContextProvider`.
 */
export class ContextProvider {
  private readonly values = new Map<symbol, unknown>()

  provide<T>(key: ContextKey<T>, value: T): this {
    this.values.set(key.symbol, value)
    return this
  }

  /** Snapshot the provided values into a fresh map for a run. */
  snapshot(): Map<symbol, unknown> {
    return new Map(this.values)
  }
}

/**
 * Read a context value provided for the current run. Throws if the key was
 * never provided. Mirrors Python's `use_context`.
 */
export function useContext<T>(key: ContextKey<T>): T {
  const store = currentStore()
  if (!store.contextMap.has(key.symbol)) {
    throw new Error(
      `Context value for key was not provided. Provide it via a ContextProvider ` +
        `passed to App.update(main, { context }).`,
    )
  }
  return store.contextMap.get(key.symbol) as T
}
