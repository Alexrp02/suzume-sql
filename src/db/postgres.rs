//! PostgreSQL backend, built on the synchronous `postgres` crate.
//!
//! Read path: every column is cast to `text` in the generated SELECT, so each
//! value arrives as `Option<String>` regardless of its native type — uniform
//! and free of per-type OID handling.
//!
//! Write path: every value is bound as text via `ToSql for Value` and the SQL
//! pins the parameter to text before casting it to the column's declared type
//! (`$n::text::<type>`), so the server does the conversion for any type without
//! binary encoding. See `build_update` for why the `::text` step is required.

use bytes::BytesMut;
use postgres::types::{IsNull, ToSql, Type, to_sql_checked};
use postgres::{Client, NoTls, SimpleQueryMessage, Transaction};

use crate::db::query::{Dialect, ParamStatement, SelectQuery, build_update};
use crate::db::{DatabaseEngine, RAW_ROW_CAP, RawResult};
use crate::error::DbError;
use crate::model::delta::RowMutation;
use crate::model::schema::{Catalog, ColumnMeta, RelationKind, TableMeta};
use crate::model::value::{TypeAffinity, Value};

pub struct PostgresEngine {
    client: Client,
}

impl PostgresEngine {
    pub fn connect(url: &str) -> Result<PostgresEngine, DbError> {
        let client = Client::connect(url, NoTls).map_err(|e| DbError::Connect(e.to_string()))?;
        Ok(PostgresEngine { client })
    }
}

impl DatabaseEngine for PostgresEngine {
    fn harvest_schema(&mut self) -> Result<Catalog, DbError> {
        let rows = self
            .client
            .query(
                "SELECT table_name, table_type FROM information_schema.tables \
                 WHERE table_schema = 'public' ORDER BY table_name",
                &[],
            )
            .map_err(|e| DbError::Schema(e.to_string()))?;

        let mut tables = Vec::with_capacity(rows.len());
        for row in rows {
            let name: String = row.try_get(0).map_err(|e| DbError::Schema(e.to_string()))?;
            let table_type: String =
                row.try_get(1).map_err(|e| DbError::Schema(e.to_string()))?;
            let kind = if table_type == "VIEW" {
                RelationKind::View
            } else {
                RelationKind::Table
            };
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
        let sql = query.render(Dialect::Postgres);
        let rows = self
            .client
            .query(&sql, &[])
            .map_err(|e| DbError::Query(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let mut values = Vec::with_capacity(row.len());
            for i in 0..row.len() {
                let cell: Option<String> = row
                    .try_get(i)
                    .map_err(|e| DbError::Query(e.to_string()))?;
                values.push(match cell {
                    Some(text) => Value::Text(text),
                    None => Value::Null,
                });
            }
            out.push(values);
        }
        Ok(out)
    }

    fn run_raw(&mut self, sql: &str) -> Result<RawResult, DbError> {
        // The simple-query (text) protocol returns every column as text,
        // regardless of its native type, so a read-only preview works for any
        // column without per-OID decoding. RowDescription carries the column
        // names even when zero rows come back.
        let messages = self
            .client
            .simple_query(sql)
            .map_err(|e| DbError::Query(e.to_string()))?;

        let mut columns: Vec<String> = Vec::new();
        let mut rows: Vec<Vec<Value>> = Vec::new();
        let mut truncated = false;

        for message in messages {
            match message {
                SimpleQueryMessage::RowDescription(cols) => {
                    columns = cols.iter().map(|c| c.name().to_string()).collect();
                }
                SimpleQueryMessage::Row(row) => {
                    if columns.is_empty() {
                        columns = row.columns().iter().map(|c| c.name().to_string()).collect();
                    }
                    if rows.len() >= RAW_ROW_CAP {
                        truncated = true;
                        continue;
                    }
                    let values = (0..row.len())
                        .map(|i| match row.get(i) {
                            Some(text) => Value::Text(text.to_string()),
                            None => Value::Null,
                        })
                        .collect();
                    rows.push(values);
                }
                SimpleQueryMessage::CommandComplete(_) => {}
                // `SimpleQueryMessage` is non-exhaustive.
                _ => {}
            }
        }

        Ok(RawResult {
            columns,
            rows,
            truncated,
        })
    }

    fn commit(&mut self, mutations: &[RowMutation], catalog: &Catalog) -> Result<(), DbError> {
        let mut tx = self
            .client
            .transaction()
            .map_err(|e| DbError::Commit(e.to_string()))?;
        for mutation in mutations {
            let table_meta = catalog.find(&mutation.table).ok_or_else(|| {
                DbError::Commit(format!("unknown table `{}`", mutation.table))
            })?;
            let stmt = build_update(Dialect::Postgres, table_meta, mutation);
            let affected = exec_update(&mut tx, &stmt)?;
            if stmt.requires_single_row_check && affected != 1 {
                // Dropping `tx` without commit rolls the whole batch back.
                return Err(DbError::AmbiguousUpdate {
                    table: mutation.table.clone(),
                    matched: affected,
                });
            }
        }
        tx.commit().map_err(|e| DbError::Commit(e.to_string()))?;
        Ok(())
    }
}

impl PostgresEngine {
    fn columns_for(&mut self, table: &str) -> Result<Vec<ColumnMeta>, DbError> {
        let pk_rows = self
            .client
            .query(
                "SELECT kcu.column_name \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name \
                  AND tc.constraint_schema = kcu.constraint_schema \
                 WHERE tc.constraint_type = 'PRIMARY KEY' \
                   AND tc.table_schema = 'public' AND tc.table_name = $1",
                &[&table],
            )
            .map_err(|e| DbError::Schema(e.to_string()))?;
        let mut pk_columns: Vec<String> = Vec::new();
        for row in pk_rows {
            pk_columns.push(row.try_get(0).map_err(|e| DbError::Schema(e.to_string()))?);
        }

        let rows = self
            .client
            .query(
                "SELECT column_name, data_type \
                 FROM information_schema.columns \
                 WHERE table_schema = 'public' AND table_name = $1 \
                 ORDER BY ordinal_position",
                &[&table],
            )
            .map_err(|e| DbError::Schema(e.to_string()))?;

        let mut columns = Vec::with_capacity(rows.len());
        for row in rows {
            let name: String = row.try_get(0).map_err(|e| DbError::Schema(e.to_string()))?;
            let data_type: String =
                row.try_get(1).map_err(|e| DbError::Schema(e.to_string()))?;
            columns.push(ColumnMeta {
                affinity: TypeAffinity::from_declared(&data_type),
                is_primary_key: pk_columns.iter().any(|c| c == &name),
                name,
                declared_type: data_type,
            });
        }
        Ok(columns)
    }
}

fn exec_update(tx: &mut Transaction<'_>, stmt: &ParamStatement) -> Result<u64, DbError> {
    let params: Vec<&(dyn ToSql + Sync)> =
        stmt.params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
    tx.execute(&stmt.sql, &params)
        .map_err(|e| DbError::Commit(e.to_string()))
}

// Lives in the DB layer (not in `model`) so `Value` stays free of any postgres
// coupling; the orphan rule allows it because `Value` is crate-local.
//
// Every value is serialized in its text form. The `$n::text::<type>` cast in
// the generated SQL forces the parameter's inferred type to text, so the server
// receives text and casts it to the column type — which is why `accepts` only
// needs to cover the text family.
impl ToSql for Value {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match self.to_sql_text() {
            Some(text) => {
                out.extend_from_slice(text.as_bytes());
                Ok(IsNull::No)
            }
            None => Ok(IsNull::Yes),
        }
    }

    fn accepts(ty: &Type) -> bool {
        <String as ToSql>::accepts(ty)
    }

    to_sql_checked!();
}
