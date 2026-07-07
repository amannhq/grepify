// Mount / orchestration APIs — the TS analogue of the mount functions in
// `python/grepify/_internal/api.py`.
//
// All of these bridge into the native `CtxJs` (see `rust/node/src/ctx.rs`),
// which owns the engine-side mount + memoization. The JS side is responsible
// for: resolving the component subpath, msgpack-encoding values and memo keys,
// and re-establishing the `AsyncLocalStorage` store inside each processor
// (which the engine invokes across the native boundary — see `context.ts`).

import type { CtxJs } from '../binding.js'
import { componentStorage, currentStore, type ComponentStore } from './context.js'
import { isGrepifyFn, type AnyAsyncFn, type GrepifyFn } from './fn.js'
import { decode, encode } from './serde.js'

/** Join parts into a component subpath (forward-slash separated). */
export function componentSubpath(...parts: (string | number)[]): string {
  return parts.map((p) => String(p)).join('/')
}

function fnMeta(processor: AnyAsyncFn | GrepifyFn): {
  name: string
  memo: boolean
  logic: string | undefined
} {
  if (isGrepifyFn(processor)) {
    return { name: processor.grepifyName, memo: processor.grepifyMemo, logic: processor.grepifyLogic }
  }
  return { name: processor.name || 'anonymous', memo: false, logic: undefined }
}

/** Best-effort memo key: msgpack of [logic, args]; `undefined` if not memoized
 *  or the args are not serializable. */
function memoKeyOf(logic: string | undefined, memo: boolean, args: unknown[]): Buffer | undefined {
  if (!memo || logic === undefined) return undefined
  try {
    return encode([logic, args])
  } catch {
    return undefined
  }
}

/**
 * Build a native processor callback that re-establishes the ambient store
 * (captured now, while ALS is active) and runs `body` under a child context.
 */
function makeProcessor(
  parent: ComponentStore,
  body: (...a: any[]) => unknown | Promise<unknown>,
  args: unknown[],
): (childCtx: CtxJs) => Promise<Buffer> {
  return (childCtx: CtxJs) =>
    componentStorage.run({ ctx: childCtx, contextMap: parent.contextMap }, async () => {
      const result = await body(...args)
      return encode(result === undefined ? null : result)
    })
}

function splitSubpath(
  first: string | AnyAsyncFn,
  rest: unknown[],
): { subpath: string | undefined; processor: AnyAsyncFn; args: unknown[] } {
  if (typeof first === 'string') {
    return { subpath: first, processor: rest[0] as AnyAsyncFn, args: rest.slice(1) }
  }
  return { subpath: undefined, processor: first, args: rest }
}

/**
 * Mount a dependent child component and return its (decoded) result.
 * Mirrors `coco.use_mount`.
 */
export async function useMount<R = unknown>(
  subpath: string,
  processor: AnyAsyncFn,
  ...args: unknown[]
): Promise<R>
export async function useMount<R = unknown>(processor: AnyAsyncFn, ...args: unknown[]): Promise<R>
export async function useMount<R = unknown>(
  first: string | AnyAsyncFn,
  ...rest: unknown[]
): Promise<R> {
  const { subpath, processor, args } = splitSubpath(first, rest)
  const parent = currentStore()
  const meta = fnMeta(processor)
  const key = subpath ?? meta.name
  const memoKey = memoKeyOf(meta.logic, meta.memo, args)
  const proc = makeProcessor(parent, processor, args)
  const out = await parent.ctx.useMount(key, memoKey ?? null, proc)
  return decode<R>(out)
}

/**
 * A handle for background-style mounts. `ready()` waits until the underlying
 * component(s) have synced. Mirrors `coco.ComponentMountHandle`.
 */
export class ComponentMountHandle {
  private readonly promise: Promise<unknown>
  private awaited = false
  constructor(promise: Promise<unknown>) {
    this.promise = promise
    // Prevent unhandled-rejection noise before `ready()` is awaited.
    this.promise.catch(() => {})
  }
  async ready(): Promise<void> {
    this.awaited = true
    await this.promise
  }
}

/**
 * Mount a child component without depending on its result. Returns a handle;
 * call `ready()` to await sync. Mirrors `coco.mount`.
 */
export async function mount(
  subpath: string,
  processor: AnyAsyncFn,
  ...args: unknown[]
): Promise<ComponentMountHandle>
export async function mount(processor: AnyAsyncFn, ...args: unknown[]): Promise<ComponentMountHandle>
export async function mount(
  first: string | AnyAsyncFn,
  ...rest: unknown[]
): Promise<ComponentMountHandle> {
  const { subpath, processor, args } = splitSubpath(first, rest)
  const parent = currentStore()
  const meta = fnMeta(processor)
  const key = subpath ?? meta.name
  const memoKey = memoKeyOf(meta.logic, meta.memo, args)
  const proc = makeProcessor(parent, processor, args)
  const promise = parent.ctx.useMount(key, memoKey ?? null, proc)
  return new ComponentMountHandle(promise)
}

type KeyedItems<V> = Iterable<readonly [string, V]> | AsyncIterable<readonly [string, V]>

async function collectItems<V>(items: KeyedItems<V>): Promise<[string, V][]> {
  const out: [string, V][] = []
  if (Symbol.asyncIterator in (items as any)) {
    for await (const [k, v] of items as AsyncIterable<readonly [string, V]>) out.push([k, v])
  } else {
    for (const [k, v] of items as Iterable<readonly [string, V]>) out.push([k, v])
  }
  return out
}

/**
 * Mount one independent child component per `(key, value)` item, concurrently.
 * Mirrors `coco.mount_each`.
 */
export async function mountEach<V>(
  subpath: string,
  processor: AnyAsyncFn,
  items: KeyedItems<V>,
  ...extraArgs: unknown[]
): Promise<ComponentMountHandle>
export async function mountEach<V>(
  processor: AnyAsyncFn,
  items: KeyedItems<V>,
  ...extraArgs: unknown[]
): Promise<ComponentMountHandle>
export async function mountEach<V>(
  first: string | AnyAsyncFn,
  ...rest: unknown[]
): Promise<ComponentMountHandle> {
  let subpath: string | undefined
  let processor: AnyAsyncFn
  let items: KeyedItems<V>
  let extraArgs: unknown[]
  if (typeof first === 'string') {
    subpath = first
    processor = rest[0] as AnyAsyncFn
    items = rest[1] as KeyedItems<V>
    extraArgs = rest.slice(2)
  } else {
    subpath = undefined
    processor = first
    items = rest[0] as KeyedItems<V>
    extraArgs = rest.slice(1)
  }

  const parent = currentStore()
  const meta = fnMeta(processor)
  const base = subpath ?? meta.name
  const collected = await collectItems(items)

  const promise = Promise.all(
    collected.map(([key, value]) => {
      const args = [value, ...extraArgs]
      const memoKey = memoKeyOf(meta.logic, meta.memo, args)
      const proc = makeProcessor(parent, processor, args)
      return parent.ctx.useMount(`${base}/${key}`, memoKey ?? null, proc)
    }),
  )
  return new ComponentMountHandle(promise)
}

/**
 * Run `fn` concurrently over each item within the *current* component (no child
 * components created). Mirrors `coco.map`.
 */
export async function map<T, R>(
  fn: (item: T, ...args: any[]) => Promise<R> | R,
  items: Iterable<T> | AsyncIterable<T>,
  ...args: unknown[]
): Promise<R[]> {
  const collected: T[] = []
  if (Symbol.asyncIterator in (items as any)) {
    for await (const item of items as AsyncIterable<T>) collected.push(item)
  } else {
    for (const item of items as Iterable<T>) collected.push(item)
  }
  return Promise.all(collected.map((item) => fn(item, ...args)))
}
