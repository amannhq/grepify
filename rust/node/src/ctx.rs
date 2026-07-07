//! The pipeline context bridge — the napi analogue of `python/grepify`'s
//! `ComponentContext` + the mount APIs in `_internal/api.py`.
//!
//! `CtxJs` wraps the Rust SDK [`grepify::Ctx`] and exposes `mount`/`useMount`/
//! `mountEach`/`memo`/`useState` to JavaScript. JS processor functions are
//! bridged into the engine via napi [`ThreadsafeFunction`]s:
//!
//! * Values cross the boundary as msgpack `Buffer`s — the JS host owns
//!   (de)serialization (there is no pickle path, unlike Python).
//! * Each mount hands the JS processor a *child* `CtxJs` plus the item's value
//!   buffer; the processor returns a `Promise<Buffer>` whose resolved bytes
//!   become the component's stored value.
//! * The engine future runs on the shared `get_runtime()`; the napi method
//!   awaits its `JoinHandle`, so LMDB writes stay on the engine runtime.

use futures::future::try_join_all;
use grepify::Ctx;
use grepify_core::engine::runtime::get_runtime;
use grepify_utils::fingerprint::Fingerprint;
use napi::bindgen_prelude::{Buffer, FnArgs, Promise};
use napi::threadsafe_function::ThreadsafeFunction;
use napi::Status;
use napi_derive::napi;

use crate::error::to_napi;

/// A JS processor invoked with `(childCtx, valueBuffer)` returning
/// `Promise<Buffer>`. `CalleeHandled = false`: called with plain args (no
/// Node error-first convention).
pub(crate) type ProcessorTsfn =
    ThreadsafeFunction<FnArgs<(CtxJs, Buffer)>, Promise<Buffer>, FnArgs<(CtxJs, Buffer)>, Status, false>;

/// A JS processor invoked with just `(childCtx)` returning `Promise<Buffer>`.
pub(crate) type CtxProcessorTsfn =
    ThreadsafeFunction<CtxJs, Promise<Buffer>, CtxJs, Status, false>;

/// Invoke a `(ctx, value)` JS processor and await its resolved bytes.
async fn call_processor(
    processor: &ProcessorTsfn,
    child: Ctx,
    value: Vec<u8>,
) -> grepify::error::Result<Vec<u8>> {
    let child = CtxJs { inner: child };
    let promise = processor
        .call_async(FnArgs::from((child, Buffer::from(value))))
        .await
        .map_err(|e| grepify::error::Error::engine(format!("JS processor dispatch failed: {e}")))?;
    let out = promise
        .await
        .map_err(|e| grepify::error::Error::engine(format!("JS processor rejected: {e}")))?;
    Ok(out.to_vec())
}

/// Invoke a `(ctx)` JS processor and await its resolved bytes.
async fn call_ctx_processor(
    processor: &CtxProcessorTsfn,
    child: Ctx,
) -> grepify::error::Result<Vec<u8>> {
    let child = CtxJs { inner: child };
    let promise = processor
        .call_async(child)
        .await
        .map_err(|e| grepify::error::Error::engine(format!("JS processor dispatch failed: {e}")))?;
    let out = promise
        .await
        .map_err(|e| grepify::error::Error::engine(format!("JS processor rejected: {e}")))?;
    Ok(out.to_vec())
}

/// One `(key, value)` item for `mountEach`, with an optional precomputed
/// component-memo key (the TS `fn()` wrapper folds logic hash + input
/// fingerprint into `memoKey`).
#[napi(object)]
pub struct MountItem {
    pub key: String,
    pub value: Buffer,
    /// msgpack bytes of the memo key; when present, an unchanged key lets the
    /// engine skip re-running the whole child component.
    pub memo_key: Option<Buffer>,
}

fn memo_fp(memo_key: &Option<Buffer>) -> Option<Fingerprint> {
    memo_key.as_ref().map(|b| Fingerprint::from_bytes(b))
}

/// Pipeline context handed to JS processors. Wraps `grepify::Ctx`.
#[napi]
pub struct CtxJs {
    pub(crate) inner: Ctx,
}

#[napi]
impl CtxJs {
    /// Whether this context is running inside an active `App.update()` pipeline
    /// (i.e. LMDB memoization / target states are available).
    #[napi]
    pub fn has_pipeline_context(&self) -> bool {
        self.inner.has_pipeline_context()
    }

    /// Mount a single child component under `key` and return its result bytes.
    ///
    /// `memoKey` (msgpack) enables component-level memoization: on an unchanged
    /// key the engine skips running `processor` entirely. Mirrors `useMount`.
    #[napi(
        ts_args_type = "key: string, memoKey: Buffer | undefined | null, processor: (ctx: CtxJs) => Promise<Buffer>",
        ts_return_type = "Promise<Buffer>"
    )]
    pub async fn use_mount(
        &self,
        key: String,
        memo_key: Option<Buffer>,
        processor: CtxProcessorTsfn,
    ) -> napi::Result<Buffer> {
        let ctx = self.inner.clone();
        let fp = memo_fp(&memo_key);
        let processor = std::sync::Arc::new(processor);
        let jh = get_runtime().spawn(async move {
            ctx.__use_mount_fp::<Vec<u8>, _, _>(key, fp, move |child| {
                let processor = processor.clone();
                async move { call_ctx_processor(&processor, child).await }
            })
            .await
        });
        let bytes = jh.await.map_err(to_napi)?.map_err(to_napi)?;
        Ok(Buffer::from(bytes))
    }

    // NOTE: `processor` closures capture an `Arc<Tsfn>`; the `call_*` helpers
    // take `&Arc<Tsfn>` and deref internally.

    /// Mount a named sub-component (child scope) with no memoization. Mirrors
    /// `Ctx::scope` / `componentSubpath` + `mount`.
    #[napi(
        ts_args_type = "key: string, processor: (ctx: CtxJs) => Promise<Buffer>",
        ts_return_type = "Promise<Buffer>"
    )]
    pub async fn scope(&self, key: String, processor: CtxProcessorTsfn) -> napi::Result<Buffer> {
        self.use_mount(key, None, processor).await
    }

    /// Mount one child component per `(key, value)` item, all concurrently, and
    /// return their result bytes in order. Mirrors `mountEach`.
    #[napi(
        ts_args_type = "items: Array<MountItem>, processor: (ctx: CtxJs, value: Buffer) => Promise<Buffer>",
        ts_return_type = "Promise<Buffer[]>"
    )]
    pub async fn mount_each(
        &self,
        items: Vec<MountItem>,
        processor: ProcessorTsfn,
    ) -> napi::Result<Vec<Buffer>> {
        let ctx = self.inner.clone();
        let processor = std::sync::Arc::new(processor);
        let jh = get_runtime().spawn(async move {
            let futs = items.into_iter().map(|item| {
                let ctx = ctx.clone();
                let processor = processor.clone();
                let fp = memo_fp(&item.memo_key);
                let value = item.value.to_vec();
                async move {
                    ctx.__use_mount_fp::<Vec<u8>, _, _>(item.key, fp, move |child| {
                        let processor = processor.clone();
                        let value = value.clone();
                        async move { call_processor(&processor, child, value).await }
                    })
                    .await
                }
            });
            try_join_all(futs).await
        });
        let results = jh.await.map_err(to_napi)?.map_err(to_napi)?;
        Ok(results.into_iter().map(Buffer::from).collect())
    }

    /// Cached computation keyed by `keyBytes` (msgpack). If the key is unchanged
    /// since the last run, returns the cached bytes without invoking
    /// `processor`. Mirrors `Ctx::memo`.
    #[napi(
        ts_args_type = "keyBytes: Buffer, processor: (ctx: CtxJs) => Promise<Buffer>",
        ts_return_type = "Promise<Buffer>"
    )]
    pub async fn memo(&self, key_bytes: Buffer, processor: CtxProcessorTsfn) -> napi::Result<Buffer> {
        let ctx = self.inner.clone();
        let key = key_bytes.to_vec();
        let processor = std::sync::Arc::new(processor);
        let jh = get_runtime().spawn(async move {
            ctx.memo(&key, move |child| {
                let processor = processor.clone();
                async move { call_ctx_processor(&processor, child).await }
            })
            .await
        });
        let bytes = jh.await.map_err(to_napi)?.map_err(to_napi)?;
        Ok(Buffer::from(bytes))
    }

    /// Declare a persistent per-component state initialized to `initialValue`
    /// (msgpack) on first run. Mirrors `useState` / `Ctx::use_state`.
    #[napi]
    pub fn use_state(&self, key: String, initial_value: Buffer) -> napi::Result<StateHandleJs> {
        let handle = self
            .inner
            .use_state::<String, Vec<u8>>(key, initial_value.to_vec())
            .map_err(to_napi)?;
        Ok(StateHandleJs {
            inner: std::sync::Mutex::new(handle),
        })
    }
}

/// Handle to a persistent component state. `get()` returns the current bytes;
/// `set(bytes)` persists new bytes for the next run.
#[napi]
pub struct StateHandleJs {
    inner: std::sync::Mutex<grepify::StateHandle<Vec<u8>>>,
}

#[napi]
impl StateHandleJs {
    /// The current value bytes (msgpack).
    #[napi]
    pub fn get(&self) -> Buffer {
        Buffer::from(self.inner.lock().unwrap().value().clone())
    }

    /// Persist `value` (msgpack) as the state for the next run.
    #[napi]
    pub fn set(&self, value: Buffer) -> napi::Result<()> {
        self.inner
            .lock()
            .unwrap()
            .set(value.to_vec())
            .map_err(to_napi)
    }
}
