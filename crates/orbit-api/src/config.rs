//! API server configuration, read from `ORBIT_*` environment variables.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use orbit_core::config::{ConfigError, StoreConfig};

/// Default listen address: loopback only.
pub const DEFAULT_ADDR: &str = "127.0.0.1:7878";
/// Default database path, relative to the working directory.
pub const DEFAULT_DB_PATH: &str = "data/orbit.db";
/// Default allowed CORS origins: the local Next.js development server.
pub const DEFAULT_CORS_ORIGINS: &str = "http://localhost:3000,http://127.0.0.1:3000";

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
        Ok(Self {
            addr,
            store,
            cors_origins,
            benchmark_dir,
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
