//! Typed errors. Each layer returns its own error enum so call sites
//! must handle failure explicitly instead of stringly-typed panics.

use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum DomainError {
    #[error("target name must not be empty")]
    EmptyTargetName,
    #[error("target name too long: {len} > {max}")]
    TargetNameTooLong { len: usize, max: usize },
    #[error("host must not be empty")]
    EmptyHost,
    #[error("host too long: {len} > {max}")]
    HostTooLong { len: usize, max: usize },
    #[error("invalid host {host:?}: {reason}")]
    InvalidHost { host: String, reason: &'static str },
    #[error("invalid port {port}: must be 1..=65535")]
    InvalidPort { port: u16 },
    #[error("invalid check interval {secs}s: must be 1..=3600")]
    InvalidInterval { secs: u64 },
    #[error("invalid probe timeout {secs}s: must be 1..=60")]
    InvalidTimeout { secs: u64 },
    #[error("invalid history limit {n}: must be 1..=10000")]
    InvalidHistoryLimit { n: usize },
    #[error("invalid lookback {hours:?}h: must be 0.05..=720")]
    InvalidLookback { hours: f64 },
    #[error("negative latency {ms}ms")]
    NegativeLatency { ms: i64 },
    #[error("at least one monitor target is required")]
    NoTargets,
    #[error("duplicate target name {name:?}")]
    DuplicateTarget { name: String },
    #[error("db path must not be empty")]
    EmptyDbPath,
    #[error(
        "inconsistent check row: success={success} latency={latency_ms:?} has_error={has_error}"
    )]
    InconsistentCheckRow {
        success: bool,
        latency_ms: Option<i64>,
        has_error: bool,
    },
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error("invalid config file {path:?}: {reason}")]
    InvalidFile { path: String, reason: String },
    #[error("probe timeout ({timeout}s) must not exceed check interval ({interval}s)")]
    TimeoutExceedsInterval { timeout: u64, interval: u64 },
    #[error("invalid TARGETS entry {index}: {entry:?} ({reason})")]
    InvalidTargetEntry {
        index: usize,
        entry: String,
        reason: String,
    },
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Domain(#[from] DomainError),
}

impl From<StoreError> for String {
    fn from(e: StoreError) -> String {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn every_domain_error_displays() {
        let variants = vec![
            DomainError::EmptyTargetName,
            DomainError::TargetNameTooLong { len: 99, max: 64 },
            DomainError::EmptyHost,
            DomainError::HostTooLong { len: 300, max: 253 },
            DomainError::InvalidHost {
                host: "x y".into(),
                reason: "test",
            },
            DomainError::InvalidPort { port: 0 },
            DomainError::InvalidInterval { secs: 0 },
            DomainError::InvalidTimeout { secs: 99 },
            DomainError::InvalidHistoryLimit { n: 0 },
            DomainError::InvalidLookback { hours: f64::NAN },
            DomainError::NegativeLatency { ms: -1 },
            DomainError::NoTargets,
            DomainError::DuplicateTarget { name: "a".into() },
            DomainError::EmptyDbPath,
            DomainError::InconsistentCheckRow {
                success: true,
                latency_ms: Some(1),
                has_error: true,
            },
        ];
        for e in variants {
            assert!(!e.to_string().is_empty(), "{e:?}");
        }
    }

    #[test]
    fn every_config_error_displays() {
        let variants = vec![
            ConfigError::Domain(DomainError::NoTargets),
            ConfigError::InvalidFile {
                path: "c.json".into(),
                reason: "boom".into(),
            },
            ConfigError::TimeoutExceedsInterval {
                timeout: 10,
                interval: 5,
            },
            ConfigError::InvalidTargetEntry {
                index: 2,
                entry: "bad".into(),
                reason: "nope".into(),
            },
        ];
        for e in variants {
            assert!(!e.to_string().is_empty(), "{e:?}");
        }
    }

    #[test]
    fn store_error_converts_and_displays() {
        let sqlite: StoreError = rusqlite::Error::InvalidPath(PathBuf::from("/bad")).into();
        assert!(sqlite.to_string().contains("sqlite"));
        let s: String = sqlite.into();
        assert!(!s.is_empty());

        let domain: StoreError = DomainError::NoTargets.into();
        assert!(domain.to_string().contains("target"));
        let s: String = domain.into();
        assert!(!s.is_empty());
    }
}
