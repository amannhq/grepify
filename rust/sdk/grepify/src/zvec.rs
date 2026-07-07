//! zvec embedded vector-collection target connector.
//!
//! [zvec](https://zvec.org) is an embedded, in-process vector database. This
//! connector mirrors Python's `zvec` connector with a two-level target:
//!
//! 1. **Collection level** — creates/destroys on-disk collections
//!    ([`CollectionHandler`]).
//! 2. **Document level** — upserts/deletes documents within a collection
//!    ([`DocHandler`]).
//!
//! zvec is path-based: a [`ManagedConnection`] owns a base directory and each
//! collection lives in a subdirectory under it. Each document has a single
//! string `id` (the primary key), one or more dense vector fields, and scalar
//! fields used for filtering.
//!
//! Build a schema with [`CollectionSchema::builder`] (or
//! [`CollectionSchema::from_row`]) and declare rows through a
//! [`CollectionTarget`] returned by [`declare_collection_target`] /
//! [`mount_collection_target`].
//!
//! # Parity note
//!
//! The Python connector also supports sparse-vector and full-text (FTS) fields.
//! This initial Rust port covers scalar fields and **dense `fp32` vectors** —
//! the common vector-search path shared with the `qdrant` / `lancedb` targets.
//! Sparse vectors and FTS fields are a TODO (they need the raw
//! `FieldSchema::new` + `Doc::add_field_raw` paths and are not yet exposed here).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use grepify_utils::fingerprint::Fingerprint;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as JsonValue};
use zvec::{Collection, CollectionOptions, Doc, FieldSchema, MetricType, QuantizeType};

use crate::ctx::{ContextKey, ContextStore, Ctx};
use crate::error::{Error, Result};
use crate::statediff::{DiffAction, ManagedBy, MutualTrackingRecord, diff, resolve_system_transition};
use crate::target_state::{
    ChildTargetDef, StableKey, TargetAction, TargetActionSink, TargetChildInvalidation,
    TargetHandler, TargetReconcileOutput, TargetState, TargetStateProvider, declare_target_state,
    declare_target_state_with_child, mount_target, register_root_target_states_provider,
};

const MIN_COLLECTION_NAME_LEN: usize = 3;

// ---------------------------------------------------------------------------
// Identifier validation
// ---------------------------------------------------------------------------

fn validate_identifier(name: &str, kind: &str) -> Result<()> {
    let ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(Error::engine(format!("invalid zvec {kind}: {name:?}")))
    }
}

fn validate_collection_name(name: &str) -> Result<()> {
    validate_identifier(name, "collection name")?;
    if name.len() < MIN_COLLECTION_NAME_LEN {
        return Err(Error::engine(format!(
            "invalid zvec collection name {name:?}: zvec requires at least \
             {MIN_COLLECTION_NAME_LEN} characters"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// A handle to a base directory holding zvec collections. Clone-cheap (the open
/// handle cache is shared).
///
/// zvec opens each collection as a live handle and takes an exclusive write lock
/// on it; opening the same collection twice (even in one process) fails, so open
/// handles are cached by collection name and reused.
#[derive(Clone)]
pub struct ManagedConnection {
    base_path: Arc<PathBuf>,
    enable_mmap: bool,
    open: Arc<Mutex<BTreeMap<String, Arc<Collection>>>>,
    state_id: Arc<str>,
}

/// Create a [`ManagedConnection`] rooted at `base_path` (created if missing).
pub fn connect(base_path: impl AsRef<Path>, enable_mmap: bool) -> Result<ManagedConnection> {
    let base_path = base_path.as_ref().to_path_buf();
    std::fs::create_dir_all(&base_path)
        .map_err(|e| Error::engine(format!("zvec create base dir {base_path:?}: {e}")))?;
    let state_id = format!("zvec:{}", base_path.display());
    Ok(ManagedConnection {
        base_path: Arc::new(base_path),
        enable_mmap,
        open: Arc::new(Mutex::new(BTreeMap::new())),
        state_id: Arc::from(state_id),
    })
}

impl ManagedConnection {
    pub fn state_id(&self) -> &str {
        &self.state_id
    }

    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    fn collection_path(&self, name: &str) -> PathBuf {
        self.base_path.join(name)
    }

    fn options(&self) -> Result<CollectionOptions> {
        let mut opts = CollectionOptions::new().map_err(zv_err)?;
        opts.set_enable_mmap(self.enable_mmap).map_err(zv_err)?;
        Ok(opts)
    }

    /// Open the collection, creating it from `schema` if it doesn't exist yet.
    fn open_or_create(&self, name: &str, schema: &zvec::CollectionSchema) -> Result<Arc<Collection>> {
        let mut open = self.open.lock().unwrap();
        if let Some(col) = open.get(name) {
            return Ok(col.clone());
        }
        let path = self.collection_path(name);
        let opts = self.options()?;
        let col = if path.exists() {
            Collection::open(&path_str(&path)?, Some(&opts)).map_err(zv_err)?
        } else {
            Collection::create_and_open(&path_str(&path)?, schema, Some(&opts)).map_err(zv_err)?
        };
        let col = Arc::new(col);
        open.insert(name.to_string(), col.clone());
        Ok(col)
    }

    /// Open an existing collection (used at document-apply time).
    fn open_existing(&self, name: &str) -> Result<Arc<Collection>> {
        let mut open = self.open.lock().unwrap();
        if let Some(col) = open.get(name) {
            return Ok(col.clone());
        }
        let opts = self.options()?;
        let col = Collection::open(&path_str(&self.collection_path(name))?, Some(&opts))
            .map_err(zv_err)?;
        let col = Arc::new(col);
        open.insert(name.to_string(), col.clone());
        Ok(col)
    }

    /// Number of documents currently in a collection (opens it if needed).
    /// Useful for verifying reconcile results.
    pub fn document_count(&self, name: &str) -> Result<u64> {
        let col = self.open_existing(name)?;
        Ok(col.stats().map_err(zv_err)?.doc_count())
    }

    /// Release all cached collection handles (dropping their write locks).
    /// Mirrors Python's `ManagedConnection.close`.
    pub fn close(&self) {
        self.open.lock().unwrap().clear();
    }

    /// Permanently delete a collection from disk (drops the cached handle first
    /// so its write lock is released).
    fn destroy(&self, name: &str) -> Result<()> {
        let mut open = self.open.lock().unwrap();
        open.remove(name);
        drop(open);
        let path = self.collection_path(name);
        if path.exists() {
            std::fs::remove_dir_all(&path)
                .map_err(|e| Error::engine(format!("zvec destroy collection {name:?}: {e}")))?;
        }
        Ok(())
    }
}

fn path_str(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| Error::engine(format!("non-UTF-8 zvec collection path {path:?}")))
}

fn zv_err(e: zvec::ZvecError) -> Error {
    Error::engine(format!("zvec: {e}"))
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// Distance metric for a dense vector field.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Metric {
    #[default]
    Cosine,
    Ip,
    L2,
}

impl Metric {
    fn to_zvec(self) -> MetricType {
        match self {
            Metric::Cosine => MetricType::Cosine,
            Metric::Ip => MetricType::Ip,
            Metric::L2 => MetricType::L2,
        }
    }
}

/// Optional post-quantization applied to a dense vector before indexing.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Quantize {
    #[default]
    None,
    Fp16,
    Int8,
    Int4,
}

impl Quantize {
    fn to_zvec(self) -> Option<QuantizeType> {
        match self {
            Quantize::None => None,
            Quantize::Fp16 => Some(QuantizeType::Fp16),
            Quantize::Int8 => Some(QuantizeType::Int8),
            Quantize::Int4 => Some(QuantizeType::Int4),
        }
    }
}

/// Scalar field type (a subset of zvec's data types covering the values a JSON
/// row can carry).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ScalarType {
    Bool,
    Int64,
    Double,
    /// Any value stored as a string (text, uuid, dates, or JSON-encoded complex
    /// values). Mirrors Python's fallback to `STRING`.
    String,
}

/// A dense `fp32` vector field.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VectorField {
    pub dim: u32,
    pub metric: Metric,
    pub quantize: Quantize,
}

impl VectorField {
    pub fn new(dim: u32) -> Self {
        Self {
            dim,
            metric: Metric::default(),
            quantize: Quantize::default(),
        }
    }

    pub fn metric(mut self, metric: Metric) -> Self {
        self.metric = metric;
        self
    }

    pub fn quantize(mut self, quantize: Quantize) -> Self {
        self.quantize = quantize;
        self
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum FieldKind {
    Scalar { data_type: ScalarType, indexed: bool },
    DenseVector(VectorField),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct FieldDef {
    kind: FieldKind,
    nullable: bool,
}

/// Schema definition for a zvec collection. The primary-key column becomes the
/// document `id`; other columns become scalar or dense-vector fields.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollectionSchema {
    id_field: String,
    fields: BTreeMap<String, FieldDef>,
}

/// Builder for [`CollectionSchema`].
pub struct CollectionSchemaBuilder {
    id_field: String,
    fields: BTreeMap<String, FieldDef>,
}

impl CollectionSchema {
    /// Start a schema whose `id_field` (a string scalar) becomes the document id.
    pub fn builder(id_field: impl Into<String>) -> CollectionSchemaBuilder {
        CollectionSchemaBuilder {
            id_field: id_field.into(),
            fields: BTreeMap::new(),
        }
    }

    pub fn primary_key(&self) -> &str {
        &self.id_field
    }

    /// Derive a schema from a `#[derive(SchemaFields)]` row type. Exactly one
    /// primary-key column is required (it becomes the document id). Vector
    /// columns (`#[coco(vector = N)]`) become dense `fp32` cosine vectors.
    pub fn from_row<T: crate::row_schema::SchemaFields>(
        primary_key: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self> {
        let pk: Vec<String> = primary_key.into_iter().map(Into::into).collect();
        if pk.len() != 1 {
            return Err(Error::engine(
                "zvec collections require exactly one primary key column",
            ));
        }
        let id_field = pk.into_iter().next().unwrap();
        let mut builder = CollectionSchema::builder(id_field.clone());
        for field in T::schema_fields() {
            if field.name == id_field {
                continue;
            }
            builder = add_row_field(builder, &field);
        }
        builder.build()
    }
}

fn add_row_field(
    builder: CollectionSchemaBuilder,
    field: &crate::row_schema::SchemaField,
) -> CollectionSchemaBuilder {
    use crate::row_schema::LogicalType as L;
    match &field.logical_type {
        L::Vector { dim, .. } => builder.dense_vector(&field.name, VectorField::new(*dim)),
        other => {
            let data_type = match other {
                L::Bool => ScalarType::Bool,
                L::Int16 | L::Int32 | L::Int64 => ScalarType::Int64,
                L::Float32 | L::Float64 | L::Duration => ScalarType::Double,
                // Text / Bytes / Uuid / Decimal / Date / Time / DateTime / Json /
                // Custom all fall back to a string field (matching Python).
                _ => ScalarType::String,
            };
            builder.scalar_nullable(&field.name, data_type, true, field.nullable)
        }
    }
}

impl CollectionSchemaBuilder {
    /// Add an indexed, nullable scalar field.
    pub fn scalar(self, name: impl Into<String>, data_type: ScalarType) -> Self {
        self.scalar_nullable(name, data_type, true, true)
    }

    /// Add a scalar field with explicit `indexed` (invert index for filtering)
    /// and `nullable` flags.
    pub fn scalar_nullable(
        mut self,
        name: impl Into<String>,
        data_type: ScalarType,
        indexed: bool,
        nullable: bool,
    ) -> Self {
        self.fields.insert(
            name.into(),
            FieldDef {
                kind: FieldKind::Scalar { data_type, indexed },
                nullable,
            },
        );
        self
    }

    /// Add a dense `fp32` vector field.
    pub fn dense_vector(mut self, name: impl Into<String>, vector: VectorField) -> Self {
        self.fields.insert(
            name.into(),
            FieldDef {
                kind: FieldKind::DenseVector(vector),
                nullable: true,
            },
        );
        self
    }

    pub fn build(self) -> Result<CollectionSchema> {
        validate_identifier(&self.id_field, "primary key column")?;
        for name in self.fields.keys() {
            validate_identifier(name, "field name")?;
        }
        let has_vector = self
            .fields
            .values()
            .any(|f| matches!(f.kind, FieldKind::DenseVector(_)));
        if !has_vector {
            return Err(Error::engine(
                "zvec collections require at least one dense vector field",
            ));
        }
        Ok(CollectionSchema {
            id_field: self.id_field,
            fields: self.fields,
        })
    }
}

/// Build the native zvec schema from our [`CollectionSchema`].
fn build_zvec_schema(collection_name: &str, schema: &CollectionSchema) -> Result<zvec::CollectionSchema> {
    let mut builder = zvec::CollectionSchema::builder(collection_name);
    for (name, def) in &schema.fields {
        let field = match &def.kind {
            FieldKind::Scalar { data_type, indexed } => {
                let mut f = match data_type {
                    ScalarType::Bool => FieldSchema::bool(name.clone()),
                    ScalarType::Int64 => FieldSchema::int64(name.clone()),
                    ScalarType::Double => FieldSchema::double(name.clone()),
                    ScalarType::String => FieldSchema::string(name.clone()),
                }
                .nullable(def.nullable);
                if *indexed {
                    f = f.invert_index(true, false);
                }
                f
            }
            FieldKind::DenseVector(v) => {
                let mut f = FieldSchema::vector_fp32(name.clone(), v.dim)
                    .nullable(def.nullable)
                    // Reasonable HNSW defaults (M=16, efConstruction=200), matching
                    // the crate's own quickstart.
                    .hnsw(16, 200)
                    .metric(v.metric.to_zvec());
                if let Some(q) = v.quantize.to_zvec() {
                    f = f.quantize(q);
                }
                f
            }
        };
        builder = builder.field(field);
    }
    builder.build().map_err(zv_err)
}

// ---------------------------------------------------------------------------
// Options / target API
// ---------------------------------------------------------------------------

/// Options for the collection-target constructors.
#[derive(Clone, Debug, Default)]
pub struct ZvecCollectionOptions {
    pub managed_by: ManagedBy,
}

/// A declarative zvec collection target — a handle to declare documents on.
#[derive(Clone)]
pub struct CollectionTarget {
    schema: CollectionSchema,
    docs: TargetStateProvider<DocState>,
}

/// Build a composable [`TargetState`] for a zvec collection.
pub fn collection_target(
    ctx: &Ctx,
    conn: &ContextKey<ManagedConnection>,
    collection_name: impl Into<String>,
    schema: CollectionSchema,
    options: ZvecCollectionOptions,
) -> Result<TargetState<CollectionSpec>> {
    let collection_name = collection_name.into();
    validate_collection_name(&collection_name)?;
    let provider = register_root_target_states_provider(
        ctx,
        format!("grepify/zvec/collection/{}/{}", conn.name(), collection_name),
        CollectionHandler {
            conn_key: conn.name().to_string(),
        },
    )?;
    Ok(provider.target_state(
        "default",
        CollectionSpec {
            collection_name,
            schema,
            managed_by: options.managed_by,
        },
    ))
}

/// Declare a zvec collection target in the current component and return a handle.
pub fn declare_collection_target(
    ctx: &Ctx,
    conn: &ContextKey<ManagedConnection>,
    collection_name: impl Into<String>,
    schema: CollectionSchema,
    options: ZvecCollectionOptions,
) -> Result<CollectionTarget> {
    let ts = collection_target(ctx, conn, collection_name, schema, options)?;
    let spec = ts.value().clone();
    let docs = declare_target_state_with_child::<CollectionSpec, DocState>(ctx, ts)?;
    Ok(CollectionTarget {
        schema: spec.schema,
        docs,
    })
}

/// Mount a zvec collection target foreground (documents can be declared
/// immediately).
pub async fn mount_collection_target(
    ctx: &Ctx,
    conn: &ContextKey<ManagedConnection>,
    collection_name: impl Into<String>,
    schema: CollectionSchema,
    options: ZvecCollectionOptions,
) -> Result<CollectionTarget> {
    let ts = collection_target(ctx, conn, collection_name, schema, options)?;
    let spec = ts.value().clone();
    let docs = mount_target::<CollectionSpec, DocState>(ctx, ts).await?;
    Ok(CollectionTarget {
        schema: spec.schema,
        docs,
    })
}

impl CollectionTarget {
    /// Declare a document (row) to be upserted. The primary-key value becomes
    /// the document id (converted to a string).
    pub fn declare_row<R: Serialize>(&self, ctx: &Ctx, row: &R) -> Result<()> {
        let (doc_id, fields) = doc_state(row, &self.schema)?;
        declare_target_state(
            ctx,
            self.docs.target_state(
                StableKey::Str(Arc::from(doc_id.clone())),
                DocState { doc_id, fields },
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// Internal specs / actions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollectionSpec {
    collection_name: String,
    schema: CollectionSchema,
    #[serde(default)]
    managed_by: ManagedBy,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DocState {
    doc_id: String,
    fields: Map<String, JsonValue>,
}

type CollectionTrackingRecord = MutualTrackingRecord<CollectionCore>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct CollectionCore {
    collection_name: String,
    primary_key: String,
    fields: BTreeMap<String, FieldDef>,
}

fn collection_core(spec: &CollectionSpec) -> CollectionCore {
    CollectionCore {
        collection_name: spec.collection_name.clone(),
        primary_key: spec.schema.id_field.clone(),
        fields: spec.schema.fields.clone(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CollectionAction {
    spec: Option<CollectionSpec>,
    drop_name: Option<String>,
    main_action: Option<DiffAction>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DocAction {
    doc_id: String,
    state: Option<DocState>,
}

// ---------------------------------------------------------------------------
// Collection handler (root)
// ---------------------------------------------------------------------------

fn resolve_conn(host_ctx: &Arc<ContextStore>, conn_key: &str) -> Result<Arc<ManagedConnection>> {
    host_ctx
        .resolve::<ManagedConnection>(conn_key)
        .ok_or_else(|| {
            Error::engine(format!(
                "zvec target: connection `{conn_key}` was not provided in the environment \
                 (call Environment::builder().provide_key(&KEY, conn))"
            ))
        })
}

struct CollectionHandler {
    conn_key: String,
}

impl TargetHandler<CollectionSpec> for CollectionHandler {
    type TrackingRecord = CollectionTrackingRecord;
    type Action = CollectionAction;

    fn reconcile(
        &self,
        _key: StableKey,
        desired: Option<CollectionSpec>,
        prev: Vec<CollectionTrackingRecord>,
        prev_may_be_missing: bool,
    ) -> Result<Option<TargetReconcileOutput<CollectionAction, CollectionTrackingRecord>>> {
        match desired {
            Some(spec) => {
                let tracking = MutualTrackingRecord::new(collection_core(&spec), spec.managed_by);
                let resolved =
                    resolve_system_transition(Some(tracking.clone()), prev, prev_may_be_missing);
                let main_action = diff(resolved.as_ref());
                // v1 has no in-place schema evolution: any schema change rebuilds
                // the collection, destroying all documents.
                let child_invalidation = if matches!(main_action, Some(DiffAction::Replace)) {
                    Some(TargetChildInvalidation::Destructive)
                } else {
                    None
                };
                Ok(Some(TargetReconcileOutput {
                    action: TargetAction::Update(CollectionAction {
                        spec: Some(spec),
                        drop_name: None,
                        main_action,
                    }),
                    sink: self.collection_sink(),
                    tracking_record: Some(tracking),
                    child_invalidation,
                }))
            }
            None => {
                let resolved = resolve_system_transition(None, prev.clone(), prev_may_be_missing);
                if resolved.is_none() {
                    return Ok(None);
                }
                let Some(prev_record) = prev.into_iter().find(|v| v.managed_by.is_system()) else {
                    return Ok(None);
                };
                Ok(Some(TargetReconcileOutput {
                    action: TargetAction::Delete(CollectionAction {
                        spec: None,
                        drop_name: Some(prev_record.tracking_record.collection_name),
                        main_action: Some(DiffAction::Delete),
                    }),
                    sink: self.collection_sink(),
                    tracking_record: None,
                    child_invalidation: Some(TargetChildInvalidation::Destructive),
                }))
            }
        }
    }
}

impl CollectionHandler {
    fn collection_sink(&self) -> TargetActionSink<CollectionAction> {
        let conn_key = self.conn_key.clone();
        TargetActionSink::from_async_fn_with_children_ctx(
            move |host_ctx, actions: Vec<TargetAction<CollectionAction>>| {
                let conn_key = conn_key.clone();
                async move {
                    let conn = resolve_conn(&host_ctx, &conn_key)?;
                    let mut out: Vec<Option<ChildTargetDef>> = Vec::with_capacity(actions.len());
                    for action in actions {
                        let a = match action {
                            TargetAction::Create(a)
                            | TargetAction::Update(a)
                            | TargetAction::Delete(a) => a,
                        };
                        out.push(apply_collection_action(&conn, &conn_key, a)?);
                    }
                    Ok(out)
                }
            },
        )
    }
}

fn apply_collection_action(
    conn: &ManagedConnection,
    conn_key: &str,
    action: CollectionAction,
) -> Result<Option<ChildTargetDef>> {
    let CollectionAction {
        spec,
        drop_name,
        main_action,
    } = action;

    if matches!(
        main_action,
        Some(DiffAction::Replace) | Some(DiffAction::Delete)
    ) {
        let name = spec
            .as_ref()
            .map(|s| s.collection_name.clone())
            .or(drop_name)
            .ok_or_else(|| Error::engine("zvec drop action missing collection name"))?;
        conn.destroy(&name)?;
    }

    let Some(spec) = spec else {
        return Ok(None);
    };

    if matches!(
        main_action,
        Some(DiffAction::Insert | DiffAction::Upsert | DiffAction::Replace)
    ) {
        let zschema = build_zvec_schema(&spec.collection_name, &spec.schema)?;
        conn.open_or_create(&spec.collection_name, &zschema)?;
    }

    Ok(Some(ChildTargetDef::new::<DocState, _>(DocHandler {
        conn_key: conn_key.to_string(),
        collection_name: spec.collection_name,
        schema: spec.schema,
    })))
}

// ---------------------------------------------------------------------------
// Document handler (child)
// ---------------------------------------------------------------------------

struct DocHandler {
    conn_key: String,
    collection_name: String,
    schema: CollectionSchema,
}

impl TargetHandler<DocState> for DocHandler {
    type TrackingRecord = Fingerprint;
    type Action = DocAction;

    fn reconcile(
        &self,
        key: StableKey,
        desired: Option<DocState>,
        prev: Vec<Fingerprint>,
        prev_may_be_missing: bool,
    ) -> Result<Option<TargetReconcileOutput<DocAction, Fingerprint>>> {
        let doc_id = match &key {
            StableKey::Str(s) | StableKey::Symbol(s) => s.to_string(),
            other => return Err(Error::engine(format!("zvec document key must be a string: {other:?}"))),
        };
        let desired_fp = match &desired {
            Some(state) => Some(Fingerprint::from(state).map_err(Error::from)?),
            None => None,
        };
        let prev_same = desired_fp
            .as_ref()
            .is_some_and(|fp| !prev.is_empty() && prev.iter().all(|p| p == fp));
        if desired.is_some() && prev_same && !prev_may_be_missing {
            return Ok(None);
        }
        if desired.is_none() && prev.is_empty() && !prev_may_be_missing {
            return Ok(None);
        }
        Ok(Some(TargetReconcileOutput {
            action: TargetAction::Update(DocAction {
                doc_id,
                state: desired,
            }),
            sink: self.doc_sink(),
            tracking_record: desired_fp,
            child_invalidation: None,
        }))
    }
}

impl DocHandler {
    fn doc_sink(&self) -> TargetActionSink<DocAction> {
        let conn_key = self.conn_key.clone();
        let collection_name = self.collection_name.clone();
        let schema = self.schema.clone();
        TargetActionSink::from_async_fn_with_ctx(
            move |host_ctx, actions: Vec<TargetAction<DocAction>>| {
                let conn_key = conn_key.clone();
                let collection_name = collection_name.clone();
                let schema = schema.clone();
                async move {
                    let conn = resolve_conn(&host_ctx, &conn_key)?;
                    apply_docs(&conn, &collection_name, &schema, actions)
                }
            },
        )
    }
}

fn apply_docs(
    conn: &ManagedConnection,
    collection_name: &str,
    schema: &CollectionSchema,
    actions: Vec<TargetAction<DocAction>>,
) -> Result<()> {
    if actions.is_empty() {
        return Ok(());
    }
    let col = conn.open_existing(collection_name)?;

    let mut upsert_docs: Vec<Doc> = Vec::new();
    let mut delete_ids: Vec<String> = Vec::new();
    for action in actions {
        let a = match action {
            TargetAction::Create(a) | TargetAction::Update(a) | TargetAction::Delete(a) => a,
        };
        match a.state {
            Some(state) => upsert_docs.push(build_doc(schema, &state)?),
            None => delete_ids.push(a.doc_id),
        }
    }

    if !upsert_docs.is_empty() {
        let refs: Vec<&Doc> = upsert_docs.iter().collect();
        let summary = col.upsert(&refs).map_err(zv_err)?;
        if summary.error > 0 {
            return Err(Error::engine(format!(
                "zvec upsert into {collection_name:?}: {} of {} documents failed",
                summary.error,
                summary.success + summary.error
            )));
        }
    }
    if !delete_ids.is_empty() {
        let refs: Vec<&str> = delete_ids.iter().map(String::as_str).collect();
        let summary = col.delete(&refs).map_err(zv_err)?;
        if summary.error > 0 {
            return Err(Error::engine(format!(
                "zvec delete from {collection_name:?}: {} of {} documents failed",
                summary.error,
                summary.success + summary.error
            )));
        }
    }
    if !upsert_docs.is_empty() || !delete_ids.is_empty() {
        col.flush().map_err(zv_err)?;
        col.optimize().map_err(zv_err)?;
    }
    Ok(())
}

/// Build a native zvec [`Doc`] from a declared [`DocState`], routing each field
/// by its schema kind. Null values are skipped (mirrors Python's `_build_doc`).
fn build_doc(schema: &CollectionSchema, state: &DocState) -> Result<Doc> {
    let mut doc = Doc::new().map_err(zv_err)?;
    doc.set_pk(&state.doc_id).map_err(zv_err)?;

    for (name, def) in &schema.fields {
        let Some(value) = state.fields.get(name) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        match &def.kind {
            FieldKind::Scalar { data_type, .. } => match data_type {
                ScalarType::Bool => {
                    if let Some(b) = value.as_bool() {
                        doc.add_bool(name, b).map_err(zv_err)?;
                    }
                }
                ScalarType::Int64 => {
                    if let Some(i) = value.as_i64() {
                        doc.add_int64(name, i).map_err(zv_err)?;
                    } else if let Some(f) = value.as_f64() {
                        doc.add_int64(name, f as i64).map_err(zv_err)?;
                    }
                }
                ScalarType::Double => {
                    if let Some(f) = value.as_f64() {
                        doc.add_double(name, f).map_err(zv_err)?;
                    }
                }
                ScalarType::String => {
                    let s = match value {
                        JsonValue::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    doc.add_string(name, &s).map_err(zv_err)?;
                }
            },
            FieldKind::DenseVector(_) => {
                let vector = json_to_f32_vec(value).ok_or_else(|| {
                    Error::engine(format!("zvec vector field {name:?} must be an array of numbers"))
                })?;
                doc.add_vector_fp32(name, &vector).map_err(zv_err)?;
            }
        }
    }
    Ok(doc)
}

fn json_to_f32_vec(value: &JsonValue) -> Option<Vec<f32>> {
    let arr = value.as_array()?;
    arr.iter().map(|v| v.as_f64().map(|f| f as f32)).collect()
}

/// Serialize a row and split it into `(doc_id, fields)`. `fields` retains only
/// schema columns (excluding the id field).
fn doc_state<R: Serialize>(
    row: &R,
    schema: &CollectionSchema,
) -> Result<(String, Map<String, JsonValue>)> {
    crate::finite::ensure_finite(row)
        .map_err(|e| Error::engine(format!("zvec target row has a {e}")))?;
    let value = serde_json::to_value(row)
        .map_err(|e| Error::engine(format!("serialize zvec target row: {e}")))?;
    let JsonValue::Object(obj) = value else {
        return Err(Error::engine("zvec target row must serialize to an object"));
    };

    let pk_value = obj
        .get(&schema.id_field)
        .ok_or_else(|| Error::engine(format!("missing primary key column {:?}", schema.id_field)))?;
    if pk_value.is_null() {
        return Err(Error::engine(format!(
            "zvec primary key {:?} value cannot be null",
            schema.id_field
        )));
    }
    let doc_id = match pk_value {
        JsonValue::String(s) => s.clone(),
        other => other.to_string(),
    };

    let mut fields = Map::new();
    for name in schema.fields.keys() {
        if let Some(v) = obj.get(name) {
            fields.insert(name.clone(), v.clone());
        }
    }
    Ok((doc_id, fields))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> CollectionSchema {
        CollectionSchema::builder("id")
            .scalar("title", ScalarType::String)
            .dense_vector("embedding", VectorField::new(3))
            .build()
            .unwrap()
    }

    #[test]
    fn schema_requires_a_vector_field() {
        let err = CollectionSchema::builder("id")
            .scalar("title", ScalarType::String)
            .build();
        assert!(err.is_err());
    }

    #[test]
    fn collection_name_min_length_enforced() {
        assert!(validate_collection_name("ab").is_err());
        assert!(validate_collection_name("abc").is_ok());
        assert!(validate_collection_name("1bad").is_err());
    }

    #[test]
    fn doc_state_extracts_pk_and_fields() {
        let schema = schema();
        let row = serde_json::json!({
            "id": 42,
            "title": "hi",
            "embedding": [0.1, 0.2, 0.3],
            "ignored": true,
        });
        let (doc_id, fields) = doc_state(&row, &schema).unwrap();
        assert_eq!(doc_id, "42");
        assert!(fields.contains_key("title"));
        assert!(fields.contains_key("embedding"));
        assert!(!fields.contains_key("ignored"));
    }

    #[test]
    fn json_to_f32_vec_parses_numbers() {
        let v = json_to_f32_vec(&serde_json::json!([1, 2.5, 3])).unwrap();
        assert_eq!(v, vec![1.0, 2.5, 3.0]);
        assert!(json_to_f32_vec(&serde_json::json!("x")).is_none());
    }

    #[test]
    fn from_row_maps_scalar_and_vector_fields() {
        // A lightweight SchemaFields impl mirroring `#[derive(SchemaFields)]`.
        struct Row;
        impl crate::row_schema::SchemaFields for Row {
            fn schema_fields() -> Vec<crate::row_schema::SchemaField> {
                use crate::row_schema::{LogicalType, SchemaField};
                vec![
                    SchemaField {
                        name: "id".into(),
                        logical_type: LogicalType::Text,
                        nullable: false,
                    },
                    SchemaField {
                        name: "views".into(),
                        logical_type: LogicalType::Int64,
                        nullable: false,
                    },
                    SchemaField {
                        name: "embedding".into(),
                        logical_type: LogicalType::Vector { dim: 8, half: false },
                        nullable: false,
                    },
                ]
            }
        }
        let schema = CollectionSchema::from_row::<Row>(["id"]).unwrap();
        assert_eq!(schema.primary_key(), "id");
        assert!(schema.fields.contains_key("views"));
        assert!(matches!(
            schema.fields["embedding"].kind,
            FieldKind::DenseVector(_)
        ));
    }
}
