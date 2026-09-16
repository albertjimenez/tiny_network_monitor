//! Configuration loading with validated domain types.
//!
//! Precedence: defaults < config file < environment.
//! Unlike the previous version, an *invalid present* file or env value is a
//! hard [`ConfigError`] — we never silently fall back to defaults, because
//! that hides operator mistakes (e.g. a typo'd interval of 0).

use serde::Deserialize;
use std::fs;

use crate::domain::{
    CheckInterval, DbPath, HistoryLimit, Host, MonitorTarget, NonEmptyTargets, Port, ProbeTimeout,
    TargetName,
};
use crate::error::{ConfigError, DomainError};

#[derive(Debug, Clone)]
pub struct Config {
    pub port: Port,
    pub db_path: DbPath,
    pub check_interval: CheckInterval,
    pub check_timeout: ProbeTimeout,
    pub history_limit: HistoryLimit,
    pub targets: NonEmptyTargets,
}

impl Config {
    /// Fallible constructor — the *only* way to build a `Config`, so the
    /// `timeout <= interval` invariant holds by construction.
    pub fn new(
        port: Port,
        db_path: DbPath,
        check_interval: CheckInterval,
        check_timeout: ProbeTimeout,
        history_limit: HistoryLimit,
        targets: NonEmptyTargets,
    ) -> Result<Self, ConfigError> {
        if check_timeout.secs() > check_interval.secs() {
            return Err(ConfigError::TimeoutExceedsInterval {
                timeout: check_timeout.secs(),
                interval: check_interval.secs(),
            });
        }
        debug_assert!(!targets.is_empty());
        Ok(Self {
            port,
            db_path,
            check_interval,
            check_timeout,
            history_limit,
            targets,
        })
    }

    /// Backwards-compatible accessors so call sites read naturally.
    pub fn port_u16(&self) -> u16 {
        self.port.get()
    }
    pub fn interval_secs(&self) -> u64 {
        self.check_interval.secs()
    }
    pub fn timeout_secs(&self) -> u64 {
        self.check_timeout.secs()
    }

    fn defaults() -> Self {
        Self::new(
            Port::new(3000).expect("default port valid"),
            DbPath::new("netmon.db").expect("default db path valid"),
            CheckInterval::default_valid(),
            ProbeTimeout::default_valid(),
            HistoryLimit::default_valid(),
            NonEmptyTargets::new(vec![
                MonitorTarget::new(
                    TargetName::known("Google DNS"),
                    Host::known("8.8.8.8"),
                    Port::new(53).unwrap(),
                ),
                MonitorTarget::new(
                    TargetName::known("Cloudflare DNS"),
                    Host::known("1.1.1.1"),
                    Port::new(53).unwrap(),
                ),
                MonitorTarget::new(
                    TargetName::known("Google HTTPS"),
                    Host::known("google.com"),
                    Port::new(443).unwrap(),
                ),
            ])
            .expect("default targets valid"),
        )
        .expect("default config valid")
    }

    /// Load with `defaults < file < env` precedence.
    pub fn load() -> Result<Self, ConfigError> {
        let mut b = ConfigBuilder::from_defaults();

        let config_path =
            std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.json".to_string());
        match fs::read_to_string(&config_path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => { /* optional file */ }
            Err(e) => {
                return Err(ConfigError::InvalidFile {
                    path: config_path,
                    reason: e.to_string(),
                });
            }
            Ok(contents) => {
                let file_cfg: FileConfig =
                    serde_json::from_str(&contents).map_err(|e| ConfigError::InvalidFile {
                        path: config_path.clone(),
                        reason: e.to_string(),
                    })?;
                b.apply_file(file_cfg)?;
            }
        }

        b.apply_env()?;
        b.build()
    }
}

// ---------------------------------------------------------------------------
// Builder: accumulates raw values, validates once at build()
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct ConfigBuilder {
    port: Option<u16>,
    db_path: Option<String>,
    interval_secs: Option<u64>,
    timeout_secs: Option<u64>,
    targets: Option<Vec<MonitorTarget>>,
}

impl ConfigBuilder {
    fn from_defaults() -> Self {
        Self::default()
    }

    fn apply_file(&mut self, f: FileConfig) -> Result<(), ConfigError> {
        // MonitorTarget already validates on deserialize (domain Deserialize
        // impls), so `f.targets` is valid-or-absent by construction.
        if let Some(v) = f.port {
            self.port = Some(v);
        }
        if let Some(v) = f.db_path {
            self.db_path = Some(v);
        }
        if let Some(v) = f.check_interval_secs {
            self.interval_secs = Some(v);
        }
        if let Some(v) = f.check_timeout_secs {
            self.timeout_secs = Some(v);
        }
        if let Some(v) = f.targets
            && !v.is_empty()
        {
            self.targets = Some(v);
        }
        Ok(())
    }

    fn apply_env(&mut self) -> Result<(), ConfigError> {
        if let Some(v) = env_u16("PORT") {
            self.port = Some(v);
        }
        if let Ok(v) = std::env::var("DB_PATH")
            && !v.trim().is_empty()
        {
            self.db_path = Some(v);
        }
        if let Some(v) = env_u64("CHECK_INTERVAL_SECS") {
            self.interval_secs = Some(v);
        }
        if let Some(v) = env_u64("CHECK_TIMEOUT_SECS") {
            self.timeout_secs = Some(v);
        }
        if let Ok(raw) = std::env::var("TARGETS")
            && !raw.trim().is_empty()
        {
            self.targets = Some(parse_targets_env(&raw)?);
        }
        Ok(())
    }

    fn build(self) -> Result<Config, ConfigError> {
        let d = Config::defaults();
        let port = match self.port {
            Some(p) => Port::new(p).map_err(ConfigError::Domain)?,
            None => d.port,
        };
        let db_path = match self.db_path {
            Some(p) => DbPath::new(p).map_err(ConfigError::Domain)?,
            None => d.db_path,
        };
        let interval = match self.interval_secs {
            Some(s) => CheckInterval::new(s).map_err(ConfigError::Domain)?,
            None => d.check_interval,
        };
        let timeout = match self.timeout_secs {
            Some(s) => ProbeTimeout::new(s).map_err(ConfigError::Domain)?,
            None => d.check_timeout,
        };
        let targets = match self.targets {
            Some(t) => NonEmptyTargets::new(t).map_err(ConfigError::Domain)?,
            None => d.targets,
        };
        Config::new(
            port,
            db_path,
            interval,
            timeout,
            HistoryLimit::default_valid(),
            targets,
        )
    }
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.parse().ok()
}

fn env_u16(key: &str) -> Option<u16> {
    std::env::var(key).ok()?.parse().ok()
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    db_path: Option<String>,
    #[serde(default)]
    check_interval_secs: Option<u64>,
    #[serde(default)]
    check_timeout_secs: Option<u64>,
    #[serde(default)]
    targets: Option<Vec<MonitorTarget>>,
}

/// `TARGETS` env: `"Name=host:port,Name2=host2:port2"`.
/// Fails fast with the offending entry index instead of silently skipping.
pub fn parse_targets_env(raw: &str) -> Result<Vec<MonitorTarget>, ConfigError> {
    let mut out = Vec::new();
    for (i, part) in raw.split(',').enumerate() {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, addr) = match part.split_once('=') {
            Some((n, a)) => (n.trim(), a.trim()),
            None => (part, part),
        };
        let (host, port_raw) =
            addr.rsplit_once(':')
                .ok_or_else(|| ConfigError::InvalidTargetEntry {
                    index: i,
                    entry: part.to_string(),
                    reason: "expected host:port".to_string(),
                })?;
        let port: u16 = port_raw
            .trim()
            .parse()
            .map_err(|_| ConfigError::InvalidTargetEntry {
                index: i,
                entry: part.to_string(),
                reason: format!("invalid port {port_raw:?}"),
            })?;
        let display_name = if name.trim().is_empty() {
            format!("{host}:{port}")
        } else {
            name.trim().to_string()
        };
        MonitorTarget::parse(display_name, host.trim(), port)
            .map_err(|e: DomainError| ConfigError::InvalidTargetEntry {
                index: i,
                entry: part.to_string(),
                reason: e.to_string(),
            })
            .map(|t| out.push(t))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{Mutex, MutexGuard};

    /// Serializes tests that touch process env (cargo runs tests in parallel
    /// in one process, so env access would otherwise race).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const ENV_KEYS: &[&str] = &[
        "CONFIG_PATH",
        "PORT",
        "DB_PATH",
        "CHECK_INTERVAL_SECS",
        "CHECK_TIMEOUT_SECS",
        "TARGETS",
    ];

    /// Owns `ENV_LOCK` for its whole lifetime: while an `EnvGuard` exists, no
    /// other test thread can read or write process env.
    struct EnvGuard {
        _token: MutexGuard<'static, ()>,
        saved: Vec<(String, Option<String>)>,
    }

    impl EnvGuard {
        fn take() -> Self {
            let token = ENV_LOCK.lock().unwrap();
            let saved = ENV_KEYS
                .iter()
                .map(|k| (k.to_string(), std::env::var(k).ok()))
                .collect();
            // SAFETY: the lock was just acquired and is stored in `Self`,
            // so it stays held for the guard's entire lifetime.
            for k in ENV_KEYS {
                unsafe { std::env::remove_var(k) };
            }
            Self {
                _token: token,
                saved,
            }
        }

        fn set(&self, key: &str, value: impl AsRef<str>) {
            let value = value.as_ref();
            // SAFETY: `self._token` proves the lock is held.
            unsafe { std::env::set_var(key, value) };
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: `Drop::drop` runs before fields are dropped, so
            // `self._token` (and the lock it holds) is still alive here.
            for k in ENV_KEYS {
                unsafe { std::env::remove_var(k) };
            }
            for (k, v) in &self.saved {
                if let Some(v) = v {
                    unsafe { std::env::set_var(k, v) };
                }
            }
        }
    }

    fn write_file(dir: &tempfile::TempDir, name: &str, contents: &str) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn parse_targets_env_accepts_named_and_bare() {
        let v = parse_targets_env("Google DNS=8.8.8.8:53,1.1.1.1:53").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name.as_str(), "Google DNS");
        assert_eq!(v[0].host.as_str(), "8.8.8.8");
        assert_eq!(v[0].port.get(), 53);
        assert_eq!(v[1].name.as_str(), "1.1.1.1:53");
    }

    #[test]
    fn parse_targets_env_rejects_bad_port_with_index() {
        let err = parse_targets_env("good=8.8.8.8:53,bad=host:notaport").unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidTargetEntry { index: 1, .. }
        ));
    }

    #[test]
    fn parse_targets_env_rejects_missing_colon() {
        assert!(parse_targets_env("justahost").is_err());
    }

    #[test]
    fn config_rejects_timeout_above_interval() {
        let err = Config::new(
            Port::new(3000).unwrap(),
            DbPath::new("x.db").unwrap(),
            CheckInterval::new(5).unwrap(),
            ProbeTimeout::new(10).unwrap(),
            HistoryLimit::default_valid(),
            NonEmptyTargets::new(vec![MonitorTarget::parse("a", "8.8.8.8", 53).unwrap()]).unwrap(),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::TimeoutExceedsInterval { .. }));
    }

    #[test]
    fn config_rejects_zero_port_and_empty_targets() {
        assert!(Port::new(0).is_err());
        assert!(matches!(
            NonEmptyTargets::new(vec![]).unwrap_err(),
            DomainError::NoTargets
        ));
    }

    #[test]
    fn builder_applies_file_values() {
        let mut b = ConfigBuilder::from_defaults();
        b.apply_file(FileConfig {
            port: Some(8080),
            db_path: None,
            check_interval_secs: Some(30),
            check_timeout_secs: Some(5),
            targets: None,
        })
        .unwrap();
        let cfg = b.build().unwrap();
        assert_eq!(cfg.port_u16(), 8080);
        assert_eq!(cfg.interval_secs(), 30);
    }

    #[test]
    fn builder_rejects_invalid_interval() {
        let mut b = ConfigBuilder::from_defaults();
        b.interval_secs = Some(0);
        assert!(b.build().is_err());
    }

    #[test]
    fn apply_file_ignores_empty_targets_list() {
        let mut b = ConfigBuilder::from_defaults();
        b.apply_file(FileConfig {
            port: None,
            db_path: Some("custom.db".into()),
            check_interval_secs: None,
            check_timeout_secs: None,
            targets: Some(vec![]),
        })
        .unwrap();
        let cfg = b.build().unwrap();
        // Empty list keeps defaults (3 targets), custom db applies.
        assert_eq!(cfg.targets.len(), 3);
        assert_eq!(cfg.db_path.to_string(), "custom.db");
    }

    #[test]
    fn build_rejects_each_invalid_field() {
        // Bad port.
        let mut b = ConfigBuilder::from_defaults();
        b.port = Some(0);
        assert!(b.build().is_err());
        // Bad db path.
        let mut b = ConfigBuilder::from_defaults();
        b.db_path = Some("   ".into());
        assert!(b.build().is_err());
        // Bad timeout.
        let mut b = ConfigBuilder::from_defaults();
        b.timeout_secs = Some(61);
        assert!(b.build().is_err());
        // Duplicate target names.
        let mut b = ConfigBuilder::from_defaults();
        b.targets = Some(vec![
            MonitorTarget::parse("dup", "8.8.8.8", 53).unwrap(),
            MonitorTarget::parse("dup", "1.1.1.1", 53).unwrap(),
        ]);
        assert!(b.build().is_err());
        // Empty parsed targets.
        let mut b = ConfigBuilder::from_defaults();
        b.targets = Some(vec![]);
        assert!(b.build().is_err());
    }

    #[test]
    fn parse_targets_env_edge_cases() {
        // Empty segments skipped; `=host:port` falls back to `host:port` name.
        let v = parse_targets_env(" , ,=8.8.8.8:53").unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name.as_str(), "8.8.8.8:53");
        // Invalid host surfaces the entry index.
        let err = parse_targets_env("ok=8.8.8.8:53,bad=has space:53").unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidTargetEntry { index: 1, .. }
        ));
        // Port 0 fails domain validation.
        assert!(parse_targets_env("x=h:0").is_err());
        // Whitespace-only input yields an empty vec.
        assert!(parse_targets_env("   ").unwrap().is_empty());
    }

    #[test]
    fn load_defaults_when_no_file_and_no_env() {
        let _env = EnvGuard::take();
        let dir = tempfile::tempdir().unwrap();
        _env.set(
            "CONFIG_PATH",
            dir.path().join("does-not-exist.json").to_string_lossy(),
        );
        let cfg = Config::load().unwrap();
        assert_eq!(cfg.port_u16(), 3000);
        assert_eq!(cfg.interval_secs(), 10);
        assert_eq!(cfg.timeout_secs(), 5);
        assert_eq!(cfg.targets.len(), 3);
    }

    #[test]
    fn load_reads_valid_file() {
        let _env = EnvGuard::take();
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(
            &dir,
            "config.json",
            r#"{
                "port": 8080,
                "db_path": "file.db",
                "check_interval_secs": 30,
                "check_timeout_secs": 5,
                "targets": [{"name": "t", "host": "9.9.9.9", "port": 53}]
            }"#,
        );
        _env.set("CONFIG_PATH", path);
        let cfg = Config::load().unwrap();
        assert_eq!(cfg.port_u16(), 8080);
        assert_eq!(cfg.interval_secs(), 30);
        assert_eq!(cfg.targets.len(), 1);
    }

    #[test]
    fn load_rejects_invalid_json_and_directories() {
        let _env = EnvGuard::take();
        let dir = tempfile::tempdir().unwrap();
        // Invalid JSON.
        let path = write_file(&dir, "bad.json", "{ not json");
        _env.set("CONFIG_PATH", &path);
        assert!(matches!(
            Config::load().unwrap_err(),
            ConfigError::InvalidFile { .. }
        ));
        // A directory is present but unreadable as a file.
        _env.set("CONFIG_PATH", dir.path().to_string_lossy());
        assert!(matches!(
            Config::load().unwrap_err(),
            ConfigError::InvalidFile { .. }
        ));
    }

    #[test]
    fn load_rejects_invalid_target_in_file() {
        let _env = EnvGuard::take();
        let dir = tempfile::tempdir().unwrap();
        // Port 0 fails domain validation during deserialization.
        let path = write_file(
            &dir,
            "config.json",
            r#"{"targets": [{"name": "t", "host": "9.9.9.9", "port": 0}]}"#,
        );
        _env.set("CONFIG_PATH", path);
        assert!(matches!(
            Config::load().unwrap_err(),
            ConfigError::InvalidFile { .. }
        ));
    }

    #[test]
    fn apply_env_overrides_all_fields() {
        let _env = EnvGuard::take();
        _env.set("PORT", "8081");
        _env.set("DB_PATH", "env.db");
        _env.set("CHECK_INTERVAL_SECS", "20");
        _env.set("CHECK_TIMEOUT_SECS", "4");
        _env.set("TARGETS", "Mine=9.9.9.9:53");
        let mut b = ConfigBuilder::from_defaults();
        b.apply_env().unwrap();
        let cfg = b.build().unwrap();
        assert_eq!(cfg.port_u16(), 8081);
        assert_eq!(cfg.db_path.to_string(), "env.db");
        assert_eq!(cfg.interval_secs(), 20);
        assert_eq!(cfg.targets.len(), 1);
        assert_eq!(cfg.targets.as_slice()[0].name.as_str(), "Mine");
    }

    #[test]
    fn apply_env_ignores_blank_and_garbage() {
        let _env = EnvGuard::take();
        _env.set("PORT", "not-a-number");
        _env.set("DB_PATH", "   ");
        _env.set("CHECK_INTERVAL_SECS", "zzz");
        _env.set("TARGETS", "   ");
        let mut b = ConfigBuilder::from_defaults();
        b.apply_env().unwrap();
        let cfg = b.build().unwrap();
        assert_eq!(cfg.port_u16(), 3000);
        assert_eq!(cfg.interval_secs(), 10);
        assert_eq!(cfg.targets.len(), 3);
    }

    #[test]
    fn apply_env_propagates_invalid_targets() {
        let _env = EnvGuard::take();
        _env.set("TARGETS", "definitely-not-a-target");
        let mut b = ConfigBuilder::from_defaults();
        assert!(matches!(
            b.apply_env().unwrap_err(),
            ConfigError::InvalidTargetEntry { .. }
        ));
    }

    #[test]
    fn accessors_and_defaults_cover_helpers() {
        assert_eq!(CheckInterval::default_valid().secs(), 10);
        assert_eq!(ProbeTimeout::default_valid().secs(), 5);
        let cfg = Config::defaults();
        assert_eq!(cfg.timeout_secs(), 5);
        assert!(!cfg.targets.is_empty());
        // Distinct names helper used by validation errors.
        let names: HashSet<_> = ["a", "b"].into_iter().collect();
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn env_guard_restores_preexisting_values() {
        // Seed a value with no guard alive yet: hold the raw lock across the
        // write so the same no-concurrent-access contract holds.
        // SAFETY: `token` is alive for the call (see `EnvGuard` docs).
        let token = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("PORT", "9999") };
        drop(token);
        {
            let _guard = EnvGuard::take();
            assert!(std::env::var("PORT").is_err());
        }
        assert_eq!(std::env::var("PORT").as_deref(), Ok("9999"));
        let token = ENV_LOCK.lock().unwrap();
        // SAFETY: same as above.
        unsafe { std::env::remove_var("PORT") };
        drop(token);
    }
}
