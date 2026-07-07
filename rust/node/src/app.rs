//! App/Environment lifecycle bindings — the napi analogue of
//! `rust/py/src/{environment,app}.rs`, layered over the Rust SDK's
//! `grepify::{Environment, App}` (which are themselves a host over
//! `grepify_core`).
//!
//! The root pipeline function (`appMain`) is a JS async function bridged in via
//! a [`ThreadsafeFunction`]; it receives a root [`CtxJs`] and returns
//! `Promise<Buffer>` (msgpack of its result, or empty).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use grepify::app::{App, Environment, UpdateHandle};
use grepify::UpdateStatus;
use grepify_core::engine::runtime::get_runtime;
use napi::bindgen_prelude::{Buffer, Promise};
use napi::threadsafe_function::ThreadsafeFunction;
use napi::Status;
use napi_derive::napi;

use crate::ctx::CtxJs;
use crate::error::to_napi;

/// A JS root pipeline function: `(ctx) => Promise<Buffer>`.
type AppMainTsfn = ThreadsafeFunction<CtxJs, Promise<Buffer>, CtxJs, Status, false>;

/// LMDB / concurrency settings for an [`EnvironmentJs`].
#[napi(object)]
#[derive(Default, Clone)]
pub struct EnvironmentOptions {
    /// LMDB database directory (default: `./coco_state`).
    pub db_path: Option<String>,
    /// LMDB maximum number of named databases.
    pub lmdb_max_dbs: Option<u32>,
    /// LMDB map size in bytes.
    pub lmdb_map_size: Option<i64>,
    /// Limit the number of concurrently processing components per app.
    pub max_inflight_components: Option<u32>,
}

/// Default LMDB directory, matching the SDK's `EnvironmentBuilder` default.
const DEFAULT_DB_PATH: &str = "./coco_state";

/// Process-global environment cache keyed by db path.
///
/// LMDB forbids opening the same database directory twice in one process
/// ("environment already open in this program"). The Node host is single
/// process and users routinely open the same DB more than once (e.g. an app
/// plus an inspect/CLI read), so opening returns a cached [`Environment`] for a
/// path already open. New LMDB options on a subsequent open of the same path
/// are ignored (the first open wins), mirroring LMDB's own constraint.
fn env_cache() -> &'static Mutex<HashMap<String, Environment>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Environment>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Canonical cache key for a db path (best-effort absolute; falls back to the
/// raw string when the path does not yet exist).
fn cache_key(db_path: &str) -> String {
    std::fs::canonicalize(db_path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| db_path.to_string())
}

fn build_environment(options: Option<EnvironmentOptions>) -> grepify::error::Result<Environment> {
    let options = options.unwrap_or_default();
    let mut builder = Environment::builder();
    if let Some(db_path) = options.db_path {
        builder = builder.db_path(db_path);
    }
    if let Some(v) = options.lmdb_max_dbs {
        builder = builder.lmdb_max_dbs(v);
    }
    if let Some(v) = options.lmdb_map_size {
        builder = builder.lmdb_map_size(v as usize);
    }
    if let Some(v) = options.max_inflight_components {
        builder = builder.max_inflight_components(v as usize);
    }
    builder.build_blocking()
}

/// A Grepify environment: the LMDB store shared by apps built from it.
#[napi]
pub struct EnvironmentJs {
    pub(crate) inner: Environment,
}

#[napi]
impl EnvironmentJs {
    /// Open (or create) an environment at the given `dbPath` with LMDB settings.
    ///
    /// Opening the same db path more than once in a process returns a cached
    /// handle (LMDB forbids a second open); LMDB options on later opens of the
    /// same path are ignored.
    #[napi(factory)]
    pub async fn open(options: Option<EnvironmentOptions>) -> napi::Result<EnvironmentJs> {
        let db_path = options
            .as_ref()
            .and_then(|o| o.db_path.clone())
            .unwrap_or_else(|| DEFAULT_DB_PATH.to_string());

        let inner = get_runtime()
            .spawn_blocking(move || -> grepify::error::Result<Environment> {
                let key = cache_key(&db_path);
                if let Some(env) = env_cache().lock().unwrap().get(&key) {
                    return Ok(env.clone());
                }
                let env = build_environment(options)?;
                // Re-key after build: the directory now exists, so canonicalize.
                let key = cache_key(&db_path);
                env_cache().lock().unwrap().insert(key, env.clone());
                Ok(env)
            })
            .await
            .map_err(to_napi)?
            .map_err(to_napi)?;
        Ok(EnvironmentJs { inner })
    }

    /// Create an app in this environment. Each `name` is its own LMDB namespace.
    #[napi]
    pub async fn app(&self, name: String) -> napi::Result<AppJs> {
        let env = self.inner.clone();
        let app = get_runtime()
            .spawn(async move { env.app(&name).await })
            .await
            .map_err(to_napi)?
            .map_err(to_napi)?;
        Ok(AppJs { inner: app })
    }
}

/// A runnable Grepify app.
#[napi]
pub struct AppJs {
    pub(crate) inner: App,
}

#[napi]
impl AppJs {
    /// Open an app with its own single-app environment at `dbPath`.
    #[napi(factory)]
    pub async fn open(name: String, options: Option<EnvironmentOptions>) -> napi::Result<AppJs> {
        let env = EnvironmentJs::open(options).await?;
        env.app(name).await
    }

    /// The app name (its LMDB namespace).
    #[napi(getter)]
    pub fn name(&self) -> String {
        self.inner.name().to_string()
    }

    /// Start an update, running `appMain` as the root pipeline function.
    /// Returns a handle for awaiting completion / polling stats.
    #[napi(ts_args_type = "appMain: (ctx: CtxJs) => Promise<Buffer>")]
    pub fn start_update(&self, app_main: AppMainTsfn) -> napi::Result<UpdateHandleJs> {
        let app_main = std::sync::Arc::new(app_main);
        let handle = self
            .inner
            .start_update(move |ctx| {
                let app_main = app_main.clone();
                async move {
                    let ctxjs = CtxJs { inner: ctx };
                    let promise = app_main.call_async(ctxjs).await.map_err(|e| {
                        grepify::error::Error::engine(format!("appMain dispatch failed: {e}"))
                    })?;
                    let out = promise.await.map_err(|e| {
                        grepify::error::Error::engine(format!("appMain rejected: {e}"))
                    })?;
                    Ok::<Vec<u8>, grepify::error::Error>(out.to_vec())
                }
            })
            .map_err(to_napi)?;
        Ok(UpdateHandleJs {
            inner: Mutex::new(Some(handle)),
        })
    }

    /// Run `appMain` to completion and return its result bytes. Convenience over
    /// `startUpdate(...).result()`.
    #[napi(
        ts_args_type = "appMain: (ctx: CtxJs) => Promise<Buffer>",
        ts_return_type = "Promise<Buffer>"
    )]
    pub async fn update(&self, app_main: AppMainTsfn) -> napi::Result<Buffer> {
        let handle = self.start_update(app_main)?;
        handle.result().await
    }

    /// Start dropping all persisted app state (LMDB data). Irreversible.
    #[napi]
    pub fn start_drop(&self) -> napi::Result<DropHandleJs> {
        let handle = self.inner.start_drop_state().map_err(to_napi)?;
        Ok(DropHandleJs {
            inner: Mutex::new(Some(handle)),
        })
    }

    /// Drop all persisted app state and await completion.
    #[napi]
    pub async fn drop_state(&self) -> napi::Result<()> {
        self.start_drop()?.result().await
    }
}

/// Per-component statistics snapshot (mirrors Python's `UpdateHandle.stats()`).
#[napi(object)]
pub struct ComponentStatsJs {
    pub num_execution_starts: i64,
    pub num_unchanged: i64,
    pub num_adds: i64,
    pub num_deletes: i64,
    pub num_reprocesses: i64,
    pub num_errors: i64,
}

/// A detailed update-stats snapshot.
#[napi(object)]
pub struct UpdateStatsJs {
    pub ready: bool,
    pub by_component: HashMap<String, ComponentStatsJs>,
}

/// Handle for a running update: await [`result`], poll [`stats`], or wait for
/// the next change via [`changed`].
#[napi]
pub struct UpdateHandleJs {
    inner: Mutex<Option<UpdateHandle<Vec<u8>>>>,
}

#[napi]
impl UpdateHandleJs {
    /// A point-in-time, per-component stats snapshot.
    #[napi]
    pub fn stats(&self) -> napi::Result<UpdateStatsJs> {
        let guard = self.inner.lock().unwrap();
        let handle = guard
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("update handle already consumed"))?;
        Ok(to_update_stats_js(handle.detailed_stats_snapshot()))
    }

    /// Wait for the update to advance; resolves to `true` when it is still
    /// running and `false` once it has terminated (mirrors `watch()`).
    #[napi]
    pub async fn changed(&self) -> napi::Result<bool> {
        // Briefly take the handle out to satisfy the `&mut` requirement, then
        // put it back so `result()` can still consume it afterwards.
        let mut handle = {
            let mut guard = self.inner.lock().unwrap();
            guard
                .take()
                .ok_or_else(|| napi::Error::from_reason("update handle already consumed"))?
        };
        let progress = handle.changed().await.map_err(to_napi);
        *self.inner.lock().unwrap() = Some(handle);
        Ok(!progress?.is_done())
    }

    /// Drive the update to completion and return `appMain`'s result bytes.
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub async fn result(&self) -> napi::Result<Buffer> {
        let handle = {
            let mut guard = self.inner.lock().unwrap();
            guard
                .take()
                .ok_or_else(|| napi::Error::from_reason("update handle already consumed"))?
        };
        let bytes = handle.result().await.map_err(to_napi)?;
        Ok(Buffer::from(bytes))
    }
}

/// Handle for a running drop-state operation.
#[napi]
pub struct DropHandleJs {
    inner: Mutex<Option<grepify::DropHandle>>,
}

#[napi]
impl DropHandleJs {
    /// A point-in-time, per-component stats snapshot.
    #[napi]
    pub fn stats(&self) -> napi::Result<UpdateStatsJs> {
        let guard = self.inner.lock().unwrap();
        let handle = guard
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("drop handle already consumed"))?;
        Ok(to_update_stats_js(handle.detailed_stats_snapshot()))
    }

    /// Drive the drop to completion.
    #[napi]
    pub async fn result(&self) -> napi::Result<()> {
        let handle = {
            let mut guard = self.inner.lock().unwrap();
            guard
                .take()
                .ok_or_else(|| napi::Error::from_reason("drop handle already consumed"))?
        };
        handle.result().await.map_err(to_napi)
    }
}

fn to_update_stats_js(stats: grepify::UpdateStats) -> UpdateStatsJs {
    let by_component = stats
        .by_component
        .into_iter()
        .map(|(name, c)| {
            (
                name,
                ComponentStatsJs {
                    num_execution_starts: c.num_execution_starts as i64,
                    num_unchanged: c.num_unchanged as i64,
                    num_adds: c.num_adds as i64,
                    num_deletes: c.num_deletes as i64,
                    num_reprocesses: c.num_reprocesses as i64,
                    num_errors: c.num_errors as i64,
                },
            )
        })
        .collect();
    UpdateStatsJs {
        ready: matches!(stats.status, UpdateStatus::Ready),
        by_component,
    }
}
