//! Custom error types for mavrouter-rs.
//!
//! This module defines structured error types that provide better error handling
//! and debugging compared to untyped errors. Each error variant includes
//! contextual information about what went wrong and where.

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

    /// Serial port open exceeded the configured timeout.
    ///
    /// Distinct from [`Self::Serial`] so logs and callers can tell
    /// "device responded with an error" apart from "device never
    /// responded at all" (driver wedged, USB adapter disconnected
    /// mid-enumeration, etc.).
    #[error("Serial port open on '{device}' timed out after {timeout_secs}s")]
    SerialTimeout {
        /// Path to the serial device
        device: String,
        /// The timeout that elapsed, in whole seconds
        timeout_secs: u64,
    },

    /// File system errors (log file creation, stats socket)
    #[error("Filesystem error at '{path}': {source}")]
    Filesystem {
        /// Path that caused the error
        path: String,
        /// Underlying I/O error
        #[source]
        source: io::Error,
    },
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

    /// Create a new serial-open timeout error
    pub fn serial_timeout(device: impl Into<String>, timeout_secs: u64) -> Self {
        Self::SerialTimeout {
            device: device.into(),
            timeout_secs,
        }
    }

    /// Create a new filesystem error
    pub fn filesystem(path: impl Into<String>, source: io::Error) -> Self {
        Self::Filesystem {
            path: path.into(),
            source,
        }
    }
}

// Blanket From<io::Error>, From<tokio_serial::Error>, and From<anyhow::Error> impls
// intentionally omitted. Use RouterError::config(), RouterError::network(),
// RouterError::serial(), or RouterError::filesystem() with .map_err() at each
// call site to provide proper context instead of silently wrapping opaque errors.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_error_creation() {
        let err = RouterError::config("invalid setting");
        assert!(matches!(err, RouterError::Config(_)));
        assert!(err.to_string().contains("invalid setting"));
    }

    #[test]
    fn test_network_error_creation() {
        let io_err = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
        let err = RouterError::network("127.0.0.1:5760", io_err);
        assert!(matches!(err, RouterError::Network { .. }));
        assert!(err.to_string().contains("127.0.0.1:5760"));
    }

    #[test]
    fn test_serial_timeout_error_creation() {
        let err = RouterError::serial_timeout("/dev/ttyUSB0", 3);
        assert!(matches!(
            err,
            RouterError::SerialTimeout {
                ref device,
                timeout_secs: 3,
            } if device == "/dev/ttyUSB0"
        ));
        assert!(err.to_string().contains("/dev/ttyUSB0"));
        assert!(err.to_string().contains("3s"));
    }

    #[test]
    fn test_filesystem_error_creation() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "not found");
        let err = RouterError::filesystem("/var/log/test.log", io_err);
        assert!(matches!(err, RouterError::Filesystem { .. }));
        assert!(err.to_string().contains("/var/log/test.log"));
    }

    #[test]
    fn test_io_error_requires_context() {
        // Verify that io::Error must be wrapped with context via helper methods
        let io_err = io::Error::new(io::ErrorKind::BrokenPipe, "broken");
        let router_err = RouterError::network("test-endpoint", io_err);
        assert!(matches!(router_err, RouterError::Network { .. }));
        assert!(router_err.to_string().contains("test-endpoint"));
    }
}
