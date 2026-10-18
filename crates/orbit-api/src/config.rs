//! API server configuration, read from `ORBIT_*` environment variables.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use orbit_core::config::{ConfigError, StoreConfig};

/// Default listen address: loopback only.
pub const DEFAULT_ADDR: &str = "127.0.0.1:7878";
/// Default database path, relative to the working directory.
pub const DEFAULT_DB_PATH: &str = "data/orbit.db";
/// Default lab directory (experiment registry and artifacts), relative to the working directory.
pub const DEFAULT_LAB_DIR: &str = "data/lab";
/// Default allowed CORS origins: the local Next.js development server.
pub const DEFAULT_CORS_ORIGINS: &str = "http://localhost:3000,http://127.0.0.1:3000";
/// Host names always accepted in the `Host` header (loopback only).
pub const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];
/// Default per-request deadline.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest accepted per-request deadline.
pub const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Configuration of the API server.
#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// Address to listen on.
    pub addr: SocketAddr,
    /// Store configuration (database path, durability, limits).
    pub store: StoreConfig,
    /// Browser origins allowed by CORS.
    pub cors_origins: Vec<String>,
    /// Directory of Criterion results to expose read-only at `/api/v1/benchmarks`.
    pub benchmark_dir: Option<PathBuf>,
    /// Accepted `Host` header names (lowercase, without port). Requests with
    /// any other `Host` are rejected, which defeats DNS-rebinding attacks.
    pub allowed_hosts: Vec<String>,
    /// Deadline for one request.
    pub request_timeout: Duration,
    /// Lab directory (`lab.db` and artifacts), or `None` to disable the lab
    /// endpoints.
    pub lab_dir: Option<PathBuf>,
}

/// Invalid API configuration.
#[derive(Debug, thiserror::Error)]
pub enum ApiConfigError {
    /// Store settings were invalid.
    #[error(transparent)]
    Store(#[from] ConfigError),
    /// The listen address could not be parsed.
    #[error("ORBIT_API_ADDR={0:?} is not a socket address")]
    Addr(String),
    /// A setting had an unparseable or out-of-range value.
    #[error("{name}={value:?} is invalid: {reason}")]
    Invalid {
        /// Variable name.
        name: &'static str,
        /// Rejected value.
        value: String,
        /// Why.
        reason: String,
    },
    /// A non-loopback address was requested without explicit opt-in.
    #[error(
        "refusing to listen on non-loopback address {0}: the API has no authentication. \
         Set ORBIT_API_ALLOW_REMOTE=1 to override"
    )]
    RemoteNotAllowed(SocketAddr),
}

impl ApiConfig {
    /// Reads the configuration from the process environment.
    ///
    /// # Errors
    /// See [`ApiConfigError`].
    pub fn from_env() -> Result<Self, ApiConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads the configuration through an arbitrary lookup (used by tests).
    ///
    /// | Variable | Default |
    /// |---|---|
    /// | `ORBIT_API_ADDR` | `127.0.0.1:7878` |
    /// | `ORBIT_API_ALLOW_REMOTE` | unset (only loopback addresses allowed) |
    /// | `ORBIT_DB_PATH` | `data/orbit.db` |
    /// | `ORBIT_API_CORS_ORIGINS` | `http://localhost:3000,http://127.0.0.1:3000` |
    /// | `ORBIT_BENCHMARK_DIR` | `target/criterion` if it exists |
    /// | `ORBIT_API_ALLOWED_HOSTS` | none (only `localhost`, `127.0.0.1`, `[::1]` are accepted in `Host`) |
    /// | `ORBIT_API_REQUEST_TIMEOUT_SECS` | `30` (1–600) |
    /// | `ORBIT_LAB_DIR` | `data/lab` (empty disables the lab endpoints) |
    ///
    /// plus every store variable understood by [`StoreConfig::from_lookup`].
    ///
    /// # Errors
    /// See [`ApiConfigError`].
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ApiConfigError> {
        let addr_text = lookup("ORBIT_API_ADDR").unwrap_or_else(|| DEFAULT_ADDR.to_owned());
        let addr: SocketAddr = addr_text
            .trim()
            .parse()
            .map_err(|_| ApiConfigError::Addr(addr_text.clone()))?;
        let allow_remote =
            lookup("ORBIT_API_ALLOW_REMOTE").is_some_and(|v| v == "1" || v == "true");
        if !is_loopback(addr.ip()) && !allow_remote {
            return Err(ApiConfigError::RemoteNotAllowed(addr));
        }
        let store = StoreConfig::from_lookup(|name| {
            lookup(name)
                .or_else(|| (name == StoreConfig::ENV_PATH).then(|| DEFAULT_DB_PATH.to_owned()))
        })?;
        let cors_origins = lookup("ORBIT_API_CORS_ORIGINS")
            .unwrap_or_else(|| DEFAULT_CORS_ORIGINS.to_owned())
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let benchmark_dir = match lookup("ORBIT_BENCHMARK_DIR") {
            Some(dir) if !dir.trim().is_empty() => Some(PathBuf::from(dir)),
            Some(_) => None,
            None => Some(PathBuf::from("target/criterion")).filter(|p| p.is_dir()),
        };
        let mut allowed_hosts: Vec<String> =
            LOOPBACK_HOSTS.iter().map(|h| (*h).to_owned()).collect();
        if let Some(extra) = lookup("ORBIT_API_ALLOWED_HOSTS") {
            for host in extra.split(',').map(str::trim).filter(|h| !h.is_empty()) {
                let host = host.to_ascii_lowercase();
                if host.contains('/') || host.contains(' ') {
                    return Err(ApiConfigError::Invalid {
                        name: "ORBIT_API_ALLOWED_HOSTS",
                        value: extra.clone(),
                        reason: "expected comma-separated host names without scheme or path"
                            .to_owned(),
                    });
                }
                if !allowed_hosts.contains(&host) {
                    allowed_hosts.push(host);
                }
            }
        }
        let request_timeout = match lookup("ORBIT_API_REQUEST_TIMEOUT_SECS") {
            None => DEFAULT_REQUEST_TIMEOUT,
            Some(value) => {
                let secs = value
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|s| (1..=MAX_REQUEST_TIMEOUT.as_secs()).contains(s));
                Duration::from_secs(secs.ok_or_else(|| ApiConfigError::Invalid {
                    name: "ORBIT_API_REQUEST_TIMEOUT_SECS",
                    value: value.clone(),
                    reason: format!(
                        "expected whole seconds between 1 and {}",
                        MAX_REQUEST_TIMEOUT.as_secs()
                    ),
                })?)
            }
        };
        let lab_dir = match lookup("ORBIT_LAB_DIR") {
            Some(dir) if dir.trim().is_empty() => None,
            Some(dir) => Some(PathBuf::from(dir)),
            None => Some(PathBuf::from(DEFAULT_LAB_DIR)),
        };
        Ok(Self {
            addr,
            store,
            cors_origins,
            benchmark_dir,
            allowed_hosts,
            request_timeout,
            lab_dir,
        })
    }
}

fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    #[test]
    fn defaults_are_local_only() {
        let config = ApiConfig::from_lookup(lookup(&[("ORBIT_BENCHMARK_DIR", "")])).unwrap();
        assert_eq!(config.addr.to_string(), DEFAULT_ADDR);
        assert_eq!(config.store.path, PathBuf::from(DEFAULT_DB_PATH));
        assert_eq!(
            config.cors_origins,
            vec!["http://localhost:3000", "http://127.0.0.1:3000"]
        );
        assert_eq!(config.benchmark_dir, None);
        assert_eq!(config.lab_dir, Some(PathBuf::from(DEFAULT_LAB_DIR)));
        let disabled = ApiConfig::from_lookup(lookup(&[("ORBIT_LAB_DIR", " ")])).unwrap();
        assert_eq!(disabled.lab_dir, None);
    }

    #[test]
    fn remote_binding_requires_opt_in() {
        assert!(matches!(
            ApiConfig::from_lookup(lookup(&[("ORBIT_API_ADDR", "0.0.0.0:7878")])),
            Err(ApiConfigError::RemoteNotAllowed(_))
        ));
        assert!(
            ApiConfig::from_lookup(lookup(&[
                ("ORBIT_API_ADDR", "0.0.0.0:7878"),
                ("ORBIT_API_ALLOW_REMOTE", "1")
            ]))
            .is_ok()
        );
        assert!(ApiConfig::from_lookup(lookup(&[("ORBIT_API_ADDR", "[::1]:9000")])).is_ok());
        assert!(matches!(
            ApiConfig::from_lookup(lookup(&[("ORBIT_API_ADDR", "nonsense")])),
            Err(ApiConfigError::Addr(_))
        ));
    }

    #[test]
    fn host_allowlist_and_timeout() {
        let config = ApiConfig::from_lookup(lookup(&[])).unwrap();
        assert_eq!(
            config.allowed_hosts,
            vec!["localhost", "127.0.0.1", "[::1]"]
        );
        assert_eq!(config.request_timeout, DEFAULT_REQUEST_TIMEOUT);
        let config = ApiConfig::from_lookup(lookup(&[
            ("ORBIT_API_ALLOWED_HOSTS", "Orbit.Internal, localhost"),
            ("ORBIT_API_REQUEST_TIMEOUT_SECS", "5"),
        ]))
        .unwrap();
        assert_eq!(
            config.allowed_hosts,
            vec!["localhost", "127.0.0.1", "[::1]", "orbit.internal"]
        );
        assert_eq!(config.request_timeout, Duration::from_secs(5));
        for (name, value) in [
            ("ORBIT_API_REQUEST_TIMEOUT_SECS", "0"),
            ("ORBIT_API_REQUEST_TIMEOUT_SECS", "601"),
            ("ORBIT_API_REQUEST_TIMEOUT_SECS", "soon"),
            ("ORBIT_API_ALLOWED_HOSTS", "http://evil.test/"),
        ] {
            assert!(
                matches!(
                    ApiConfig::from_lookup(lookup(&[(name, value)])),
                    Err(ApiConfigError::Invalid { .. })
                ),
                "{name}={value}"
            );
        }
    }

    #[test]
    fn store_settings_pass_through() {
        let config = ApiConfig::from_lookup(lookup(&[
            ("ORBIT_DB_PATH", "/tmp/x.db"),
            ("ORBIT_DURABILITY", "normal"),
            ("ORBIT_API_CORS_ORIGINS", "http://a.test, http://b.test"),
        ]))
        .unwrap();
        assert_eq!(config.store.path, PathBuf::from("/tmp/x.db"));
        assert_eq!(config.store.durability, orbit_core::Durability::Normal);
        assert_eq!(config.cors_origins, vec!["http://a.test", "http://b.test"]);
        assert!(matches!(
            ApiConfig::from_lookup(lookup(&[("ORBIT_DURABILITY", "maybe")])),
            Err(ApiConfigError::Store(_))
        ));
    }
}
