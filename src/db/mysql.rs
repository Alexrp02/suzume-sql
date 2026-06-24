//! MySQL backend, built on the synchronous `mysql` crate.
//!
//! Read path: native typed values are decoded directly into [`Value`]; numeric
//! and boolean columns keep their typing, byte payloads become text when valid
//! UTF-8 (else a blob), and temporal types are rendered as text.
//!
//! Write path: each [`Value`] is bound as a positional `?` parameter and the
//! server coerces it to the column's type — no per-type cast juggling.
//!
//! The connection sets `CLIENT_FOUND_ROWS` so an UPDATE reports rows *matched*
//! rather than rows *changed*, matching the SQLite/Postgres engines' contract
//! for the full-row single-row check.

use mysql::consts::CapabilityFlags;
use mysql::prelude::Queryable;
use mysql::{Conn, Opts, OptsBuilder, Row, TxOpts, Value as MyValue};

use crate::db::query::{Dialect, SelectQuery, build_update};
use crate::db::{DatabaseEngine, RAW_ROW_CAP, RawResult};
use crate::error::DbError;
use crate::model::delta::RowMutation;
use crate::model::schema::{Catalog, ColumnMeta, RelationKind, TableMeta};
use crate::model::value::{TypeAffinity, Value};

pub struct MysqlEngine {
    conn: Conn,
}

impl MysqlEngine {
    pub fn connect(url: &str) -> Result<MysqlEngine, DbError> {
        let opts = Opts::from_url(url).map_err(|e| DbError::Connect(e.to_string()))?;
        // OR in CLIENT_FOUND_ROWS so `affected_rows` counts matched rows.
        let opts = OptsBuilder::from_opts(opts)
            .additional_capabilities(CapabilityFlags::CLIENT_FOUND_ROWS);
        let conn = Conn::new(opts).map_err(|e| DbError::Connect(e.to_string()))?;
        Ok(MysqlEngine { conn })
    }
}

impl DatabaseEngine for MysqlEngine {
    fn harvest_schema(&mut self) -> Result<Catalog, DbError> {
        // `DATABASE()` resolves to the schema named in the connection URL.
        let relations: Vec<(String, String)> = self
            .conn
            .query_map(
                "SELECT table_name, table_type FROM information_schema.tables \
                 WHERE table_schema = DATABASE() ORDER BY table_name",
                |(name, table_type): (String, String)| (name, table_type),
            )
            .map_err(|e| DbError::Schema(e.to_string()))?;

        let mut tables = Vec::with_capacity(relations.len());
        for (name, table_type) in relations {
            let kind = if table_type.eq_ignore_ascii_case("VIEW") {
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
        let sql = query.render(Dialect::Mysql);
        self.conn
            .query_map(&sql, row_to_values)
            .map_err(|e| DbError::Query(e.to_string()))
    }

    fn run_raw(&mut self, sql: &str) -> Result<RawResult, DbError> {
        let mut result = self
            .conn
            .query_iter(sql)
            .map_err(|e| DbError::Query(e.to_string()))?;

        // Column metadata is carried by the result set even when no rows match.
        let columns: Vec<String> = result
            .columns()
            .as_ref()
            .iter()
            .map(|c| c.name_str().into_owned())
            .collect();

        let mut rows = Vec::new();
        let mut truncated = false;
        for row in result.by_ref() {
            let row = row.map_err(|e| DbError::Query(e.to_string()))?;
            if rows.len() >= RAW_ROW_CAP {
                truncated = true;
                break;
            }
            rows.push(row_to_values(row));
        }
        // Dropping `result` before it is exhausted drains the remaining rows so
        // the connection stays usable for the next statement.
        drop(result);

        Ok(RawResult {
            columns,
            rows,
            truncated,
        })
    }

    fn commit(&mut self, mutations: &[RowMutation], catalog: &Catalog) -> Result<(), DbError> {
        let mut tx = self
            .conn
            .start_transaction(TxOpts::default())
            .map_err(|e| DbError::Commit(e.to_string()))?;
        for mutation in mutations {
            let table_meta = catalog.find(&mutation.table).ok_or_else(|| {
                DbError::Commit(format!("unknown table `{}`", mutation.table))
            })?;
            let stmt = build_update(Dialect::Mysql, table_meta, mutation);
            let params: Vec<MyValue> = stmt.params.iter().map(to_my_value).collect();
            tx.exec_drop(stmt.sql.as_str(), params)
                .map_err(|e| DbError::Commit(e.to_string()))?;
            if stmt.requires_single_row_check {
                let affected = tx.affected_rows();
                if affected != 1 {
                    // Dropping `tx` without commit rolls the whole batch back.
                    return Err(DbError::AmbiguousUpdate {
                        table: mutation.table.clone(),
                        matched: affected,
                    });
                }
            }
        }
        tx.commit().map_err(|e| DbError::Commit(e.to_string()))?;
        Ok(())
    }
}

impl MysqlEngine {
    fn columns_for(&mut self, table: &str) -> Result<Vec<ColumnMeta>, DbError> {
        // `column_key` is 'PRI' for any column that is part of the primary key.
        let rows: Vec<(String, String, String)> = self
            .conn
            .exec_map(
                "SELECT column_name, data_type, column_key \
                 FROM information_schema.columns \
                 WHERE table_schema = DATABASE() AND table_name = ? \
                 ORDER BY ordinal_position",
                (table,),
                |(name, data_type, column_key): (String, String, String)| {
                    (name, data_type, column_key)
                },
            )
            .map_err(|e| DbError::Schema(e.to_string()))?;

        let mut columns = Vec::with_capacity(rows.len());
        for (name, data_type, column_key) in rows {
            columns.push(ColumnMeta {
                affinity: TypeAffinity::from_declared(&data_type),
                is_primary_key: column_key == "PRI",
                name,
                declared_type: data_type,
            });
        }
        Ok(columns)
    }
}

/// Decode one result row's columns into owned [`Value`]s.
fn row_to_values(row: Row) -> Vec<Value> {
    // A freshly-fetched row has all its values present, so `unwrap` is total.
    row.unwrap().into_iter().map(Value::from).collect()
}

// These conversions live in the DB layer (not in `model`) so `Value` stays free
// of any mysql coupling; the orphan rule allows them because `Value` is local.

/// Read path: a native mysql value becomes an owned [`Value`].
impl From<MyValue> for Value {
    fn from(value: MyValue) -> Value {
        match value {
            MyValue::NULL => Value::Null,
            MyValue::Int(n) => Value::Integer(n),
            // Unsigned values beyond i64 keep full precision as text.
            MyValue::UInt(n) => match i64::try_from(n) {
                Ok(n) => Value::Integer(n),
                Err(_) => Value::Text(n.to_string()),
            },
            MyValue::Float(f) => Value::Real(f64::from(f)),
            MyValue::Double(f) => Value::Real(f),
            // Text and binary columns both arrive as bytes; valid UTF-8 is text,
            // anything else is a blob.
            MyValue::Bytes(bytes) => match String::from_utf8(bytes) {
                Ok(text) => Value::Text(text),
                Err(e) => Value::Blob(e.into_bytes()),
            },
            MyValue::Date(year, month, day, hour, min, sec, micro) => {
                Value::Text(format_datetime(year, month, day, hour, min, sec, micro))
            }
            MyValue::Time(neg, days, hours, mins, secs, micro) => {
                Value::Text(format_time(neg, days, hours, mins, secs, micro))
            }
        }
    }
}

/// Write path: convert a [`Value`] into a mysql bind value. The server coerces
/// it to the target column's type.
fn to_my_value(value: &Value) -> MyValue {
    match value {
        Value::Null => MyValue::NULL,
        Value::Integer(n) => MyValue::Int(*n),
        Value::Real(f) => MyValue::Double(*f),
        Value::Text(s) => MyValue::Bytes(s.clone().into_bytes()),
        Value::Boolean(b) => MyValue::Int(i64::from(*b)),
        Value::Blob(bytes) => MyValue::Bytes(bytes.clone()),
    }
}

/// Render a `DATE`/`DATETIME`/`TIMESTAMP` value as text. The time portion is
/// omitted when it is exactly midnight (a bare `DATE`), and fractional seconds
/// only when present.
fn format_datetime(
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    min: u8,
    sec: u8,
    micro: u32,
) -> String {
    let date = format!("{year:04}-{month:02}-{day:02}");
    if hour == 0 && min == 0 && sec == 0 && micro == 0 {
        date
    } else if micro == 0 {
        format!("{date} {hour:02}:{min:02}:{sec:02}")
    } else {
        format!("{date} {hour:02}:{min:02}:{sec:02}.{micro:06}")
    }
}

/// Render a `TIME` value as text. MySQL `TIME` can exceed 24h and be negative,
/// so days roll up into the hour field.
fn format_time(neg: bool, days: u32, hours: u8, mins: u8, secs: u8, micro: u32) -> String {
    let sign = if neg { "-" } else { "" };
    let total_hours = days * 24 + u32::from(hours);
    if micro == 0 {
        format!("{sign}{total_hours:02}:{mins:02}:{secs:02}")
    } else {
        format!("{sign}{total_hours:02}:{mins:02}:{secs:02}.{micro:06}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_native_value_types() {
        assert_eq!(Value::from(MyValue::NULL), Value::Null);
        assert_eq!(Value::from(MyValue::Int(42)), Value::Integer(42));
        assert_eq!(Value::from(MyValue::Double(1.5)), Value::Real(1.5));
        assert_eq!(
            Value::from(MyValue::Bytes(b"hello".to_vec())),
            Value::Text("hello".to_string())
        );
        // Invalid UTF-8 falls back to a blob rather than lossy text.
        assert_eq!(
            Value::from(MyValue::Bytes(vec![0xff, 0xfe])),
            Value::Blob(vec![0xff, 0xfe])
        );
        // u64 beyond i64::MAX keeps precision as text.
        assert_eq!(
            Value::from(MyValue::UInt(u64::MAX)),
            Value::Text(u64::MAX.to_string())
        );
    }

    #[test]
    fn formats_temporal_values() {
        // A bare date drops the midnight time component.
        assert_eq!(
            Value::from(MyValue::Date(2026, 6, 16, 0, 0, 0, 0)),
            Value::Text("2026-06-16".to_string())
        );
        assert_eq!(
            Value::from(MyValue::Date(2026, 6, 16, 9, 30, 15, 0)),
            Value::Text("2026-06-16 09:30:15".to_string())
        );
        // A TIME spanning more than a day rolls the days into the hours.
        assert_eq!(
            Value::from(MyValue::Time(false, 1, 2, 3, 4, 0)),
            Value::Text("26:03:04".to_string())
        );
        assert_eq!(
            Value::from(MyValue::Time(true, 0, 1, 0, 0, 0)),
            Value::Text("-01:00:00".to_string())
        );
    }

    #[test]
    fn binds_values_to_native_params() {
        assert_eq!(to_my_value(&Value::Null), MyValue::NULL);
        assert_eq!(to_my_value(&Value::Integer(7)), MyValue::Int(7));
        assert_eq!(to_my_value(&Value::Boolean(true)), MyValue::Int(1));
        assert_eq!(
            to_my_value(&Value::Text("x".to_string())),
            MyValue::Bytes(b"x".to_vec())
        );
    }
}
