//! Router configuration: TOML parsing, defaults, merge semantics, and
//! validation, organised into per-concern submodules and stitched back
//! together here so that the public path
//! `mavrouter::config::{Config, GeneralConfig, EndpointConfig, EndpointMode,
//! FlowControl, BaudRate}` stays stable.
//!
//! # TOML shape
//!
//! ```toml
//! [general]
//! tcp_port = 5760
//! dedup_period_ms = 100
//! log = "logs"
//! log_telemetry = true
//! bus_capacity = 1000
//! routing_table_ttl_secs = 300
//! routing_table_prune_interval_secs = 60
//!
//! [[endpoint]]
//! type = "serial"
//! device = "/dev/ttyACM0"
//! baud = 115200
//!
//! [[endpoint]]
//! type = "udp"
//! address = "0.0.0.0:14550"
//! mode = "server"
//!
//! [[endpoint]]
//! type = "tcp"
//! address = "127.0.0.1:5761"
//! mode = "client"
//! ```

mod defaults;
mod endpoint;
mod general;
mod merge;
mod validate;

#[cfg(test)]
mod tests;

// The bin crate (`main.rs`) only consumes `Config` itself; the supporting
// types are re-exported purely to keep `mavrouter::config::*` stable for
// library users, so `unused_imports` fires in the bin build. Allow it —
// these re-exports are the contract.
#[allow(unused_imports)]
pub use endpoint::{BaudRate, EndpointConfig, EndpointMode, FlowControl};
#[allow(unused_imports)]
pub use general::GeneralConfig;

use crate::error::{Result, RouterError};
use serde::Deserialize;
use std::path::Path;
use std::str::FromStr;
use tokio::fs;

/// Configuration for MAVLink router.
///
/// Loaded from a TOML file via [`Config::load`], from a directory of
/// TOML fragments via [`Config::load_dir`], from an inline TOML string via
/// [`Config::parse`] / [`FromStr`], or synthesised programmatically.
#[derive(Debug, Default, Deserialize)]
pub struct Config {
    /// General router configuration.
    #[serde(default)]
    pub general: GeneralConfig,
    /// List of endpoint configurations.
    #[serde(default)]
    pub endpoint: Vec<EndpointConfig>,
}

impl Config {
    /// Parses router configuration from a TOML string.
    ///
    /// Useful for programmatically generating configurations without
    /// writing temporary files.
    ///
    /// # Errors
    ///
    /// Returns a `RouterError` if the string cannot be parsed or validation fails.
    ///
    /// # Example
    ///
    /// ```
    /// use mavrouter::config::Config;
    /// use std::str::FromStr;
    ///
    /// let toml = r#"
    /// [general]
    /// bus_capacity = 1000
    ///
    /// [[endpoint]]
    /// type = "udp"
    /// address = "0.0.0.0:14550"
    /// mode = "server"
    /// "#;
    ///
    /// let config = Config::from_str(toml).expect("valid config");
    /// assert_eq!(config.endpoint.len(), 1);
    /// ```
    pub fn parse(toml: &str) -> Result<Self> {
        let config: Config = toml::from_str(toml)
            .map_err(|e| RouterError::config(format!("Failed to parse config: {}", e)))?;

        config.validate()?;

        Ok(config)
    }

    /// Loads the router configuration from a TOML file.
    ///
    /// # Errors
    ///
    /// Returns a `RouterError` if the file cannot be read or parsed, or if
    /// the configuration fails validation.
    pub async fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path_str = path.as_ref().display().to_string();
        let content = fs::read_to_string(path.as_ref())
            .await
            .map_err(|e| RouterError::filesystem(&path_str, e))?;

        Self::parse(&content)
    }

    /// Loads all `*.toml` files from a directory, sorted alphabetically,
    /// and merges them sequentially into a single configuration.
    ///
    /// Later files override earlier files for general settings. Endpoint
    /// lists are concatenated across all files. Non-TOML files are ignored.
    /// An empty directory produces a valid config with defaults and no
    /// endpoints.
    ///
    /// # Errors
    ///
    /// Returns a `RouterError` if the directory cannot be read, or if any
    /// `.toml` file cannot be parsed, or if the final merged config fails
    /// validation.
    pub async fn load_dir(path: impl AsRef<Path>) -> Result<Self> {
        let dir_path = path.as_ref();
        let dir_str = dir_path.display().to_string();

        let mut entries = fs::read_dir(dir_path)
            .await
            .map_err(|e| RouterError::filesystem(&dir_str, e))?;

        let mut toml_files: Vec<String> = Vec::new();

        loop {
            match entries.next_entry().await {
                Ok(Some(entry)) => {
                    let file_path = entry.path();
                    if file_path.extension().and_then(|e| e.to_str()) == Some("toml") {
                        if let Some(name) = file_path.file_name().and_then(|n| n.to_str()) {
                            toml_files.push(name.to_string());
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => return Err(RouterError::filesystem(&dir_str, e)),
            }
        }

        toml_files.sort();

        let mut merged = Config::default();

        for filename in &toml_files {
            let file_path = dir_path.join(filename);
            let content = fs::read_to_string(&file_path)
                .await
                .map_err(|e| RouterError::filesystem(file_path.display().to_string(), e))?;

            let config: Config = toml::from_str(&content).map_err(|e| {
                RouterError::config(format!(
                    "Failed to parse config {}: {}",
                    file_path.display(),
                    e
                ))
            })?;

            merged = merged.merge(config);
        }

        merged.validate()?;
        Ok(merged)
    }

    /// Loads configuration from a file and/or directory, merging them.
    ///
    /// If both `file` and `dir` are provided, the file is loaded first,
    /// then the directory configs are merged on top (directory wins for
    /// general settings, endpoints accumulate).
    ///
    /// If neither is provided, returns a default configuration.
    ///
    /// # Errors
    ///
    /// Returns a `RouterError` if any file cannot be read or parsed, or if
    /// the final merged config fails validation.
    pub async fn load_merged(file: Option<&str>, dir: Option<&str>) -> Result<Self> {
        let mut config = match file {
            Some(path) => Self::load(path).await?,
            None => Config::default(),
        };

        if let Some(dir_path) = dir {
            let dir_config = Self::load_dir(dir_path).await?;
            // dir_config is already validated by load_dir, but we need to
            // re-merge and re-validate the combination
            config = config.merge(dir_config);
            config.validate()?;
        }

        Ok(config)
    }
}

impl FromStr for Config {
    type Err = RouterError;

    /// Parses router configuration from a TOML string.
    ///
    /// Implements the standard `FromStr` trait, allowing
    /// `str.parse::<Config>()` or `Config::from_str(s)`.
    ///
    /// # Example
    ///
    /// ```
    /// use mavrouter::config::Config;
    /// use std::str::FromStr;
    ///
    /// let toml = r#"
    /// [[endpoint]]
    /// type = "udp"
    /// address = "0.0.0.0:14550"
    /// "#;
    ///
    /// let config: Config = toml.parse().expect("valid config");
    /// ```
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Config::parse(s)
    }
}
