//! Azure Blob Storage object source connector.
//!
//! Read-only listing and reading of Azure blobs, mirroring Python's
//! `grepify.connectors.azure_blob`:
//!
//! - [`AzureBlobClient`] — a clone-cheap connection handle wrapping Microsoft's
//!   GA [`azure_storage_blob::BlobContainerClient`]. Build one from a container
//!   URL + an Entra ID credential ([`AzureBlobClient::new`]) or from an account
//!   + container name using [Azure developer tooling
//!   credentials][azure_identity::DeveloperToolsCredential]
//!   ([`AzureBlobClient::connect`]).
//! - [`list_blobs`] returns an [`AzureBlobWalker`]; [`AzureBlobWalker::list`] /
//!   [`AzureBlobWalker::items`] enumerate matching blobs as [`AzureBlobFile`]s.
//!   Use each [`AzureBlobFile::key`] with `Ctx::mount_each` so per-file
//!   memoization handles edits and target reconciliation removes derived rows
//!   for deleted blobs.
//! - [`AzureBlobClient::get_blob`] fetches a single blob's metadata; reads go
//!   through [`AzureBlobFile::read`] / [`AzureBlobFile::read_text`].
//!
//! Like the S3 source, [`AzureBlobFile`] serializes only stable metadata for
//! memo keys. The blob's ETag is used as the content fingerprint.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use azure_core::http::Url;
use azure_storage_blob::clients::{BlobContainerClient, BlobContainerClientOptions};
use azure_storage_blob::models::{
    BlobClientGetPropertiesResultHeaders, BlobContainerClientListBlobsOptions,
};
use futures::TryStreamExt;
use grepify_utils::fingerprint::Fingerprint;
use serde::{Deserialize, Serialize};

/// Re-export of the upstream [`azure_storage_blob`] crate. The container and its
/// blobs are user-managed (the source is read-only), so callers use this —
/// together with [`AzureBlobClient::container_client`] — to create/manage
/// containers and upload blobs without depending on `azure_storage_blob`
/// directly.
pub use azure_storage_blob;

use crate::error::{Error, Result};
use crate::file::{
    FileContentCache, FileLike, FileMetadata, FilePath, FilePathMatcher, FileSourceItem,
    MatchAllFilePathMatcher,
};

// ---------------------------------------------------------------------------
// AzureBlobFilePath / AzureBlobFile — source items
// ---------------------------------------------------------------------------

/// Path of an Azure blob: the account, container, the path relative to the
/// walker prefix, and the full blob name. Mirrors Python's `AzureBlobFilePath`;
/// its memo key includes the account + container so the same relative path in
/// two containers stays distinct.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AzureBlobFilePath {
    account_name: String,
    container_name: String,
    /// Path relative to the walker prefix (forward slashes), or the full blob
    /// name if no prefix was used. The user-facing display path / `mount_each` key.
    relative_path: String,
    /// Full blob name (what [`resolve`](Self::resolve) returns).
    blob_name: String,
}

impl AzureBlobFilePath {
    /// The storage account name.
    pub fn account_name(&self) -> &str {
        &self.account_name
    }

    /// The container name.
    pub fn container_name(&self) -> &str {
        &self.container_name
    }

    /// Path relative to the walker prefix (forward slashes).
    pub fn path(&self) -> &str {
        &self.relative_path
    }

    /// The full blob name.
    pub fn resolve(&self) -> &str {
        &self.blob_name
    }

    /// Memo key: `(account, container, relative_path)`. Stable across accounts /
    /// containers and decoupled from the live connection (matches Python's
    /// `__coco_memo_key__`).
    pub fn memo_key(&self) -> impl Serialize + '_ {
        (&self.account_name, &self.container_name, &self.relative_path)
    }
}

/// A blob discovered in or fetched from an Azure container.
///
/// Files returned by [`AzureBlobWalker`] carry a clone-cheap client handle so
/// they can be read through the shared async [`FileLike`] API.
#[derive(Clone, Serialize, Deserialize)]
pub struct AzureBlobFile {
    file_path: AzureBlobFilePath,
    /// Blob size in bytes.
    pub size: u64,
    /// Last-modified time as Unix seconds (`None` if the server omitted it).
    pub modified_secs: Option<i64>,
    /// Blob ETag (content fingerprint), if returned.
    pub etag: Option<String>,
    #[serde(skip)]
    client: Option<AzureBlobClient>,
    #[serde(skip, default = "default_file_cache")]
    cache: Arc<FileContentCache>,
}

impl AzureBlobFile {
    fn new(
        client: Option<AzureBlobClient>,
        file_path: AzureBlobFilePath,
        size: u64,
        modified_secs: Option<i64>,
        etag: Option<String>,
    ) -> Self {
        let metadata = FileMetadata {
            size,
            modified: modified_time(modified_secs),
            content_fingerprint: etag_fingerprint(etag.as_deref()),
        };
        Self {
            file_path,
            size,
            modified_secs,
            etag,
            client,
            cache: Arc::new(FileContentCache::with_metadata(metadata)),
        }
    }

    /// Attach a client so the file can be read (used after deserialization).
    pub fn with_client(mut self, client: AzureBlobClient) -> Self {
        self.client = Some(client);
        self
    }

    /// Stable key for `Ctx::mount_each`: the blob's path relative to the walker
    /// prefix. Unique within a container+prefix.
    pub fn key(&self) -> String {
        self.file_path.relative_path.clone()
    }

    /// The blob's [`AzureBlobFilePath`].
    pub fn file_path(&self) -> &AzureBlobFilePath {
        &self.file_path
    }

    pub async fn read(&self) -> Result<Vec<u8>> {
        <Self as FileLike>::read(self).await
    }

    pub async fn read_size(&self, size: usize) -> Result<Vec<u8>> {
        <Self as FileLike>::read_size(self, size).await
    }

    pub async fn read_text(&self) -> Result<String> {
        <Self as FileLike>::read_text(self).await
    }
}

impl std::fmt::Debug for AzureBlobFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AzureBlobFile")
            .field("file_path", &self.file_path)
            .field("size", &self.size)
            .field("modified_secs", &self.modified_secs)
            .field("etag", &self.etag)
            .finish()
    }
}

impl PartialEq for AzureBlobFile {
    fn eq(&self, other: &Self) -> bool {
        self.file_path == other.file_path
            && self.size == other.size
            && self.modified_secs == other.modified_secs
            && self.etag == other.etag
    }
}

impl Eq for AzureBlobFile {}

#[async_trait]
impl FileLike for AzureBlobFile {
    fn file_path(&self) -> FilePath {
        FilePath::with_base_dir(
            format!(
                "azure://{}/{}",
                self.file_path.account_name, self.file_path.container_name
            ),
            PathBuf::new(),
            &self.file_path.relative_path,
        )
    }

    fn cache(&self) -> &FileContentCache {
        &self.cache
    }

    async fn fetch_metadata(&self) -> Result<FileMetadata> {
        Ok(FileMetadata {
            size: self.size,
            modified: modified_time(self.modified_secs),
            content_fingerprint: etag_fingerprint(self.etag.as_deref()),
        })
    }

    async fn read_impl(&self, size: Option<usize>) -> Result<Vec<u8>> {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| Error::engine("Azure blob file is not attached to an AzureBlobClient"))?;
        match size {
            Some(0) => Ok(Vec::new()),
            // The Azure SDK download always fetches the whole blob; take a prefix
            // to honor a bounded read (matches Python's `download_blob(length=)`
            // truncation behavior closely enough for source sampling).
            Some(size) => {
                let bytes = client.read_blob(&self.file_path.blob_name).await?;
                Ok(bytes.into_iter().take(size).collect())
            }
            None => client.read_blob(&self.file_path.blob_name).await,
        }
    }
}

impl FileSourceItem for AzureBlobFile {}

fn default_file_cache() -> Arc<FileContentCache> {
    Arc::new(FileContentCache::new())
}

fn modified_time(secs: Option<i64>) -> SystemTime {
    match secs {
        Some(secs) if secs >= 0 => UNIX_EPOCH + Duration::from_secs(secs as u64),
        _ => UNIX_EPOCH,
    }
}

fn etag_fingerprint(etag: Option<&str>) -> Option<Fingerprint> {
    etag.and_then(|etag| Fingerprint::from(&etag).ok())
}

// ---------------------------------------------------------------------------
// AzureBlobClient — connection handle (Clone-cheap)
// ---------------------------------------------------------------------------

/// An Azure Blob Storage connection scoped to a single container. Clone-cheap
/// (the underlying client is shared).
#[derive(Clone)]
pub struct AzureBlobClient {
    inner: Arc<BlobContainerClient>,
    account_name: Arc<str>,
    container_name: Arc<str>,
    state_id: Arc<str>,
}

impl AzureBlobClient {
    /// Build a client from a full container URL (for example
    /// `https://myaccount.blob.core.windows.net/mycontainer`) and an optional
    /// Entra ID [`TokenCredential`]. The account and container names are derived
    /// from the URL.
    pub fn new(
        container_url: &str,
        credential: Option<Arc<dyn TokenCredential>>,
    ) -> Result<Self> {
        let url = Url::parse(container_url)
            .map_err(|e| Error::engine(format!("invalid Azure container URL {container_url:?}: {e}")))?;
        let (account_name, container_name) = identity_from_url(&url).ok_or_else(|| {
            Error::engine(format!(
                "Azure container URL {container_url:?} must contain an account host and a container path"
            ))
        })?;
        let options: Option<BlobContainerClientOptions> = None;
        let inner = BlobContainerClient::new(url, credential, options)
            .map_err(|e| Error::engine(format!("azure blob container client: {e}")))?;
        let state_id = format!("azure-blob:{account_name}/{container_name}");
        Ok(Self {
            inner: Arc::new(inner),
            account_name: Arc::from(account_name),
            container_name: Arc::from(container_name),
            state_id: Arc::from(state_id),
        })
    }

    /// Build a client for `account`/`container` authenticating through the local
    /// Azure developer tooling (Azure CLI / Azure Developer CLI / environment) —
    /// the Rust analogue of Python constructing a `ContainerClient` from the
    /// default credential chain.
    pub fn connect(account: &str, container: &str) -> Result<Self> {
        let credential = azure_identity::DeveloperToolsCredential::new(None)
            .map_err(|e| Error::engine(format!("azure developer credential: {e}")))?;
        let url = format!("https://{account}.blob.core.windows.net/{container}");
        Self::new(&url, Some(credential as Arc<dyn TokenCredential>))
    }

    /// Wrap an already-built [`BlobContainerClient`] (advanced use / tests).
    pub fn from_container_client(client: BlobContainerClient) -> Result<Self> {
        let url = client.url().clone();
        let (account_name, container_name) = identity_from_url(&url).ok_or_else(|| {
            Error::engine("Azure container client URL must contain an account host and container path")
        })?;
        let state_id = format!("azure-blob:{account_name}/{container_name}");
        Ok(Self {
            inner: Arc::new(client),
            account_name: Arc::from(account_name),
            container_name: Arc::from(container_name),
            state_id: Arc::from(state_id),
        })
    }

    /// The underlying [`BlobContainerClient`].
    pub fn container_client(&self) -> &BlobContainerClient {
        &self.inner
    }

    /// The storage account name.
    pub fn account_name(&self) -> &str {
        &self.account_name
    }

    /// The container name.
    pub fn container_name(&self) -> &str {
        &self.container_name
    }

    /// Stable identity (for use as a `ContextKey` state id / memo dependency).
    pub fn state_id(&self) -> &str {
        &self.state_id
    }

    /// Fetch a single blob's metadata as an [`AzureBlobFile`] (via
    /// `get_properties`). Its relative path equals the full blob name.
    pub async fn get_blob(&self, blob_name: &str) -> Result<AzureBlobFile> {
        let blob_client = self.inner.blob_client(blob_name);
        let props = blob_client
            .get_properties(None)
            .await
            .map_err(|e| Error::engine(format!("azure get_properties {blob_name}: {e}")))?;
        let size = props
            .content_length()
            .ok()
            .flatten()
            .unwrap_or(0);
        let modified_secs = props.last_modified().ok().flatten().map(|d| d.unix_timestamp());
        let etag = props.etag().ok().flatten().map(|e| e.to_string());
        Ok(AzureBlobFile::new(
            Some(self.clone()),
            AzureBlobFilePath {
                account_name: self.account_name.to_string(),
                container_name: self.container_name.to_string(),
                relative_path: blob_name.to_string(),
                blob_name: blob_name.to_string(),
            },
            size,
            modified_secs,
            etag,
        ))
    }

    /// Read a blob's full content.
    pub async fn read_blob(&self, blob_name: &str) -> Result<Vec<u8>> {
        let blob_client = self.inner.blob_client(blob_name);
        let response = blob_client
            .download(None)
            .await
            .map_err(|e| Error::engine(format!("azure download {blob_name}: {e}")))?;
        let bytes = response
            .body
            .collect()
            .await
            .map_err(|e| Error::engine(format!("azure read body {blob_name}: {e}")))?;
        Ok(bytes.to_vec())
    }
}

/// Derive `(account_name, container_name)` from a blob-service URL such as
/// `https://account.blob.core.windows.net/container[/...]`.
fn identity_from_url(url: &Url) -> Option<(String, String)> {
    let host = url.host_str()?;
    let account = host.split('.').next().filter(|s| !s.is_empty())?;
    let container = url
        .path_segments()?
        .find(|seg| !seg.is_empty())?;
    Some((account.to_string(), container.to_string()))
}

// ---------------------------------------------------------------------------
// AzureBlobWalker — list a container/prefix into AzureBlobFiles
// ---------------------------------------------------------------------------

/// Options for [`list_blobs`].
#[derive(Default)]
pub struct ListOptions {
    /// Only list blobs whose name starts with this prefix.
    pub prefix: String,
    /// Filter by relative path (after prefix stripping). Defaults to match-all.
    pub path_matcher: Option<Arc<dyn FilePathMatcher>>,
    /// Skip blobs larger than this many bytes.
    pub max_file_size: Option<u64>,
}

/// Lists blobs in an Azure container as [`AzureBlobFile`]s. Build with
/// [`list_blobs`].
pub struct AzureBlobWalker {
    client: AzureBlobClient,
    prefix: String,
    path_matcher: Arc<dyn FilePathMatcher>,
    max_file_size: Option<u64>,
}

/// List blobs in an Azure container. Returns an [`AzureBlobWalker`]; call
/// [`AzureBlobWalker::list`] or [`AzureBlobWalker::items`] to enumerate matching
/// blobs.
pub fn list_blobs(client: &AzureBlobClient, options: ListOptions) -> AzureBlobWalker {
    AzureBlobWalker {
        client: client.clone(),
        prefix: options.prefix,
        path_matcher: options
            .path_matcher
            .unwrap_or_else(|| Arc::new(MatchAllFilePathMatcher)),
        max_file_size: options.max_file_size,
    }
}

impl AzureBlobWalker {
    /// List all matching blobs (paginating through the container), skipping
    /// directory markers, applying the prefix, path matcher, and max-size filter.
    pub async fn list(&self) -> Result<Vec<AzureBlobFile>> {
        let mut out = Vec::new();
        let mut options = BlobContainerClientListBlobsOptions::default();
        if !self.prefix.is_empty() {
            options.prefix = Some(self.prefix.clone());
        }
        // Azure's `Pager` for List Blobs flattens pages into individual
        // `BlobItem`s, so each `try_next()` yields one blob.
        let mut pager = self
            .client
            .inner
            .list_blobs(Some(options))
            .map_err(|e| Error::engine(format!("azure list_blobs: {e}")))?;

        while let Some(item) = pager
            .try_next()
            .await
            .map_err(|e| Error::engine(format!("azure list_blobs page: {e}")))?
        {
            let Some(blob_name) = item.name else { continue };
            let Some(relative_key) = relative_key(&self.prefix, &blob_name) else {
                continue;
            };
            if !self
                .path_matcher
                .is_file_included(&PathBuf::from(&relative_key))
            {
                continue;
            }
            let props = item.properties.unwrap_or_default();
            let size = props.content_length.unwrap_or(0);
            if self.max_file_size.is_some_and(|max| size > max) {
                continue;
            }
            let modified_secs = props.last_modified.map(|d| d.unix_timestamp());
            let etag = props.etag.map(|e| e.to_string());
            out.push(AzureBlobFile::new(
                Some(self.client.clone()),
                AzureBlobFilePath {
                    account_name: self.client.account_name.to_string(),
                    container_name: self.client.container_name.to_string(),
                    relative_path: relative_key,
                    blob_name,
                },
                size,
                modified_secs,
                etag,
            ));
        }
        Ok(out)
    }

    /// Like [`list`](Self::list) but returns `(key, file)` pairs where `key` is
    /// the blob's relative path — ready to hand to `Ctx::mount_each`.
    pub async fn items(&self) -> Result<Vec<(String, AzureBlobFile)>> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .map(|f| (f.key(), f))
            .collect())
    }
}

/// Compute a blob's path relative to `prefix`. Returns `None` for directory
/// markers (names ending in `/`) and for the prefix itself (empty relative path).
fn relative_key(prefix: &str, blob_name: &str) -> Option<String> {
    if blob_name.ends_with('/') {
        return None;
    }
    let relative = if prefix.is_empty() {
        blob_name
    } else {
        blob_name.strip_prefix(prefix)?.trim_start_matches('/')
    };
    if relative.is_empty() {
        None
    } else {
        Some(relative.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::PatternFilePathMatcher;

    #[test]
    fn relative_key_strips_prefix_and_skips_markers() {
        assert_eq!(relative_key("", "a/b.md").as_deref(), Some("a/b.md"));
        assert_eq!(
            relative_key("data/", "data/a/b.md").as_deref(),
            Some("a/b.md")
        );
        assert_eq!(relative_key("data", "data/a/b.md").as_deref(), Some("a/b.md"));
        assert_eq!(relative_key("", "a/"), None);
        assert_eq!(relative_key("data/", "data/"), None);
        assert_eq!(relative_key("data/x", "data/x"), None);
        assert_eq!(relative_key("data/", "other/a.md"), None);
    }

    #[test]
    fn identity_parses_account_and_container() {
        let url = Url::parse("https://acct.blob.core.windows.net/mycontainer/sub/x.txt").unwrap();
        assert_eq!(
            identity_from_url(&url),
            Some(("acct".to_string(), "mycontainer".to_string()))
        );
    }

    #[tokio::test]
    async fn azure_blob_file_implements_shared_filelike_metadata_and_fingerprint() {
        let file = AzureBlobFile::new(
            None,
            AzureBlobFilePath {
                account_name: "acct".to_string(),
                container_name: "c".to_string(),
                relative_path: "docs/a.md".to_string(),
                blob_name: "prefix/docs/a.md".to_string(),
            },
            42,
            Some(123),
            Some("\"etag-1\"".to_string()),
        );
        assert_eq!(
            FileLike::file_path(&file).path(),
            std::path::Path::new("docs/a.md")
        );
        assert_eq!(FileSourceItem::key(&file), "docs/a.md");
        assert_eq!(FileLike::metadata(&file).await.unwrap().size, 42);
        assert!(
            FileLike::read(&file)
                .await
                .unwrap_err()
                .to_string()
                .contains("not attached")
        );
    }

    #[test]
    fn memo_key_differs_across_containers_same_path() {
        let mk = |container: &str| {
            let fp = AzureBlobFilePath {
                account_name: "acct".to_string(),
                container_name: container.to_string(),
                relative_path: "a.md".to_string(),
                blob_name: "a.md".to_string(),
            };
            serde_json::to_value(fp.memo_key()).unwrap()
        };
        assert_ne!(mk("c1"), mk("c2"));
    }

    #[test]
    fn pattern_matcher_applies_to_relative_path() {
        let matcher = PatternFilePathMatcher::new(["**/*.md"], ["**/skip/**"]).unwrap();
        assert!(matcher.is_file_included(&PathBuf::from("a/b.md")));
        assert!(!matcher.is_file_included(&PathBuf::from("a/b.txt")));
        assert!(!matcher.is_file_included(&PathBuf::from("skip/b.md")));
    }
}
