use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    ParseError(#[from] toml::de::Error),
    #[error("TOML serialize error: {0}")]
    SerializeError(#[from] toml::ser::Error),
    #[error("Home directory not found")]
    HomeDirNotFound,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub indexing: Indexing,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Indexing {
    pub include: Vec<PathBuf>,
    pub exclude: Vec<String>,
}

impl Config {
    pub fn default_path() -> Result<PathBuf, ConfigError> {
        let home_path = dirs::home_dir().ok_or(ConfigError::HomeDirNotFound)?;
        let config_dir = home_path.join(".scout.toml");
        Ok(config_dir)
    }
}

// Loads the configuration from the specified path, creates the file with default configuration if it doesn't exist, and validates the configuration.
pub fn load_and_validate_config() -> Result<Config, ConfigError> {
    let path = Config::default_path()?;

    create_file_if_missing(&path)?;
    let config = load_config(&path)?;
    validate_config(&config)?;
    Ok(config)
}

fn load_config(path: impl AsRef<Path>) -> Result<Config, ConfigError> {
    let config_str = fs::read_to_string(path)?;
    let config: Config = toml::from_str(config_str.as_str())?;
    Ok(config)
}

fn create_file_if_missing(path: impl AsRef<Path>) -> Result<(), ConfigError> {
    if !path.as_ref().exists() {
        let home_path = dirs::home_dir().ok_or(ConfigError::HomeDirNotFound)?;

        let default_dir = home_path.join("Documents");

        let config = Config {
            indexing: Indexing {
                include: vec![default_dir],
                // Default exclude directories
                exclude: vec![
                    "node_modules".to_string(),
                    ".git".to_string(),
                    "target".to_string(),
                    "venv".to_string(),
                    "vendor".to_string(),
                    ".claude".to_string(),
                    ".cache".to_string(),
                    ".idea".to_string(),
                    ".vscode".to_string(),
                ],
            },
        };

        let config_str = toml::to_string(&config)?;

        fs::write(path, config_str)?;
    }
    Ok(())
}

fn validate_config(config: &Config) -> Result<(), ConfigError> {
    if config.indexing.include.is_empty() {
        return Err(ConfigError::InvalidConfig(
            "Include list can not be empty".to_string(),
        ));
    }
    Ok(())
}
