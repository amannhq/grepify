// Persistent per-component state — the TS analogue of `coco.use_state` /
// `StateHandle` in `python/grepify/_internal/api.py`. Values are msgpack-encoded
// on write and decoded on read.

import { currentCtx } from './context.js'
import { decode, encode } from './serde.js'

/** A handle to a component's persistent state slot. */
export class StateHandle<T> {
  constructor(private readonly inner: { get(): Buffer; set(value: Buffer): void }) {}

  /** The current value (decoded from the stored bytes). */
  get value(): T {
    return decode<T>(this.inner.get())
  }

  /** Persist a new value for the next run. */
  set value(next: T) {
    this.inner.set(encode(next))
  }
}

/**
 * Declare a persistent state for the current component, initialized to
 * `initialValue` on first run. Mirrors `coco.use_state`.
 */
export function useState<T>(key: string, initialValue: T): StateHandle<T> {
  const inner = currentCtx().useState(key, encode(initialValue))
  return new StateHandle<T>(inner)
}
