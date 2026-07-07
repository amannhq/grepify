// The `fn()` wrapper — the TS analogue of `python/grepify/_internal/function.py`
// (`@coco.fn`). It attaches memoization metadata and a *logic fingerprint* to a
// user function so mounts can skip re-execution when neither the code nor the
// inputs changed.
//
// The logic fingerprint is derived from `Function.prototype.toString()` (plus an
// optional explicit `version`), analogous to Python's AST-based hash. It is
// folded into each mount's memo key so editing a processor invalidates its
// cached results.

import { createHash } from 'node:crypto'

const FN_MARKER = Symbol('grepify.fn')

export type AnyAsyncFn = (...args: any[]) => unknown | Promise<unknown>

export interface FnOptions {
  /** Enable component-level memoization (skip re-run on unchanged inputs). */
  memo?: boolean
  /**
   * Explicit version. When set, it replaces the source-derived hash, so bumping
   * it forces re-execution even if the code text is unchanged.
   */
  version?: number
  /**
   * External dependency values folded into the logic fingerprint (mirrors
   * Python's `deps=`). Changing these invalidates memoized results.
   */
  deps?: unknown
}

/** A user function wrapped by {@link fn}, carrying memo metadata. */
export interface GrepifyFn<F extends AnyAsyncFn = AnyAsyncFn> {
  (...args: Parameters<F>): ReturnType<F>
  readonly [FN_MARKER]: true
  readonly grepifyName: string
  readonly grepifyMemo: boolean
  readonly grepifyLogic: string
}

function computeLogic(target: AnyAsyncFn, options: FnOptions): string {
  const h = createHash('sha256')
  const name = target.name || 'anonymous'
  h.update(name)
  h.update('\0')
  if (options.version !== undefined) {
    h.update(`<version>${options.version}`)
  } else {
    h.update(target.toString())
  }
  if (options.deps !== undefined) {
    h.update('\0<deps>')
    h.update(JSON.stringify(options.deps ?? null))
  }
  return h.digest('hex')
}

/**
 * Wrap a function as a Grepify processor. The returned value is directly
 * callable (running the underlying function), but also carries metadata read by
 * `mount` / `useMount` / `mountEach`.
 *
 * @example
 *   const processFile = fn(async (file: FileEntry, target: DirTarget) => { ... }, { memo: true })
 *   await useMount(processFile, file, target)
 */
export function fn<F extends AnyAsyncFn>(target: F, options: FnOptions = {}): GrepifyFn<F> {
  const wrapped = ((...args: Parameters<F>) => target(...args)) as GrepifyFn<F>
  Object.defineProperties(wrapped, {
    [FN_MARKER]: { value: true, enumerable: false },
    grepifyName: { value: target.name || 'anonymous', enumerable: false },
    grepifyMemo: { value: options.memo ?? false, enumerable: false },
    grepifyLogic: { value: computeLogic(target, options), enumerable: false },
  })
  return wrapped
}

export function isGrepifyFn(value: unknown): value is GrepifyFn {
  return typeof value === 'function' && (value as Partial<GrepifyFn>)[FN_MARKER] === true
}
