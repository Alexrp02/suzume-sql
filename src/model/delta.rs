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

/// All pending changes for one row, ready to be compiled into a single
/// parameterised `UPDATE`.
#[derive(Debug, Clone, PartialEq)]
pub struct RowMutation {
    pub table: String,
    pub key: RowKey,
    pub changes: Vec<CellDelta>,
}
