// GPU runner — documented stub with clear TODOs.
//
// Status: **not implemented**. Python's GPU runner
// (`python/grepify/_internal/runner.py`) is built around a `ProcessPoolExecutor`
// that forks worker subprocesses and pins each to a CUDA device via
// `CUDA_VISIBLE_DEVICES`, dispatching pickled callables into them. That model
// does not map onto the Node host:
//
//   * Node is single-process; there is no fork-based `ProcessPoolExecutor`, and
//     the pipeline runs in-process against the shared Rust engine runtime.
//   * There is no pickle transport (the TS host is msgpack-only), so arbitrary
//     JS closures cannot be shipped to a worker the way Python ships callables.
//   * GPU work in Node is typically delegated to native addons / child services
//     (e.g. an ONNX runtime binding, a local inference server) that manage their
//     own device placement — GPU scheduling belongs in that userland library,
//     not in the harness.
//
// If per-device scheduling is needed later, the likely shape is a `worker_threads`
// pool (structured-clone transport) or an out-of-process inference service with
// a small round-robin scheduler here. Until a concrete need exists, this module
// only records intent and exposes the Python-parity surface as no-ops so callers
// can guard on `currentGpu() === null`.

let configuredGpuCount = 0

/**
 * Record an intended GPU pool size. Advisory only in the TS host (see module
 * docs); it does not spawn workers or pin devices.
 */
export function configureGpuPool(numGpus: number): void {
  if (!Number.isInteger(numGpus) || numGpus < 0) {
    throw new RangeError(`configureGpuPool: numGpus must be a non-negative integer, got ${numGpus}`)
  }
  configuredGpuCount = numGpus
}

/** The configured pool size (0 if never configured). Advisory. */
export function gpuPoolSize(): number {
  return configuredGpuCount
}

/**
 * The GPU device index for the current execution context. Always `null` in the
 * TS host today (no subprocess device pinning) — analogue of Python's
 * `current_gpu()` returning `None` outside a GPU subprocess.
 */
export function currentGpu(): number | null {
  return null
}
