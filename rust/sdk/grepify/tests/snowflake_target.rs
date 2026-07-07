//! Live-Snowflake integration test for the `snowflake` table target connector.
//!
//! Skips gracefully unless `SNOWFLAKE_ACCOUNT`, `SNOWFLAKE_USER`, and
//! `SNOWFLAKE_PASSWORD` are set (optionally `SNOWFLAKE_WAREHOUSE`,
//! `SNOWFLAKE_DATABASE`, `SNOWFLAKE_SCHEMA`). Run:
//!   SNOWFLAKE_ACCOUNT=... SNOWFLAKE_USER=... SNOWFLAKE_PASSWORD=... \
//!   SNOWFLAKE_DATABASE=GREPIFY_TEST SNOWFLAKE_SCHEMA=PUBLIC \
//!     cargo test -p grepify --features snowflake --test snowflake_target
//!
//! Exercises the managed reconcile path: table create, row upsert,
//! skip-unchanged, in-place update, and orphan delete.
#![cfg(feature = "snowflake")]

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use grepify::snowflake::{
    self, ColumnDef, SnowflakeConfig, SnowflakeConnection, SnowflakeTableOptions, TableSchema,
};
use grepify::{ContextKey, Environment, Result};
use serde::Serialize;

static DB: LazyLock<ContextKey<SnowflakeConnection>> = LazyLock::new(|| {
    ContextKey::new_with_state("snowflake_test", |c: &SnowflakeConnection| {
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
            ("id", ColumnDef::new("VARCHAR").not_null()),
            ("name", ColumnDef::new("VARCHAR")),
            ("value", ColumnDef::new("NUMBER")),
        ],
        ["id"],
    )
    .unwrap()
}

fn try_connect() -> Option<SnowflakeConnection> {
    let (account, user, password) = (
        std::env::var("SNOWFLAKE_ACCOUNT")
            .ok()
            .filter(|s| !s.is_empty())?,
        std::env::var("SNOWFLAKE_USER")
            .ok()
            .filter(|s| !s.is_empty())?,
        std::env::var("SNOWFLAKE_PASSWORD")
            .ok()
            .filter(|s| !s.is_empty())?,
    );
    let mut config = SnowflakeConfig::new(account, user, password);
    if let Ok(wh) = std::env::var("SNOWFLAKE_WAREHOUSE") {
        config = config.warehouse(wh);
    }
    if let Ok(role) = std::env::var("SNOWFLAKE_ROLE") {
        config = config.role(role);
    }
    match SnowflakeConnection::connect(config) {
        Ok(conn) => Some(conn),
        Err(e) => {
            eprintln!("skipping live Snowflake test; connect failed: {e}");
            None
        }
    }
}

#[tokio::test]
async fn snowflake_target_creates_upserts_and_reconciles_when_available() -> Result<()> {
    let Some(conn) = try_connect() else {
        eprintln!(
            "skipping live Snowflake test; set SNOWFLAKE_ACCOUNT / SNOWFLAKE_USER / SNOWFLAKE_PASSWORD"
        );
        return Ok(());
    };
    let database = std::env::var("SNOWFLAKE_DATABASE").ok();
    let schema_name = std::env::var("SNOWFLAKE_SCHEMA").ok();

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let table = format!("GREPIFY_TEST_{nonce}");
    let tempdir = tempfile::tempdir().unwrap();
    let coco_db = tempdir.path().join(".grepify_db");

    let run = |rows: Vec<Row>| {
        let conn = conn.clone();
        let coco_db = coco_db.clone();
        let table = table.clone();
        let database = database.clone();
        let schema_name = schema_name.clone();
        async move {
            let app = Environment::builder()
                .db_path(&coco_db)
                .provide_key(&DB, conn)
                .build()
                .await
                .unwrap()
                .app("SnowflakeTargetTest")
                .await
                .unwrap();
            app.run(move |ctx| {
                let table = table.clone();
                let database = database.clone();
                let schema_name = schema_name.clone();
                let rows = rows.clone();
                async move {
                    let target = snowflake::mount_table_target(
                        &ctx,
                        &DB,
                        &table,
                        schema(),
                        SnowflakeTableOptions {
                            database,
                            schema: schema_name,
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

    // create + 3 rows, re-run unchanged, then update one and drop one.
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
