//! End-to-end integration test for the `zvec` embedded vector-collection target.
//!
//! zvec runs in-process against an on-disk directory, so this test needs no
//! external service and always runs:
//!   cargo test -p grepify --features zvec --test zvec_target
//!
//! Exercises the full managed reconcile path over the public target API:
//! collection create, document upsert, skip-unchanged, in-place update,
//! orphan delete, and schema-change collection recreate.
#![cfg(feature = "zvec")]

use std::sync::LazyLock;

use grepify::zvec::{
    self, CollectionSchema, ManagedConnection, ScalarType, VectorField, ZvecCollectionOptions,
};
use grepify::{ContextKey, Environment, Result};
use serde::Serialize;

static DB: LazyLock<ContextKey<ManagedConnection>> = LazyLock::new(|| {
    ContextKey::new_with_state("zvec_test", |c: &ManagedConnection| c.state_id().to_string())
});

#[derive(Serialize, Clone)]
struct Row {
    id: String,
    title: String,
    embedding: Vec<f32>,
}

fn schema() -> CollectionSchema {
    CollectionSchema::builder("id")
        .scalar("title", ScalarType::String)
        .dense_vector("embedding", VectorField::new(3))
        .build()
        .unwrap()
}

fn row(id: &str, title: &str, v: [f32; 3]) -> Row {
    Row {
        id: id.to_string(),
        title: title.to_string(),
        embedding: v.to_vec(),
    }
}

#[tokio::test]
async fn zvec_target_creates_upserts_and_reconciles() -> Result<()> {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = zvec::connect(tempdir.path().join("collections"), true)?;
    let coco_db = tempdir.path().join(".grepify_db");
    let collection = "docs".to_string();

    let run = |rows: Vec<Row>| {
        let conn = conn.clone();
        let coco_db = coco_db.clone();
        let collection = collection.clone();
        async move {
            let app = Environment::builder()
                .db_path(&coco_db)
                .provide_key(&DB, conn)
                .build()
                .await
                .unwrap()
                .app("ZvecTargetTest")
                .await
                .unwrap();
            app.run(move |ctx| {
                let collection = collection.clone();
                let rows = rows.clone();
                async move {
                    let target = zvec::mount_collection_target(
                        &ctx,
                        &DB,
                        &collection,
                        schema(),
                        ZvecCollectionOptions::default(),
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

    // --- Run 1: create collection + 3 documents ---
    run(vec![
        row("a", "alpha", [1.0, 0.0, 0.0]),
        row("b", "beta", [0.0, 1.0, 0.0]),
        row("c", "gamma", [0.0, 0.0, 1.0]),
    ])
    .await;
    assert_eq!(conn.document_count(&collection)?, 3, "3 documents created");

    // --- Run 2: unchanged → still 3 documents ---
    run(vec![
        row("a", "alpha", [1.0, 0.0, 0.0]),
        row("b", "beta", [0.0, 1.0, 0.0]),
        row("c", "gamma", [0.0, 0.0, 1.0]),
    ])
    .await;
    assert_eq!(conn.document_count(&collection)?, 3, "no duplicates on re-run");

    // --- Run 3: update one document + drop another (orphan delete) ---
    run(vec![
        row("a", "alpha-updated", [1.0, 0.0, 0.0]),
        row("b", "beta", [0.0, 1.0, 0.0]),
    ])
    .await;
    assert_eq!(
        conn.document_count(&collection)?,
        2,
        "orphaned document deleted"
    );

    Ok(())
}

#[tokio::test]
async fn zvec_target_schema_change_recreates_collection() -> Result<()> {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = zvec::connect(tempdir.path().join("collections"), true)?;
    let coco_db = tempdir.path().join(".grepify_db");
    let collection = "vectors".to_string();

    let run = |dim: u32, rows: Vec<(String, Vec<f32>)>| {
        let conn = conn.clone();
        let coco_db = coco_db.clone();
        let collection = collection.clone();
        async move {
            let sch = CollectionSchema::builder("id")
                .dense_vector("embedding", VectorField::new(dim))
                .build()
                .unwrap();
            let app = Environment::builder()
                .db_path(&coco_db)
                .provide_key(&DB, conn)
                .build()
                .await
                .unwrap()
                .app("ZvecSchemaChangeTest")
                .await
                .unwrap();
            app.run(move |ctx| {
                let collection = collection.clone();
                let rows = rows.clone();
                let sch = sch.clone();
                async move {
                    let target = zvec::mount_collection_target(
                        &ctx,
                        &DB,
                        &collection,
                        sch,
                        ZvecCollectionOptions::default(),
                    )
                    .await?;
                    for (id, v) in &rows {
                        #[derive(serde::Serialize)]
                        struct R<'a> {
                            id: &'a str,
                            embedding: &'a [f32],
                        }
                        target.declare_row(&ctx, &R { id, embedding: v })?;
                    }
                    Ok(())
                }
            })
            .await
            .unwrap();
        }
    };

    run(
        3,
        vec![
            ("a".to_string(), vec![1.0, 0.0, 0.0]),
            ("b".to_string(), vec![0.0, 1.0, 0.0]),
        ],
    )
    .await;
    assert_eq!(conn.document_count(&collection)?, 2);

    // Schema change (dim 3 -> 4) rebuilds the collection, clearing documents;
    // only the single new 4-dim document remains.
    run(4, vec![("a".to_string(), vec![1.0, 0.0, 0.0, 0.0])]).await;
    assert_eq!(
        conn.document_count(&collection)?,
        1,
        "schema change recreated the collection with just the new document"
    );

    Ok(())
}
