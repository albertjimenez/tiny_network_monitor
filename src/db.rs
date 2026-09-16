//! SQLite persistence behind the `Store` repository.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::domain::{loss_pct, HistoryLimit, ProbeOutcome, TargetName, UnixSecs};
use crate::domain::{CompactError, DbPath};
use crate::error::StoreError;

// ---------------------------------------------------------------------------
// SQL sources. Every statement lives in `db/` (schema + one file per query)
// and is embedded at compile time, so the scratch image needs no extra
// files. `build.rs` executes the schema and EXPLAINs every query against
// `:memory:` — invalid SQL fails the build, not first boot.
// ---------------------------------------------------------------------------

const SCHEMA_SQL: &str = include_str!("../db/schema.sql");
const INSERT_CHECK_SQL: &str = include_str!("../db/queries/insert_check.sql");
const LATEST_PER_TARGET_SQL: &str = include_str!("../db/queries/latest_per_target.sql");
const HISTORY_SINCE_TARGET_SQL: &str = include_str!("../db/queries/history_since_target.sql");
const HISTORY_SINCE_SQL: &str = include_str!("../db/queries/history_since.sql");
const HISTORY_TARGET_SQL: &str = include_str!("../db/queries/history_target.sql");
const HISTORY_ALL_SQL: &str = include_str!("../db/queries/history_all.sql");
const STATS_SQL: &str = include_str!("../db/queries/stats.sql");
const STATS_LATEST_SUCCESS_SQL: &str = include_str!("../db/queries/stats_latest_success.sql");
const OUTAGES_SQL: &str = include_str!("../db/queries/outages.sql");
const COUNT_SQL: &str = include_str!("../db/queries/count.sql");

#[derive(Debug, Clone)]
pub struct Check {
    pub id: i64,
    pub ts: UnixSecs,
    pub timestamp: DateTime<Utc>,
    pub target: TargetName,
    pub outcome: ProbeOutcome,
}

impl Check {
    /// Enforced by compile time rules: every field is already a validated domain type
    pub fn new(id: i64, ts: UnixSecs, target: TargetName, outcome: ProbeOutcome) -> Self {
        Self {
            id,
            ts,
            timestamp: ts.as_datetime(),
            target,
            outcome,
        }
    }

    pub fn success(&self) -> bool {
        self.outcome.is_success()
    }

    pub fn latency_ms(&self) -> Option<i64> {
        self.outcome.latency().map(|l| l.as_i64())
    }

    pub fn error(&self) -> Option<String> {
        self.outcome.error().map(|e| e.as_str().to_string())
    }
}

impl Serialize for Check {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;
        let mut st = s.serialize_struct("Check", 7)?;
        st.serialize_field("id", &self.id)?;
        st.serialize_field("ts", &self.ts.as_i64())?;
        st.serialize_field("timestamp", &self.timestamp)?;
        st.serialize_field("target", self.target.as_str())?;
        st.serialize_field("success", &self.success())?;
        st.serialize_field("latency_ms", &self.latency_ms())?;
        st.serialize_field("error", &self.error())?;
        st.end()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetStats {
    pub target: TargetName,
    pub total: i64,
    pub success: i64,
    pub failed: i64,
    pub loss_pct: f64,
    pub uptime_pct: f64,
    pub avg_latency_ms: Option<f64>,
    pub last_success: Option<DateTime<Utc>>,
    pub last_failure: Option<DateTime<Utc>>,
    pub currently_up: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Outage {
    pub target: TargetName,
    pub start_ts: UnixSecs,
    pub start: DateTime<Utc>,
    pub end_ts: Option<UnixSecs>,
    pub end: Option<DateTime<Utc>>,
    pub failed_checks: i64,
    pub duration_secs: Option<i64>,
    pub ongoing: bool,
}

impl Outage {
    fn closed(target: TargetName, start: UnixSecs, end: UnixSecs, failed: i64) -> Self {
        Self {
            target,
            start_ts: start,
            start: start.as_datetime(),
            end_ts: Some(end),
            end: Some(end.as_datetime()),
            failed_checks: failed,
            duration_secs: Some((end.as_i64() - start.as_i64()).max(0)),
            ongoing: false,
        }
    }

    fn ongoing(target: TargetName, start: UnixSecs, failed: i64) -> Self {
        Self {
            target,
            start_ts: start,
            start: start.as_datetime(),
            end_ts: None,
            end: None,
            failed_checks: failed,
            duration_secs: None,
            ongoing: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Store repository
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Store {
    db_path: DbPath,
}

impl Store {
    pub fn new(db_path: DbPath) -> Self {
        Self { db_path }
    }

    pub fn db_path(&self) -> &DbPath {
        &self.db_path
    }

    fn connect(&self) -> Result<Connection, StoreError> {
        Ok(Connection::open(self.db_path.as_path())?)
    }

    pub fn init(&self) -> Result<(), StoreError> {
        let conn = self.connect()?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(())
    }

    pub fn insert(
        &self,
        ts: UnixSecs,
        target: &TargetName,
        outcome: &ProbeOutcome,
    ) -> Result<(), StoreError> {
        let conn = self.connect()?;
        let (success, latency_ms, error) = outcome.as_row();
        conn.execute(
            INSERT_CHECK_SQL,
            params![ts.as_i64(), target.as_str(), success, latency_ms, error],
        )?;
        Ok(())
    }

    pub fn latest_per_target(&self) -> Result<Vec<Check>, StoreError> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(LATEST_PER_TARGET_SQL)?;
        let mapped = stmt.query_map([], row_to_check)?;
        let mut out = Vec::new();
        for r in mapped {
            out.push(r.map_err(StoreError::Sqlite)?);
        }
        Ok(out)
    }

    pub fn history(
        &self,
        since: Option<UnixSecs>,
        target: Option<&TargetName>,
        limit: HistoryLimit,
    ) -> Result<Vec<Check>, StoreError> {
        let conn = self.connect()?;
        let lim = limit.get().min(10_000) as i64;
        if let (Some(since), Some(t)) = (since, target) {
            let mut stmt = conn.prepare(HISTORY_SINCE_TARGET_SQL)?;
            let mapped = stmt.query_map(params![since.as_i64(), t.as_str(), lim], row_to_check)?;
            let mut out = Vec::new();
            for r in mapped {
                out.push(r.map_err(StoreError::Sqlite)?);
            }
            return Ok(out);
        }
        if let Some(since) = since {
            let mut stmt = conn.prepare(HISTORY_SINCE_SQL)?;
            let mapped = stmt.query_map(params![since.as_i64(), lim], row_to_check)?;
            let mut out = Vec::new();
            for r in mapped {
                out.push(r.map_err(StoreError::Sqlite)?);
            }
            return Ok(out);
        }
        if let Some(t) = target {
            let mut stmt = conn.prepare(HISTORY_TARGET_SQL)?;
            let mapped = stmt.query_map(params![t.as_str(), lim], row_to_check)?;
            let mut out = Vec::new();
            for r in mapped {
                out.push(r.map_err(StoreError::Sqlite)?);
            }
            return Ok(out);
        }
        let mut stmt = conn.prepare(HISTORY_ALL_SQL)?;
        let mapped = stmt.query_map(params![lim], row_to_check)?;
        let mut out = Vec::new();
        for r in mapped {
            out.push(r.map_err(StoreError::Sqlite)?);
        }
        Ok(out)
    }

    pub fn stats(&self, since: UnixSecs) -> Result<Vec<TargetStats>, StoreError> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(STATS_SQL)?;
        let mapped = stmt.query_map(params![since.as_i64()], |row| {
            let target: String = row.get(0)?;
            let total: i64 = row.get(1)?;
            let success: i64 = row.get::<_, Option<i64>>(2)?.unwrap_or(0);
            let avg_lat: Option<f64> = row.get(3)?;
            let last_ok: Option<i64> = row.get(4)?;
            let last_fail: Option<i64> = row.get(5)?;
            Ok((target, total, success, avg_lat, last_ok, last_fail))
        })?;
        let mut out = Vec::new();
        for r in mapped {
            let (target_raw, total, success, avg_lat, last_ok, last_fail) =
                r.map_err(StoreError::Sqlite)?;
            // Rows already in the DB predate strict validation; fall back to a
            // lossy-but-explicit placeholder rather than failing the whole query.
            let target = TargetName::new(target_raw.clone())
                .unwrap_or_else(|_| TargetName::known("unknown"));
            let failed = (total - success).max(0);
            let loss = loss_pct(total, success);
            let currently_up: Option<bool> = conn
                .query_row(
                    STATS_LATEST_SUCCESS_SQL,
                    params![target_raw, since.as_i64()],
                    |row| {
                        let s: i64 = row.get(0)?;
                        Ok(s != 0)
                    },
                )
                .ok();
            out.push(TargetStats {
                target,
                total,
                success,
                failed,
                loss_pct: loss,
                uptime_pct: 100.0 - loss,
                avg_latency_ms: avg_lat,
                last_success: last_ok.map(UnixSecs::new).map(|t| t.as_datetime()),
                last_failure: last_fail.map(UnixSecs::new).map(|t| t.as_datetime()),
                currently_up,
            });
        }
        Ok(out)
    }

    pub fn outages(&self, since: UnixSecs, limit: HistoryLimit) -> Result<Vec<Outage>, StoreError> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(OUTAGES_SQL)?;
        let mapped = stmt.query_map(
            params![since.as_i64(), limit.get().min(20_000) as i64],
            |row| {
                let t: String = row.get(0)?;
                let ts: i64 = row.get(1)?;
                let s: i64 = row.get(2)?;
                Ok((t, ts, s != 0))
            },
        )?;
        let mut rows: Vec<(String, i64, bool)> = Vec::new();
        for r in mapped {
            rows.push(r.map_err(StoreError::Sqlite)?);
        }
        Ok(group_outages(&rows))
    }

    pub fn count(&self) -> Result<i64, StoreError> {
        let conn = self.connect()?;
        Ok(conn.query_row(COUNT_SQL, [], |r| r.get(0))?)
    }
}

fn row_to_check(row: &rusqlite::Row) -> rusqlite::Result<Check> {
    let id: i64 = row.get(0)?;
    let ts: i64 = row.get(1)?;
    let target_raw: String = row.get(2)?;
    let success_i: i64 = row.get(3)?;
    let latency_ms: Option<i64> = row.get(4)?;
    let error: Option<String> = row.get(5)?;
    // DB rows predate strict types; coerce defensively. `from_row` still
    // rejects truly inconsistent combinations.
    let target = TargetName::new(target_raw).unwrap_or_else(|_| TargetName::known("unknown"));
    let outcome = ProbeOutcome::from_row(success_i != 0, latency_ms, error).unwrap_or_else(|_| {
        ProbeOutcome::Failure {
            error: CompactError::new("inconsistent row"),
        }
    });
    Ok(Check::new(id, UnixSecs::new(ts), target, outcome))
}

// ---------------------------------------------------------------------------
// Pure outage grouping — no DB, fully unit-testable
// ---------------------------------------------------------------------------

/// Group consecutive failures per target. Input must be sorted by
/// `(target, ts, id)` ascending (the SQL query guarantees this).
pub fn group_outages(rows: &[(String, i64, bool)]) -> Vec<Outage> {
    let mut out: Vec<Outage> = Vec::new();
    let mut cur_target: Option<TargetName> = None;
    let mut cur_start: Option<UnixSecs> = None;
    let mut cur_count: i64 = 0;

    let target_of = |raw: &str| {
        TargetName::new(raw.to_string()).unwrap_or_else(|_| TargetName::known("unknown"))
    };

    for (raw_target, ts_raw, success) in rows {
        let ts = UnixSecs::new(*ts_raw);
        let changed = cur_target.as_ref().map(|t| t.as_str()) != Some(raw_target.as_str());
        if changed {
            if let (Some(t), Some(s)) = (cur_target.take(), cur_start.take()) {
                out.push(Outage::ongoing(t, s, cur_count));
            }
            cur_target = Some(target_of(raw_target));
            cur_start = None;
            cur_count = 0;
        }
        if !success {
            if cur_start.is_none() {
                cur_start = Some(ts);
                cur_count = 0;
            }
            cur_count += 1;
        } else if let (Some(t), Some(s)) = (cur_target.clone(), cur_start.take()) {
            out.push(Outage::closed(t, s, ts, cur_count));
            cur_count = 0;
        }
    }
    if let (Some(t), Some(s)) = (cur_target, cur_start) {
        out.push(Outage::ongoing(t, s, cur_count));
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.start_ts));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DbPath, HistoryLimit, Host, LatencyMs, Port};

    fn test_store(name: &str) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        let store = Store::new(DbPath::new(path.to_string_lossy().to_string()).unwrap());
        store.init().unwrap();
        (dir, store)
    }

    fn target(name: &str) -> TargetName {
        TargetName::new(name).unwrap()
    }

    fn ok(ms: i64) -> ProbeOutcome {
        ProbeOutcome::success(LatencyMs::new(ms).unwrap())
    }

    fn fail(msg: &str) -> ProbeOutcome {
        ProbeOutcome::failure(CompactError::new(msg))
    }

    #[test]
    fn insert_and_latest_round_trip() {
        let (_dir, s) = test_store("basic.db");
        let ts = UnixSecs::new(1_000);
        s.insert(ts, &target("a"), &ok(10)).unwrap();
        s.insert(ts, &target("b"), &fail("boom")).unwrap();
        let latest = s.latest_per_target().unwrap();
        assert_eq!(latest.len(), 2);
        let b = latest.iter().find(|c| c.target.as_str() == "b").unwrap();
        assert!(!b.success());
        assert_eq!(b.error().as_deref(), Some("boom"));
        assert_eq!(s.count().unwrap(), 2);
    }

    #[test]
    fn history_filters_by_target_and_since() {
        let (_dir, s) = test_store("hist.db");
        s.insert(UnixSecs::new(100), &target("a"), &ok(5)).unwrap();
        s.insert(UnixSecs::new(200), &target("b"), &ok(6)).unwrap();
        s.insert(UnixSecs::new(300), &target("a"), &fail("x"))
            .unwrap();

        let all = s
            .history(None, None, HistoryLimit::new(10).unwrap())
            .unwrap();
        assert_eq!(all.len(), 3);
        // DESC order
        assert_eq!(all[0].ts.as_i64(), 300);

        let a_only = s
            .history(None, Some(&target("a")), HistoryLimit::new(10).unwrap())
            .unwrap();
        assert_eq!(a_only.len(), 2);

        let recent = s
            .history(
                Some(UnixSecs::new(250)),
                None,
                HistoryLimit::new(10).unwrap(),
            )
            .unwrap();
        assert_eq!(recent.len(), 1);
    }

    #[test]
    fn stats_compute_loss_and_avg() {
        let (_dir, s) = test_store("stats.db");
        let t = target("a");
        s.insert(UnixSecs::new(100), &t, &ok(10)).unwrap();
        s.insert(UnixSecs::new(110), &t, &ok(30)).unwrap();
        s.insert(UnixSecs::new(120), &t, &fail("down")).unwrap();
        let stats = s.stats(UnixSecs::new(0)).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].total, 3);
        assert!((stats[0].loss_pct - 100.0 / 3.0).abs() < 1e-9);
        assert!((stats[0].avg_latency_ms.unwrap() - 20.0).abs() < 1e-9);
        assert_eq!(stats[0].currently_up, Some(false));
    }

    #[test]
    fn outages_group_closed_and_ongoing() {
        // closed: F F S
        let rows = vec![
            ("a".to_string(), 100, false),
            ("a".to_string(), 110, false),
            ("a".to_string(), 120, true),
        ];
        let out = group_outages(&rows);
        assert_eq!(out.len(), 1);
        assert!(!out[0].ongoing);
        assert_eq!(out[0].failed_checks, 2);
        assert_eq!(out[0].duration_secs, Some(20));

        // ongoing: F F (no recovery)
        let rows = vec![("a".to_string(), 100, false), ("a".to_string(), 110, false)];
        let out = group_outages(&rows);
        assert_eq!(out.len(), 1);
        assert!(out[0].ongoing);
    }

    #[test]
    fn outages_separate_targets_and_sort_newest_first() {
        let rows = vec![
            ("a".to_string(), 100, false),
            ("a".to_string(), 110, true),
            ("b".to_string(), 500, false),
            ("b".to_string(), 510, true),
        ];
        let out = group_outages(&rows);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].target.as_str(), "b");
    }

    #[test]
    fn outages_empty_input() {
        assert!(group_outages(&[]).is_empty());
    }

    #[test]
    fn store_outages_end_to_end() {
        let (_dir, s) = test_store("out.db");
        let t = target("web");
        s.insert(UnixSecs::new(100), &t, &fail("d1")).unwrap();
        s.insert(UnixSecs::new(110), &t, &fail("d2")).unwrap();
        s.insert(UnixSecs::new(120), &t, &ok(5)).unwrap();
        let out = s
            .outages(UnixSecs::new(0), HistoryLimit::new(100).unwrap())
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].failed_checks, 2);
    }

    #[test]
    fn check_serializes_to_legacy_json_shape() {
        let c = Check::new(7, UnixSecs::new(123), target("a"), ok(9));
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["id"], 7);
        assert_eq!(v["ts"], 123);
        assert_eq!(v["target"], "a");
        assert_eq!(v["success"], true);
        assert_eq!(v["latency_ms"], 9);
        assert!(v["error"].is_null());
    }

    #[test]
    fn host_port_types_thread_through() {
        let h = Host::new("example.com").unwrap();
        let p = Port::new(443).unwrap();
        assert_eq!(h.socket_hint(p), "example.com:443");
    }

    fn bad_store() -> Store {
        // Parent directory does not exist → every open fails.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-dir").join("x.db");
        // Keep `dir` alive is unnecessary: path stays missing either way.
        let _ = dir.keep();
        Store::new(DbPath::new(missing.to_string_lossy().to_string()).unwrap())
    }

    #[test]
    fn store_reports_io_errors_instead_of_panicking() {
        let s = bad_store();
        assert!(s.init().is_err());
        assert!(s.insert(UnixSecs::new(1), &target("a"), &ok(1)).is_err());
        assert!(s.latest_per_target().is_err());
        assert!(s
            .history(None, None, HistoryLimit::new(5).unwrap())
            .is_err());
        assert!(s.stats(UnixSecs::new(0)).is_err());
        assert!(s
            .outages(UnixSecs::new(0), HistoryLimit::new(5).unwrap())
            .is_err());
        assert!(s.count().is_err());
        assert!(s.db_path().as_path().to_string_lossy().contains("x.db"));
    }

    #[test]
    fn stats_on_empty_db_and_all_failures() {
        let (_dir, s) = test_store("empty-stats.db");
        assert!(s.stats(UnixSecs::new(0)).unwrap().is_empty());

        let t = target("solo");
        s.insert(UnixSecs::new(100), &t, &fail("d1")).unwrap();
        s.insert(UnixSecs::new(110), &t, &fail("d2")).unwrap();
        let stats = s.stats(UnixSecs::new(0)).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].failed, 2);
        assert!(stats[0].avg_latency_ms.is_none());
        assert!(stats[0].last_success.is_none());
        assert!(stats[0].last_failure.is_some());
        assert_eq!(stats[0].currently_up, Some(false));
        // Window excluding all rows → empty, not NaN.
        assert!(s.stats(UnixSecs::new(10_000)).unwrap().is_empty());

        let v = serde_json::to_value(&stats[0]).unwrap();
        assert_eq!(v["target"], "solo");
    }

    #[test]
    fn corrupt_rows_degrade_gracefully() {
        let (_dir, s) = test_store("corrupt.db");
        // Bypass the typed API: empty target + success-with-error is
        // inconsistent, and must not fail the whole query.
        let conn = rusqlite::Connection::open(s.db_path().as_path()).unwrap();
        conn.execute(
            "INSERT INTO checks (ts, target, success, latency_ms, error) VALUES (100, '', 1, NULL, 'stale')",
            [],
        )
        .unwrap();
        let latest = s.latest_per_target().unwrap();
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].target.as_str(), "unknown");
        assert!(!latest[0].success());
        // Stats fall back the same way.
        let stats = s.stats(UnixSecs::new(0)).unwrap();
        assert_eq!(stats[0].target.as_str(), "unknown");
    }

    #[test]
    fn group_outages_edge_cases() {
        // Open outage flushed when the target changes.
        let rows = vec![
            ("a".to_string(), 100, false),
            ("b".to_string(), 200, false),
            ("b".to_string(), 210, true),
        ];
        let out = group_outages(&rows);
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|o| o.target.as_str() == "a" && o.ongoing));

        // Leading successes with no open outage are ignored.
        let rows = vec![
            ("a".to_string(), 100, true),
            ("a".to_string(), 110, false),
            ("a".to_string(), 120, true),
        ];
        let out = group_outages(&rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].failed_checks, 1);

        // Two separate outages on one target, newest first.
        let rows = vec![
            ("a".to_string(), 100, false),
            ("a".to_string(), 110, true),
            ("a".to_string(), 200, false),
            ("a".to_string(), 230, true),
        ];
        let out = group_outages(&rows);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].start_ts.as_i64(), 200);
        assert_eq!(out[0].duration_secs, Some(30));

        // Success-only input → no outages.
        let rows = vec![("a".to_string(), 100, true)];
        assert!(group_outages(&rows).is_empty());

        // Outage serialization shape for the dashboard.
        let v = serde_json::to_value(&out[0]).unwrap();
        assert_eq!(v["ongoing"], false);
        assert_eq!(v["failed_checks"], 1);
    }

    #[test]
    fn corrupt_db_file_surfaces_errors() {
        // A file that is not a database: open succeeds, every statement fails.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.db");
        std::fs::write(&path, b"definitely not sqlite").unwrap();
        let s = Store::new(DbPath::new(path.to_string_lossy().to_string()).unwrap());
        assert!(s.init().is_err());
        assert!(s.insert(UnixSecs::new(1), &target("a"), &ok(1)).is_err());
        assert!(s.latest_per_target().is_err());
        assert!(s
            .history(None, None, HistoryLimit::new(5).unwrap())
            .is_err());
        assert!(s.stats(UnixSecs::new(0)).is_err());
        assert!(s
            .outages(UnixSecs::new(0), HistoryLimit::new(5).unwrap())
            .is_err());
        assert!(s.count().is_err());
    }

    #[test]
    fn check_accessors_and_failure_json() {
        let failed = Check::new(3, UnixSecs::new(50), target("w"), fail("kaput"));
        assert!(!failed.success());
        assert!(failed.latency_ms().is_none());
        assert_eq!(failed.error().as_deref(), Some("kaput"));
        let v = serde_json::to_value(&failed).unwrap();
        assert_eq!(v["success"], false);
        assert!(v["latency_ms"].is_null());
        assert_eq!(v["error"], "kaput");

        let succeeded = Check::new(4, UnixSecs::new(51), target("w"), ok(11));
        assert!(succeeded.success());
        assert_eq!(succeeded.latency_ms(), Some(11));
        assert!(succeeded.error().is_none());
    }
}
