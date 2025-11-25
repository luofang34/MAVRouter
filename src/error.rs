//! Custom error types for mavrouter-rs.
//!
//! This module defines structured error types that provide better error handling
//! and debugging compared to using `anyhow::Error` everywhere. Each error variant
//! includes contextual information about what went wrong and where.

use std::io;
use thiserror::Error;

/// Main error type for mavrouter-rs operations.
///
/// This enum covers all possible error scenarios that can occur during
/// router operation, from configuration parsing to network communication.
#[derive(Error, Debug)]
pub enum RouterError {
    /// Configuration-related errors (parsing, validation, missing files)
    #[error("Configuration error: {0}")]
    Config(String),

    /// Network I/O errors (connection failures, socket errors)
    #[error("Network error on endpoint '{endpoint}': {source}")]
    Network {
        /// Name or address of the endpoint that failed
        endpoint: String,
        /// Underlying I/O error
        #[source]
        source: io::Error,
    },

    /// Serial port errors (device not found, permission denied, hardware issues)
    #[error("Serial port error on '{device}': {source}")]
    Serial {
        /// Path to the serial device
        device: String,
        /// Underlying serial error
        #[source]
        source: tokio_serial::Error,
    },

    /// Routing errors (invalid system/component IDs, routing table issues)
    #[error("Routing error: {0}")]
    Routing(String),

    /// MAVLink protocol errors (invalid frames, parsing failures)
    #[error("MAVLink protocol error: {0}")]
    Protocol(String),

    /// File system errors (log file creation, stats socket)
    #[error("Filesystem error at '{path}': {source}")]
    Filesystem {
        /// Path that caused the error
        path: String,
        /// Underlying I/O error
        #[source]
        source: io::Error,
    },

    /// Endpoint initialization errors
    #[error("Failed to initialize {endpoint_type} endpoint: {reason}")]
    EndpointInit {
        /// Type of endpoint (TCP, UDP, Serial, etc.)
        endpoint_type: String,
        /// Reason for failure
        reason: String,
    },

    /// Other unexpected errors
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Type alias for Results that use RouterError
pub type Result<T> = std::result::Result<T, RouterError>;

impl RouterError {
    /// Create a new configuration error
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// Create a new network error
    pub fn network(endpoint: impl Into<String>, source: io::Error) -> Self {
        Self::Network {
            endpoint: endpoint.into(),
            source,
        }
    }

    /// Create a new serial error
    pub fn serial(device: impl Into<String>, source: tokio_serial::Error) -> Self {
        Self::Serial {
            device: device.into(),
            source,
        }
    }

    /// Create a new routing error
    pub fn routing(msg: impl Into<String>) -> Self {
        Self::Routing(msg.into())
    }

    /// Create a new protocol error
    pub fn protocol(msg: impl Into<String>) -> Self {
        Self::Protocol(msg.into())
    }

    /// Create a new filesystem error
    pub fn filesystem(path: impl Into<String>, source: io::Error) -> Self {
        Self::Filesystem {
            path: path.into(),
            source,
        }
    }

    /// Create a new endpoint initialization error
    pub fn endpoint_init(endpoint_type: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::EndpointInit {
            endpoint_type: endpoint_type.into(),
            reason: reason.into(),
        }
    }

    /// Create a new internal error
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}

/// Convert from anyhow::Error (for gradual migration)
impl From<anyhow::Error> for RouterError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal(err.to_string())
    }
}

/// Convert from std::io::Error
impl From<io::Error> for RouterError {
    fn from(err: io::Error) -> Self {
        Self::Network {
            endpoint: "unknown".to_string(),
            source: err,
        }
    }
}

/// Convert from tokio_serial::Error
impl From<tokio_serial::Error> for RouterError {
    fn from(err: tokio_serial::Error) -> Self {
        Self::Serial {
            device: "unknown".to_string(),
            source: err,
        }
    }
}
