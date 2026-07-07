//! Live-Azure integration test for the `azure_blob` source connector.
//!
//! Skips gracefully unless `AZURE_BLOB_TEST_CONTAINER_URL` is set (a full
//! container URL such as `https://acct.blob.core.windows.net/mycontainer`).
//! Authentication uses the local Azure developer tooling (az login / env). Run:
//!   AZURE_BLOB_TEST_CONTAINER_URL=https://acct.blob.core.windows.net/data \
//!     cargo test -p grepify --features azure_blob --test azure_blob_source
#![cfg(feature = "azure_blob")]

use grepify::azure_blob::{self, AzureBlobClient, ListOptions};
use grepify::Result;

/// Build a client from `AZURE_BLOB_TEST_CONTAINER_URL`, or `None` to skip.
fn try_client() -> Option<AzureBlobClient> {
    let url = std::env::var("AZURE_BLOB_TEST_CONTAINER_URL")
        .ok()
        .filter(|s| !s.is_empty())?;
    // Uses the Azure developer credential chain (az CLI / azd / env).
    match AzureBlobClient::new(&url, None) {
        Ok(client) => Some(client),
        Err(e) => {
            eprintln!("skipping live Azure blob test; client build failed: {e}");
            None
        }
    }
}

#[tokio::test]
async fn azure_blob_source_lists_and_reads_when_available() -> Result<()> {
    let Some(client) = try_client() else {
        eprintln!("skipping live Azure blob test; AZURE_BLOB_TEST_CONTAINER_URL is not set");
        return Ok(());
    };

    // List the whole container; directory markers are skipped by the walker.
    let files = azure_blob::list_blobs(&client, ListOptions::default())
        .list()
        .await?;
    eprintln!(
        "listed {} blobs from {}/{}",
        files.len(),
        client.account_name(),
        client.container_name()
    );

    // If any blob exists, read the first one back through the shared FileLike API.
    if let Some(first) = files.first() {
        let bytes = first.read().await?;
        assert_eq!(
            bytes.len() as u64,
            first.size,
            "read byte count matches listed size"
        );
        // Fetching a single blob by name via get_blob round-trips its metadata.
        let fetched = client.get_blob(first.file_path().resolve()).await?;
        assert_eq!(fetched.size, first.size);
    }

    Ok(())
}
