//! SQLite backend, built on bundled `rusqlite`.

use rusqlite::types::{ToSqlOutput, Value as SqliteValue, ValueRef};
use rusqlite::{Connection, ToSql, params_from_iter};

use crate::db::query::{Dialect, SelectQuery, build_statement};
use crate::db::{DatabaseEngine, RAW_ROW_CAP, RawResult};
use crate::error::DbError;
use crate::model::delta::RowMutation;
use crate::model::schema::{Catalog, ColumnMeta, RelationKind, TableMeta};
use crate::model::value::{TypeAffinity, Value};

pub struct SqliteEngine {
    conn: Connection,
}

impl SqliteEngine {
    pub fn connect(path: &str) -> Result<SqliteEngine, DbError> {
        let conn = Connection::open(path).map_err(|e| DbError::Connect(e.to_string()))?;
        Ok(SqliteEngine { conn })
    }
}

impl DatabaseEngine for SqliteEngine {
    fn harvest_schema(&mut self) -> Result<Catalog, DbError> {
        let mut relations: Vec<(String, RelationKind)> = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT name, type FROM sqlite_master \
                     WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' \
                     ORDER BY name",
                )
                .map_err(|e| DbError::Schema(e.to_string()))?;
            let mut rows = stmt
                .query([])
                .map_err(|e| DbError::Schema(e.to_string()))?;
            while let Some(row) = rows.next().map_err(|e| DbError::Schema(e.to_string()))? {
                let name: String = row.get(0).map_err(|e| DbError::Schema(e.to_string()))?;
                let kind: String = row.get(1).map_err(|e| DbError::Schema(e.to_string()))?;
                let kind = if kind == "view" {
                    RelationKind::View
                } else {
                    RelationKind::Table
                };
                relations.push((name, kind));
            }
        }

        let mut tables = Vec::with_capacity(relations.len());
        for (name, kind) in relations {
            let columns = self.columns_for(&name)?;
            tables.push(TableMeta {
                name,
                kind,
                columns,
            });
        }
        Ok(Catalog { tables })
    }

    fn run_select(&mut self, query: &SelectQuery) -> Result<Vec<Vec<Value>>, DbError> {
        let sql = query.render(Dialect::Sqlite);
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| DbError::Query(e.to_string()))?;
        let column_count = stmt.column_count();
        let mut out = Vec::new();
        let mut rows = stmt
            .query([])
            .map_err(|e| DbError::Query(e.to_string()))?;
        while let Some(row) = rows.next().map_err(|e| DbError::Query(e.to_string()))? {
            let mut values = Vec::with_capacity(column_count);
            for i in 0..column_count {
                let value_ref = row
                    .get_ref(i)
                    .map_err(|e| DbError::Query(e.to_string()))?;
                values.push(Value::from(value_ref));
            }
            out.push(values);
        }
        Ok(out)
    }

    fn run_raw(&mut self, sql: &str) -> Result<RawResult, DbError> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|e| DbError::Query(e.to_string()))?;
        let columns: Vec<String> = stmt
            .column_names()
            .into_iter()
            .map(str::to_string)
            .collect();
        let column_count = stmt.column_count();

        let mut out = Vec::new();
        let mut truncated = false;
        let mut rows = stmt
            .query([])
            .map_err(|e| DbError::Query(e.to_string()))?;
        while let Some(row) = rows.next().map_err(|e| DbError::Query(e.to_string()))? {
            if out.len() >= RAW_ROW_CAP {
                truncated = true;
                break;
            }
            let mut values = Vec::with_capacity(column_count);
            for i in 0..column_count {
                let value_ref = row
                    .get_ref(i)
                    .map_err(|e| DbError::Query(e.to_string()))?;
                values.push(Value::from(value_ref));
            }
            out.push(values);
        }
        Ok(RawResult {
            columns,
            rows: out,
            truncated,
        })
    }

    fn commit(&mut self, mutations: &[RowMutation], catalog: &Catalog) -> Result<(), DbError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| DbError::Commit(e.to_string()))?;
        for mutation in mutations {
            let table_meta = catalog.find(mutation.table()).ok_or_else(|| {
                DbError::Commit(format!("unknown table `{}`", mutation.table()))
            })?;
            let stmt = build_statement(Dialect::Sqlite, table_meta, mutation);
            let affected = tx
                .execute(&stmt.sql, params_from_iter(&stmt.params))
                .map_err(|e| DbError::Commit(e.to_string()))?;
            if stmt.requires_single_row_check && affected != 1 {
                // Dropping `tx` without commit rolls the whole batch back.
                return Err(DbError::AmbiguousMatch {
                    table: mutation.table().to_string(),
                    matched: affected as u64,
                });
            }
        }
        tx.commit().map_err(|e| DbError::Commit(e.to_string()))?;
        Ok(())
    }
}

impl SqliteEngine {
    fn columns_for(&self, table: &str) -> Result<Vec<ColumnMeta>, DbError> {
        let escaped = table.replace('"', "\"\"");
        let sql = format!("PRAGMA table_info(\"{escaped}\")");
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| DbError::Schema(e.to_string()))?;
        let mut columns = Vec::new();
        let mut rows = stmt
            .query([])
            .map_err(|e| DbError::Schema(e.to_string()))?;
        while let Some(row) = rows.next().map_err(|e| DbError::Schema(e.to_string()))? {
            // table_info columns: cid, name, type, notnull, dflt_value, pk
            let name: String = row.get(1).map_err(|e| DbError::Schema(e.to_string()))?;
            let declared: String = row.get(2).map_err(|e| DbError::Schema(e.to_string()))?;
            let pk: i64 = row.get(5).map_err(|e| DbError::Schema(e.to_string()))?;
            columns.push(ColumnMeta {
                affinity: TypeAffinity::from_declared(&declared),
                name,
                declared_type: declared,
                is_primary_key: pk > 0,
            });
        }
        Ok(columns)
    }
}

// These conversions live in the DB layer (not in `model`) so `Value` stays
// free of any rusqlite coupling. The orphan rule allows them here because
// `Value` is a crate-local type — coherence is per-crate, not per-module.

/// Read path: a borrowed SQLite value becomes an owned [`Value`].
impl From<ValueRef<'_>> for Value {
    fn from(value_ref: ValueRef<'_>) -> Value {
        match value_ref {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(n) => Value::Integer(n),
            ValueRef::Real(f) => Value::Real(f),
            ValueRef::Text(bytes) => Value::Text(String::from_utf8_lossy(bytes).into_owned()),
            ValueRef::Blob(bytes) => Value::Blob(bytes.to_vec()),
        }
    }
}

/// Write path: bind a [`Value`] directly as a SQL parameter. Implementing
/// rusqlite's own `ToSql` (rather than a `Value → rusqlite::Value` conversion,
/// which the orphan rule forbids) lets us pass `Value`s straight to
/// `params_from_iter` and borrows where possible instead of cloning.
impl ToSql for Value {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        let output = match self {
            Value::Null => ToSqlOutput::Borrowed(ValueRef::Null),
            Value::Integer(n) => ToSqlOutput::Borrowed(ValueRef::Integer(*n)),
            Value::Real(f) => ToSqlOutput::Borrowed(ValueRef::Real(*f)),
            Value::Text(s) => ToSqlOutput::Borrowed(ValueRef::Text(s.as_bytes())),
            Value::Boolean(b) => ToSqlOutput::Owned(SqliteValue::Integer(i64::from(*b))),
            Value::Blob(bytes) => ToSqlOutput::Borrowed(ValueRef::Blob(bytes)),
            Value::Json(s) => ToSqlOutput::Borrowed(ValueRef::Text(s.as_bytes())),
        };
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::delta::{CellDelta, KeyPart, RowKey};

    fn select_all(table: &str, columns: &[&str], filter: Option<&str>, order: Option<&str>) -> SelectQuery {
        SelectQuery {
            table: table.to_string(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            filter: filter.map(str::to_string),
            order_by: order.map(str::to_string),
            limit: 100,
        }
    }

    #[test]
    fn harvest_select_and_commit_round_trip() {
        let mut engine = SqliteEngine::connect(":memory:").expect("open in-memory db");
        engine
            .conn
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER);\
                 INSERT INTO users VALUES (1, 'Alejandro', 31), (2, 'John', 35);",
            )
            .expect("seed schema");

        // Schema harvest detects the table, columns, and primary key.
        let catalog = engine.harvest_schema().expect("harvest");
        let users = catalog.find("users").expect("users table");
        assert_eq!(users.columns.len(), 3);
        assert!(users.column("id").expect("id col").is_primary_key);
        assert!(!users.column("name").expect("name col").is_primary_key);

        // Filter + order are honoured.
        let rows = engine
            .run_select(&select_all(
                "users",
                &["id", "name", "age"],
                Some("age > 30"),
                Some("id DESC"),
            ))
            .expect("select");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], Value::Integer(2)); // DESC: id 2 first

        // Commit a primary-key update.
        let mutation = RowMutation::Update {
            table: "users".to_string(),
            key: RowKey::PrimaryKey(vec![KeyPart {
                column: "id".to_string(),
                value: Value::Integer(1),
            }]),
            changes: vec![CellDelta {
                column: "name".to_string(),
                original: Value::Text("Alejandro".to_string()),
                new: Value::Text("Alex".to_string()),
            }],
        };
        engine.commit(&[mutation], &catalog).expect("commit");

        // The change is persisted.
        let rows = engine
            .run_select(&select_all("users", &["name"], Some("id = 1"), None))
            .expect("verify select");
        assert_eq!(rows, vec![vec![Value::Text("Alex".to_string())]]);
    }

    #[test]
    fn run_raw_returns_result_columns_and_rows() {
        let mut engine = SqliteEngine::connect(":memory:").expect("open");
        engine
            .conn
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER);\
                 INSERT INTO users VALUES (1, 'Alejandro', 31), (2, 'John', 35);",
            )
            .expect("seed");

        // Column names come from the result set, including an alias on an
        // expression that doesn't exist as a real column.
        let result = engine
            .run_raw("SELECT name, age * 2 AS double_age FROM users WHERE age > 30 ORDER BY id")
            .expect("raw query");
        assert_eq!(result.columns, vec!["name", "double_age"]);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0][0], Value::Text("Alejandro".to_string()));
        assert_eq!(result.rows[0][1], Value::Integer(62));
        assert!(!result.truncated);
    }

    #[test]
    fn full_row_update_matching_multiple_rows_rolls_back() {
        let mut engine = SqliteEngine::connect(":memory:").expect("open in-memory db");
        // No primary key, and two identical rows.
        engine
            .conn
            .execute_batch(
                "CREATE TABLE logs (msg TEXT);\
                 INSERT INTO logs VALUES ('dup'), ('dup');",
            )
            .expect("seed");
        let catalog = engine.harvest_schema().expect("harvest");

        let mutation = RowMutation::Update {
            table: "logs".to_string(),
            key: RowKey::FullRow(vec![KeyPart {
                column: "msg".to_string(),
                value: Value::Text("dup".to_string()),
            }]),
            changes: vec![CellDelta {
                column: "msg".to_string(),
                original: Value::Text("dup".to_string()),
                new: Value::Text("changed".to_string()),
            }],
        };
        let result = engine.commit(&[mutation], &catalog);
        assert!(matches!(
            result,
            Err(crate::error::DbError::AmbiguousMatch { matched: 2, .. })
        ));

        // Rolled back: both rows untouched.
        let rows = engine
            .run_select(&select_all("logs", &["msg"], None, None))
            .expect("verify");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r[0] == Value::Text("dup".to_string())));
    }

    #[test]
    fn delete_by_primary_key_round_trips() {
        let mut engine = SqliteEngine::connect(":memory:").expect("open in-memory db");
        engine
            .conn
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);\
                 INSERT INTO users VALUES (1, 'Alejandro'), (2, 'John');",
            )
            .expect("seed");
        let catalog = engine.harvest_schema().expect("harvest");

        let mutation = RowMutation::Delete {
            table: "users".to_string(),
            key: RowKey::PrimaryKey(vec![KeyPart {
                column: "id".to_string(),
                value: Value::Integer(1),
            }]),
        };
        engine.commit(&[mutation], &catalog).expect("commit delete");

        // Only the unmarked row remains.
        let rows = engine
            .run_select(&select_all("users", &["id", "name"], None, None))
            .expect("verify");
        assert_eq!(rows, vec![vec![
            Value::Integer(2),
            Value::Text("John".to_string()),
        ]]);
    }
}
