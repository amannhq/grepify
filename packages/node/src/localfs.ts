// The `localfs` connector — ergonomic TS wrappers over the native `DirTarget`
// (see `rust/node/src/target.rs`). These read the ambient component context so
// callers never pass a `ctx` explicitly, mirroring the Python connector API.

import { DirTarget as NativeDirTarget, mountDirTarget as nativeMountDirTarget } from '../binding.js'
import { currentCtx } from './context.js'

/** A declarative directory target. Declared files are reconciled to disk. */
export class DirTarget {
  constructor(private readonly native: NativeDirTarget) {}

  /** The target directory path. */
  get dir(): string {
    return this.native.dir
  }

  /**
   * Declare that `name` (relative to the target dir) should hold `content`.
   * Uses the current component context, so in a `mountEach` fan-out each child
   * owns the files it declares.
   */
  declareFile(name: string, content: string | Uint8Array): void {
    const buf = typeof content === 'string' ? Buffer.from(content, 'utf8') : Buffer.from(content)
    this.native.declareFile(currentCtx(), name, buf)
  }
}

/**
 * Mount a declarative directory target rooted at `dir`. Must be called inside an
 * `App.update()` pipeline. Mirrors `localfs.declare_dir_target`.
 */
export function mountDirTarget(dir: string): DirTarget {
  return new DirTarget(nativeMountDirTarget(currentCtx(), dir))
}
