//! Dialect-aware SQL generation.
//!
//! The TUI never asks the user to write SELECT/UPDATE boilerplate; it compiles
//! the execution-ready statement here from the cached schema plus the user's
//! filter/order fragments and the pending delta queue.

use crate::model::delta::{CellDelta, RowKey, RowMutation};
use crate::model::schema::TableMeta;
use crate::model::value::Value;

/// The SQL flavour a statement is being generated for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
    Mysql,
}

impl Dialect {
    /// Quote an identifier, escaping the embedded quote character by doubling it.
    /// SQLite and Postgres use double quotes; MySQL uses backticks (its default
    /// mode does not treat double quotes as identifier delimiters).
    pub fn quote_ident(&self, ident: &str) -> String {
        match self {
            Dialect::Mysql => {
                let escaped = ident.replace('`', "``");
                format!("`{escaped}`")
            }
            Dialect::Sqlite | Dialect::Postgres => {
                let escaped = ident.replace('"', "\"\"");
                format!("\"{escaped}\"")
            }
        }
    }
}

/// A browse query built from a table selection plus optional filter/order.
#[derive(Debug, Clone)]
pub struct SelectQuery {
    pub table: String,
    /// Explicit column list (from the schema), so Postgres can cast each to text.
    pub columns: Vec<String>,
    /// Raw `WHERE` fragment as typed by the user (already trusted, local).
    pub filter: Option<String>,
    /// Raw `ORDER BY` fragment as typed by the user.
    pub order_by: Option<String>,
    pub limit: u32,
}

impl SelectQuery {
    /// Render the executable SELECT string for the given dialect.
    pub fn render(&self, dialect: Dialect) -> String {
        let cols = if self.columns.is_empty() {
            "*".to_string()
        } else {
            self.columns
                .iter()
                .map(|c| match dialect {
                    // Cast every column to text so the Postgres read path is
                    // uniform and never trips over an exotic type's binary form.
                    Dialect::Postgres => format!("{}::text", dialect.quote_ident(c)),
                    // SQLite and MySQL decode native typed values directly.
                    Dialect::Sqlite | Dialect::Mysql => dialect.quote_ident(c),
                })
                .collect::<Vec<_>>()
                .join(", ")
        };

        let mut sql = format!("SELECT {cols} FROM {}", dialect.quote_ident(&self.table));
        if let Some(filter) = self
            .filter
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty())
        {
            sql.push_str(" WHERE ");
            sql.push_str(filter);
        }
        if let Some(order) = self
            .order_by
            .as_deref()
            .map(str::trim)
            .filter(|o| !o.is_empty())
        {
            sql.push_str(" ORDER BY ");
            sql.push_str(order);
        }
        sql.push_str(&format!(" LIMIT {}", self.limit));
        sql
    }
}

/// A rendered, parameterised UPDATE: the SQL text plus the ordered bind values.
#[derive(Debug, Clone)]
pub struct ParamStatement {
    pub sql: String,
    pub params: Vec<Value>,
    /// True when the row is matched by full-row equality (no PK). The engine
    /// must verify exactly one row was affected.
    pub requires_single_row_check: bool,
}

/// Compile a single-row UPDATE or DELETE for the given mutation.
///
/// SQLite uses positional `?` placeholders and relies on column affinity.
/// Postgres uses `$n` placeholders and casts each bind back to the column's
/// declared type (we bind text, the server casts), so type safety is preserved
/// without OID juggling. We specify the data comes in text and then the cast we want.
pub fn build_statement(
    dialect: Dialect,
    table_meta: &TableMeta,
    mutation: &RowMutation,
) -> ParamStatement {
    match mutation {
        RowMutation::Update {
            table,
            key,
            changes,
        } => build_update(dialect, table_meta, table, key, changes),
        RowMutation::Delete { table, key } => build_delete(dialect, table_meta, table, key),
    }
}

fn build_update(
    dialect: Dialect,
    table_meta: &TableMeta,
    table: &str,
    key: &RowKey,
    changes: &[CellDelta],
) -> ParamStatement {
    let mut params: Vec<Value> = Vec::new();
    let mut next_index = 0usize;

    let set_clause = changes
        .iter()
        .map(|c| {
            let ph = placeholder(
                dialect,
                table_meta,
                &c.column,
                c.new.clone(),
                &mut params,
                &mut next_index,
            );
            format!("{} = {ph}", dialect.quote_ident(&c.column))
        })
        .collect::<Vec<_>>()
        .join(", ");

    let (where_clause, requires_single_row_check) =
        compile_where(dialect, table_meta, key, &mut params, &mut next_index);

    let sql = format!(
        "UPDATE {} SET {set_clause} WHERE {where_clause}",
        dialect.quote_ident(table)
    );

    ParamStatement {
        sql,
        params,
        requires_single_row_check,
    }
}

fn build_delete(
    dialect: Dialect,
    table_meta: &TableMeta,
    table: &str,
    key: &RowKey,
) -> ParamStatement {
    let mut params: Vec<Value> = Vec::new();
    let mut next_index = 0usize;

    let (where_clause, requires_single_row_check) =
        compile_where(dialect, table_meta, key, &mut params, &mut next_index);

    let sql = format!(
        "DELETE FROM {} WHERE {where_clause}",
        dialect.quote_ident(table)
    );

    ParamStatement {
        sql,
        params,
        requires_single_row_check,
    }
}

/// Emit a placeholder for one bound value, recording it in `params`. Postgres
/// pins the bind to text then casts to the column's declared type.
fn placeholder(
    dialect: Dialect,
    table_meta: &TableMeta,
    column: &str,
    value: Value,
    params: &mut Vec<Value>,
    idx: &mut usize,
) -> String {
    *idx += 1;
    params.push(value);
    match dialect {
        // SQLite and MySQL use positional `?` placeholders; values are bound
        // directly and the server coerces to the column type.
        Dialect::Sqlite | Dialect::Mysql => "?".to_string(),
        Dialect::Postgres => {
            let cast = table_meta
                .column(column)
                .map(|c| c.declared_type.as_str())
                .unwrap_or("text");
            format!("${}::text::{cast}", *idx)
        }
    }
}

/// Compile the `WHERE` clause locating a row from its key. NULL key parts become
/// `IS NULL` (no bind). Returns the clause and whether the engine must verify
/// exactly one row was affected (true for a full-row fallback match).
fn compile_where(
    dialect: Dialect,
    table_meta: &TableMeta,
    key: &RowKey,
    params: &mut Vec<Value>,
    idx: &mut usize,
) -> (String, bool) {
    let (key_parts, requires_single_row_check) = match key {
        RowKey::PrimaryKey(parts) => (parts, false),
        RowKey::FullRow(parts) => (parts, true),
    };

    let where_clause = key_parts
        .iter()
        .map(|part| {
            if part.value.is_null() {
                format!("{} IS NULL", dialect.quote_ident(&part.column))
            } else {
                let ph = placeholder(
                    dialect,
                    table_meta,
                    &part.column,
                    part.value.clone(),
                    params,
                    idx,
                );
                format!("{} = {ph}", dialect.quote_ident(&part.column))
            }
        })
        .collect::<Vec<_>>()
        .join(" AND ");

    (where_clause, requires_single_row_check)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::delta::{CellDelta, KeyPart};
    use crate::model::schema::{ColumnMeta, RelationKind, TableMeta};
    use crate::model::value::TypeAffinity;

    fn col(name: &str, declared: &str, pk: bool) -> ColumnMeta {
        ColumnMeta {
            name: name.to_string(),
            declared_type: declared.to_string(),
            affinity: TypeAffinity::from_declared(declared),
            is_primary_key: pk,
        }
    }

    fn users_meta() -> TableMeta {
        TableMeta {
            name: "users".to_string(),
            kind: RelationKind::Table,
            columns: vec![
                col("id", "integer", true),
                col("name", "character varying", false),
                col("email", "text", false),
                col("age", "integer", false),
            ],
        }
    }

    #[test]
    fn renders_sqlite_select_with_filter_and_order() {
        let q = SelectQuery {
            table: "users".to_string(),
            columns: vec!["id".to_string(), "name".to_string()],
            filter: Some("age > 30".to_string()),
            order_by: Some("id DESC".to_string()),
            limit: 100,
        };
        assert_eq!(
            q.render(Dialect::Sqlite),
            r#"SELECT "id", "name" FROM "users" WHERE age > 30 ORDER BY id DESC LIMIT 100"#
        );
    }

    #[test]
    fn renders_postgres_select_with_text_casts() {
        let q = SelectQuery {
            table: "users".to_string(),
            columns: vec!["id".to_string(), "name".to_string()],
            filter: None,
            order_by: None,
            limit: 100,
        };
        assert_eq!(
            q.render(Dialect::Postgres),
            r#"SELECT "id"::text, "name"::text FROM "users" LIMIT 100"#
        );
    }

    #[test]
    fn build_update_sqlite_uses_primary_key() {
        let mutation = RowMutation::Update {
            table: "users".to_string(),
            key: RowKey::PrimaryKey(vec![KeyPart {
                column: "id".to_string(),
                value: Value::Integer(42),
            }]),
            changes: vec![CellDelta {
                column: "email".to_string(),
                original: Value::Text("old@x".to_string()),
                new: Value::Text("new@x".to_string()),
            }],
        };
        let stmt = build_statement(Dialect::Sqlite, &users_meta(), &mutation);
        assert_eq!(stmt.sql, r#"UPDATE "users" SET "email" = ? WHERE "id" = ?"#);
        assert_eq!(
            stmt.params,
            vec![Value::Text("new@x".to_string()), Value::Integer(42)]
        );
        assert!(!stmt.requires_single_row_check);
    }

    #[test]
    fn build_delete_sqlite_uses_primary_key() {
        let mutation = RowMutation::Delete {
            table: "users".to_string(),
            key: RowKey::PrimaryKey(vec![KeyPart {
                column: "id".to_string(),
                value: Value::Integer(42),
            }]),
        };
        let stmt = build_statement(Dialect::Sqlite, &users_meta(), &mutation);
        assert_eq!(stmt.sql, r#"DELETE FROM "users" WHERE "id" = ?"#);
        assert_eq!(stmt.params, vec![Value::Integer(42)]);
        assert!(!stmt.requires_single_row_check);
    }

    #[test]
    fn build_delete_postgres_full_row_casts_and_checks_single_row() {
        // No primary key: every column's original value locates the row, NULLs
        // become `IS NULL`, and the engine must verify exactly one row matched.
        let mutation = RowMutation::Delete {
            table: "users".to_string(),
            key: RowKey::FullRow(vec![
                KeyPart {
                    column: "name".to_string(),
                    value: Value::Text("John".to_string()),
                },
                KeyPart {
                    column: "email".to_string(),
                    value: Value::Null,
                },
            ]),
        };
        let stmt = build_statement(Dialect::Postgres, &users_meta(), &mutation);
        assert_eq!(
            stmt.sql,
            r#"DELETE FROM "users" WHERE "name" = $1::text::character varying AND "email" IS NULL"#
        );
        assert_eq!(stmt.params, vec![Value::Text("John".to_string())]);
        assert!(stmt.requires_single_row_check);
    }

    #[test]
    fn build_update_postgres_full_row_casts_and_is_null() {
        // A NULL key part must become `IS NULL` (no bind), and remaining parts
        // must be cast to their declared types in the right parameter order.
        let mutation = RowMutation::Update {
            table: "users".to_string(),
            key: RowKey::FullRow(vec![
                KeyPart {
                    column: "name".to_string(),
                    value: Value::Text("John".to_string()),
                },
                KeyPart {
                    column: "email".to_string(),
                    value: Value::Null,
                },
                KeyPart {
                    column: "age".to_string(),
                    value: Value::Integer(35),
                },
            ]),
            changes: vec![CellDelta {
                column: "email".to_string(),
                original: Value::Null,
                new: Value::Text("set@x".to_string()),
            }],
        };
        let stmt = build_statement(Dialect::Postgres, &users_meta(), &mutation);
        assert_eq!(
            stmt.sql,
            r#"UPDATE "users" SET "email" = $1::text::text WHERE "name" = $2::text::character varying AND "email" IS NULL AND "age" = $3::text::integer"#
        );
        assert_eq!(
            stmt.params,
            vec![
                Value::Text("set@x".to_string()),
                Value::Text("John".to_string()),
                Value::Integer(35),
            ]
        );
        assert!(stmt.requires_single_row_check);
    }
}
