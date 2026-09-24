//! Cached schema metadata harvested from the backend catalog.

use std::fmt;

use crate::model::value::TypeAffinity;

/// The namespace relations are harvested from: a Postgres schema or a MySQL
/// database. SQLite has a single implicit namespace, reported as `main`.
///
/// The name is only ever interpolated into a quoted identifier (never into a
/// value position), so it is kept as a distinct type to make that boundary
/// explicit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SchemaName(String);

impl SchemaName {
    pub fn new(name: impl Into<String>) -> SchemaName {
        SchemaName(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SchemaName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether a relation can be mutated in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    Table,
    View,
}

/// One column of a table or view.
#[derive(Debug, Clone)]
pub struct ColumnMeta {
    pub name: String,
    /// The declared type name as reported by the catalog (e.g. `INTEGER`,
    /// `character varying`). Used verbatim as the Postgres cast target.
    pub declared_type: String,
    pub affinity: TypeAffinity,
    pub is_primary_key: bool,
    /// True when the server supplies a value if the column is left out of an
    /// INSERT (a `DEFAULT` clause, an identity/serial column, or SQLite's
    /// `INTEGER PRIMARY KEY` rowid alias).
    pub has_default: bool,
}

/// A table or view plus its columns.
#[derive(Debug, Clone)]
pub struct TableMeta {
    pub name: String,
    pub kind: RelationKind,
    pub columns: Vec<ColumnMeta>,
}

impl TableMeta {
    /// Views are always read-only in this build.
    pub fn is_editable(&self) -> bool {
        matches!(self.kind, RelationKind::Table)
    }

    pub fn column(&self, name: &str) -> Option<&ColumnMeta> {
        self.columns.iter().find(|c| c.name == name)
    }
}

/// The full catalog of relations in the connected database.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub tables: Vec<TableMeta>,
}

impl Catalog {
    pub fn find(&self, name: &str) -> Option<&TableMeta> {
        self.tables.iter().find(|t| t.name == name)
    }
}
