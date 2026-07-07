//! Snowflake table target connector.
//!
//! Table targets reconcile declared rows against the previous run: changed rows
//! are upserted (via `MERGE INTO`), unchanged rows are skipped, and rows no
//! longer declared are deleted. `managed_by` controls whether Grepify owns table
//! DDL.
//!
//! Mirrors Python's `snowflake` connector. The two-level shape matches the other
//! SQL targets (`postgres`, `sqlite`, `doris`, `bigquery`): a container
//! [`TableHandler`] owns table DDL and yields a per-row [`RowHandler`] child.
//! Statements run through the undocumented driver API exposed by the
//! [`snowflake_api`] crate.
//!
//! `snowflake_api::exec` has no bind-parameter support, so DML is built with
//! inline, escaped SQL literals (`VARIANT` columns via `PARSE_JSON('...')`) —
//! the same approach as the Doris `DELETE` path.
//!
//! Use [`table_target`] to build a composable target state,
//! [`declare_table_target`] inside the current component, or
//! [`mount_table_target`] when rows must be declared immediately.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use grepify_utils::fingerprint::Fingerprint;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as JsonValue};
use snowflake_api::SnowflakeApi;

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

/// Connection configuration for Snowflake. Mirrors Python's `ConnectionConfig`.
#[derive(Clone, Debug)]
pub struct SnowflakeConfig {
    pub account: String,
    pub user: String,
    pub password: String,
    pub warehouse: Option<String>,
    pub role: Option<String>,
}

impl SnowflakeConfig {
    pub fn new(
        account: impl Into<String>,
        user: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            account: account.into(),
            user: user.into(),
            password: password.into(),
            warehouse: None,
            role: None,
        }
    }

    pub fn warehouse(mut self, warehouse: impl Into<String>) -> Self {
        self.warehouse = Some(warehouse.into());
        self
    }

    pub fn role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }
}

/// A Snowflake connection handle. Clone-cheap (the API client / session pool is
/// shared). `state_id` (`account`, credentials excluded) is the stable identity
/// used for target-state keys.
///
/// Database and schema are *not* bound to the connection: they qualify each
/// table name, matching Python (which connects with only account / user /
/// password / warehouse / role).
#[derive(Clone)]
pub struct SnowflakeConnection {
    api: Arc<SnowflakeApi>,
    state_id: Arc<str>,
}

impl SnowflakeConnection {
    /// Connect to Snowflake with password auth. Authentication is deferred to
    /// the first statement, so this does no network I/O.
    pub fn connect(config: SnowflakeConfig) -> Result<Self> {
        let api = SnowflakeApi::with_password_auth(
            &config.account,
            config.warehouse.as_deref(),
            None,
            None,
            &config.user,
            config.role.as_deref(),
            &config.password,
        )
        .map_err(sf_err)?;
        let state_id = format!("snowflake:{}", config.account);
        Ok(Self {
            api: Arc::new(api),
            state_id: Arc::from(state_id),
        })
    }

    /// Wrap an already-built [`SnowflakeApi`] (advanced use / tests).
    pub fn from_api(account: impl Into<String>, api: SnowflakeApi) -> Self {
        let state_id = format!("snowflake:{}", account.into());
        Self {
            api: Arc::new(api),
            state_id: Arc::from(state_id),
        }
    }

    pub fn state_id(&self) -> &str {
        &self.state_id
    }

    async fn exec(&self, sql: String) -> Result<()> {
        self.api.exec(&sql).await.map_err(sf_err)?;
        Ok(())
    }
}

fn sf_err(e: snowflake_api::SnowflakeApiError) -> Error {
    Error::engine(format!("snowflake: {e}"))
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// A Snowflake column. `snowflake_type` is the SQL type as written in
/// `CREATE TABLE` (e.g. `NUMBER`, `VARCHAR`, `TIMESTAMP_TZ`, `VARIANT`). Set
/// `use_parse_json` for `VARIANT`-style columns fed from JSON text via
/// `PARSE_JSON`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ColumnDef {
    pub snowflake_type: String,
    pub nullable: bool,
    pub use_parse_json: bool,
}

impl ColumnDef {
    pub fn new(snowflake_type: impl Into<String>) -> Self {
        Self {
            snowflake_type: snowflake_type.into(),
            nullable: true,
            use_parse_json: false,
        }
    }

    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    /// A `VARIANT` column whose value is provided as JSON text and wrapped in
    /// `PARSE_JSON(...)`.
    pub fn variant() -> Self {
        Self {
            snowflake_type: "VARIANT".to_string(),
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
            validate_snowflake_type(&def.snowflake_type)?;
            out.insert(name, def);
        }
        let primary_key: Vec<String> = primary_key.into_iter().map(Into::into).collect();
        if primary_key.is_empty() {
            return Err(Error::engine("Snowflake table primary key cannot be empty"));
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
            .map(|f| (f.name.clone(), snowflake_column_def(&f)));
        Self::new(columns, primary_key)
    }
}

/// Map a connector-agnostic [`SchemaField`](crate::row_schema::SchemaField) to a
/// Snowflake [`ColumnDef`], mirroring Python's `_LEAF_TYPE_MAPPINGS`.
fn snowflake_column_def(field: &crate::row_schema::SchemaField) -> ColumnDef {
    use crate::row_schema::LogicalType as L;
    let (ty, use_parse_json) = match &field.logical_type {
        L::Bool => ("BOOLEAN", false),
        L::Int16 | L::Int32 | L::Int64 | L::Decimal | L::Duration => ("NUMBER", false),
        L::Float32 | L::Float64 => ("FLOAT", false),
        L::Text => ("VARCHAR", false),
        L::Bytes => ("BINARY", false),
        L::Uuid => ("VARCHAR", false),
        L::Date => ("DATE", false),
        L::Time => ("TIME", false),
        L::DateTime => ("TIMESTAMP_TZ", false),
        L::Json | L::Vector { .. } => ("VARIANT", true),
        L::Custom(s) => (s.as_str(), false),
    };
    ColumnDef {
        snowflake_type: ty.to_string(),
        nullable: field.nullable,
        use_parse_json,
    }
}

/// Options for the `*_with_options` table-target constructors.
#[derive(Clone, Debug, Default)]
pub struct SnowflakeTableOptions {
    pub managed_by: ManagedBy,
    /// Optional database qualifier (created if managed).
    pub database: Option<String>,
    /// Optional schema qualifier (created if managed).
    pub schema: Option<String>,
}

// ---------------------------------------------------------------------------
// Public target API
// ---------------------------------------------------------------------------

/// A declarative Snowflake table target — a handle to declare rows on.
#[derive(Clone)]
pub struct TableTarget {
    table_schema: TableSchema,
    rows: TargetStateProvider<RowState>,
}

/// Build a composable [`TargetState`] for a Snowflake table.
pub fn table_target(
    ctx: &Ctx,
    conn: &ContextKey<SnowflakeConnection>,
    table_name: impl Into<String>,
    table_schema: TableSchema,
    options: SnowflakeTableOptions,
) -> Result<TargetState<TableSpec>> {
    let table_name = table_name.into();
    validate_ident(&table_name, "table name")?;
    if let Some(database) = &options.database {
        validate_ident(database, "database name")?;
    }
    if let Some(schema) = &options.schema {
        validate_ident(schema, "schema name")?;
    }
    let provider = register_root_target_states_provider(
        ctx,
        format!(
            "grepify/snowflake/table/{}/{}/{}/{}",
            conn.name(),
            options.database.as_deref().unwrap_or(""),
            options.schema.as_deref().unwrap_or(""),
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
            database: options.database,
            schema: options.schema,
            table_schema,
            managed_by: options.managed_by,
        },
    ))
}

/// Declare a Snowflake table target in the current component and return a handle.
pub fn declare_table_target(
    ctx: &Ctx,
    conn: &ContextKey<SnowflakeConnection>,
    table_name: impl Into<String>,
    table_schema: TableSchema,
    options: SnowflakeTableOptions,
) -> Result<TableTarget> {
    let ts = table_target(ctx, conn, table_name, table_schema, options)?;
    let spec = ts.value().clone();
    let rows = declare_target_state_with_child::<TableSpec, RowState>(ctx, ts)?;
    Ok(TableTarget {
        table_schema: spec.table_schema,
        rows,
    })
}

/// Mount a Snowflake table target foreground (rows can be declared immediately).
pub async fn mount_table_target(
    ctx: &Ctx,
    conn: &ContextKey<SnowflakeConnection>,
    table_name: impl Into<String>,
    table_schema: TableSchema,
    options: SnowflakeTableOptions,
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
    #[serde(default)]
    database: Option<String>,
    #[serde(default)]
    schema: Option<String>,
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
    snowflake_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct TablePrimaryTrackingRecord {
    table_name: String,
    database: Option<String>,
    schema: Option<String>,
    primary_key_columns: Vec<PkColumnInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct NonPkColumnTrackingRecord {
    snowflake_type: String,
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
        database: spec.database.clone(),
        schema: spec.schema.clone(),
        primary_key_columns: schema
            .primary_key()
            .iter()
            .map(|name| PkColumnInfo {
                name: name.clone(),
                snowflake_type: schema.columns()[name].snowflake_type.clone(),
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
                    snowflake_type: col.snowflake_type.clone(),
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

fn resolve_conn(host_ctx: &Arc<ContextStore>, conn_key: &str) -> Result<Arc<SnowflakeConnection>> {
    host_ctx
        .resolve::<SnowflakeConnection>(conn_key)
        .ok_or_else(|| {
            Error::engine(format!(
                "snowflake target: connection `{conn_key}` was not provided in the environment \
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
                            database: record.database,
                            schema: record.schema,
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
    conn: &SnowflakeConnection,
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

fn quote_ident(name: &str) -> String {
    format!("\"{name}\"")
}

fn qualified_table_name(spec: &TableSpec) -> String {
    let mut parts = Vec::new();
    if let Some(db) = &spec.database {
        parts.push(quote_ident(db));
    }
    if let Some(schema) = &spec.schema {
        parts.push(quote_ident(schema));
    }
    parts.push(quote_ident(&spec.table_name));
    parts.join(".")
}

async fn create_table(
    conn: &SnowflakeConnection,
    spec: &TableSpec,
    if_not_exists: bool,
) -> Result<()> {
    if spec.managed_by.is_user() {
        return Ok(());
    }
    if let Some(db) = &spec.database {
        conn.exec(format!("CREATE DATABASE IF NOT EXISTS {}", quote_ident(db)))
            .await?;
    }
    if let Some(schema) = &spec.schema {
        let qualified = match &spec.database {
            Some(db) => format!("{}.{}", quote_ident(db), quote_ident(schema)),
            None => quote_ident(schema),
        };
        conn.exec(format!("CREATE SCHEMA IF NOT EXISTS {qualified}"))
            .await?;
    }

    let schema = &spec.table_schema;
    let pk: HashSet<&String> = schema.primary_key().iter().collect();
    let mut col_defs: Vec<String> = Vec::new();
    for (name, col) in schema.columns() {
        col_defs.push(column_sql(name, col, &pk));
    }
    let pk_cols = schema
        .primary_key()
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    col_defs.push(format!("PRIMARY KEY ({pk_cols})"));

    let if_not_exists_sql = if if_not_exists { " IF NOT EXISTS" } else { "" };
    conn.exec(format!(
        "CREATE TABLE{if_not_exists_sql} {} ({})",
        qualified_table_name(spec),
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
    format!("{} {}{nullable}", quote_ident(col_name), col.snowflake_type)
}

async fn drop_table(conn: &SnowflakeConnection, spec: &TableSpec) -> Result<()> {
    if spec.managed_by.is_user() {
        return Ok(());
    }
    conn.exec(format!(
        "DROP TABLE IF EXISTS {}",
        qualified_table_name(spec)
    ))
    .await
}

async fn apply_column_actions(
    conn: &SnowflakeConnection,
    spec: &TableSpec,
    column_actions: &BTreeMap<String, DiffAction>,
) -> Result<()> {
    if spec.managed_by.is_user() {
        return Ok(());
    }
    let qualified = qualified_table_name(spec);
    let schema = &spec.table_schema;
    let pk: HashSet<&String> = schema.primary_key().iter().collect();

    for (sub_key, action) in column_actions {
        let Some(col_name) = sub_key.strip_prefix(COL_SUBKEY_PREFIX) else {
            return Err(Error::engine(format!(
                "snowflake column action has unexpected sub-key {sub_key:?}"
            )));
        };
        if pk.contains(&col_name.to_string()) {
            continue;
        }
        match action {
            DiffAction::Delete => {
                conn.exec(format!(
                    "ALTER TABLE {qualified} DROP COLUMN IF EXISTS {}",
                    quote_ident(col_name)
                ))
                .await?;
            }
            DiffAction::Insert => {
                if let Some(col) = schema.columns().get(col_name) {
                    conn.exec(format!(
                        "ALTER TABLE {qualified} ADD COLUMN {}",
                        column_sql(col_name, col, &pk)
                    ))
                    .await?;
                }
            }
            DiffAction::Upsert => {
                if let Some(col) = schema.columns().get(col_name) {
                    conn.exec(format!(
                        "ALTER TABLE {qualified} ADD COLUMN IF NOT EXISTS {}",
                        column_sql(col_name, col, &pk)
                    ))
                    .await?;
                }
            }
            DiffAction::Replace => {
                if let Some(col) = schema.columns().get(col_name) {
                    conn.exec(format!(
                        "ALTER TABLE {qualified} DROP COLUMN IF EXISTS {}",
                        quote_ident(col_name)
                    ))
                    .await?;
                    conn.exec(format!(
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
    conn: &SnowflakeConnection,
    spec: &TableSpec,
    mutations: Vec<(Vec<JsonValue>, Option<RowState>)>,
) -> Result<()> {
    if mutations.is_empty() {
        return Ok(());
    }
    let qualified = qualified_table_name(spec);
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
        conn.exec(merge_sql(&qualified, schema, row)).await?;
    }
    if !delete_keys.is_empty() {
        conn.exec(delete_sql(&qualified, pk_cols, &delete_keys))
            .await?;
    }
    Ok(())
}

/// Build a single-row `MERGE INTO` with inline literals for `row`.
fn merge_sql(qualified: &str, schema: &TableSchema, row: &Map<String, JsonValue>) -> String {
    let all_cols: Vec<&String> = schema.columns().keys().collect();
    let pk_cols: HashSet<&String> = schema.primary_key().iter().collect();

    let source_cols = schema
        .columns()
        .iter()
        .map(|(name, col)| {
            let value = row.get(name).unwrap_or(&JsonValue::Null);
            format!("{} AS {}", sf_literal(col, value), quote_ident(name))
        })
        .collect::<Vec<_>>()
        .join(", ");

    let on_clause = schema
        .primary_key()
        .iter()
        .map(|c| format!("target.{q} = source.{q}", q = quote_ident(c)))
        .collect::<Vec<_>>()
        .join(" AND ");
    let insert_cols = all_cols
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_values = all_cols
        .iter()
        .map(|c| format!("source.{}", quote_ident(c)))
        .collect::<Vec<_>>()
        .join(", ");

    let mut sql = format!(
        "MERGE INTO {qualified} AS target USING (SELECT {source_cols}) AS source ON {on_clause}"
    );
    let non_pk: Vec<&&String> = all_cols.iter().filter(|c| !pk_cols.contains(**c)).collect();
    if !non_pk.is_empty() {
        let update_list = non_pk
            .iter()
            .map(|c| format!("{q} = source.{q}", q = quote_ident(c)))
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
        let col = quote_ident(&pk_cols[0]);
        let markers = keys
            .iter()
            .map(|k| scalar_literal(k.first().unwrap_or(&JsonValue::Null)))
            .collect::<Vec<_>>()
            .join(", ");
        return format!("DELETE FROM {qualified} WHERE {col} IN ({markers})");
    }
    let groups = keys
        .iter()
        .map(|key| {
            let parts = pk_cols
                .iter()
                .zip(key)
                .map(|(col, val)| {
                    let q = quote_ident(col);
                    if val.is_null() {
                        format!("{q} IS NULL")
                    } else {
                        format!("{q} = {}", scalar_literal(val))
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

/// Render a value as a Snowflake SQL literal honoring the column's `PARSE_JSON`.
fn sf_literal(col: &ColumnDef, value: &JsonValue) -> String {
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
        .map_err(|e| Error::engine(format!("Snowflake target row has a {e}")))?;
    let value = serde_json::to_value(row)
        .map_err(|e| Error::engine(format!("serialize Snowflake target row: {e}")))?;
    let JsonValue::Object(mut fields) = value else {
        return Err(Error::engine(
            "Snowflake target row must serialize to an object",
        ));
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
        other => Err(Error::engine(format!(
            "unsupported Snowflake row key: {other:?}"
        ))),
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

fn validate_snowflake_type(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '(' | ')' | ',' | ' '))
    {
        return Err(Error::engine(format!("invalid Snowflake type: {value}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> TableSchema {
        TableSchema::new(
            [
                ("id", ColumnDef::new("VARCHAR")),
                ("name", ColumnDef::new("VARCHAR")),
                ("value", ColumnDef::new("NUMBER")),
            ],
            ["id"],
        )
        .unwrap()
    }

    fn spec(table_schema: TableSchema) -> TableSpec {
        TableSpec {
            table_name: "items".to_string(),
            database: Some("db".to_string()),
            schema: Some("sc".to_string()),
            table_schema,
            managed_by: ManagedBy::System,
        }
    }

    #[test]
    fn qualified_name_quotes_all_parts() {
        assert_eq!(
            qualified_table_name(&spec(schema())),
            "\"db\".\"sc\".\"items\""
        );
    }

    #[test]
    fn merge_sql_builds_upsert_with_inline_literals() {
        let schema = schema();
        let mut row = Map::new();
        row.insert("id".into(), JsonValue::from("a"));
        row.insert("name".into(), JsonValue::from("b'c"));
        row.insert("value".into(), JsonValue::from(7));
        let sql = merge_sql("\"t\"", &schema, &row);
        assert!(sql.contains("MERGE INTO \"t\" AS target"), "{sql}");
        assert!(sql.contains("'b\\'c' AS \"name\""), "{sql}");
        assert!(sql.contains("WHEN MATCHED THEN UPDATE SET"), "{sql}");
        assert!(sql.contains("WHEN NOT MATCHED THEN INSERT"), "{sql}");
    }

    #[test]
    fn delete_sql_single_and_composite_keys() {
        let single = delete_sql("\"t\"", &["id".to_string()], &[vec![JsonValue::from("x")]]);
        assert_eq!(single, "DELETE FROM \"t\" WHERE \"id\" IN ('x')");
        let composite = delete_sql(
            "\"t\"",
            &["a".to_string(), "b".to_string()],
            &[vec![JsonValue::from(1), JsonValue::from("y")]],
        );
        assert_eq!(
            composite,
            "DELETE FROM \"t\" WHERE (\"a\" = 1 AND \"b\" = 'y')"
        );
    }

    #[test]
    fn variant_column_uses_parse_json() {
        let col = ColumnDef::variant();
        let lit = sf_literal(&col, &serde_json::json!({"k": 1}));
        assert!(lit.starts_with("PARSE_JSON('"), "{lit}");
    }

    #[test]
    fn schema_rejects_pk_not_in_columns() {
        assert!(TableSchema::new([("id", ColumnDef::new("NUMBER"))], ["missing"]).is_err());
    }
}
