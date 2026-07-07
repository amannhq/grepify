//! Filesystem walking bindings — the napi analogue of `grepify::fs`.
//!
//! Exposes `walkDir` plus a `FileEntry` class (stable key, relative path,
//! lazy content) and a `PatternFilePathMatcher`-backed options object. Mirrors
//! the SDK `DirWalker`/`FileEntry`/`PatternFilePathMatcher` surface.

use grepify::fs::{walk_dir, DirWalker, FileEntry};
use grepify::{MatchAllFilePathMatcher, PatternFilePathMatcher};
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::error::IntoNapiResult;

/// Options controlling a `walkDir` traversal.
#[napi(object)]
#[derive(Default)]
pub struct WalkOptions {
    /// Recurse into subdirectories (default: false).
    pub recursive: Option<bool>,
    /// Glob patterns to include. When omitted, all files match.
    pub included_patterns: Option<Vec<String>>,
    /// Glob patterns to exclude. Supports gitignore-style `!` negations.
    pub excluded_patterns: Option<Vec<String>>,
}

/// A walked file with a stable key and lazy content access — the napi mirror of
/// `grepify::fs::FileEntry`.
#[napi]
pub struct FileEntryJs {
    inner: FileEntry,
}

#[napi]
impl FileEntryJs {
    /// Stable key for component paths (relative path, forward slashes).
    #[napi(getter)]
    pub fn key(&self) -> String {
        self.inner.key()
    }

    /// Relative path from the walk root (forward slashes).
    #[napi(getter)]
    pub fn relative_path(&self) -> String {
        self.inner.relative_path().to_string_lossy().replace('\\', "/")
    }

    /// Full filesystem path.
    #[napi(getter)]
    pub fn path(&self) -> String {
        self.inner.path().to_string_lossy().into_owned()
    }

    /// File stem (name without extension).
    #[napi(getter)]
    pub fn stem(&self) -> String {
        self.inner.stem().to_string()
    }

    /// Read the file contents as raw bytes.
    #[napi]
    pub fn content(&self) -> napi::Result<Buffer> {
        Ok(Buffer::from(self.inner.content().into_napi()?))
    }

    /// Read the file contents as a UTF-8 string (BOM-aware, lossy decode).
    #[napi]
    pub fn content_str(&self) -> napi::Result<String> {
        self.inner.content_str().into_napi()
    }
}

fn build_walker(path: String, options: Option<WalkOptions>) -> napi::Result<DirWalker> {
    let options = options.unwrap_or_default();
    let mut walker = walk_dir(std::path::PathBuf::from(path));
    if options.recursive.unwrap_or(false) {
        walker = walker.recursive(true);
    }
    let included = options.included_patterns.unwrap_or_default();
    let excluded = options.excluded_patterns.unwrap_or_default();
    if included.is_empty() && excluded.is_empty() {
        walker = walker.path_matcher(MatchAllFilePathMatcher);
    } else {
        let matcher = PatternFilePathMatcher::new(&included, &excluded).into_napi()?;
        walker = walker.path_matcher(matcher);
    }
    Ok(walker)
}

/// Walk a directory and return the matching files (sorted by relative path).
///
/// Mirrors `grepify::fs::walk_dir(path).recursive(..).path_matcher(..)`.
#[napi]
pub fn walk_dir_js(path: String, options: Option<WalkOptions>) -> napi::Result<Vec<FileEntryJs>> {
    let walker = build_walker(path, options)?;
    let files = walker.walk().into_napi()?;
    Ok(files.into_iter().map(|inner| FileEntryJs { inner }).collect())
}
