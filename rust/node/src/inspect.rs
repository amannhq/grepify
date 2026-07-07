//! Read-only inspect bindings — the napi analogue of `rust/py/src/inspect.rs`.
//!
//! These re-expose the engine's LMDB inspection API (`grepify_core::inspect`)
//! through the SDK's public inspect accessors (`App::inspect_*` /
//! `Environment::inspect_*`, added in `rust/sdk/grepify/src/app.rs`).
//!
//! `StablePath`s cross the boundary two ways: a human-readable `path` string
//! (via `Display`) and a `pathBytes` msgpack `Buffer` that round-trips back into
//! the detail/query calls (the JS host holds it opaquely and passes it back).
//! Everything here is read-only.

use grepify_core::inspect::db_inspect::{
    StablePathDetail, StablePathInfo, StablePathNodeType,
};
use grepify_core::state::stable_path::StablePath;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::app::{AppJs, EnvironmentJs};
use crate::error::to_napi;

fn node_type_str(node_type: StablePathNodeType) -> String {
    match node_type {
        StablePathNodeType::Directory => "Directory".to_string(),
        StablePathNodeType::Component => "Component".to_string(),
    }
}

/// msgpack-encode a `StablePath` for opaque round-tripping through JS.
fn encode_path(path: &StablePath) -> napi::Result<Buffer> {
    let bytes = rmp_serde::to_vec(path).map_err(to_napi)?;
    Ok(Buffer::from(bytes))
}

/// Decode a `StablePath` that JS previously received via [`encode_path`].
fn decode_path(bytes: &Buffer) -> napi::Result<StablePath> {
    rmp_serde::from_slice(bytes.as_ref()).map_err(to_napi)
}

/// A stable path plus its node-type. `pathBytes` round-trips into the detail /
/// query functions.
#[napi(object)]
pub struct StablePathInfoJs {
    /// Human-readable path (e.g. `/"process"/"a.md"`).
    pub path: String,
    /// Opaque msgpack encoding of the path; pass back to `getStablePathDetail`.
    pub path_bytes: Buffer,
    /// `"Directory"` or `"Component"`.
    pub node_type: String,
}

fn info_to_js(info: StablePathInfo) -> napi::Result<StablePathInfoJs> {
    Ok(StablePathInfoJs {
        path: info.path.to_string(),
        path_bytes: encode_path(&info.path)?,
        node_type: node_type_str(info.node_type),
    })
}

/// Version + state label for one target-state entry.
#[napi(object)]
pub struct TargetStateVersionJs {
    pub version: i64,
    pub state: String,
}

/// Provider generation info for a target-state item.
#[napi(object)]
pub struct ProviderGenerationJs {
    pub provider_id: i64,
    pub provider_schema_version: i64,
}

/// Per-item summary of a target-state entry at a stable path.
#[napi(object)]
pub struct TargetStateInfoItemSummaryJs {
    pub target_state_path: String,
    /// Human-readable stable key.
    pub key: String,
    /// Opaque msgpack encoding of the key.
    pub key_bytes: Buffer,
    pub states: Vec<TargetStateVersionJs>,
    pub provider_schema_version: i64,
    pub provider_generation: Option<ProviderGenerationJs>,
}

/// Detailed info about a single stable path stored in LMDB.
#[napi(object)]
pub struct StablePathDetailJs {
    pub path: String,
    pub path_bytes: Buffer,
    pub node_type: String,
    pub version: i64,
    pub processor_name: String,
    pub target_state_count: i64,
    pub has_memoization: bool,
    pub target_state_items: Vec<TargetStateInfoItemSummaryJs>,
}

fn detail_to_js(d: StablePathDetail) -> napi::Result<StablePathDetailJs> {
    let target_state_items = d
        .target_state_items
        .into_iter()
        .map(|item| {
            let key_bytes = rmp_serde::to_vec(&item.key).map_err(to_napi)?;
            Ok(TargetStateInfoItemSummaryJs {
                target_state_path: item.target_state_path,
                key: item.key.to_string(),
                key_bytes: Buffer::from(key_bytes),
                states: item
                    .states
                    .into_iter()
                    .map(|s| TargetStateVersionJs {
                        version: s.version as i64,
                        state: s.state,
                    })
                    .collect(),
                provider_schema_version: item.provider_schema_version as i64,
                provider_generation: item.provider_generation.map(|g| ProviderGenerationJs {
                    provider_id: g.provider_id as i64,
                    provider_schema_version: g.provider_schema_version as i64,
                }),
            })
        })
        .collect::<napi::Result<Vec<_>>>()?;
    Ok(StablePathDetailJs {
        path: d.path.to_string(),
        path_bytes: encode_path(&d.path)?,
        node_type: node_type_str(d.node_type),
        version: d.version as i64,
        processor_name: d.processor_name,
        target_state_count: d.target_state_count as i64,
        has_memoization: d.has_memoization,
        target_state_items,
    })
}

// ---------------------------------------------------------------------------
// App-scoped inspect functions
// ---------------------------------------------------------------------------

/// All stable paths for `app`, with node-type metadata (mirrors Python's
/// `iter_stable_paths`, collected eagerly).
#[napi]
pub async fn iter_stable_paths(app: &AppJs) -> napi::Result<Vec<StablePathInfoJs>> {
    let infos = app.inner.inspect_stable_path_infos().await.map_err(to_napi)?;
    infos.into_iter().map(info_to_js).collect()
}

/// Detailed info for one stable path (as `pathBytes` from `iterStablePaths`).
#[napi]
pub async fn get_stable_path_detail(
    app: &AppJs,
    path_bytes: Buffer,
) -> napi::Result<Option<StablePathDetailJs>> {
    let path = decode_path(&path_bytes)?;
    let detail = app
        .inner
        .inspect_stable_path_detail(&path)
        .await
        .map_err(to_napi)?;
    detail.map(detail_to_js).transpose()
}

/// Query detail for a path and (optionally) its children/parents.
#[napi]
pub async fn query_stable_path_details(
    app: &AppJs,
    path_bytes: Buffer,
    include_children: bool,
    recursive: bool,
    include_parents: bool,
) -> napi::Result<Vec<StablePathDetailJs>> {
    let path = decode_path(&path_bytes)?;
    let details = app
        .inner
        .inspect_query_stable_path_details(&path, include_children, recursive, include_parents)
        .await
        .map_err(to_napi)?;
    details.into_iter().map(detail_to_js).collect()
}

/// The msgpack encoding of the LMDB root path (`/`), for querying from the top.
#[napi]
pub fn root_stable_path() -> napi::Result<Buffer> {
    encode_path(&StablePath::root())
}

// ---------------------------------------------------------------------------
// Environment-scoped (by-name) inspect functions
// ---------------------------------------------------------------------------

/// Names of all apps with persisted state in this environment.
#[napi]
pub async fn list_app_names(env: &EnvironmentJs) -> napi::Result<Vec<String>> {
    env.inner.inspect_list_app_names().await.map_err(to_napi)
}

/// Stable paths (with metadata) for an app by name (opens read-only; empty if
/// the app does not exist).
#[napi]
pub async fn iter_stable_paths_by_name(
    env: &EnvironmentJs,
    app_name: String,
) -> napi::Result<Vec<StablePathInfoJs>> {
    let infos = env
        .inner
        .inspect_stable_path_infos_by_name(&app_name)
        .await
        .map_err(to_napi)?;
    infos.into_iter().map(info_to_js).collect()
}

/// Detailed info for one path of an app by name.
#[napi]
pub async fn get_stable_path_detail_by_name(
    env: &EnvironmentJs,
    app_name: String,
    path_bytes: Buffer,
) -> napi::Result<Option<StablePathDetailJs>> {
    let path = decode_path(&path_bytes)?;
    let detail = env
        .inner
        .inspect_stable_path_detail_by_name(&app_name, &path)
        .await
        .map_err(to_napi)?;
    detail.map(detail_to_js).transpose()
}

/// Query detail for a path (and optionally children/parents) of an app by name.
#[napi]
pub async fn query_stable_path_details_by_name(
    env: &EnvironmentJs,
    app_name: String,
    path_bytes: Buffer,
    include_children: bool,
    recursive: bool,
    include_parents: bool,
) -> napi::Result<Vec<StablePathDetailJs>> {
    let path = decode_path(&path_bytes)?;
    let details = env
        .inner
        .inspect_query_stable_path_details_by_name(
            &app_name,
            &path,
            include_children,
            recursive,
            include_parents,
        )
        .await
        .map_err(to_napi)?;
    details.into_iter().map(detail_to_js).collect()
}
