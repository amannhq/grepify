mod app;
mod chunk;
mod code;
mod ctx;
mod error;
mod fingerprint;
mod fs;
mod inspect;
mod js_target;
mod live;
mod ratelimit;
mod runtime;
mod target;
mod text;

use napi_derive::napi;

#[napi]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

pub use code::{index_terms, match_code};
pub use fingerprint::fingerprint_bytes;
pub use fs::walk_dir_js;
pub use inspect::{
    get_stable_path_detail, get_stable_path_detail_by_name, iter_stable_paths,
    iter_stable_paths_by_name, list_app_names, query_stable_path_details,
    query_stable_path_details_by_name, root_stable_path,
};
pub use js_target::{mount_js_target, JsTargetJs};
pub use runtime::{cancel_all_js, init_runtime, reset_global_cancellation_js};
pub use target::{mount_dir_target, DirTargetJs};
pub use text::{detect_code_language_js, split_text_recursive};
