// Custom target connector — implement a target in pure TypeScript by supplying
// an async `applyActions` callback. Wraps the native `JsTarget` (see
// `rust/node/src/js_target.rs`).
//
// Reconciliation (create/update/delete detection) runs generically in Rust; you
// only implement the side effect. Each declared row is reconciled against the
// previous run: unchanged rows are skipped, changed rows produce an `upsert`,
// and rows no longer declared produce a `delete`. See the module docs in
// `js_target.rs` for why a fully custom (synchronous) `reconcile` is not exposed.

import { JsTarget as NativeJsTarget, mountJsTarget as nativeMountJsTarget } from '../../binding.js'
import { currentCtx } from '../context.js'
import { decode, encode } from '../serde.js'

/** A reconciled change delivered to {@link TargetOptions.applyActions}. */
export interface TargetAction<V = unknown> {
  /** `"upsert"` (create or update) or `"delete"`. */
  kind: 'upsert' | 'delete'
  /** The row key that was declared. */
  key: string
  /** The decoded row value for an upsert, or `null` for a delete. */
  value: V | null
}

/** A custom target handle: declare rows, and the engine reconciles them. */
export class CustomTarget {
  constructor(private readonly native: NativeJsTarget) {}

  /**
   * Declare that row `key` should hold `value`. `value` is msgpack-encoded for
   * transport. The engine decides upsert/skip/delete during reconciliation.
   */
  declareRow(key: string, value: unknown): void {
    this.native.declareRow(currentCtx(), key, encode(value))
  }
}

/**
 * Mount a custom target rooted at the unique `name`. `applyActions` is called
 * (async) with each reconciled batch. Must be called inside an `App.update()`
 * pipeline.
 */
export function mountTarget<V = unknown>(
  name: string,
  applyActions: (actions: TargetAction<V>[]) => Promise<void>,
): CustomTarget {
  const native = nativeMountJsTarget(currentCtx(), name, async (raw) => {
    const actions: TargetAction<V>[] = raw.map((a) => ({
      kind: a.kind === 'delete' ? 'delete' : 'upsert',
      key: a.key,
      value: a.value == null ? null : decode<V>(a.value),
    }))
    await applyActions(actions)
  })
  return new CustomTarget(native)
}
