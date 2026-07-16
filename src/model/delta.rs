//! The transactional delta queue: pending in-memory edits awaiting commit.

use crate::model::value::Value;

/// One column/value pair used to locate a row in a `WHERE` clause.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyPart {
    pub column: String,
    /// The *original* (pre-edit) value, so the predicate still matches the row.
    pub value: Value,
}

/// How a pending row mutation identifies its target row.
///
/// The two cases are distinct states, not a nullable field: a row is either
/// addressable by primary key or it falls back to a full-row equality match.
#[derive(Debug, Clone, PartialEq)]
pub enum RowKey {
    /// Locate the row by its primary-key column(s).
    PrimaryKey(Vec<KeyPart>),
    /// No primary key: match on every column's original value. The engine must
    /// roll back if this matches more than one row.
    FullRow(Vec<KeyPart>),
}

/// A single column change within a row.
#[derive(Debug, Clone, PartialEq)]
pub struct CellDelta {
    pub column: String,
    pub original: Value,
    pub new: Value,
}

/// A pending mutation of one row, ready to be compiled into a single
/// parameterised statement.
///
/// A row is either being updated (one or more cell changes) or deleted outright
/// — two distinct states, not a struct with an optional "deleted" flag. Both
/// carry the [`RowKey`] that locates the target row.
#[derive(Debug, Clone, PartialEq)]
pub enum RowMutation {
    Update {
        table: String,
        key: RowKey,
        changes: Vec<CellDelta>,
    },
    Delete {
        table: String,
        key: RowKey,
    },
    Insert {
        table: String,
        row: Vec<Value>,
    },
}

impl RowMutation {
    pub fn table(&self) -> &str {
        match self {
            RowMutation::Update { table, .. }
            | RowMutation::Delete { table, .. }
            | RowMutation::Insert { table, .. } => table,
        }
    }
}
