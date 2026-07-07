//! Live-BigQuery integration test for the `bigquery` table target connector.
//!
//! Skips gracefully unless `BIGQUERY_TEST_PROJECT` and `BIGQUERY_TEST_DATASET`
//! are set. Uses Application Default Credentials (gcloud auth / a
//! `GOOGLE_APPLICATION_CREDENTIALS` service-account key). Run:
//!   BIGQUERY_TEST_PROJECT=my-proj BIGQUERY_TEST_DATASET=grepify_test \
//!     cargo test -p grepify --features bigquery --test bigquery_target
//!
//! Exercises the managed reconcile path: table create, row upsert,
//! skip-unchanged, in-place update, and orphan delete.
#![cfg(feature = "bigquery")]

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use grepify::bigquery::{
    self, BigQueryConfig, BigQueryConnection, BigQueryTableOptions, ColumnDef, TableSchema,
};
use grepify::{ContextKey, Environment, Result};
use serde::Serialize;

static DB: LazyLock<ContextKey<BigQueryConnection>> = LazyLock::new(|| {
    ContextKey::new_with_state("bigquery_test", |c: &BigQueryConnection| {
        c.state_id().to_string()
    })
});

#[derive(Serialize, Clone)]
struct Row {
    id: String,
    name: String,
    value: i64,
}

fn schema() -> TableSchema {
    TableSchema::new(
        [
            ("id", ColumnDef::new("STRING").not_null()),
            ("name", ColumnDef::new("STRING")),
            ("value", ColumnDef::new("INT64")),
        ],
        ["id"],
    )
    .unwrap()
}

#[tokio::test]
async fn bigquery_target_creates_upserts_and_reconciles_when_available() -> Result<()> {
    let (Ok(project), Ok(dataset)) = (
        std::env::var("BIGQUERY_TEST_PROJECT"),
        std::env::var("BIGQUERY_TEST_DATASET"),
    ) else {
        eprintln!(
            "skipping live BigQuery test; set BIGQUERY_TEST_PROJECT and BIGQUERY_TEST_DATASET"
        );
        return Ok(());
    };

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let table = format!("grepify_test_{nonce}");
    let conn = BigQueryConnection::connect(BigQueryConfig::new(project)).await?;
    let tempdir = tempfile::tempdir().unwrap();
    let coco_db = tempdir.path().join(".grepify_db");

    let run = |rows: Vec<Row>| {
        let conn = conn.clone();
        let coco_db = coco_db.clone();
        let table = table.clone();
        let dataset = dataset.clone();
        async move {
            let app = Environment::builder()
                .db_path(&coco_db)
                .provide_key(&DB, conn)
                .build()
                .await
                .unwrap()
                .app("BigQueryTargetTest")
                .await
                .unwrap();
            app.run(move |ctx| {
                let table = table.clone();
                let dataset = dataset.clone();
                let rows = rows.clone();
                async move {
                    let target = bigquery::mount_table_target(
                        &ctx,
                        &DB,
                        &table,
                        schema(),
                        BigQueryTableOptions {
                            dataset,
                            ..Default::default()
                        },
                    )
                    .await?;
                    for r in &rows {
                        target.declare_row(&ctx, r)?;
                    }
                    Ok(())
                }
            })
            .await
            .unwrap();
        }
    };

    let r = |id: &str, name: &str, value: i64| Row {
        id: id.to_string(),
        name: name.to_string(),
        value,
    };

    // create + 3 rows, re-run unchanged, then update one and drop one. Each
    // phase drives real DDL/DML; the test table is nonce-named so parallel /
    // repeated runs don't collide (left in the test dataset for inspection).
    run(vec![
        r("a", "alpha", 1),
        r("b", "beta", 2),
        r("c", "gamma", 3),
    ])
    .await;
    run(vec![
        r("a", "alpha", 1),
        r("b", "beta", 2),
        r("c", "gamma", 3),
    ])
    .await;
    run(vec![r("a", "alpha-updated", 10), r("b", "beta", 2)]).await;

    Ok(())
}
