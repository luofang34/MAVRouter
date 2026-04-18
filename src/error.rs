//! Custom error types for mavrouter-rs.
//!
//! This module defines structured error types that provide better error handling
//! and debugging compared to untyped errors. Each error variant carries the
//! contextual information needed to diagnose the failure (paths, IDs, source
//! errors with span/line context, etc.).

use std::io;
use std::path::PathBuf;
use thiserror::Error;

/// Main error type for mavrouter-rs operations.
///
/// Variants split by concern (config parse vs config validate vs network vs
/// serial vs filesystem) so callers can pattern-match without parsing strings.
/// TOML parse errors carry the underlying [`toml::de::Error`] via `#[source]`,
/// so span/line context is preserved across the error chain.
#[derive(Error, Debug)]
pub enum RouterError {
    /// Failed to parse a TOML configuration string (no file context).
    ///
    /// The wrapped [`toml::de::Error`] carries span/line information; access
    /// it via [`std::error::Error::source`] or render the chain with `{:#}`.
    #[error("Failed to parse TOML config: {source}")]
    ConfigParse {
        /// Underlying TOML parse error (preserves line, column, span).
        #[source]
        source: toml::de::Error,
    },

    /// Failed to parse a TOML configuration file at the given path.
    ///
    /// The wrapped [`toml::de::Error`] carries span/line information within
    /// the file. The `path` field locates the offending file.
    #[error("Failed to parse TOML config file '{}': {source}", path.display())]
    ConfigParseFile {
        /// Path of the TOML file that failed to parse.
        path: PathBuf,
        /// Underlying TOML parse error (preserves line, column, span).
        #[source]
        source: toml::de::Error,
    },

    /// Configuration is syntactically valid TOML but fails semantic
    /// validation: duplicate ports, out-of-range numeric fields, malformed
    /// addresses, baud rates outside supported range, missing endpoints, etc.
    ///
    /// The `context` string locates the offending field (typically the
    /// endpoint index and field name).
    #[error("Configuration validation error: {context}")]
    ConfigValidation {
        /// Human-readable description locating the validation failure.
        context: String,
    },

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
    /// Wrap a TOML parse error from an inline string (no file path).
    pub fn config_parse(source: toml::de::Error) -> Self {
        Self::ConfigParse { source }
    }

    /// Wrap a TOML parse error from a known file path. Preserves both the
    /// path and the original `toml::de::Error` (which carries span/line).
    pub fn config_parse_file(path: impl Into<PathBuf>, source: toml::de::Error) -> Self {
        Self::ConfigParseFile {
            path: path.into(),
            source,
        }
    }

    /// Construct a semantic validation error (duplicate ports, malformed
    /// addresses, out-of-range numeric fields, etc.). Use this when the
    /// failure isn't from `toml::from_str` but from a downstream sanity
    /// check on the parsed `Config`.
    pub fn config_validation(context: impl Into<String>) -> Self {
        Self::ConfigValidation {
            context: context.into(),
        }
    }

    /// Backwards-compatible constructor for semantic validation errors.
    ///
    /// Equivalent to [`Self::config_validation`]; retained because it's part
    /// of the published public API surface (see `tests/public_api_test.rs`).
    /// New code should prefer [`Self::config_parse`], [`Self::config_parse_file`],
    /// or [`Self::config_validation`] for the most precise variant.
    pub fn config(msg: impl Into<String>) -> Self {
        Self::config_validation(msg)
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
// intentionally omitted. Use the named constructors with `.map_err()` at each
// call site to provide proper context instead of silently wrapping opaque errors.

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(clippy::panic)]

    use super::*;
    use std::error::Error as _;

    #[test]
    fn config_validation_constructor_works() {
        let err = RouterError::config_validation("invalid setting");
        assert!(matches!(err, RouterError::ConfigValidation { .. }));
        assert!(err.to_string().contains("invalid setting"));
    }

    #[test]
    fn legacy_config_constructor_maps_to_validation() {
        let err = RouterError::config("invalid setting");
        assert!(matches!(err, RouterError::ConfigValidation { .. }));
        assert!(err.to_string().contains("invalid setting"));
    }

    #[test]
    fn config_parse_preserves_toml_span() {
        // Force toml to fail on a known-bad inline string. The toml::de::Error
        // Display includes line/column markers — verify our error chain
        // surfaces them so operators see *where* in the file the parse failed.
        let bad = "key = \"unterminated\nkey2 = 1\n";
        let parse_err = toml::from_str::<toml::Value>(bad).expect_err("expected parse failure");
        let raw_msg = parse_err.to_string();

        let wrapped = RouterError::config_parse(parse_err);
        assert!(matches!(wrapped, RouterError::ConfigParse { .. }));

        // The source chain must surface the original toml::de::Error so its
        // span/line context is reachable. `to_string()` on the source must
        // match what toml itself produced.
        let source = wrapped.source().expect("ConfigParse should expose source");
        assert_eq!(source.to_string(), raw_msg);
    }

    #[test]
    fn config_parse_file_preserves_path_and_span() {
        let bad = "key =\n";
        let parse_err = toml::from_str::<toml::Value>(bad).expect_err("expected parse failure");
        let wrapped = RouterError::config_parse_file("/tmp/foo.toml", parse_err);
        let display = wrapped.to_string();
        assert!(display.contains("/tmp/foo.toml"));
        assert!(matches!(wrapped, RouterError::ConfigParseFile { .. }));
        assert!(wrapped.source().is_some());
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
