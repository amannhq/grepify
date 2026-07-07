//! Target-state connector bindings.
//!
//! This is the napi surface over the Rust SDK's built-in connectors. It starts
//! with the self-contained `localfs` [`DirTarget`] (no external services), which
//! exercises the full declare → reconcile → cleanup path end-to-end.
//!
//! Both `mount` and `declare_file` take a [`CtxJs`] explicitly: the provider is
//! registered under the component that mounts it, while each file is declared
//! under whichever component context is current when `declareFile` runs (so, in
//! a `mountEach` fan-out, each child owns the files it declares). The TS layer
//! passes the ambient context automatically (see `src/localfs.ts`).
//!
//! For fully custom targets implemented in TypeScript, see `js_target.rs`
//! (`mountJsTarget`) — the generic keyed-row bridge whose async `applyActions`
//! callback lets a TS user apply reconciled upserts/deletes to any backend
//! (e.g. `better-sqlite3`, an HTTP API) without a Rust rewrite.
//!
//! TODO(phase 4): feature-gated `#[napi]` wrappers for the built-in Rust SDK
//! connectors (`sqlite`, `zvec`, `postgres`, `lancedb`, `qdrant`, …). Those
//! follow the same shape as `DirTargetJs` — a `*TargetJs` handle plus a
//! `mount_*` function — but every one of them (including the self-contained
//! `sqlite`/`zvec`) resolves its connection from a `ContextKey<Conn>` provided
//! into the *environment* `ContextStore` at build time (see
//! `rust/sdk/grepify/src/{sqlite,zvec}.rs` and `design_connectors.md` §5.5).
//!
//! Wiring that from the Node host has two coupling points not yet solved here:
//!   1. `EnvironmentBuilder::provide_key` is builder-only (`&mut`), so the typed
//!      Rust connection (`sqlite::Database`, `zvec::ManagedConnection`, a pool)
//!      must be opened and provided during `EnvironmentJs::open` — the lifecycle
//!      API would gain per-connector provisioning fields.
//!   2. `ContextKey::new(name)` is process-unique (panics on a duplicate name),
//!      so the key created at provision time must be *stored* and reused at
//!      `mount_*` time (a node-side registry), not reconstructed.
//! Neither is hard, but both couple `app.rs`/`EnvironmentOptions` to specific
//! connectors and pull heavy deps (`sqlx`, the bundled `zvec` native lib) into
//! `grepify_node`; deferred so the default binding stays lean and fast to build.

use grepify::fs::DirTarget;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::ctx::CtxJs;
use crate::error::to_napi;

/// A declarative directory target. Files you `declareFile` are reconciled
/// against the previous run: new/changed files written, unchanged skipped,
/// orphaned files deleted.
#[napi(js_name = "DirTarget")]
pub struct DirTargetJs {
    inner: DirTarget,
}

#[napi]
impl DirTargetJs {
    /// The target directory path.
    #[napi(getter)]
    pub fn dir(&self) -> String {
        self.inner.dir().to_string_lossy().to_string()
    }

    /// Declare that `name` (a relative path under the target dir) should hold
    /// `content`. The write/skip/delete is decided by the engine at reconcile.
    #[napi]
    pub fn declare_file(&self, ctx: &CtxJs, name: String, content: Buffer) -> napi::Result<()> {
        self.inner
            .declare_file(&ctx.inner, &name, content.as_ref())
            .map_err(to_napi)
    }
}

/// Mount a declarative directory target rooted at `dir`. Must be called inside
/// an `App.update()` pipeline (uses the current component context).
#[napi]
pub fn mount_dir_target(ctx: &CtxJs, dir: String) -> napi::Result<DirTargetJs> {
    let inner = grepify::fs::mount_dir_target(&ctx.inner, dir).map_err(to_napi)?;
    Ok(DirTargetJs { inner })
}
