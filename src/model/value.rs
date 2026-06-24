//! A typed, engine-agnostic cell value.
//!
//! Both engines normalise their native rows into [`Value`], so the grid, the
//! delta queue, and the SQL generator all operate on one representation.

use std::fmt;

/// The broad type category of a column, derived from its declared type name.
///
/// We keep this coarse on purpose: it is only used to interpret free-text the
/// user types into a cell, and to pick a cast target for Postgres writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeAffinity {
    Integer,
    Real,
    Text,
    Blob,
    Boolean,
    /// Anything we do not recognise (dates, json, numeric, ...). Treated as text.
    Unknown,
}

impl TypeAffinity {
    /// Classify a declared SQL type name (case-insensitive, substring based,
    /// matching SQLite's affinity rules and common Postgres type names).
    pub fn from_declared(declared: &str) -> TypeAffinity {
        let t = declared.to_ascii_lowercase();
        // Order matters: check the more specific names first.
        if t.contains("bool") {
            TypeAffinity::Boolean
        } else if t.contains("int")
            || t == "serial"
            || t == "bigserial"
            || t == "smallserial"
        {
            TypeAffinity::Integer
        } else if t.contains("char") || t.contains("text") || t.contains("clob") {
            TypeAffinity::Text
        } else if t.contains("blob") || t.contains("bytea") {
            TypeAffinity::Blob
        } else if t.contains("real")
            || t.contains("floa")
            || t.contains("doub")
            || t.contains("numeric")
            || t.contains("decimal")
        {
            TypeAffinity::Real
        } else {
            TypeAffinity::Unknown
        }
    }
}

/// A single cell value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
    Boolean(bool),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Parse free text entered in a cell into a typed value, using the column's
    /// affinity as a hint. An empty buffer maps to `NULL` for non-text columns
    /// and to the empty string for text columns.
    pub fn parse(raw: &str, affinity: TypeAffinity) -> Value {
        match affinity {
            TypeAffinity::Text => Value::Text(raw.to_string()),
            TypeAffinity::Unknown => {
                if raw.is_empty() {
                    Value::Null
                } else {
                    Value::Text(raw.to_string())
                }
            }
            _ if raw.is_empty() => Value::Null,
            TypeAffinity::Integer => match raw.parse::<i64>() {
                Ok(n) => Value::Integer(n),
                // Keep the user's text if it is not a clean integer; the engine
                // cast (Postgres) or affinity (SQLite) will reject it loudly.
                Err(_) => Value::Text(raw.to_string()),
            },
            TypeAffinity::Real => match raw.parse::<f64>() {
                Ok(n) => Value::Real(n),
                Err(_) => Value::Text(raw.to_string()),
            },
            TypeAffinity::Boolean => match raw.to_ascii_lowercase().as_str() {
                "1" | "t" | "true" | "yes" | "y" => Value::Boolean(true),
                "0" | "f" | "false" | "no" | "n" => Value::Boolean(false),
                _ => Value::Text(raw.to_string()),
            },
            TypeAffinity::Blob => Value::Text(raw.to_string()),
        }
    }

    /// Convert to a JSON value, preserving the typed shape where known
    /// (numbers/booleans/null stay native; text stays a string).
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as Json;
        match self {
            Value::Null => Json::Null,
            Value::Integer(n) => Json::from(*n),
            // Non-finite floats have no JSON representation; fall back to null.
            Value::Real(f) => serde_json::Number::from_f64(*f)
                .map(Json::Number)
                .unwrap_or(Json::Null),
            Value::Text(s) => Json::String(s.clone()),
            Value::Boolean(b) => Json::Bool(*b),
            Value::Blob(bytes) => Json::String(format!("<blob {} bytes>", bytes.len())),
        }
    }

    /// The text representation used both for display and as the textual SQL
    /// parameter form on engines that bind everything as text (Postgres).
    /// Returns `None` for `NULL`.
    pub fn to_sql_text(&self) -> Option<String> {
        match self {
            Value::Null => None,
            Value::Integer(n) => Some(n.to_string()),
            Value::Real(n) => Some(n.to_string()),
            Value::Text(s) => Some(s.clone()),
            Value::Boolean(b) => Some(if *b { "true" } else { "false" }.to_string()),
            Value::Blob(bytes) => Some(format!("\\x{}", hex_encode(bytes))),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("NULL"),
            Value::Integer(n) => write!(f, "{n}"),
            Value::Real(n) => write!(f, "{n}"),
            Value::Text(s) => f.write_str(s),
            Value::Boolean(b) => f.write_str(if *b { "true" } else { "false" }),
            Value::Blob(bytes) => write!(f, "<blob {} bytes>", bytes.len()),
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap_or('0'));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_declared_types() {
        assert_eq!(TypeAffinity::from_declared("INTEGER"), TypeAffinity::Integer);
        assert_eq!(TypeAffinity::from_declared("bigint"), TypeAffinity::Integer);
        assert_eq!(
            TypeAffinity::from_declared("character varying"),
            TypeAffinity::Text
        );
        assert_eq!(TypeAffinity::from_declared("boolean"), TypeAffinity::Boolean);
        assert_eq!(TypeAffinity::from_declared("numeric"), TypeAffinity::Real);
        assert_eq!(TypeAffinity::from_declared("bytea"), TypeAffinity::Blob);
        assert_eq!(TypeAffinity::from_declared("jsonb"), TypeAffinity::Unknown);
    }

    #[test]
    fn parses_by_affinity() {
        assert_eq!(Value::parse("42", TypeAffinity::Integer), Value::Integer(42));
        // Empty input is NULL for non-text columns...
        assert_eq!(Value::parse("", TypeAffinity::Integer), Value::Null);
        // ...but the empty string for text columns.
        assert_eq!(
            Value::parse("", TypeAffinity::Text),
            Value::Text(String::new())
        );
        assert_eq!(
            Value::parse("true", TypeAffinity::Boolean),
            Value::Boolean(true)
        );
        // Non-numeric text in a numeric column is kept as text so the engine
        // surfaces the error rather than silently coercing.
        assert_eq!(
            Value::parse("abc", TypeAffinity::Integer),
            Value::Text("abc".to_string())
        );
    }

    #[test]
    fn null_has_no_sql_text() {
        assert_eq!(Value::Null.to_sql_text(), None);
        assert_eq!(Value::Integer(7).to_sql_text(), Some("7".to_string()));
        assert_eq!(Value::Boolean(false).to_sql_text(), Some("false".to_string()));
    }

    #[test]
    fn to_json_keeps_native_shapes() {
        assert_eq!(Value::Null.to_json(), serde_json::Value::Null);
        assert_eq!(Value::Integer(7).to_json(), serde_json::json!(7));
        assert_eq!(Value::Boolean(true).to_json(), serde_json::json!(true));
        // Text with characters needing escaping round-trips correctly.
        assert_eq!(
            serde_json::to_string(&Value::Text("a\"b\n".to_string()).to_json()).unwrap(),
            r#""a\"b\n""#
        );
    }
}
