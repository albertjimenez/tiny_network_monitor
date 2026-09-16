//! Connectivity probing.
//!
//! The probe returns [`ProbeOutcome`] (success XOR failure) instead of a
//! `(bool, Option, Option)` triple, so inconsistent states like
//! "success with an error attached" cannot be constructed.

use tokio::net::TcpStream;

use crate::config::Config;
use crate::db::Store;
use crate::domain::{CompactError, LatencyMs, MonitorTarget, ProbeOutcome, ProbeTimeout, UnixSecs};

/// Single TCP-connect check with timeout. Works without root (unlike ICMP)
/// and is a good proxy for "packet loss / no internet".
pub async fn check_target(target: &MonitorTarget, timeout: ProbeTimeout) -> ProbeOutcome {
    let addr_hint = target.socket_hint();
    let start = std::time::Instant::now();

    // Resolve + connect with timeout. lookup_host handles both IP and DNS names,
    // so a DNS failure also counts as an outage (which is what we want).
    let connect = async {
        let mut addrs = tokio::net::lookup_host(addr_hint).await?;
        // Try each resolved address until one works.
        let mut last_err: Option<std::io::Error> = None;
        for addr in addrs.by_ref() {
            match TcpStream::connect(addr).await {
                Ok(stream) => {
                    drop(stream);
                    return Ok::<(), std::io::Error>(());
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| std::io::Error::other("no addresses resolved")))
    };

    match tokio::time::timeout(timeout.duration(), connect).await {
        Ok(Ok(())) => ProbeOutcome::success(LatencyMs::from_elapsed(start.elapsed())),
        Ok(Err(e)) => ProbeOutcome::failure(CompactError::new(&e.to_string())),
        Err(_) => ProbeOutcome::failure(CompactError::timeout()),
    }
}

pub(crate) async fn run_once(cfg: &Config, store: &Store) {
    let mut jobs = Vec::new();
    for t in cfg.targets.iter().cloned() {
        let timeout = cfg.check_timeout;
        jobs.push(tokio::spawn(async move {
            let outcome = check_target(&t, timeout).await;
            (t, outcome)
        }));
    }
    for j in jobs {
        match j.await {
            Ok((target, outcome)) => {
                let ts = UnixSecs::now();
                let (ok, latency, error) = match &outcome {
                    ProbeOutcome::Success { latency } => (true, Some(latency.as_i64()), None),
                    ProbeOutcome::Failure { error } => (false, None, Some(error.as_str())),
                };
                if let Err(e) = store.insert(ts, &target.name, &outcome) {
                    tracing::error!("db insert failed: {e}");
                } else {
                    tracing::info!(
                        target = %target.name,
                        success = ok,
                        latency_ms = ?latency,
                        error = ?error,
                        "check done"
                    );
                }
            }
            Err(e) => tracing::error!("check task panicked: {e}"),
        }
    }
}

/// Spawns the periodic probe loop. Returns the [`JoinHandle`] so callers
/// (and tests) can observe or abort it; the loop itself runs forever.
pub fn spawn_monitor(cfg: Config, store: Store) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Run one round immediately so the dashboard isn't empty on boot.
        run_once(&cfg, &store).await;
        let mut ticker = tokio::time::interval(cfg.check_interval.duration());
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            run_once(&cfg, &store).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Store;
    use crate::domain::{
        CheckInterval, DbPath, Host, NonEmptyTargets, Port, ProbeTimeout, TargetName,
    };
    use crate::domain::{HistoryLimit, MonitorTarget};

    fn local_target(port: u16) -> MonitorTarget {
        MonitorTarget::new(
            TargetName::new("local").unwrap(),
            Host::new("127.0.0.1").unwrap(),
            Port::new(port).unwrap(),
        )
    }

    /// Success path against a real local listener — no internet needed.
    #[tokio::test]
    async fn check_target_succeeds_against_local_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // Keep the listener alive while we probe.
        let target = local_target(port);
        let probe = check_target(&target, ProbeTimeout::new(2).unwrap());
        let (outcome, _) = tokio::join!(probe, async {
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(2), listener.accept()).await;
        });
        assert!(outcome.is_success());
        assert!(outcome.latency().is_some());
    }

    /// Failure path: nothing listening → connection refused, no internet needed.
    #[tokio::test]
    async fn check_target_fails_on_refused_port() {
        // Bind then drop to get a (very likely) closed port.
        let port = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };
        let outcome = check_target(&local_target(port), ProbeTimeout::new(2).unwrap()).await;
        assert!(!outcome.is_success());
        assert!(outcome.error().is_some());
        assert!(outcome.latency().is_none());
    }

    #[test]
    fn outcome_is_exhaustive_by_construction() {
        let ok = ProbeOutcome::success(LatencyMs::new(1).unwrap());
        assert!(ok.is_success());
        assert!(ok.error().is_none());
        let fail = ProbeOutcome::failure(CompactError::timeout());
        assert!(!fail.is_success());
        assert!(fail.latency().is_none());
    }

    /// run_once probes every target and persists one row each.
    #[tokio::test]
    async fn run_once_persists_one_row_per_target() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open_port = listener.local_addr().unwrap().port();
        let closed_port = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("run_once.db");
        let cfg = Config::new(
            Port::new(3000).unwrap(),
            DbPath::new(db.to_string_lossy().to_string()).unwrap(),
            CheckInterval::new(10).unwrap(),
            ProbeTimeout::new(2).unwrap(),
            HistoryLimit::default_valid(),
            NonEmptyTargets::new(vec![
                MonitorTarget::new(
                    TargetName::new("open").unwrap(),
                    Host::new("127.0.0.1").unwrap(),
                    Port::new(open_port).unwrap(),
                ),
                MonitorTarget::new(
                    TargetName::new("closed").unwrap(),
                    Host::new("127.0.0.1").unwrap(),
                    Port::new(closed_port).unwrap(),
                ),
            ])
            .unwrap(),
        )
        .unwrap();
        let store = Store::new(cfg.db_path.clone());
        store.init().unwrap();

        // Accept one connection in the background so the "open" probe succeeds.
        let accept = tokio::spawn(async move {
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept()).await;
        });
        run_once(&cfg, &store).await;
        let _ = accept.await;

        assert_eq!(store.count().unwrap(), 2);
        let latest = store.latest_per_target().unwrap();
        assert_eq!(latest.len(), 2);
        assert!(
            latest
                .iter()
                .find(|c| c.target.as_str() == "open")
                .unwrap()
                .success()
        );
        assert!(
            !latest
                .iter()
                .find(|c| c.target.as_str() == "closed")
                .unwrap()
                .success()
        );
    }

    /// Unroutable TEST-NET-1 address: the connect hangs until the probe
    /// timeout fires, covering the elapsed-timeout arm.
    #[tokio::test]
    async fn check_target_reports_timeout_as_failure() {
        let target = MonitorTarget::new(
            TargetName::new("blackhole").unwrap(),
            Host::new("192.0.2.1").unwrap(),
            Port::new(81).unwrap(),
        );
        let outcome = check_target(&target, ProbeTimeout::new(1).unwrap()).await;
        assert!(!outcome.is_success());
        let err = outcome.error().expect("timeout must carry an error");
        assert_eq!(err.as_str(), "timeout");
    }

    /// A broken store must not take the probe loop down: the error is logged
    /// and the round completes.
    #[tokio::test]
    async fn run_once_survives_db_errors() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("no-such-dir").join("x.db");
        let _ = dir.keep();
        let cfg = Config::new(
            Port::new(3000).unwrap(),
            DbPath::new(bad.to_string_lossy().to_string()).unwrap(),
            CheckInterval::new(10).unwrap(),
            ProbeTimeout::new(1).unwrap(),
            HistoryLimit::default_valid(),
            NonEmptyTargets::new(vec![local_target(9)]).unwrap(),
        )
        .unwrap();
        let store = Store::new(cfg.db_path.clone());
        // No panic, no result: errors go to tracing.
        run_once(&cfg, &store).await;
    }

    /// The spawned loop probes immediately and then on every tick.
    #[tokio::test]
    async fn spawn_monitor_probes_on_start_and_tick() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("spawned.db");
        let cfg = Config::new(
            Port::new(3000).unwrap(),
            DbPath::new(db.to_string_lossy().to_string()).unwrap(),
            CheckInterval::new(1).unwrap(),
            ProbeTimeout::new(1).unwrap(),
            HistoryLimit::default_valid(),
            NonEmptyTargets::new(vec![local_target(port)]).unwrap(),
        )
        .unwrap();
        let store = Store::new(cfg.db_path.clone());
        store.init().unwrap();

        let accept = tokio::spawn(async move {
            loop {
                if tokio::time::timeout(std::time::Duration::from_millis(400), listener.accept())
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let handle = spawn_monitor(cfg, store.clone());
        tokio::time::sleep(std::time::Duration::from_millis(2200)).await;
        handle.abort();
        let _ = accept.await;
        // Immediate run + at least one tick.
        assert!(store.count().unwrap() >= 2);
    }
}
