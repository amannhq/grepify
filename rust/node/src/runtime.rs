//! Runtime plumbing — the napi analogue of `rust/py/src/runtime.rs`.
//!
//! The Grepify engine owns a process-global Tokio runtime
//! (`grepify_core::engine::runtime::get_runtime`). All async napi methods here
//! drive engine work on that shared runtime, so LMDB writes stay funneled
//! through the single-writer batcher regardless of which host (Rust, Python,
//! Node) started them.

use grepify_core::engine::runtime::{cancel_all, get_runtime, reset_global_cancellation};
use napi_derive::napi;

/// Initialize the shared engine runtime. Idempotent — the runtime is a lazy
/// global, so this simply forces it up front (analogous to Python's
/// `init_runtime`, minus the pickle serializer injection which the TS host does
/// not use: JS values are msgpack-encoded on the JS side).
#[napi]
pub fn init_runtime() {
    // Touch the global runtime so it is constructed eagerly.
    let _ = get_runtime();
}

/// Cancel the global cancellation token, causing all in-flight operations to
/// exit promptly. Safe to call from signal handlers (e.g. a Ctrl+C hook).
#[napi]
pub fn cancel_all_js() {
    cancel_all();
}

/// Replace the cancelled global token with a fresh one so new operations can
/// proceed. Call at the start of each CLI command / fresh run.
#[napi]
pub fn reset_global_cancellation_js() {
    reset_global_cancellation();
}
