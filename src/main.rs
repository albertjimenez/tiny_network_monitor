mod api;
mod config;
mod db;
mod domain;
mod error;
mod monitor;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use config::Config;
use db::Store;
use domain::Port;

#[tokio::main]
async fn main() {
    // Self-healthcheck for container runtimes. The scratch image has no
    // shell/curl/wget, so the binary probes itself over plain std sockets
    // (no HTTP client dependency): `netmon healthcheck [port]`.
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        let extra: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(run_healthcheck(&extra));
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "network_packet_drop=info,tower_http=info".into()),
        )
        .init();

    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FATAL: invalid configuration: {e}");
            std::process::exit(1);
        }
    };

    let store = Store::new(cfg.db_path.clone());
    if let Err(e) = store.init() {
        eprintln!("FATAL: cannot init sqlite db at {}: {e}", store.db_path());
        std::process::exit(1);
    }

    monitor::spawn_monitor(cfg.clone(), store.clone());

    let addr = bind_addr(&cfg);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("bind failed");

    print!("{}", banner(&cfg));

    let app = api::router(cfg, store);
    tracing::info!("listening on {addr}");
    axum::serve(listener, app).await.expect("server failed");
}

/// Pure helper so the bind address is unit-testable without opening a socket.
fn bind_addr(cfg: &Config) -> String {
    format!("0.0.0.0:{}", cfg.port_u16())
}

/// Timeout for each phase of the self-healthcheck.
const HEALTHCHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// `netmon healthcheck [port]`: exit 0 when `/api/health` answers 200 with
/// `{"ok": …}`, else 1. Returns the exit code instead of exiting so tests
/// can assert on it; only `main` calls `process::exit`.
fn run_healthcheck(extra: &[String]) -> i32 {
    let port = match resolve_health_port(extra.first(), std::env::var("PORT").ok().as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("healthcheck: {e}");
            return 1;
        }
    };
    match check_health(SocketAddr::from(([127, 0, 0, 1], port))) {
        Ok(()) => {
            println!("ok");
            0
        }
        Err(e) => {
            eprintln!("healthcheck: unhealthy: {e}");
            1
        }
    }
}

/// Precedence: CLI arg > `PORT` env > 3000. Takes the env value as a
/// parameter (instead of reading it) so tests stay hermetic.
fn resolve_health_port(arg: Option<&String>, env: Option<&str>) -> Result<u16, String> {
    let raw = match (arg, env) {
        (Some(a), _) => a.clone(),
        (None, Some(e)) if !e.trim().is_empty() => e.to_string(),
        _ => return Ok(3000),
    };
    let parsed: u16 = raw
        .trim()
        .parse()
        .map_err(|_| format!("invalid port {raw:?}"))?;
    Port::new(parsed)
        .map(|p| p.get())
        .map_err(|e| e.to_string())
}

/// Minimal HTTP/1.1 client over a blocking std socket: GET, then require
/// status 200 and an `"ok"` JSON body. Timeouts bound every phase so a
/// wedged server fails fast instead of hanging the orchestrator.
fn check_health(addr: SocketAddr) -> Result<(), String> {
    let mut stream =
        TcpStream::connect_timeout(&addr, HEALTHCHECK_TIMEOUT).map_err(|e| format!("{e}"))?;
    stream
        .set_read_timeout(Some(HEALTHCHECK_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(HEALTHCHECK_TIMEOUT)))
        .map_err(|e| format!("{e}"))?;
    write!(
        stream,
        "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| format!("{e}"))?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).map_err(|e| format!("{e}"))?;
    let text = String::from_utf8_lossy(&buf);
    let status = text.lines().next().unwrap_or_default();
    if status.split_whitespace().nth(1) != Some("200") {
        return Err(format!("unexpected status line {status:?}"));
    }
    if !text.contains("\"ok\"") {
        return Err("response is missing the ok body".to_string());
    }
    Ok(())
}

/// Pure helper rendering the startup banner (also unit-tested).
fn banner(cfg: &Config) -> String {
    let mut out = format!(
        "netmon up on http://localhost:{0}  (db={1}, every {2}s)\n",
        cfg.port_u16(),
        cfg.db_path,
        cfg.interval_secs()
    );
    out.push_str(&format!("targets ({}):\n", cfg.targets.len()));
    for t in cfg.targets.iter() {
        out.push_str(&format!("  - {t}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        CheckInterval, DbPath, HistoryLimit, Host, MonitorTarget, NonEmptyTargets, Port,
        ProbeTimeout, TargetName,
    };

    fn test_config() -> Config {
        Config::new(
            Port::new(3000).unwrap(),
            DbPath::new("netmon.db").unwrap(),
            CheckInterval::new(10).unwrap(),
            ProbeTimeout::new(5).unwrap(),
            HistoryLimit::default_valid(),
            NonEmptyTargets::new(vec![MonitorTarget::new(
                TargetName::new("Google DNS").unwrap(),
                Host::new("8.8.8.8").unwrap(),
                Port::new(53).unwrap(),
            )])
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn bind_addr_uses_configured_port() {
        assert_eq!(bind_addr(&test_config()), "0.0.0.0:3000");
    }

    #[test]
    fn banner_lists_targets_and_settings() {
        let b = banner(&test_config());
        assert!(b.contains("http://localhost:3000"));
        assert!(b.contains("netmon.db"));
        assert!(b.contains("every 10s"));
        assert!(b.contains("Google DNS"));
        assert!(b.contains("8.8.8.8:53"));
    }

    #[test]
    fn resolve_health_port_prefers_arg_then_env_then_default() {
        let arg = "8080".to_string();
        assert_eq!(resolve_health_port(Some(&arg), Some("9999")).unwrap(), 8080);
        assert_eq!(resolve_health_port(None, Some("9999")).unwrap(), 9999);
        assert_eq!(resolve_health_port(None, Some("   ")).unwrap(), 3000);
        assert_eq!(resolve_health_port(None, None).unwrap(), 3000);
        assert_eq!(
            resolve_health_port(Some(&" 8081 ".to_string()), None).unwrap(),
            8081
        );
    }

    #[test]
    fn resolve_health_port_rejects_garbage_and_zero() {
        assert!(resolve_health_port(Some(&"abc".to_string()), None).is_err());
        assert!(resolve_health_port(None, Some("zzz")).is_err());
        assert!(resolve_health_port(Some(&"0".to_string()), None).is_err());
        assert!(resolve_health_port(None, Some("70000")).is_err());
    }

    /// Stub origin server on a loopback ephemeral port serving one canned
    /// HTTP response. Returns the address to probe.
    fn stub_health_server(response: &'static str) -> SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf); // consume the request
                let _ = stream.write_all(response.as_bytes());
            }
        });
        addr
    }

    #[test]
    fn check_health_accepts_ok_json() {
        let addr = stub_health_server(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 12\r\nConnection: close\r\n\r\n{\"ok\": true}",
        );
        assert!(check_health(addr).is_ok());
    }

    #[test]
    fn check_health_rejects_bad_status_and_body() {
        let bad_status = stub_health_server(
            "HTTP/1.1 500 Bang\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        );
        assert!(check_health(bad_status).is_err());

        let bad_body = stub_health_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        );
        assert!(check_health(bad_body).is_err());
    }

    #[test]
    fn check_health_fails_on_refused_port() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        assert!(check_health(SocketAddr::from(([127, 0, 0, 1], port))).is_err());
    }

    #[test]
    fn run_healthcheck_returns_exit_codes() {
        let addr = stub_health_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\n{\"ok\": true}",
        );
        assert_eq!(run_healthcheck(&[addr.port().to_string()]), 0);
        assert_eq!(run_healthcheck(&["not-a-port".to_string()]), 1);
    }
}
