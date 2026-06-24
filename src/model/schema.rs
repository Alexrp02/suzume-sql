//! Cached schema metadata harvested from the backend catalog.

use crate::model::value::TypeAffinity;

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
