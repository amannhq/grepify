//! Custom JS target bridge — lets users implement a target connector in
//! TypeScript by supplying only an async `applyActions` callback.
//!
//! This is the napi analogue of `rust/py/src/target_state.rs`, but with one
//! deliberate design difference driven by a napi threading constraint:
//!
//! * In Python, `TargetHandler.reconcile` is called *synchronously* on the
//!   engine thread and dispatches straight into the interpreter under the GIL.
//! * napi has no synchronous "call JS from an arbitrary thread" primitive — the
//!   only bridge is [`ThreadsafeFunction`], which is inherently async (it queues
//!   onto the Node event loop). Blocking an engine worker thread on a JS promise
//!   from inside the *synchronous* `reconcile` is deadlock-prone and fragile.
//!
//! So the reconcile/diff logic runs generically in Rust (identical create /
//! update / delete detection as the built-in `localfs` `DirTarget`), and JS only
//! implements the *async* side effect: `applyActions(batch)`. This covers the
//! common custom-target shape (a keyed row store whose rows are upserted /
//! deleted) without exposing the sync-reconcile hazard. A fully custom JS
//! `reconcile` is intentionally not supported yet — see the module docs / report.

use std::sync::Arc;

use grepify::target_state::{
    StableKey, TargetAction, TargetActionSink, TargetHandler, TargetReconcileOutput,
    TargetStateProvider, declare_target_state, register_root_target_states_provider,
};
use grepify_utils::fingerprint::Fingerprint;
use napi::Status;
use napi::bindgen_prelude::{Buffer, Promise};
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;
use serde::{Deserialize, Serialize};

use crate::ctx::CtxJs;
use crate::error::to_napi;

/// One reconciled change handed to the JS `applyActions` callback. `value` is
/// present for an upsert (create/update) and `null` for a delete.
#[napi(object)]
pub struct JsTargetAction {
    /// `"upsert"` or `"delete"`.
    pub kind: String,
    /// The declared row key (the raw string for a string key; otherwise the
    /// engine's `StableKey` display form).
    pub key: String,
    /// The declared row value (msgpack `Buffer`), or `null` for a delete.
    pub value: Option<Buffer>,
}

/// The JS side effect: apply a batch of reconciled changes, returning a Promise.
type ApplyTsfn =
    ThreadsafeFunction<Vec<JsTargetAction>, Promise<()>, Vec<JsTargetAction>, Status, false>;

/// The row value declared from JS: opaque msgpack bytes plus a friendly key
/// string. Its byte content is fingerprinted to detect changes across runs.
#[derive(Clone, Serialize, Deserialize)]
struct JsRowValue {
    key: String,
    bytes: Vec<u8>,
}

/// What the sink applies for one row: upsert `value`, or delete when `None`.
#[derive(Serialize, Deserialize)]
struct RowAction {
    key: String,
    value: Option<Vec<u8>>,
}

/// Generic keyed-row handler. `reconcile` is pure Rust (no JS); the async sink
/// dispatches the batch to the JS `applyActions` callback.
struct JsRowHandler {
    apply: Arc<ApplyTsfn>,
}

impl JsRowHandler {
    fn sink(&self) -> TargetActionSink<RowAction> {
        let apply = self.apply.clone();
        TargetActionSink::from_async_fn(move |actions: Vec<TargetAction<RowAction>>| {
            let apply = apply.clone();
            async move {
                let items: Vec<JsTargetAction> = actions
                    .into_iter()
                    .map(|action| {
                        let row = match action {
                            TargetAction::Create(r)
                            | TargetAction::Update(r)
                            | TargetAction::Delete(r) => r,
                        };
                        let (kind, value) = match row.value {
                            Some(bytes) => ("upsert".to_string(), Some(Buffer::from(bytes))),
                            None => ("delete".to_string(), None),
                        };
                        JsTargetAction {
                            kind,
                            key: row.key,
                            value,
                        }
                    })
                    .collect();
                let promise = apply.call_async(items).await.map_err(|e| {
                    grepify::error::Error::engine(format!("applyActions dispatch failed: {e}"))
                })?;
                promise.await.map_err(|e| {
                    grepify::error::Error::engine(format!("applyActions rejected: {e}"))
                })?;
                Ok(())
            }
        })
    }
}

impl TargetHandler<JsRowValue> for JsRowHandler {
    type TrackingRecord = Fingerprint;
    type Action = RowAction;

    fn reconcile(
        &self,
        _key: StableKey,
        desired: Option<JsRowValue>,
        prev: Vec<Fingerprint>,
        prev_may_be_missing: bool,
    ) -> grepify::error::Result<Option<TargetReconcileOutput<RowAction, Fingerprint>>> {
        let desired_fp = match &desired {
            Some(v) => Some(Fingerprint::from(&v.bytes).map_err(grepify::error::Error::from)?),
            None => None,
        };
        // Skip when nothing changed (identical to the localfs DirTarget logic).
        let prev_same = desired_fp
            .as_ref()
            .is_some_and(|fp| prev.iter().any(|p| p == fp));
        if desired.is_some() && prev_same && !prev_may_be_missing {
            return Ok(None);
        }
        if desired.is_none() && prev.is_empty() && !prev_may_be_missing {
            return Ok(None);
        }
        let (key, value) = match desired {
            Some(v) => (v.key, Some(v.bytes)),
            // On delete, recover the key from the engine's StableKey.
            None => (stable_key_display(&_key), None),
        };
        Ok(Some(TargetReconcileOutput {
            action: TargetAction::Update(RowAction { key, value }),
            sink: self.sink(),
            tracking_record: desired_fp,
            child_invalidation: None,
        }))
    }
}

fn stable_key_display(key: &StableKey) -> String {
    match key {
        StableKey::Str(s) => s.to_string(),
        other => other.to_string(),
    }
}

/// A handle to a custom JS target: declare rows on it, then let the engine
/// reconcile them via your `applyActions` callback.
#[napi(js_name = "JsTarget")]
pub struct JsTargetJs {
    provider: TargetStateProvider<JsRowValue>,
}

#[napi]
impl JsTargetJs {
    /// Declare that row `key` should hold `value` (msgpack `Buffer`). The engine
    /// decides upsert/skip/delete during reconciliation and batches the results
    /// into your `applyActions` callback. Rows no longer declared are deleted.
    #[napi]
    pub fn declare_row(&self, ctx: &CtxJs, key: String, value: Buffer) -> napi::Result<()> {
        let row = JsRowValue {
            key: key.clone(),
            bytes: value.to_vec(),
        };
        declare_target_state(&ctx.inner, self.provider.target_state(key, row)).map_err(to_napi)
    }
}

/// Mount a custom JS target rooted at the unique `name`. `applyActions` is
/// invoked (async) with each reconciled batch of upserts/deletes. Must be called
/// inside an `App.update()` pipeline.
#[napi(
    ts_args_type = "ctx: CtxJs, name: string, applyActions: (actions: Array<JsTargetAction>) => Promise<void>"
)]
pub fn mount_js_target(
    ctx: &CtxJs,
    name: String,
    apply_actions: ApplyTsfn,
) -> napi::Result<JsTargetJs> {
    let handler = JsRowHandler {
        apply: Arc::new(apply_actions),
    };
    let provider =
        register_root_target_states_provider(&ctx.inner, name, handler).map_err(to_napi)?;
    Ok(JsTargetJs { provider })
}
