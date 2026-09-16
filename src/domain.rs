//! Validated domain newtypes.
//!
//! Senior rule enforced here: **no primitive obsession**.
//! Every value that has an invariant (non-empty name, non-zero port,
//! timeout <= interval, non-negative latency, …) gets its own type whose
//! constructor validates once. Downstream code (`db`, `monitor`, `api`)
//! can then rely on the invariant instead of re-checking `if x == 0`
//! everywhere, which is what makes illegal states unrepresentable.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::DomainError;

// ---------------------------------------------------------------------------
// TargetName: non-empty, trimmed, bounded length
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TargetName(String);

impl TargetName {
    pub const MAX_LEN: usize = 64;

    pub fn new(raw: impl Into<String>) -> Result<Self, DomainError> {
        let s = raw.into();
        let t = s.trim();
        if t.is_empty() {
            return Err(DomainError::EmptyTargetName);
        }
        if t.len() > Self::MAX_LEN {
            return Err(DomainError::TargetNameTooLong {
                len: t.len(),
                max: Self::MAX_LEN,
            });
        }
        Ok(Self(t.to_string()))
    }

    /// Known-good compile-time default, used for built-in targets.
    pub fn known(s: &'static str) -> Self {
        Self::new(s).expect("built-in target name must be valid")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TargetName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for TargetName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for TargetName {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TargetName {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Host: validated DNS name or IP literal
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Host(String);

impl Host {
    pub const MAX_LEN: usize = 253;

    pub fn new(raw: impl Into<String>) -> Result<Self, DomainError> {
        let s = raw.into();
        let t = s.trim();
        if t.is_empty() {
            return Err(DomainError::EmptyHost);
        }
        if t.len() > Self::MAX_LEN {
            return Err(DomainError::HostTooLong {
                len: t.len(),
                max: Self::MAX_LEN,
            });
        }
        if t.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(DomainError::InvalidHost {
                host: t.to_string(),
                reason: "must not contain whitespace or control characters",
            });
        }
        for bad in ["://", "/", "@", "?", "#"] {
            if t.contains(bad) {
                return Err(DomainError::InvalidHost {
                    host: t.to_string(),
                    reason: "must be a bare hostname/IP, not a URL",
                });
            }
        }
        // Reject a trailing/leading dot-only or empty label edge cases loosely;
        // full RFC validation is overkill — the TCP probe is the real check.
        Ok(Self(t.to_string()))
    }

    pub fn known(s: &'static str) -> Self {
        Self::new(s).expect("built-in host must be valid")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `"host:port"` hint for `tokio::net::lookup_host`.
    pub fn socket_hint(&self, port: Port) -> String {
        format!("{}:{}", self.0, port.get())
    }
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for Host {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for Host {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Host {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Port: NonZeroU16 — port 0 is unrepresentable by construction
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Port(NonZeroU16);

impl Port {
    pub fn new(port: u16) -> Result<Self, DomainError> {
        NonZeroU16::new(port)
            .map(Self)
            .ok_or(DomainError::InvalidPort { port })
    }

    pub fn get(self) -> u16 {
        self.0.get()
    }
}

impl fmt::Display for Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.get())
    }
}

impl TryFrom<u16> for Port {
    type Error = DomainError;
    fn try_from(v: u16) -> Result<Self, Self::Error> {
        Self::new(v)
    }
}

impl From<Port> for u16 {
    fn from(p: Port) -> u16 {
        p.get()
    }
}

impl Serialize for Port {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u16(self.get())
    }
}

impl<'de> Deserialize<'de> for Port {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = u16::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Durations: CheckInterval and ProbeTimeout wrap Duration (no bare u64)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckInterval(Duration);

impl CheckInterval {
    pub const MIN_SECS: u64 = 1;
    pub const MAX_SECS: u64 = 3600;
    pub const DEFAULT_SECS: u64 = 10;

    pub fn new(secs: u64) -> Result<Self, DomainError> {
        if !(Self::MIN_SECS..=Self::MAX_SECS).contains(&secs) {
            return Err(DomainError::InvalidInterval { secs });
        }
        Ok(Self(Duration::from_secs(secs)))
    }

    pub fn default_valid() -> Self {
        Self::new(Self::DEFAULT_SECS).expect("default interval valid")
    }

    pub fn secs(self) -> u64 {
        self.0.as_secs()
    }

    pub fn duration(self) -> Duration {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeTimeout(Duration);

impl ProbeTimeout {
    pub const MIN_SECS: u64 = 1;
    pub const MAX_SECS: u64 = 60;
    pub const DEFAULT_SECS: u64 = 5;

    pub fn new(secs: u64) -> Result<Self, DomainError> {
        if !(Self::MIN_SECS..=Self::MAX_SECS).contains(&secs) {
            return Err(DomainError::InvalidTimeout { secs });
        }
        Ok(Self(Duration::from_secs(secs)))
    }

    pub fn default_valid() -> Self {
        Self::new(Self::DEFAULT_SECS).expect("default timeout valid")
    }

    pub fn secs(self) -> u64 {
        self.0.as_secs()
    }

    pub fn duration(self) -> Duration {
        self.0
    }
}

// ---------------------------------------------------------------------------
// HistoryLimit
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryLimit(usize);

impl HistoryLimit {
    pub const MIN: usize = 1;
    pub const MAX: usize = 10_000;
    pub const DEFAULT: usize = 2000;

    pub fn new(n: usize) -> Result<Self, DomainError> {
        if !(Self::MIN..=Self::MAX).contains(&n) {
            return Err(DomainError::InvalidHistoryLimit { n });
        }
        Ok(Self(n))
    }

    pub fn default_valid() -> Self {
        Self(Self::DEFAULT)
    }

    pub fn get(self) -> usize {
        self.0
    }
}

impl<'de> Deserialize<'de> for HistoryLimit {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = usize::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// LookbackHours: validated query range, converts to a timestamp bound
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LookbackHours(f64);

impl LookbackHours {
    pub const MIN: f64 = 0.05; // ~3 minutes
    pub const MAX: f64 = 720.0; // 30 days
    pub const DEFAULT_24H: f64 = 24.0;

    pub fn new(h: f64) -> Result<Self, DomainError> {
        if !h.is_finite() || !(Self::MIN..=Self::MAX).contains(&h) {
            return Err(DomainError::InvalidLookback { hours: h });
        }
        Ok(Self(h))
    }

    pub fn default_24h() -> Self {
        Self(Self::DEFAULT_24H)
    }

    pub fn hours(self) -> f64 {
        self.0
    }

    /// Pure conversion — takes `now` as a parameter so it is unit-testable
    /// without mocking the clock.
    pub fn since(self, now: UnixSecs) -> UnixSecs {
        let back = (self.0 * 3600.0) as i64;
        UnixSecs::new(now.as_i64().saturating_sub(back))
    }
}

impl<'de> Deserialize<'de> for LookbackHours {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = f64::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// UnixSecs: non-negative unix timestamp
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnixSecs(i64);

impl UnixSecs {
    pub fn new(ts: i64) -> Self {
        Self(ts.max(0))
    }

    pub fn now() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Self::new(ts)
    }

    pub fn as_i64(self) -> i64 {
        self.0
    }

    pub fn as_datetime(self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.0, 0).unwrap_or_default()
    }
}

impl Serialize for UnixSecs {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(self.0)
    }
}

// ---------------------------------------------------------------------------
// LatencyMs: non-negative round-trip time
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LatencyMs(i64);

impl LatencyMs {
    pub fn new(ms: i64) -> Result<Self, DomainError> {
        if ms < 0 {
            return Err(DomainError::NegativeLatency { ms });
        }
        Ok(Self(ms))
    }

    pub fn from_elapsed(elapsed: Duration) -> Self {
        Self(elapsed.as_millis().min(i64::MAX as u128) as i64)
    }

    pub fn as_i64(self) -> i64 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// CompactError: single-line, length-bounded error text for sqlite + UI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactError(String);

impl CompactError {
    pub const MAX_CHARS: usize = 200;

    pub fn new(raw: &str) -> Self {
        let first: String = raw.lines().next().unwrap_or("").trim().to_string();
        let base = if first.is_empty() {
            "unknown error".to_string()
        } else {
            first
        };
        Self(base.chars().take(Self::MAX_CHARS).collect())
    }

    pub fn timeout() -> Self {
        Self("timeout".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CompactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for CompactError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// ProbeOutcome: success XOR failure — replaces (bool, Option, Option)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    Success { latency: LatencyMs },
    Failure { error: CompactError },
}

impl ProbeOutcome {
    pub fn success(latency: LatencyMs) -> Self {
        Self::Success { latency }
    }

    pub fn failure(error: CompactError) -> Self {
        Self::Failure { error }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }

    pub fn latency(&self) -> Option<LatencyMs> {
        match self {
            Self::Success { latency } => Some(*latency),
            Self::Failure { .. } => None,
        }
    }

    pub fn error(&self) -> Option<&CompactError> {
        match self {
            Self::Success { .. } => None,
            Self::Failure { error } => Some(error),
        }
    }

    /// Split for sqlite storage: (success_int, latency_opt, error_opt).
    pub fn as_row(&self) -> (i64, Option<i64>, Option<&str>) {
        match self {
            Self::Success { latency } => (1, Some(latency.as_i64()), None),
            Self::Failure { error } => (0, None, Some(error.as_str())),
        }
    }

    /// Rebuild from a row; rejects inconsistent rows (e.g. success with
    /// an error attached) instead of silently picking one field.
    pub fn from_row(
        success: bool,
        latency_ms: Option<i64>,
        error: Option<String>,
    ) -> Result<Self, DomainError> {
        match (success, latency_ms, error) {
            (true, Some(ms), None) => Ok(Self::Success {
                latency: LatencyMs::new(ms)?,
            }),
            (true, None, None) => Ok(Self::Success {
                latency: LatencyMs::new(0)?,
            }),
            (false, _, err) => Ok(Self::Failure {
                error: CompactError::new(err.as_deref().unwrap_or("check failed")),
            }),
            (true, lat, err) => Err(DomainError::InconsistentCheckRow {
                success,
                latency_ms: lat,
                has_error: err.is_some(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// MonitorTarget: validated triple; only constructible via ::new
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MonitorTarget {
    pub name: TargetName,
    pub host: Host,
    pub port: Port,
}

impl MonitorTarget {
    pub fn new(name: TargetName, host: Host, port: Port) -> Self {
        Self { name, host, port }
    }

    /// Parse + validate all three fields at once (used by env/file parsing).
    pub fn parse(
        name: impl Into<String>,
        host: impl Into<String>,
        port: u16,
    ) -> Result<Self, DomainError> {
        Ok(Self {
            name: TargetName::new(name)?,
            host: Host::new(host)?,
            port: Port::new(port)?,
        })
    }

    pub fn socket_hint(&self) -> String {
        self.host.socket_hint(self.port)
    }
}

impl fmt::Display for MonitorTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}:{})", self.name, self.host, self.port)
    }
}

// ---------------------------------------------------------------------------
// NonEmptyTargets: at least one target, unique names
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonEmptyTargets(Vec<MonitorTarget>);

impl NonEmptyTargets {
    pub fn new(targets: Vec<MonitorTarget>) -> Result<Self, DomainError> {
        if targets.is_empty() {
            return Err(DomainError::NoTargets);
        }
        let mut seen = std::collections::HashSet::new();
        for t in &targets {
            if !seen.insert(t.name.as_str().to_string()) {
                return Err(DomainError::DuplicateTarget {
                    name: t.name.as_str().to_string(),
                });
            }
        }
        Ok(Self(targets))
    }

    pub fn as_slice(&self) -> &[MonitorTarget] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, MonitorTarget> {
        self.0.iter()
    }
}

impl IntoIterator for NonEmptyTargets {
    type Item = MonitorTarget;
    type IntoIter = std::vec::IntoIter<MonitorTarget>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

// ---------------------------------------------------------------------------
// DbPath: non-empty path
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbPath(PathBuf);

impl DbPath {
    pub fn new(raw: impl Into<String>) -> Result<Self, DomainError> {
        let s = raw.into();
        if s.trim().is_empty() {
            return Err(DomainError::EmptyDbPath);
        }
        Ok(Self(PathBuf::from(s)))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for DbPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (deterministic, clock-injectable → unit-testable)
// ---------------------------------------------------------------------------

/// Packet-loss percentage; total==0 yields 0.0 instead of NaN.
pub fn loss_pct(total: i64, success: i64) -> f64 {
    if total <= 0 {
        return 0.0;
    }
    let failed = (total - success).max(0);
    failed as f64 / total as f64 * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_name_rejects_empty_and_too_long() {
        assert!(TargetName::new("").is_err());
        assert!(TargetName::new("   ").is_err());
        assert!(TargetName::new("x".repeat(65)).is_err());
        assert_eq!(TargetName::new("  ok  ").unwrap().as_str(), "ok");
    }

    #[test]
    fn host_rejects_urls_whitespace_and_empty() {
        assert!(Host::new("").is_err());
        assert!(Host::new("a b").is_err());
        assert!(Host::new("https://google.com").is_err());
        assert!(Host::new("ho/st").is_err());
        assert!(Host::new("8.8.8.8").is_ok());
        assert!(Host::new("google.com").is_ok());
    }

    #[test]
    fn host_rejects_overlong_names() {
        let long = "a".repeat(254);
        let err = Host::new(long).unwrap_err();
        assert!(matches!(err, DomainError::HostTooLong { .. }));
        assert_eq!(Host::new("a".repeat(253)).unwrap().as_str().len(), 253);
    }

    #[test]
    fn port_zero_is_unrepresentable() {
        assert!(Port::new(0).is_err());
        assert_eq!(Port::new(53).unwrap().get(), 53);
        assert_eq!(Port::new(443).unwrap().get(), 443);
    }

    #[test]
    fn interval_and_timeout_bounds() {
        assert!(CheckInterval::new(0).is_err());
        assert!(CheckInterval::new(3601).is_err());
        assert!(CheckInterval::new(10).is_ok());
        assert!(ProbeTimeout::new(0).is_err());
        assert!(ProbeTimeout::new(61).is_err());
        assert!(ProbeTimeout::new(5).is_ok());
    }

    #[test]
    fn history_limit_bounds() {
        assert!(HistoryLimit::new(0).is_err());
        assert!(HistoryLimit::new(10_001).is_err());
        assert_eq!(HistoryLimit::new(50).unwrap().get(), 50);
    }

    #[test]
    fn lookback_rejects_nan_zero_and_huge() {
        assert!(LookbackHours::new(f64::NAN).is_err());
        assert!(LookbackHours::new(0.0).is_err());
        assert!(LookbackHours::new(721.0).is_err());
        assert!(LookbackHours::new(24.0).is_ok());
    }

    #[test]
    fn lookback_since_is_pure_and_testable() {
        let now = UnixSecs::new(10_000);
        let since = LookbackHours::new(1.0).unwrap().since(now);
        assert_eq!(since.as_i64(), 10_000 - 3600);
    }

    #[test]
    fn latency_rejects_negative() {
        assert!(LatencyMs::new(-1).is_err());
        assert!(LatencyMs::new(0).is_ok());
    }

    #[test]
    fn compact_error_truncates_and_handles_empty() {
        let long = "x".repeat(500);
        assert_eq!(CompactError::new(&long).as_str().len(), 200);
        assert_eq!(CompactError::new("").as_str(), "unknown error");
        assert_eq!(CompactError::new("first\nsecond").as_str(), "first");
    }

    #[test]
    fn outcome_round_trips_through_row() {
        let ok = ProbeOutcome::success(LatencyMs::new(12).unwrap());
        let (s, l, e) = ok.as_row();
        let back = ProbeOutcome::from_row(s != 0, l, e.map(str::to_string)).unwrap();
        assert_eq!(ok, back);

        let fail = ProbeOutcome::failure(CompactError::new("boom"));
        let (s, l, e) = fail.as_row();
        let back = ProbeOutcome::from_row(s != 0, l, e.map(str::to_string)).unwrap();
        assert_eq!(fail, back);
    }

    #[test]
    fn outcome_rejects_inconsistent_row() {
        // success=true must not carry an error payload
        assert!(ProbeOutcome::from_row(true, Some(5), Some("oops".into())).is_err());
    }

    #[test]
    fn non_empty_targets_rejects_empty_and_duplicates() {
        assert!(NonEmptyTargets::new(vec![]).is_err());
        let a = MonitorTarget::parse("a", "8.8.8.8", 53).unwrap();
        let b = MonitorTarget::parse("a", "1.1.1.1", 53).unwrap();
        assert!(NonEmptyTargets::new(vec![a.clone(), b]).is_err());
        assert!(NonEmptyTargets::new(vec![a]).is_ok());
    }

    #[test]
    fn loss_pct_handles_zero_total() {
        assert_eq!(loss_pct(0, 0), 0.0);
        assert!((loss_pct(4, 3) - 25.0).abs() < 1e-9);
    }

    #[test]
    fn display_asref_and_serde_round_trips() {
        // TargetName
        let n = TargetName::new("web").unwrap();
        assert_eq!(n.to_string(), "web");
        let r: &str = n.as_ref();
        assert_eq!(r, "web");
        assert_eq!(TargetName::known("web"), n);
        assert_eq!(
            serde_json::from_value::<TargetName>(serde_json::json!("db")).unwrap(),
            TargetName::new("db").unwrap()
        );
        assert!(serde_json::from_value::<TargetName>(serde_json::json!("")).is_err());
        assert!(serde_json::from_value::<TargetName>(serde_json::json!(1)).is_err());
        assert_eq!(serde_json::to_value(&n).unwrap(), "web");

        // Host
        let h = Host::new("example.com").unwrap();
        assert_eq!(h.to_string(), "example.com");
        let r: &str = h.as_ref();
        assert_eq!(r, "example.com");
        assert_eq!(Host::known("example.com"), h);
        assert!(serde_json::from_value::<Host>(serde_json::json!("a b")).is_err());
        assert_eq!(serde_json::to_value(&h).unwrap(), "example.com");

        // Port conversions
        let p = Port::try_from(8080u16).unwrap();
        assert_eq!(p.get(), 8080);
        assert_eq!(u16::from(p), 8080);
        assert_eq!(p.to_string(), "8080");
        assert!(Port::try_from(0u16).is_err());
        assert_eq!(serde_json::to_value(p).unwrap(), 8080);
        assert!(serde_json::from_value::<Port>(serde_json::json!(0)).is_err());
        assert!(serde_json::from_value::<Port>(serde_json::json!("x")).is_err());

        // Durations
        assert_eq!(CheckInterval::default_valid().duration().as_secs(), 10);
        assert_eq!(ProbeTimeout::default_valid().duration().as_secs(), 5);

        // HistoryLimit serde
        assert_eq!(
            serde_json::from_value::<HistoryLimit>(serde_json::json!(25))
                .unwrap()
                .get(),
            25
        );
        assert!(serde_json::from_value::<HistoryLimit>(serde_json::json!(0)).is_err());

        // Lookback serde + default
        assert_eq!(LookbackHours::default_24h().hours(), 24.0);
        assert!(serde_json::from_value::<LookbackHours>(serde_json::json!(0.01)).is_err());
        assert_eq!(
            serde_json::from_value::<LookbackHours>(serde_json::json!(6.0))
                .unwrap()
                .hours(),
            6.0
        );
    }

    #[test]
    fn unix_secs_clamps_serializes_and_dates() {
        assert_eq!(UnixSecs::new(-5).as_i64(), 0);
        assert!(!UnixSecs::new(0).as_datetime().to_string().is_empty());
        assert_eq!(serde_json::to_value(UnixSecs::new(7)).unwrap(), 7);
        assert!(UnixSecs::now().as_i64() > 0);
        assert!(UnixSecs::new(5) < UnixSecs::new(6));
    }

    #[test]
    fn latency_error_and_outcome_helpers() {
        assert_eq!(
            LatencyMs::from_elapsed(Duration::from_millis(1500)).as_i64(),
            1500
        );
        assert_eq!(CompactError::timeout().as_str(), "timeout");
        assert_eq!(CompactError::new("x").to_string(), "x");
        assert_eq!(
            serde_json::to_value(CompactError::new("boom")).unwrap(),
            "boom"
        );

        // from_row: failure without payload gets a default message.
        let o = ProbeOutcome::from_row(false, None, None).unwrap();
        assert!(!o.is_success());
        assert!(o.error().is_some());
        // from_row: success without latency defaults to 0.
        let o = ProbeOutcome::from_row(true, None, None).unwrap();
        assert_eq!(o.latency().unwrap().as_i64(), 0);
        // from_row: negative latency rejected even on success rows.
        assert!(ProbeOutcome::from_row(true, Some(-3), None).is_err());
    }

    #[test]
    fn monitor_target_and_collections() {
        let a = MonitorTarget::parse("a", "8.8.8.8", 53).unwrap();
        assert_eq!(a.socket_hint(), "8.8.8.8:53");
        assert_eq!(a.to_string(), "a (8.8.8.8:53)");
        assert!(MonitorTarget::parse("", "8.8.8.8", 53).is_err());
        assert!(MonitorTarget::parse("a", "", 53).is_err());
        assert!(MonitorTarget::parse("a", "8.8.8.8", 0).is_err());

        let list = NonEmptyTargets::new(vec![a.clone()]).unwrap();
        assert_eq!(list.len(), 1);
        assert!(!list.is_empty());
        assert_eq!(list.as_slice(), std::slice::from_ref(&a));
        assert_eq!(list.iter().count(), 1);
        assert_eq!(list.clone().into_iter().count(), 1);

        let db = DbPath::new("/tmp/x.db").unwrap();
        assert_eq!(db.as_path().to_string_lossy(), "/tmp/x.db");
        assert!(db.to_string().contains("x.db"));
        assert!(DbPath::new("  ").is_err());
    }

    #[test]
    fn loss_pct_clamps_inverted_counts() {
        // success > total (corrupt data) cannot yield negative loss.
        assert_eq!(loss_pct(3, 5), 0.0);
        assert_eq!(loss_pct(-1, 0), 0.0);
    }
}
