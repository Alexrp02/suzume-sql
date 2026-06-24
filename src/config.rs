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

use serde::Deserialize;

use crate::error::ConfigError;

/// The backend-specific connection parameters. The `engine` tag selects the
/// variant, so an invalid combination cannot be represented.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "engine", rename_all = "lowercase")]
pub enum ConnectionConfig {
    Sqlite { path: String },
    Postgres { url: String },
}

impl ConnectionConfig {
    pub fn engine_label(&self) -> &'static str {
        match self {
            ConnectionConfig::Sqlite { .. } => "sqlite",
            ConnectionConfig::Postgres { .. } => "postgres",
        }
    }

    /// A short human-readable target, for the connection picker.
    pub fn target(&self) -> &str {
        match self {
            ConnectionConfig::Sqlite { path } => path,
            ConnectionConfig::Postgres { url } => url,
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

    /// Look up a connection by name.
    pub fn connection(&self, name: &str) -> Result<&NamedConnection, ConfigError> {
        self.connections
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| ConfigError::UnknownConnection(name.to_string()))
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
            ConnectionConfig::Postgres { .. } => panic!("expected sqlite"),
        }
        match &config.connections[1].connection {
            ConnectionConfig::Postgres { url } => {
                assert_eq!(url, "postgresql://u:p@localhost/app")
            }
            ConnectionConfig::Sqlite { .. } => panic!("expected postgres"),
        }
        assert_eq!(config.connection("prod").expect("found").name, "prod");
        assert!(config.connection("missing").is_err());
    }
}
