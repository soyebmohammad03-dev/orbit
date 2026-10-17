//! Store configuration.
//!
//! Configuration is built in code with [`StoreConfig::new`] and the `with_*`
//! methods, or read from `ORBIT_*` environment variables with
//! [`StoreConfig::from_env`]. Every configuration is validated before a
//! database is opened.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default maximum size of a canonical event payload: 1 MiB.
pub const DEFAULT_MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
/// Default maximum size of canonical event or stream metadata: 64 KiB.
pub const DEFAULT_MAX_METADATA_BYTES: usize = 64 * 1024;
/// Upper bound accepted for either size limit: 16 MiB.
pub const HARD_MAX_BYTES: usize = 16 * 1024 * 1024;
/// Default time to wait for a competing writer before failing with
/// [`StoreError::Busy`](crate::StoreError::Busy).
pub const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound accepted for the busy timeout.
pub const MAX_BUSY_TIMEOUT: Duration = Duration::from_secs(600);

/// How hard SQLite works to make a committed transaction survive a crash.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Durability {
    /// `synchronous = FULL` and, on macOS, `fullfsync = ON`. In WAL mode a
    /// committed append survives process crashes and, provided the storage
    /// device honours flush requests, power loss. This is the default.
    #[default]
    Full,
    /// `synchronous = NORMAL`. Committed appends survive process crashes,
    /// but the most recent commits may be rolled back by an operating-system
    /// crash or power loss. The database is never corrupted. Faster.
    Normal,
}

impl Durability {
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "full" => Some(Self::Full),
            "normal" => Some(Self::Normal),
            _ => None,
        }
    }
}

/// Validation limits applied to every append and stream creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum canonical JSON payload size in bytes.
    pub max_payload_bytes: usize,
    /// Maximum canonical JSON metadata size in bytes.
    pub max_metadata_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
            max_metadata_bytes: DEFAULT_MAX_METADATA_BYTES,
        }
    }
}

/// Invalid configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// A required environment variable was not set.
    #[error("environment variable {0} is required")]
    Missing(&'static str),
    /// A setting had an unparseable or out-of-range value.
    #[error("{name}={value:?} is invalid: {reason}")]
    Invalid {
        /// Setting name.
        name: &'static str,
        /// Rejected value.
        value: String,
        /// Why it was rejected.
        reason: String,
    },
}

/// Configuration for opening an [`EventStore`](crate::EventStore).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreConfig {
    /// Path of the SQLite database file. Created if it does not exist.
    pub path: PathBuf,
    /// Commit durability level.
    pub durability: Durability,
    /// How long to wait for a lock held by another connection.
    pub busy_timeout: Duration,
    /// Input size limits.
    pub limits: Limits,
}

impl StoreConfig {
    /// Environment variable holding the database path (required by [`from_env`](Self::from_env)).
    pub const ENV_PATH: &'static str = "ORBIT_DB_PATH";
    /// Environment variable selecting [`Durability`]: `full` or `normal`.
    pub const ENV_DURABILITY: &'static str = "ORBIT_DURABILITY";
    /// Environment variable for the busy timeout in milliseconds.
    pub const ENV_BUSY_TIMEOUT_MS: &'static str = "ORBIT_BUSY_TIMEOUT_MS";
    /// Environment variable for [`Limits::max_payload_bytes`].
    pub const ENV_MAX_PAYLOAD_BYTES: &'static str = "ORBIT_MAX_PAYLOAD_BYTES";
    /// Environment variable for [`Limits::max_metadata_bytes`].
    pub const ENV_MAX_METADATA_BYTES: &'static str = "ORBIT_MAX_METADATA_BYTES";

    /// Default configuration for the database at `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            durability: Durability::default(),
            busy_timeout: DEFAULT_BUSY_TIMEOUT,
            limits: Limits::default(),
        }
    }

    /// Sets the durability level.
    #[must_use]
    pub fn with_durability(mut self, durability: Durability) -> Self {
        self.durability = durability;
        self
    }

    /// Sets the busy timeout.
    #[must_use]
    pub fn with_busy_timeout(mut self, timeout: Duration) -> Self {
        self.busy_timeout = timeout;
        self
    }

    /// Sets the input size limits.
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Reads configuration from the process environment.
    ///
    /// # Errors
    /// Returns a [`ConfigError`] if `ORBIT_DB_PATH` is missing or any value
    /// is malformed or out of range.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads configuration through an arbitrary variable lookup. Used by
    /// [`from_env`](Self::from_env) and by tests.
    ///
    /// # Errors
    /// See [`from_env`](Self::from_env).
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        fn number(name: &'static str, value: &str) -> Result<u64, ConfigError> {
            value
                .trim()
                .parse::<u64>()
                .map_err(|e| ConfigError::Invalid {
                    name,
                    value: value.to_owned(),
                    reason: e.to_string(),
                })
        }
        fn size(name: &'static str, value: &str) -> Result<usize, ConfigError> {
            usize::try_from(number(name, value)?).map_err(|e| ConfigError::Invalid {
                name,
                value: value.to_owned(),
                reason: e.to_string(),
            })
        }

        let path = lookup(Self::ENV_PATH)
            .filter(|p| !p.trim().is_empty())
            .ok_or(ConfigError::Missing(Self::ENV_PATH))?;
        let mut config = Self::new(path);
        if let Some(value) = lookup(Self::ENV_DURABILITY) {
            config.durability =
                Durability::parse(value.trim()).ok_or_else(|| ConfigError::Invalid {
                    name: Self::ENV_DURABILITY,
                    value: value.clone(),
                    reason: "expected `full` or `normal`".to_owned(),
                })?;
        }
        if let Some(value) = lookup(Self::ENV_BUSY_TIMEOUT_MS) {
            config.busy_timeout = Duration::from_millis(number(Self::ENV_BUSY_TIMEOUT_MS, &value)?);
        }
        if let Some(value) = lookup(Self::ENV_MAX_PAYLOAD_BYTES) {
            config.limits.max_payload_bytes = size(Self::ENV_MAX_PAYLOAD_BYTES, &value)?;
        }
        if let Some(value) = lookup(Self::ENV_MAX_METADATA_BYTES) {
            config.limits.max_metadata_bytes = size(Self::ENV_MAX_METADATA_BYTES, &value)?;
        }
        config.validate()?;
        Ok(config)
    }

    /// Checks that every setting is within its accepted range.
    ///
    /// # Errors
    /// Returns [`ConfigError::Invalid`] naming the first offending setting.
    pub fn validate(&self) -> Result<(), ConfigError> {
        fn check_size(name: &'static str, value: usize) -> Result<(), ConfigError> {
            if value == 0 || value > HARD_MAX_BYTES {
                return Err(ConfigError::Invalid {
                    name,
                    value: value.to_string(),
                    reason: format!("must be between 1 and {HARD_MAX_BYTES} bytes"),
                });
            }
            Ok(())
        }
        if self.path.as_os_str().is_empty() || is_memory_path(&self.path) {
            return Err(ConfigError::Invalid {
                name: "path",
                value: self.path.display().to_string(),
                reason: "a file path is required (in-memory databases are not durable)".to_owned(),
            });
        }
        if self.busy_timeout > MAX_BUSY_TIMEOUT {
            return Err(ConfigError::Invalid {
                name: "busy_timeout",
                value: format!("{:?}", self.busy_timeout),
                reason: format!("must not exceed {MAX_BUSY_TIMEOUT:?}"),
            });
        }
        check_size("max_payload_bytes", self.limits.max_payload_bytes)?;
        check_size("max_metadata_bytes", self.limits.max_metadata_bytes)?;
        Ok(())
    }
}

fn is_memory_path(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|p| p == ":memory:" || p.starts_with("file::memory:"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn defaults() {
        let config = StoreConfig::new("orbit.db");
        assert_eq!(config.durability, Durability::Full);
        assert_eq!(config.busy_timeout, DEFAULT_BUSY_TIMEOUT);
        assert_eq!(config.limits, Limits::default());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn from_lookup_reads_all_settings() {
        let config = StoreConfig::from_lookup(lookup(&[
            ("ORBIT_DB_PATH", "/tmp/x.db"),
            ("ORBIT_DURABILITY", "Normal"),
            ("ORBIT_BUSY_TIMEOUT_MS", "250"),
            ("ORBIT_MAX_PAYLOAD_BYTES", "2048"),
            ("ORBIT_MAX_METADATA_BYTES", "512"),
        ]))
        .unwrap();
        assert_eq!(config.path, PathBuf::from("/tmp/x.db"));
        assert_eq!(config.durability, Durability::Normal);
        assert_eq!(config.busy_timeout, Duration::from_millis(250));
        assert_eq!(config.limits.max_payload_bytes, 2048);
        assert_eq!(config.limits.max_metadata_bytes, 512);
    }

    #[test]
    fn from_lookup_rejects_bad_values() {
        assert_eq!(
            StoreConfig::from_lookup(lookup(&[])),
            Err(ConfigError::Missing("ORBIT_DB_PATH"))
        );
        for (name, value) in [
            ("ORBIT_DURABILITY", "paranoid"),
            ("ORBIT_BUSY_TIMEOUT_MS", "-1"),
            ("ORBIT_BUSY_TIMEOUT_MS", "999999999"),
            ("ORBIT_MAX_PAYLOAD_BYTES", "0"),
            ("ORBIT_MAX_PAYLOAD_BYTES", "lots"),
            ("ORBIT_MAX_METADATA_BYTES", "999999999999"),
        ] {
            let result =
                StoreConfig::from_lookup(lookup(&[("ORBIT_DB_PATH", "x.db"), (name, value)]));
            assert!(
                matches!(result, Err(ConfigError::Invalid { .. })),
                "{name}={value} -> {result:?}"
            );
        }
    }

    #[test]
    fn rejects_in_memory_paths() {
        assert!(StoreConfig::new(":memory:").validate().is_err());
        assert!(StoreConfig::new("").validate().is_err());
    }
}
