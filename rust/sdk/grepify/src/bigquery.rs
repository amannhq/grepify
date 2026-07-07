//! Google BigQuery table target connector.
//!
//! Table targets reconcile declared rows against the previous run: changed rows
//! are upserted (via `MERGE`), unchanged rows are skipped, and rows no longer
//! declared are deleted. `managed_by` controls whether Grepify owns table DDL.
//!
//! Mirrors Python's `bigquery` connector. The two-level shape matches the SQL
//! targets (`postgres`, `sqlite`, `doris`): a container [`TableHandler`] owns
//! table DDL and yields a per-row [`RowHandler`] child. All statements run
//! through the [`gcp_bigquery_client`] REST `jobs.query` endpoint.
//!
//! Unlike the Python connector (which uses BigQuery query parameters), DML here
//! is built with inline, escaped SQL literals — the same approach as the Doris
//! `DELETE` path — so no query-parameter plumbing is needed.
//!
//! Use [`table_target`] to build a composable target state,
//! [`declare_table_target`] inside the current component, or
//! [`mount_table_target`] when rows must be declared immediately.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use gcp_bigquery_client::Client;
use gcp_bigquery_client::model::query_request::QueryRequest;
use grepify_utils::fingerprint::Fingerprint;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as JsonValue};

use crate::ctx::{ContextKey, ContextStore, Ctx};
use crate::error::{Error, Result};
use crate::sql_ident::validate_ident;
use crate::statediff::{
    CompositeTrackingRecord, DiffAction, ManagedBy, MutualTrackingRecord, diff, diff_composite,
    resolve_system_transition,
};
use crate::target_state::{
    ChildTargetDef, StableKey, TargetAction, TargetActionSink, TargetChildInvalidation,
    TargetHandler, TargetReconcileOutput, TargetState, TargetStateProvider, declare_target_state,
    declare_target_state_with_child, mount_target, register_root_target_states_provider,
};

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// Connection configuration for BigQuery. Mirrors Python's `ConnectionConfig`.
#[derive(Clone, Debug)]
pub struct BigQueryConfig {
    /// Billing / default project id. Also the default project for unqualified
    /// table targets.
    pub project: String,
    /// Path to a service-account JSON key file. When `None`, Application Default
    /// Credentials are used.
    pub credentials_path: Option<String>,
    /// Default job location (e.g. `US`, `EU`); `None` uses the API default.
    pub location: Option<String>,
}

impl BigQueryConfig {
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            credentials_path: None,
            location: None,
        }
    }

    pub fn credentials_path(mut self, path: impl Into<String>) -> Self {
        self.credentials_path = Some(path.into());
        self
    }

    pub fn location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }
}

/// A BigQuery connection handle. Clone-cheap (the REST client is shared).
/// `state_id` (`project`, credentials excluded) is the stable identity used for
/// target-state keys.
#[derive(Clone)]
pub struct BigQueryConnection {
    client: Arc<Client>,
    project: Arc<str>,
    state_id: Arc<str>,
}

impl BigQueryConnection {
    /// Connect to BigQuery. With a `credentials_path` a service-account key file
    /// is loaded; otherwise Application Default Credentials are used.
    pub async fn connect(config: BigQueryConfig) -> Result<Self> {
        let client = match &config.credentials_path {
            Some(path) => Client::from_service_account_key_file(path)
                .await
                .map_err(bq_err)?,
            None => Client::from_application_default_credentials()
                .await
                .map_err(bq_err)?,
        };
        let state_id = format!("bigquery:{}", config.project);
        Ok(Self {
            client: Arc::new(client),
            project: Arc::from(config.project.as_str()),
            state_id: Arc::from(state_id),
        })
    }

    /// Wrap an already-built [`gcp_bigquery_client::Client`] (advanced use / tests).
    pub fn from_client(project: impl Into<String>, client: Client) -> Self {
        let project = project.into();
        let state_id = format!("bigquery:{project}");
        Self {
            client: Arc::new(client),
            project: Arc::from(project.as_str()),
            state_id: Arc::from(state_id),
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn state_id(&self) -> &str {
        &self.state_id
    }

    async fn run_query(&self, sql: String) -> Result<()> {
        self.client
            .job()
            .query(&self.project, QueryRequest::new(sql))
            .await
            .map_err(bq_err)?;
        Ok(())
    }
}

fn bq_err(e: gcp_bigquery_client::error::BQError) -> Error {
    Error::engine(format!("bigquery: {e}"))
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// A BigQuery column. `bigquery_type` is the SQL type as written in
/// `CREATE TABLE` (e.g. `INT64`, `STRING`, `TIMESTAMP`, `JSON`). Set
/// `use_parse_json` for columns fed from JSON text via `PARSE_JSON`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ColumnDef {
    pub bigquery_type: String,
    pub nullable: bool,
    pub use_parse_json: bool,
}

impl ColumnDef {
    pub fn new(bigquery_type: impl Into<String>) -> Self {
        Self {
            bigquery_type: bigquery_type.into(),
            nullable: true,
            use_parse_json: false,
        }
    }

    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    /// A `JSON` column whose value is provided as JSON text and wrapped in
    /// `PARSE_JSON(...)`.
    pub fn json() -> Self {
        Self {
            bigquery_type: "JSON".to_string(),
            nullable: true,
            use_parse_json: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TableSchema {
    columns: BTreeMap<String, ColumnDef>,
    primary_key: Vec<String>,
}

impl TableSchema {
    pub fn new(
        columns: impl IntoIterator<Item = (impl Into<String>, ColumnDef)>,
        primary_key: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self> {
        let mut out = BTreeMap::new();
        for (name, def) in columns {
            let name = name.into();
            validate_ident(&name, "column name")?;
            validate_bigquery_type(&def.bigquery_type)?;
            out.insert(name, def);
        }
        let primary_key: Vec<String> = primary_key.into_iter().map(Into::into).collect();
        if primary_key.is_empty() {
            return Err(Error::engine("BigQuery table primary key cannot be empty"));
        }
        for name in &primary_key {
            validate_ident(name, "primary key column")?;
            if !out.contains_key(name) {
                return Err(Error::engine(format!(
                    "primary key column {name:?} is not in table schema"
                )));
            }
        }
        Ok(Self {
            columns: out,
            primary_key,
        })
    }

    pub fn columns(&self) -> &BTreeMap<String, ColumnDef> {
        &self.columns
    }

    pub fn primary_key(&self) -> &[String] {
        &self.primary_key
    }

    /// Derive a schema from a `#[derive(SchemaFields)]` row type, mirroring
    /// Python's `TableSchema.from_class` leaf-type mapping.
    pub fn from_row<T: crate::row_schema::SchemaFields>(
        primary_key: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self> {
        let columns = T::schema_fields()
            .into_iter()
            .map(|f| (f.name.clone(), bigquery_column_def(&f)));
        Self::new(columns, primary_key)
    }
}

/// Map a connector-agnostic [`SchemaField`](crate::row_schema::SchemaField) to a
/// BigQuery [`ColumnDef`], mirroring Python's `_LEAF_TYPE_MAPPINGS`.
fn bigquery_column_def(field: &crate::row_schema::SchemaField) -> ColumnDef {
    use crate::row_schema::LogicalType as L;
    let (ty, use_parse_json) = match &field.logical_type {
        L::Bool => ("BOOL", false),
        L::Int16 | L::Int32 | L::Int64 | L::Duration => ("INT64", false),
        L::Float32 | L::Float64 => ("FLOAT64", false),
        L::Decimal => ("NUMERIC", false),
        L::Text => ("STRING", false),
        L::Bytes => ("BYTES", false),
        L::Uuid => ("STRING", false),
        L::Date => ("DATE", false),
        L::Time => ("TIME", false),
        L::DateTime => ("TIMESTAMP", false),
        // Structured / vector values are stored as JSON (fed via PARSE_JSON).
        L::Json | L::Vector { .. } => ("JSON", true),
        L::Custom(s) => (s.as_str(), false),
    };
    ColumnDef {
        bigquery_type: ty.to_string(),
        nullable: field.nullable,
        use_parse_json,
    }
}

/// Options for the `*_with_options` table-target constructors.
#[derive(Clone, Debug, Default)]
pub struct BigQueryTableOptions {
    pub managed_by: ManagedBy,
    /// Dataset (schema) that holds the table.
    pub dataset: String,
    /// Project override for the table (defaults to the connection's project).
    pub project: Option<String>,
}

// ---------------------------------------------------------------------------
// Public target API
// ---------------------------------------------------------------------------

/// A declarative BigQuery table target — a handle to declare rows on.
#[derive(Clone)]
pub struct TableTarget {
    table_schema: TableSchema,
    rows: TargetStateProvider<RowState>,
}

/// Build a composable [`TargetState`] for a BigQuery table.
pub fn table_target(
    ctx: &Ctx,
    conn: &ContextKey<BigQueryConnection>,
    table_name: impl Into<String>,
    table_schema: TableSchema,
    options: BigQueryTableOptions,
) -> Result<TargetState<TableSpec>> {
    let table_name = table_name.into();
    validate_ident(&table_name, "table name")?;
    validate_ident(&options.dataset, "dataset name")?;
    if let Some(project) = &options.project {
        validate_project_id(project)?;
    }
    let provider = register_root_target_states_provider(
        ctx,
        format!(
            "grepify/bigquery/table/{}/{}/{}",
            conn.name(),
            options.dataset,
            table_name
        ),
        TableHandler {
            conn_key: conn.name().to_string(),
        },
    )?;
    Ok(provider.target_state(
        "default",
        TableSpec {
            table_name,
            dataset: options.dataset,
            project: options.project,
            table_schema,
            managed_by: options.managed_by,
        },
    ))
}

/// Declare a BigQuery table target in the current component and return a handle.
pub fn declare_table_target(
    ctx: &Ctx,
    conn: &ContextKey<BigQueryConnection>,
    table_name: impl Into<String>,
    table_schema: TableSchema,
    options: BigQueryTableOptions,
) -> Result<TableTarget> {
    let ts = table_target(ctx, conn, table_name, table_schema, options)?;
    let spec = ts.value().clone();
    let rows = declare_target_state_with_child::<TableSpec, RowState>(ctx, ts)?;
    Ok(TableTarget {
        table_schema: spec.table_schema,
        rows,
    })
}

/// Mount a BigQuery table target foreground (rows can be declared immediately).
pub async fn mount_table_target(
    ctx: &Ctx,
    conn: &ContextKey<BigQueryConnection>,
    table_name: impl Into<String>,
    table_schema: TableSchema,
    options: BigQueryTableOptions,
) -> Result<TableTarget> {
    let ts = table_target(ctx, conn, table_name, table_schema, options)?;
    let spec = ts.value().clone();
    let rows = mount_target::<TableSpec, RowState>(ctx, ts).await?;
    Ok(TableTarget {
        table_schema: spec.table_schema,
        rows,
    })
}

impl TableTarget {
    pub fn declare_row<R: Serialize>(&self, ctx: &Ctx, row: &R) -> Result<()> {
        let fields = row_state(row, &self.table_schema)?;
        let key = pk_stable_key(&fields, self.table_schema.primary_key())?;
        declare_target_state(ctx, self.rows.target_state(key, RowState { fields }))
    }
}

// ---------------------------------------------------------------------------
// Internal specs / actions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TableSpec {
    table_name: String,
    dataset: String,
    #[serde(default)]
    project: Option<String>,
    table_schema: TableSchema,
    #[serde(default)]
    managed_by: ManagedBy,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RowState {
    fields: Map<String, JsonValue>,
}

const COL_SUBKEY_PREFIX: &str = "col:";

fn col_subkey(col_name: &str) -> String {
    format!("{COL_SUBKEY_PREFIX}{col_name}")
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PkColumnInfo {
    name: String,
    bigquery_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct TablePrimaryTrackingRecord {
    table_name: String,
    dataset: String,
    project: Option<String>,
    primary_key_columns: Vec<PkColumnInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct NonPkColumnTrackingRecord {
    bigquery_type: String,
    nullable: bool,
}

type TableCompositeRecord =
    CompositeTrackingRecord<TablePrimaryTrackingRecord, String, NonPkColumnTrackingRecord>;

type TableTrackingRecord = MutualTrackingRecord<TableCompositeRecord>;

fn table_composite_record(spec: &TableSpec) -> TableCompositeRecord {
    let schema = &spec.table_schema;
    let pk: HashSet<&String> = schema.primary_key().iter().collect();
    let main = TablePrimaryTrackingRecord {
        table_name: spec.table_name.clone(),
        dataset: spec.dataset.clone(),
        project: spec.project.clone(),
        primary_key_columns: schema
            .primary_key()
            .iter()
            .map(|name| PkColumnInfo {
                name: name.clone(),
                bigquery_type: schema.columns()[name].bigquery_type.clone(),
            })
            .collect(),
    };
    let sub: HashMap<String, NonPkColumnTrackingRecord> = schema
        .columns()
        .iter()
        .filter(|(name, _)| !pk.contains(*name))
        .map(|(name, col)| {
            (
                col_subkey(name),
                NonPkColumnTrackingRecord {
                    bigquery_type: col.bigquery_type.clone(),
                    nullable: col.nullable,
                },
            )
        })
        .collect();
    CompositeTrackingRecord::new(main, sub)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TableAction {
    spec: Option<TableSpec>,
    drop: Option<TableSpec>,
    main_action: Option<DiffAction>,
    column_actions: BTreeMap<String, DiffAction>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RowAction {
    pk: Vec<JsonValue>,
    state: Option<RowState>,
}

// ---------------------------------------------------------------------------
// Table container handler (root)
// ---------------------------------------------------------------------------

fn resolve_conn(host_ctx: &Arc<ContextStore>, conn_key: &str) -> Result<Arc<BigQueryConnection>> {
    host_ctx.resolve::<BigQueryConnection>(conn_key).ok_or_else(|| {
        Error::engine(format!(
            "bigquery target: connection `{conn_key}` was not provided in the environment \
             (call Environment::builder().provide_key(&KEY, conn))"
        ))
    })
}

struct TableHandler {
    conn_key: String,
}

impl TargetHandler<TableSpec> for TableHandler {
    type TrackingRecord = TableTrackingRecord;
    type Action = TableAction;

    fn reconcile(
        &self,
        _key: StableKey,
        desired: Option<TableSpec>,
        prev: Vec<TableTrackingRecord>,
        prev_may_be_missing: bool,
    ) -> Result<Option<TargetReconcileOutput<TableAction, TableTrackingRecord>>> {
        match desired {
            Some(spec) => {
                let tracking =
                    MutualTrackingRecord::new(table_composite_record(&spec), spec.managed_by);
                let resolved =
                    resolve_system_transition(Some(tracking.clone()), prev, prev_may_be_missing);
                let (main_action, column_transitions) = diff_composite(resolved.as_ref());
                let mut column_actions = BTreeMap::new();
                if main_action.is_none() {
                    for (sub_key, transition) in &column_transitions {
                        if let Some(action) = diff(Some(transition)) {
                            column_actions.insert(sub_key.clone(), action);
                        }
                    }
                }
                let child_invalidation = if matches!(main_action, Some(DiffAction::Replace)) {
                    Some(TargetChildInvalidation::Destructive)
                } else if main_action.is_none()
                    && column_actions
                        .values()
                        .any(|a| !matches!(a, DiffAction::Insert))
                {
                    Some(TargetChildInvalidation::Lossy)
                } else {
                    None
                };
                Ok(Some(TargetReconcileOutput {
                    action: TargetAction::Update(TableAction {
                        spec: Some(spec),
                        drop: None,
                        main_action,
                        column_actions,
                    }),
                    sink: self.table_sink(),
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
                let record = prev_record.tracking_record.main;
                Ok(Some(TargetReconcileOutput {
                    action: TargetAction::Delete(TableAction {
                        spec: None,
                        drop: Some(TableSpec {
                            table_name: record.table_name,
                            dataset: record.dataset,
                            project: record.project,
                            // A drop needs only the qualified name; schema is unused.
                            table_schema: TableSchema {
                                columns: BTreeMap::new(),
                                primary_key: Vec::new(),
                            },
                            managed_by: ManagedBy::System,
                        }),
                        main_action: Some(DiffAction::Delete),
                        column_actions: BTreeMap::new(),
                    }),
                    sink: self.table_sink(),
                    tracking_record: None,
                    child_invalidation: Some(TargetChildInvalidation::Destructive),
                }))
            }
        }
    }
}

impl TableHandler {
    fn table_sink(&self) -> TargetActionSink<TableAction> {
        let conn_key = self.conn_key.clone();
        TargetActionSink::from_async_fn_with_children_ctx(
            move |host_ctx, actions: Vec<TargetAction<TableAction>>| {
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
                        out.push(apply_table_action(&conn, &conn_key, a).await?);
                    }
                    Ok(out)
                }
            },
        )
    }
}

async fn apply_table_action(
    conn: &BigQueryConnection,
    conn_key: &str,
    action: TableAction,
) -> Result<Option<ChildTargetDef>> {
    let TableAction {
        spec,
        drop,
        main_action,
        column_actions,
    } = action;

    if matches!(
        main_action,
        Some(DiffAction::Replace) | Some(DiffAction::Delete)
    ) {
        if let Some(target) = spec.as_ref().or(drop.as_ref()) {
            drop_table(conn, target).await?;
        }
    }

    let Some(spec) = spec else {
        return Ok(None);
    };

    match main_action {
        Some(DiffAction::Insert | DiffAction::Upsert | DiffAction::Replace) => {
            create_table(conn, &spec, matches!(main_action, Some(DiffAction::Upsert))).await?;
        }
        _ => {
            if !column_actions.is_empty() {
                apply_column_actions(conn, &spec, &column_actions).await?;
            }
        }
    }

    Ok(Some(ChildTargetDef::new::<RowState, _>(RowHandler {
        conn_key: conn_key.to_string(),
        spec,
    })))
}

// ---------------------------------------------------------------------------
// Row handler (child)
// ---------------------------------------------------------------------------

struct RowHandler {
    conn_key: String,
    spec: TableSpec,
}

impl TargetHandler<RowState> for RowHandler {
    type TrackingRecord = Fingerprint;
    type Action = RowAction;

    fn reconcile(
        &self,
        key: StableKey,
        desired: Option<RowState>,
        prev: Vec<Fingerprint>,
        prev_may_be_missing: bool,
    ) -> Result<Option<TargetReconcileOutput<RowAction, Fingerprint>>> {
        let pk = stable_key_to_pk(&key)?;
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
            action: TargetAction::Update(RowAction { pk, state: desired }),
            sink: self.row_sink(),
            tracking_record: desired_fp,
            child_invalidation: None,
        }))
    }
}

impl RowHandler {
    fn row_sink(&self) -> TargetActionSink<RowAction> {
        let conn_key = self.conn_key.clone();
        let spec = self.spec.clone();
        TargetActionSink::from_async_fn_with_ctx(
            move |host_ctx, actions: Vec<TargetAction<RowAction>>| {
                let conn_key = conn_key.clone();
                let spec = spec.clone();
                async move {
                    let conn = resolve_conn(&host_ctx, &conn_key)?;
                    let mut mutations = Vec::with_capacity(actions.len());
                    for action in actions {
                        let row = match action {
                            TargetAction::Create(r)
                            | TargetAction::Update(r)
                            | TargetAction::Delete(r) => r,
                        };
                        mutations.push((row.pk, row.state));
                    }
                    apply_rows(&conn, &spec, mutations).await
                }
            },
        )
    }
}

// ---------------------------------------------------------------------------
// DB I/O
// ---------------------------------------------------------------------------

fn qualified_table_name(conn: &BigQueryConnection, spec: &TableSpec) -> String {
    let project = spec.project.as_deref().unwrap_or(&conn.project);
    format!("`{}.{}.{}`", project, spec.dataset, spec.table_name)
}

fn qualified_dataset_name(conn: &BigQueryConnection, spec: &TableSpec) -> String {
    let project = spec.project.as_deref().unwrap_or(&conn.project);
    format!("`{}.{}`", project, spec.dataset)
}

async fn create_table(conn: &BigQueryConnection, spec: &TableSpec, if_not_exists: bool) -> Result<()> {
    if spec.managed_by.is_user() {
        return Ok(());
    }
    conn.run_query(format!(
        "CREATE SCHEMA IF NOT EXISTS {}",
        qualified_dataset_name(conn, spec)
    ))
    .await?;

    let schema = &spec.table_schema;
    let pk: HashSet<&String> = schema.primary_key().iter().collect();
    let mut col_defs: Vec<String> = Vec::new();
    for (name, col) in schema.columns() {
        col_defs.push(column_sql(name, col, &pk));
    }
    let pk_cols = schema
        .primary_key()
        .iter()
        .map(|c| format!("`{c}`"))
        .collect::<Vec<_>>()
        .join(", ");
    col_defs.push(format!("PRIMARY KEY ({pk_cols}) NOT ENFORCED"));

    let if_not_exists_sql = if if_not_exists { " IF NOT EXISTS" } else { "" };
    conn.run_query(format!(
        "CREATE TABLE{if_not_exists_sql} {} ({})",
        qualified_table_name(conn, spec),
        col_defs.join(", ")
    ))
    .await
}

fn column_sql(col_name: &str, col: &ColumnDef, pk: &HashSet<&String>) -> String {
    let nullable = if col.nullable && !pk.contains(&col_name.to_string()) {
        ""
    } else {
        " NOT NULL"
    };
    format!("`{col_name}` {}{nullable}", col.bigquery_type)
}

async fn drop_table(conn: &BigQueryConnection, spec: &TableSpec) -> Result<()> {
    if spec.managed_by.is_user() {
        return Ok(());
    }
    conn.run_query(format!(
        "DROP TABLE IF EXISTS {}",
        qualified_table_name(conn, spec)
    ))
    .await
}

async fn apply_column_actions(
    conn: &BigQueryConnection,
    spec: &TableSpec,
    column_actions: &BTreeMap<String, DiffAction>,
) -> Result<()> {
    if spec.managed_by.is_user() {
        return Ok(());
    }
    let qualified = qualified_table_name(conn, spec);
    let schema = &spec.table_schema;
    let pk: HashSet<&String> = schema.primary_key().iter().collect();

    for (sub_key, action) in column_actions {
        let Some(col_name) = sub_key.strip_prefix(COL_SUBKEY_PREFIX) else {
            return Err(Error::engine(format!(
                "bigquery column action has unexpected sub-key {sub_key:?}"
            )));
        };
        if pk.contains(&col_name.to_string()) {
            continue;
        }
        match action {
            DiffAction::Delete => {
                conn.run_query(format!(
                    "ALTER TABLE {qualified} DROP COLUMN IF EXISTS `{col_name}`"
                ))
                .await?;
            }
            DiffAction::Insert => {
                if let Some(col) = schema.columns().get(col_name) {
                    conn.run_query(format!(
                        "ALTER TABLE {qualified} ADD COLUMN {}",
                        column_sql(col_name, col, &pk)
                    ))
                    .await?;
                }
            }
            DiffAction::Upsert => {
                if let Some(col) = schema.columns().get(col_name) {
                    conn.run_query(format!(
                        "ALTER TABLE {qualified} ADD COLUMN IF NOT EXISTS {}",
                        column_sql(col_name, col, &pk)
                    ))
                    .await?;
                }
            }
            DiffAction::Replace => {
                if let Some(col) = schema.columns().get(col_name) {
                    conn.run_query(format!(
                        "ALTER TABLE {qualified} DROP COLUMN IF EXISTS `{col_name}`"
                    ))
                    .await?;
                    conn.run_query(format!(
                        "ALTER TABLE {qualified} ADD COLUMN {}",
                        column_sql(col_name, col, &pk)
                    ))
                    .await?;
                }
            }
        }
    }
    Ok(())
}

async fn apply_rows(
    conn: &BigQueryConnection,
    spec: &TableSpec,
    mutations: Vec<(Vec<JsonValue>, Option<RowState>)>,
) -> Result<()> {
    if mutations.is_empty() {
        return Ok(());
    }
    let qualified = qualified_table_name(conn, spec);
    let schema = &spec.table_schema;
    let pk_cols = schema.primary_key();

    let mut upserts: Vec<Map<String, JsonValue>> = Vec::new();
    let mut delete_keys: Vec<Vec<JsonValue>> = Vec::new();
    for (pk, state) in mutations {
        match state {
            Some(state) => upserts.push(state.fields),
            None => delete_keys.push(pk),
        }
    }

    for row in &upserts {
        conn.run_query(merge_sql(&qualified, schema, row)).await?;
    }
    if !delete_keys.is_empty() {
        conn.run_query(delete_sql(&qualified, pk_cols, &delete_keys))
            .await?;
    }
    Ok(())
}

/// Build a single-row `MERGE` with inline literals for `row`.
fn merge_sql(qualified: &str, schema: &TableSchema, row: &Map<String, JsonValue>) -> String {
    let all_cols: Vec<&String> = schema.columns().keys().collect();
    let pk_cols: HashSet<&String> = schema.primary_key().iter().collect();

    let source_cols = schema
        .columns()
        .iter()
        .map(|(name, col)| {
            let value = row.get(name).unwrap_or(&JsonValue::Null);
            format!("{} AS `{name}`", bq_literal(col, value))
        })
        .collect::<Vec<_>>()
        .join(", ");

    let on_clause = schema
        .primary_key()
        .iter()
        .map(|c| format!("target.`{c}` = source.`{c}`"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let insert_cols = all_cols
        .iter()
        .map(|c| format!("`{c}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_values = all_cols
        .iter()
        .map(|c| format!("source.`{c}`"))
        .collect::<Vec<_>>()
        .join(", ");

    let mut sql = format!(
        "MERGE {qualified} AS target USING (SELECT {source_cols}) AS source ON {on_clause}"
    );
    let non_pk: Vec<&&String> = all_cols.iter().filter(|c| !pk_cols.contains(**c)).collect();
    if !non_pk.is_empty() {
        let update_list = non_pk
            .iter()
            .map(|c| format!("`{c}` = source.`{c}`"))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(" WHEN MATCHED THEN UPDATE SET {update_list}"));
    }
    sql.push_str(&format!(
        " WHEN NOT MATCHED THEN INSERT ({insert_cols}) VALUES ({insert_values})"
    ));
    sql
}

fn delete_sql(qualified: &str, pk_cols: &[String], keys: &[Vec<JsonValue>]) -> String {
    if pk_cols.len() == 1 {
        let col = &pk_cols[0];
        let markers = keys
            .iter()
            .map(|k| scalar_literal(k.first().unwrap_or(&JsonValue::Null)))
            .collect::<Vec<_>>()
            .join(", ");
        return format!("DELETE FROM {qualified} WHERE `{col}` IN ({markers})");
    }
    let groups = keys
        .iter()
        .map(|key| {
            let parts = pk_cols
                .iter()
                .zip(key)
                .map(|(col, val)| {
                    if val.is_null() {
                        format!("`{col}` IS NULL")
                    } else {
                        format!("`{col}` = {}", scalar_literal(val))
                    }
                })
                .collect::<Vec<_>>()
                .join(" AND ");
            format!("({parts})")
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    format!("DELETE FROM {qualified} WHERE {groups}")
}

/// Render a value as a BigQuery SQL literal honoring the column's `PARSE_JSON`.
fn bq_literal(col: &ColumnDef, value: &JsonValue) -> String {
    if value.is_null() {
        return "NULL".to_string();
    }
    if col.use_parse_json {
        let text = match value {
            JsonValue::String(s) => s.clone(),
            other => other.to_string(),
        };
        return format!("PARSE_JSON('{}')", escape_sql(&text));
    }
    scalar_literal(value)
}

fn scalar_literal(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "NULL".to_string(),
        JsonValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        JsonValue::Number(n) => n.to_string(),
        JsonValue::String(s) => format!("'{}'", escape_sql(s)),
        other => format!("'{}'", escape_sql(&other.to_string())),
    }
}

fn escape_sql(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

// ---------------------------------------------------------------------------
// Value / key helpers
// ---------------------------------------------------------------------------

fn row_state<R: Serialize>(row: &R, schema: &TableSchema) -> Result<Map<String, JsonValue>> {
    crate::finite::ensure_finite(row)
        .map_err(|e| Error::engine(format!("BigQuery target row has a {e}")))?;
    let value = serde_json::to_value(row)
        .map_err(|e| Error::engine(format!("serialize BigQuery target row: {e}")))?;
    let JsonValue::Object(mut fields) = value else {
        return Err(Error::engine("BigQuery target row must serialize to an object"));
    };
    fields.retain(|name, _| schema.columns().contains_key(name));
    for name in schema.columns().keys() {
        fields.entry(name.clone()).or_insert(JsonValue::Null);
    }
    Ok(fields)
}

fn pk_stable_key(fields: &Map<String, JsonValue>, primary_key: &[String]) -> Result<StableKey> {
    let mut parts = Vec::with_capacity(primary_key.len());
    for name in primary_key {
        let value = fields
            .get(name)
            .ok_or_else(|| Error::engine(format!("missing primary key column {name:?}")))?;
        parts.push(json_scalar_to_stable_key(value)?);
    }
    if parts.len() == 1 {
        Ok(parts.remove(0))
    } else {
        Ok(StableKey::Array(Arc::from(parts)))
    }
}

fn stable_key_to_pk(key: &StableKey) -> Result<Vec<JsonValue>> {
    match key {
        StableKey::Array(parts) => parts.iter().map(stable_key_to_json).collect(),
        other => Ok(vec![stable_key_to_json(other)?]),
    }
}

fn stable_key_to_json(key: &StableKey) -> Result<JsonValue> {
    match key {
        StableKey::Int(i) => Ok(JsonValue::from(*i)),
        StableKey::Str(s) | StableKey::Symbol(s) => Ok(JsonValue::from(s.to_string())),
        StableKey::Uuid(u) => Ok(JsonValue::from(u.to_string())),
        other => Err(Error::engine(format!("unsupported BigQuery row key: {other:?}"))),
    }
}

fn json_scalar_to_stable_key(value: &JsonValue) -> Result<StableKey> {
    match value {
        JsonValue::String(s) => Ok(StableKey::Str(Arc::from(s.clone()))),
        JsonValue::Number(n) => n
            .as_i64()
            .map(StableKey::Int)
            .ok_or_else(|| Error::engine(format!("unsupported numeric primary key: {n}"))),
        JsonValue::Bool(b) => Ok(StableKey::Str(Arc::from(b.to_string()))),
        JsonValue::Null => Err(Error::engine("primary key value cannot be null")),
        other => Err(Error::engine(format!(
            "primary key value must be scalar, got {other}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate_bigquery_type(value: &str) -> Result<()> {
    if value.is_empty()
        || !value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '(' | ')' | ',' | '<' | '>' | ' ')
        })
    {
        return Err(Error::engine(format!("invalid BigQuery type: {value}")));
    }
    Ok(())
}

fn validate_project_id(project: &str) -> Result<()> {
    let ok = !project.is_empty()
        && project
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
    if ok {
        Ok(())
    } else {
        Err(Error::engine(format!("invalid BigQuery project: {project}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> TableSchema {
        TableSchema::new(
            [
                ("id", ColumnDef::new("STRING")),
                ("name", ColumnDef::new("STRING")),
                ("value", ColumnDef::new("INT64")),
            ],
            ["id"],
        )
        .unwrap()
    }

    #[test]
    fn merge_sql_builds_upsert_with_inline_literals() {
        let schema = schema();
        let mut row = Map::new();
        row.insert("id".into(), JsonValue::from("a"));
        row.insert("name".into(), JsonValue::from("b'c"));
        row.insert("value".into(), JsonValue::from(7));
        let sql = merge_sql("`p.d.t`", &schema, &row);
        assert!(sql.contains("MERGE `p.d.t` AS target"), "{sql}");
        assert!(sql.contains("'b\\'c' AS `name`"), "{sql}");
        assert!(sql.contains("7 AS `value`"), "{sql}");
        assert!(sql.contains("WHEN MATCHED THEN UPDATE SET"), "{sql}");
        assert!(sql.contains("WHEN NOT MATCHED THEN INSERT"), "{sql}");
    }

    #[test]
    fn delete_sql_single_and_composite_keys() {
        let single = delete_sql("`t`", &["id".to_string()], &[vec![JsonValue::from("x")]]);
        assert_eq!(single, "DELETE FROM `t` WHERE `id` IN ('x')");
        let composite = delete_sql(
            "`t`",
            &["a".to_string(), "b".to_string()],
            &[vec![JsonValue::from(1), JsonValue::from("y")]],
        );
        assert_eq!(composite, "DELETE FROM `t` WHERE (`a` = 1 AND `b` = 'y')");
    }

    #[test]
    fn json_column_uses_parse_json() {
        let col = ColumnDef::json();
        let lit = bq_literal(&col, &serde_json::json!({"k": 1}));
        assert!(lit.starts_with("PARSE_JSON('"), "{lit}");
    }

    #[test]
    fn schema_rejects_pk_not_in_columns() {
        assert!(TableSchema::new([("id", ColumnDef::new("INT64"))], ["missing"]).is_err());
    }
}
