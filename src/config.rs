//! Connection configuration loaded from a TOML file.
//!
//! Example `normal-sql.toml`:
//!
//! [[connections]]
//! name = "local"
//! engine = "sqlite"
//! path = "./demo.db"
//!
//! [[connections]]
//! name = "prod"
//! engine = "postgres"
//! url = "postgresql://user:pass@localhost:5432/app"
//!
//! [[connections]]
//! name = "mysql-local"
//! engine = "mysql"
//! url = "mysql://user:pass@localhost:3306/app"

use serde::Deserialize;

use crate::error::ConfigError;

/// The backend-specific connection parameters. The `engine` tag selects the
/// variant, so an invalid combination cannot be represented.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "engine", rename_all = "lowercase")]
pub enum ConnectionConfig {
    Sqlite { path: String },
    Postgres { url: String },
    Mysql { url: String },
}

impl ConnectionConfig {
    pub fn engine_label(&self) -> &'static str {
        match self {
            ConnectionConfig::Sqlite { .. } => "sqlite",
            ConnectionConfig::Postgres { .. } => "postgres",
            ConnectionConfig::Mysql { .. } => "mysql",
        }
    }

    /// A short human-readable target, for the connection picker.
    pub fn target(&self) -> &str {
        match self {
            ConnectionConfig::Sqlite { path } => path,
            ConnectionConfig::Postgres { url } => url,
            ConnectionConfig::Mysql { url } => url,
        }
    }

    /// Parse a CLI connection string into a backend config. The engine is taken
    /// from the `scheme://` prefix; every engine, SQLite included, requires one.
    pub fn parse(spec: &str) -> Result<ConnectionConfig, ConfigError> {
        let spec = spec.trim();
        let (scheme, rest) = spec.split_once("://").unwrap_or((spec, ""));
        match scheme {
            "postgres" | "postgresql" => Ok(ConnectionConfig::Postgres {
                url: spec.to_string(),
            }),
            "mysql" => Ok(ConnectionConfig::Mysql {
                url: spec.to_string(),
            }),
            "sqlite" | "sqlite3" => Ok(ConnectionConfig::Sqlite {
                path: rest.to_string(),
            }),
            other => Err(ConfigError::UnsupportedEngine {
                scheme: other.to_string(),
            }),
        }
    }
}

/// A named connection entry.
#[derive(Debug, Clone, Deserialize)]
pub struct NamedConnection {
    pub name: String,
    #[serde(flatten)]
    pub connection: ConnectionConfig,
}

/// The whole config file.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub connections: Vec<NamedConnection>,
}

impl Config {
    /// Read and parse the config file at `path`.
    pub fn load(path: &str) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_string(),
            source,
        })?;
        let config: Config = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_string(),
            source,
        })?;
        if config.connections.is_empty() {
            return Err(ConfigError::Empty);
        }
        Ok(config)
    }

    /// Reduce the config to just the connection named `name`, if present, so it
    /// connects directly instead of opening the picker.
    pub fn select(self, name: &str) -> Option<Config> {
        self.connections
            .into_iter()
            .find(|c| c.name == name)
            .map(|connection| Config {
                connections: vec![connection],
            })
    }

    /// Build a single-connection config from a CLI connection string, bypassing
    /// the config file entirely.
    pub fn from_connection_string(spec: &str) -> Result<Config, ConfigError> {
        let connection = ConnectionConfig::parse(spec)?;
        let name = connection.engine_label().to_string();
        Ok(Config {
            connections: vec![NamedConnection { name, connection }],
        })
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mixed_engine_connections() {
        let toml = r#"
            [[connections]]
            name = "local"
            engine = "sqlite"
            path = "./demo.db"

            [[connections]]
            name = "prod"
            engine = "postgres"
            url = "postgresql://u:p@localhost/app"
        "#;
        let config: Config = toml::from_str(toml).expect("parse");
        assert_eq!(config.connections.len(), 2);
        match &config.connections[0].connection {
            ConnectionConfig::Sqlite { path } => assert_eq!(path, "./demo.db"),
            other => panic!("expected sqlite, got {other:?}"),
        }
        match &config.connections[1].connection {
            ConnectionConfig::Postgres { url } => {
                assert_eq!(url, "postgresql://u:p@localhost/app")
            }
            other => panic!("expected postgres, got {other:?}"),
        }
    }

    #[test]
    fn parses_connection_strings_by_scheme() {
        match ConnectionConfig::parse("postgresql://u:p@localhost:5432/app").expect("postgres") {
            ConnectionConfig::Postgres { url } => {
                assert_eq!(url, "postgresql://u:p@localhost:5432/app")
            }
            other => panic!("expected postgres, got {other:?}"),
        }
        match ConnectionConfig::parse("postgres://u:p@localhost/app").expect("postgres alias") {
            ConnectionConfig::Postgres { .. } => {}
            other => panic!("expected postgres, got {other:?}"),
        }
        match ConnectionConfig::parse("mysql://u:p@localhost:3306/app").expect("mysql") {
            ConnectionConfig::Mysql { url } => assert_eq!(url, "mysql://u:p@localhost:3306/app"),
            other => panic!("expected mysql, got {other:?}"),
        }
        // SQLite requires the sqlite:// scheme, which is stripped to a path.
        match ConnectionConfig::parse("sqlite://./demo.db").expect("sqlite scheme") {
            ConnectionConfig::Sqlite { path } => assert_eq!(path, "./demo.db"),
            other => panic!("expected sqlite, got {other:?}"),
        }
        match ConnectionConfig::parse("sqlite://:memory:").expect("sqlite memory") {
            ConnectionConfig::Sqlite { path } => assert_eq!(path, ":memory:"),
            other => panic!("expected sqlite, got {other:?}"),
        }
    }

    #[test]
    fn select_reduces_to_a_single_named_connection() {
        let config = Config {
            connections: vec![
                NamedConnection {
                    name: "a".to_string(),
                    connection: ConnectionConfig::Sqlite {
                        path: "a.db".to_string(),
                    },
                },
                NamedConnection {
                    name: "b".to_string(),
                    connection: ConnectionConfig::Sqlite {
                        path: "b.db".to_string(),
                    },
                },
            ],
        };
        let selected = config.clone().select("b").expect("found");
        assert_eq!(selected.connections.len(), 1);
        assert_eq!(selected.connections[0].name, "b");
        assert!(config.select("missing").is_none());
    }

    #[test]
    fn rejects_unknown_engine_scheme() {
        match ConnectionConfig::parse("redis://localhost:6379") {
            Err(ConfigError::UnsupportedEngine { scheme }) => assert_eq!(scheme, "redis"),
            other => panic!("expected unsupported engine error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_schemeless_connection_string() {
        // A bare path is no longer a SQLite string; it has no recognized scheme.
        match ConnectionConfig::parse("./demo.db") {
            Err(ConfigError::UnsupportedEngine { scheme }) => assert_eq!(scheme, "./demo.db"),
            other => panic!("expected unsupported engine error, got {other:?}"),
        }
    }
}
