//! Live components — compiling stub with the design + blocker writeup.
//!
//! Status: **not yet implemented** for the Node host. This module intentionally
//! exposes no napi surface yet; it documents what a full port requires and the
//! concrete napi lifetime/threading blockers, so the rest of the crate builds
//! while this remains the last milestone (as flagged in the plan).
//!
//! # What a full port needs (mirroring `rust/py/src/live_component.rs` +
//! `python/grepify/_internal/live_component.py`)
//!
//! * `LiveComponentController` bridge over
//!   `grepify_core::engine::live_component::LiveComponentController<RustProfile>`
//!   (or the SDK's higher-level `LiveComponent`/`LiveComponentOperator` in
//!   `rust/sdk/grepify/src/live_component.rs`), exposing `update`, `updateFull`,
//!   `delete`, `markReady`, `mountInnerLive`, `readCommittedState`,
//!   `writeCommittedState`, and `start(processLiveFuture)`.
//! * `LiveMap` / `LiveMapFeed` / `LiveMapView` over the SDK's
//!   `resources::live_map` + `live_component` feed traits, so a long-running JS
//!   `processLive` can push keyed updates that the engine reconciles.
//! * `autoRefresh` scheduling and live `walkDir` (`fs::DirWalker::live`, behind
//!   the SDK `fs_live` feature → adds the `notify` dependency).
//!
//! # napi blockers (the reason this is deferred, not merely unfinished)
//!
//! 1. **Long-lived JS future → Rust future.** `LiveComponentController::start`
//!    takes a *Rust* `Future` driven on the engine runtime. Python converts a
//!    Python coroutine via `pyo3_async_runtimes::from_py_future`. napi has no
//!    direct analogue: a JS `Promise` is tied to the Node event loop, so the
//!    engine cannot `.await` it on a Tokio worker without a bridge that (a)
//!    polls the JS microtask queue and (b) survives for the (unbounded) lifetime
//!    of the live component. This needs a bespoke `JsFuture` adapter around a
//!    `ThreadsafeFunction` + a `oneshot`/`watch` completion channel, with
//!    careful `Send`/`'static` handling of the captured JS callables.
//! 2. **Cancellation across the boundary.** Live components run until cancelled
//!    (`cancelAll`, Ctrl+C, or `delete`). The JS `processLive` must observe
//!    cancellation and unwind; a dropped `ThreadsafeFunction` must not leave the
//!    engine awaiting a `Promise` that will never settle. This requires wiring
//!    the engine's cancellation token to abort the JS-future adapter.
//! 3. **Re-entrant mounts from `processLive`.** `mountInnerLive` hands back a
//!    controller the JS side must immediately drive with a nested
//!    `processLive` — a callback returning a controller that starts another
//!    callback. Modeling this cycle without leaking `ThreadsafeFunction`s or
//!    dead-locking the single engine runtime is the crux of the difficulty.
//!
//! The non-live pipeline (mount/useMount/mountEach/map/memo/useState),
//! inspect, and target reconcile are fully implemented and unaffected by this
//! gap; a live update simply cannot be started from Node yet. `App.update`'s
//! `UpdateOptions.live` flag is deliberately not surfaced until the above is in
//! place.
