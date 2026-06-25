//! Domain error types for the application.
//!
//! Every fallible boundary returns a typed error rather than panicking, so the
//! application state can always render a coherent message instead of unwinding.

use thiserror::Error;

/// Errors raised while loading or interpreting the connection config file.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read config file `{path}`: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse config file `{path}`: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("could not write config file `{path}`: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not serialize config: {0}")]
    Serialize(#[source] toml::ser::Error),
    #[error("no connection named `{0}` is defined in the config")]
    UnknownConnection(String),
    #[error(
        "unsupported database engine in connection string: `{scheme}`\n\n\
         supported engines and connection string formats:\n  \
         sqlite     sqlite://<path>                       (e.g. sqlite://./demo.db)\n  \
         postgres   postgresql://user:pass@host:port/db\n  \
         mysql      mysql://user:pass@host:port/db"
    )]
    UnsupportedEngine { scheme: String },
}

/// Errors raised by a concrete database engine while talking to the backend.
#[derive(Debug, Error)]
pub enum DbError {
    #[error("failed to connect: {0}")]
    Connect(String),
    #[error("schema introspection failed: {0}")]
    Schema(String),
    #[error("query failed: {0}")]
    Query(String),
    #[error("commit failed: {0}")]
    Commit(String),
    /// A `FullRow` fallback update or delete matched more than one row; the
    /// engine rolled back to avoid touching unintended rows.
    #[error("row match for table `{table}` matched {matched} rows (expected 1); rolled back")]
    AmbiguousMatch { table: String, matched: u64 },
}
