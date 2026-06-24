//! Database access layer: a uniform engine trait with per-backend impls.

pub mod postgres;
pub mod query;
pub mod sqlite;

use crate::config::ConnectionConfig;
use crate::error::DbError;
use crate::model::delta::RowMutation;
use crate::model::schema::Catalog;
use crate::model::value::Value;
use query::SelectQuery;

/// Maximum number of rows materialised from a raw custom query, to keep the
/// TUI responsive if the user forgets a `LIMIT`.
pub const RAW_ROW_CAP: usize = 1000;

/// The result of a raw custom query. Columns are discovered from the result
/// set (a custom query is not tied to a known table), and all values are
/// rendered read-only.
#[derive(Debug, Clone, Default)]
pub struct RawResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// True when the result was capped at [`RAW_ROW_CAP`].
    pub truncated: bool,
}

/// A live connection to one database backend.
///
/// All methods are blocking; the trait object lives on a dedicated worker
/// thread (see [`crate::worker`]) so the UI loop never blocks.
pub trait DatabaseEngine: Send {
    /// Introspect the catalog: tables, views, columns, types, primary keys.
    fn harvest_schema(&mut self) -> Result<Catalog, DbError>;

    /// Run a browse query and return its rows. The returned rows have one
    /// [`Value`] per column in `query.columns` order.
    fn run_select(&mut self, query: &SelectQuery) -> Result<Vec<Vec<Value>>, DbError>;

    /// Run an arbitrary user-supplied SQL statement and return its result set.
    /// Used by the query pane; results are always displayed read-only.
    fn run_raw(&mut self, sql: &str) -> Result<RawResult, DbError>;

    /// Apply all pending mutations inside a single transaction. Any failure —
    /// including a full-row match touching more than one row — rolls the whole
    /// batch back.
    fn commit(&mut self, mutations: &[RowMutation], catalog: &Catalog) -> Result<(), DbError>;
}

/// Open a connection for the given config, returning a boxed engine.
pub fn connect(config: &ConnectionConfig) -> Result<Box<dyn DatabaseEngine>, DbError> {
    match config {
        ConnectionConfig::Sqlite { path } => {
            let engine = sqlite::SqliteEngine::connect(path)?;
            Ok(Box::new(engine))
        }
        ConnectionConfig::Postgres { url } => {
            let engine = postgres::PostgresEngine::connect(url)?;
            Ok(Box::new(engine))
        }
    }
}
