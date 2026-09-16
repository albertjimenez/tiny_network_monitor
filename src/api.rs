//! HTTP API. Query params deserialize directly into validated domain
//! types ([`LookbackHours`], [`HistoryLimit`], [`TargetName`]), so invalid
//! ranges are rejected with 422 before any handler logic runs.

use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::config::Config;
use crate::db::{self, Check, Store};
use crate::domain::{HistoryLimit, LookbackHours, TargetName, UnixSecs};
use crate::error::StoreError;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub store: Store,
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    pub hours: Option<LookbackHours>,
    #[serde(default)]
    pub target: Option<TargetName>,
    #[serde(default)]
    pub limit: Option<HistoryLimit>,
}

#[derive(Debug, Deserialize)]
pub struct StatsQuery {
    #[serde(default)]
    pub hours: Option<LookbackHours>,
}

#[derive(Debug, Deserialize)]
pub struct OutagesQuery {
    #[serde(default)]
    pub hours: Option<LookbackHours>,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub online: bool,
    pub checked_at: i64,
    pub targets: Vec<Check>,
    pub total_checks: i64,
}

/// Typed API error → proper status codes (500 for storage, never 200-on-error).
#[derive(Debug)]
pub enum ApiError {
    Store(StoreError),
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            Self::Store(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        (status, msg).into_response()
    }
}

/// Pure: is the fleet online given the latest check per target?
/// `true` iff at least one target's latest probe succeeded.
pub fn fleet_online(latest: &[Check]) -> bool {
    !latest.is_empty() && latest.iter().any(|c| c.success())
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true}))
}

async fn get_config(State(s): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "port": s.cfg.port_u16(),
        "check_interval_secs": s.cfg.interval_secs(),
        "check_timeout_secs": s.cfg.timeout_secs(),
        "targets": s.cfg.targets.as_slice(),
    }))
}

async fn get_status(State(s): State<AppState>) -> Result<Json<StatusResponse>, ApiError> {
    let latest = s.store.latest_per_target()?;
    let total = s.store.count().unwrap_or(0);
    Ok(Json(StatusResponse {
        online: fleet_online(&latest),
        checked_at: UnixSecs::now().as_i64(),
        targets: latest,
        total_checks: total,
    }))
}

async fn get_history(
    State(s): State<AppState>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Vec<Check>>, ApiError> {
    let now = UnixSecs::now();
    let since = q.hours.map(|h| h.since(now));
    let rows = s.store.history(
        since,
        q.target.as_ref(),
        q.limit.unwrap_or(s.cfg.history_limit),
    )?;
    Ok(Json(rows))
}

async fn get_stats(
    State(s): State<AppState>,
    Query(q): Query<StatsQuery>,
) -> Result<Json<Vec<db::TargetStats>>, ApiError> {
    tracing::debug!(
        lookback_h = q.hours.map(|h| h.hours()).unwrap_or(24.0),
        "stats query"
    );
    let since = q
        .hours
        .unwrap_or(LookbackHours::default_24h())
        .since(UnixSecs::now());
    Ok(Json(s.store.stats(since)?))
}

async fn get_outages(
    State(s): State<AppState>,
    Query(q): Query<OutagesQuery>,
) -> Result<Json<Vec<db::Outage>>, ApiError> {
    let since = q
        .hours
        .unwrap_or(LookbackHours::default_24h())
        .since(UnixSecs::now());
    Ok(Json(s.store.outages(since, s.cfg.history_limit)?))
}

pub fn router(cfg: Config, store: Store) -> Router {
    let state = AppState {
        cfg: Arc::new(cfg),
        store,
    };
    Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
        .route("/api/config", get(get_config))
        .route("/api/status", get(get_status))
        .route("/api/history", get(get_history))
        .route("/api/stats", get(get_stats))
        .route("/api/outages", get(get_outages))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        CheckInterval, DbPath, Host, LatencyMs, MonitorTarget, NonEmptyTargets, Port, ProbeTimeout,
    };
    use crate::domain::{CompactError, ProbeOutcome, TargetName};

    fn check(id: i64, target: &str, ok: bool) -> Check {
        let outcome = if ok {
            ProbeOutcome::success(LatencyMs::new(5).unwrap())
        } else {
            ProbeOutcome::failure(CompactError::new("down"))
        };
        Check::new(
            id,
            UnixSecs::new(100),
            TargetName::new(target).unwrap(),
            outcome,
        )
    }

    #[test]
    fn fleet_online_needs_at_least_one_success() {
        assert!(!fleet_online(&[]));
        assert!(!fleet_online(&[check(1, "a", false)]));
        assert!(fleet_online(&[check(1, "a", false), check(2, "b", true)]));
        assert!(fleet_online(&[check(1, "a", true)]));
    }

    #[test]
    fn history_query_rejects_out_of_range_hours() {
        // axum uses serde Deserialize: invalid hours must fail deserialization
        let bad: Result<HistoryQuery, _> = serde_json::from_value(serde_json::json!({
            "hours": 5000.0
        }));
        assert!(bad.is_err());
        let ok: HistoryQuery = serde_json::from_value(serde_json::json!({
            "hours": 6.0, "limit": 50
        }))
        .unwrap();
        assert_eq!(ok.hours.unwrap().hours(), 6.0);
    }

    #[test]
    fn history_query_rejects_empty_target_name() {
        let bad: Result<HistoryQuery, _> =
            serde_json::from_value(serde_json::json!({ "target": "   " }));
        assert!(bad.is_err());
    }

    fn test_state(name: &str) -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join(name);
        let cfg = Config::new(
            Port::new(3000).unwrap(),
            DbPath::new(db.to_string_lossy().to_string()).unwrap(),
            CheckInterval::new(10).unwrap(),
            ProbeTimeout::new(5).unwrap(),
            HistoryLimit::default_valid(),
            NonEmptyTargets::new(vec![MonitorTarget::new(
                TargetName::new("web").unwrap(),
                Host::new("example.com").unwrap(),
                Port::new(443).unwrap(),
            )])
            .unwrap(),
        )
        .unwrap();
        let store = Store::new(cfg.db_path.clone());
        store.init().unwrap();
        (
            dir,
            AppState {
                cfg: Arc::new(cfg),
                store,
            },
        )
    }

    #[tokio::test]
    async fn status_reflects_latest_probe() {
        let (_dir, st) = test_state("status.db");
        // No data → offline=false, empty list.
        let s = get_status(State(st.clone())).await.unwrap();
        assert!(!s.online);
        assert!(s.targets.is_empty());

        st.store
            .insert(
                UnixSecs::now(),
                &TargetName::new("web").unwrap(),
                &ProbeOutcome::success(LatencyMs::new(7).unwrap()),
            )
            .unwrap();
        let s = get_status(State(st)).await.unwrap();
        assert!(s.online);
        assert_eq!(s.total_checks, 1);
    }

    #[tokio::test]
    async fn history_stats_outages_config_health() {
        let (_dir, st) = test_state("handlers.db");
        let t = TargetName::new("web").unwrap();
        let now = UnixSecs::now().as_i64();
        st.store
            .insert(
                UnixSecs::new(now - 100),
                &t,
                &ProbeOutcome::failure(CompactError::new("down")),
            )
            .unwrap();
        st.store
            .insert(
                UnixSecs::new(now),
                &t,
                &ProbeOutcome::success(LatencyMs::new(7).unwrap()),
            )
            .unwrap();

        let h = get_history(
            State(st.clone()),
            Query(HistoryQuery {
                hours: None,
                target: None,
                limit: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(h.len(), 2);

        let stats = get_stats(State(st.clone()), Query(StatsQuery { hours: None }))
            .await
            .unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].total, 2);

        let outages = get_outages(State(st.clone()), Query(OutagesQuery { hours: None }))
            .await
            .unwrap();
        assert_eq!(outages.len(), 1);
        assert!(!outages[0].ongoing);

        let cfg = get_config(State(st)).await;
        assert_eq!(cfg.0["port"], 3000);

        let health = health().await;
        assert_eq!(health.0["ok"], true);
    }

    #[tokio::test]
    async fn filtered_queries_hit_all_branches() {
        let (_dir, st) = test_state("filtered.db");
        let t = TargetName::new("web").unwrap();
        let now = UnixSecs::now().as_i64();
        for (i, outcome) in [
            ProbeOutcome::success(LatencyMs::new(3).unwrap()),
            ProbeOutcome::failure(CompactError::new("down")),
        ]
        .into_iter()
        .enumerate()
        {
            st.store
                .insert(UnixSecs::new(now - 50 + i as i64 * 10), &t, &outcome)
                .unwrap();
        }

        // hours + target + limit all Some.
        let h = get_history(
            State(st.clone()),
            Query(HistoryQuery {
                hours: Some(LookbackHours::new(1.0).unwrap()),
                target: Some(TargetName::new("web").unwrap()),
                limit: Some(HistoryLimit::new(5).unwrap()),
            }),
        )
        .await
        .unwrap();
        assert_eq!(h.len(), 2);

        // target-only filter (since=None branch).
        let h = get_history(
            State(st.clone()),
            Query(HistoryQuery {
                hours: None,
                target: Some(TargetName::new("web").unwrap()),
                limit: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(h.len(), 2);

        let stats = get_stats(
            State(st.clone()),
            Query(StatsQuery {
                hours: Some(LookbackHours::new(1.0).unwrap()),
            }),
        )
        .await
        .unwrap();
        assert_eq!(stats.len(), 1);

        let outages = get_outages(
            State(st.clone()),
            Query(OutagesQuery {
                hours: Some(LookbackHours::new(1.0).unwrap()),
            }),
        )
        .await
        .unwrap();
        assert_eq!(outages.len(), 1);
    }

    #[tokio::test]
    async fn store_failure_maps_to_500() {
        use axum::response::IntoResponse;
        let (_dir, mut st) = test_state("err.db");
        // Point the store at a path whose parent does not exist.
        st.store = Store::new(
            DbPath::new(
                st.store
                    .db_path()
                    .as_path()
                    .join("no-such-dir")
                    .join("x.db")
                    .to_string_lossy()
                    .to_string(),
            )
            .unwrap(),
        );
        let err = get_status(State(st.clone())).await.unwrap_err();
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

        // Same failure through the history endpoint (covers its error arm).
        let err = get_history(
            State(st),
            Query(HistoryQuery {
                hours: None,
                target: None,
                limit: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn index_returns_dashboard_html() {
        let html = index().await;
        assert!(html.0.contains("NetMon"));
    }

    #[tokio::test]
    async fn router_serves_health_and_dashboard_over_http() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let (_dir, st) = test_state("router.db");
        let app = router((*st.cfg).clone(), st.store.clone());

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
