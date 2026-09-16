# netmon — packet loss & connectivity dashboard

A tiny Rust webapp that probes the internet every 10 seconds, persists every
result to SQLite, and serves a live dashboard showing packet loss, outages,
and latency per target.

Probes are plain TCP connects (+ DNS resolution) against free endpoints such
as Google/Cloudflare — no root, no ICMP privileges needed. The dashboard page
has zero external dependencies (no CDN), so it still renders while your
internet is down.

## Quickstart

### Local

```bash
cargo run
# open http://localhost:3000
```

### Docker Compose

```bash
docker compose up -d --build
# open http://localhost:3000
docker compose logs -f
```

The SQLite database is **auto-created on first boot** at `DB_PATH`
(`/data/netmon.db` in the container, on the `netmon-data` volume). You never
need to create it manually — just make sure the *parent directory* is
writable. Prefer a named volume or a bind-mounted **directory** (never a
single file: SQLite needs `-wal`/`-shm` sidecars next to the `.db`).

## Configuration

Precedence: **built-in defaults < config file < environment variables**.
A present-but-invalid file or env value is a hard startup error — the app
never silently falls back, so typos fail fast instead of hiding.

`config.json`:

```json
{
  "port": 3000,
  "db_path": "netmon.db",
  "check_interval_secs": 10,
  "check_timeout_secs": 5,
  "targets": [
    { "name": "Google DNS", "host": "8.8.8.8", "port": 53 },
    { "name": "Cloudflare DNS", "host": "1.1.1.1", "port": 53 },
    { "name": "Google HTTPS", "host": "google.com", "port": 443 }
  ]
}
```

| Env var               | File key              | Default      | Constraints                              |
| --------------------- | --------------------- | ------------ | ---------------------------------------- |
| `PORT`                | `port`                | `3000`       | 1–65535 (`0` is rejected at type level)  |
| `DB_PATH`             | `db_path`             | `netmon.db`  | non-empty path                           |
| `CHECK_INTERVAL_SECS` | `check_interval_secs` | `10`         | 1–3600 s                                 |
| `CHECK_TIMEOUT_SECS`  | `check_timeout_secs`  | `5`          | 1–60 s, must be ≤ interval               |
| `TARGETS`             | `targets`             | 8 built-ins  | `"Name=host:port,host2:port2"`           |
| `CONFIG_PATH`         | —                     | `config.json`| path to the JSON file (optional)         |

Invariants are enforced by validated newtypes (`Port(NonZeroU16)`,
`TargetName`, `Host`, `CheckInterval`, `ProbeTimeout`, …) plus a fallible
`Config::new`, so illegal states are unrepresentable downstream.

## HTTP API

| Method | Route                   | Query params                          | Description                              |
| ------ | ----------------------- | ------------------------------------- | ---------------------------------------- |
| `GET`  | `/`                     | —                                     | Dashboard (single self-contained page)   |
| `GET`  | `/api/health`           | —                                     | `{"ok": true}`                           |
| `GET`  | `/api/config`           | —                                     | Effective interval, timeout, targets     |
| `GET`  | `/api/status`           | —                                     | Online flag + latest check per target    |
| `GET`  | `/api/history`          | `hours`, `target`, `limit`            | Raw checks, newest first (max 10 000)    |
| `GET`  | `/api/stats`            | `hours` (default 24)                  | Totals, loss %, avg latency per target   |
| `GET`  | `/api/outages`          | `hours` (default 24)                  | Consecutive failures grouped into outages|

`hours` must be within `0.05–720`; invalid values are rejected with `422`
before any handler logic runs.

## Dashboard

- `ONLINE` / `DEGRADED` / `OFFLINE` badge from the latest round.
- Per-target cards: current state, loss % over the selected range and 24 h,
  average latency, last failure.
- Latency chart (canvas, dependency-free); failed checks are red dots
  pinned to the top so packet loss is visible at a glance.
- Outage table (grouped consecutive failures with duration) and a recent
  failures feed. Range selector 1 h – 7 d, auto-refresh on the probe cadence.

## Development

```bash
cargo test                 # 74 unit + integration tests
cargo llvm-cov --summary-only            # ≈97% line coverage (≈99% outside main())
cargo llvm-cov --show-missing-lines      # what is left (main() glue + defensive arms)
```

### Project layout

```
src/
  main.rs     startup glue (thin by design)
  domain.rs   validated newtypes + ProbeOutcome (illegal states unrepresentable)
  error.rs    typed errors per layer (thiserror)
  config.rs   defaults < file < env loading with hard validation
  db.rs       Store repository over SQLite + pure outage grouping
  monitor.rs  TCP/DNS prober returning ProbeOutcome
  api.rs      axum routes with validated query types
static/       dashboard (embedded into the binary via include_str!)
db/
  schema.sql      single source of truth for the schema
  queries/        one .sql file per statement, embedded via include_str!
build.rs      compile-time SQL gate: applies the schema to :memory: and
              EXPLAINs every query — invalid SQL fails the build, no
              DATABASE_URL or live database needed
```

### Docker image

Multi-stage build on `rust:1.98-alpine` (musl target natively → fully static
binary), final stage is `scratch`: no shell, no libc, no package manager —
just `/network_packet_drop` + `/config.json`. SQLite is compiled in via
rusqlite's `bundled` feature, and the dashboard HTML is embedded, so the
runtime image needs nothing else.

```bash
docker build -t netmon .
docker run -p 3000:3000 -v netmon-data:/data --restart unless-stopped netmon
```

Prebuilt multi-arch images (`linux/amd64` + `linux/arm64`) are published to
Docker Hub on every `v*` tag:

```bash
docker run -p 3000:3000 -v netmon-data:/data --restart unless-stopped \
  beruto/netmon:latest
```

### Healthcheck

The scratch image has no shell/curl, so the binary checks itself with zero
extra dependencies:

```bash
netmon healthcheck        # uses $PORT, default 3000
netmon healthcheck 3000   # explicit port
```

It GETs `/api/health` over a plain socket (3 s timeout per phase) and exits
`0` on status 200 + `{"ok": …}`, `1` otherwise. Both the `Dockerfile`
(`HEALTHCHECK`) and `compose.yaml` (`healthcheck:`) use it, so orchestrators
restart the container when the web layer wedges.

### Releases

Pipelines are split: `.github/workflows/ci.yml` runs fmt, clippy
`-D warnings` and tests on every push/PR; `.github/workflows/cd.yml` runs
only on `v*` tags, re-runs the test gate, then publishes:

| Platform | Target | Archive |
|---|---|---|
| 64-bit Linux (fully static musl) | `x86_64-unknown-linux-musl` | `.tar.gz` |
| 32-bit ARM Linux, musl (old Raspberry Pi / routers) | `arm-unknown-linux-musleabihf` | `.tar.gz` |
| Apple Silicon macOS | `aarch64-apple-darwin` | `.tar.gz` |
| 64-bit Windows, cross-compiled with mingw-w64 (mingw runtime linked statically — no extra DLLs) | `x86_64-pc-windows-gnu` | `.zip` |

Each archive bundles the binary with `config.json` + `README.md` and is
attached to the GitHub release. Docker images stay `linux/amd64` +
`linux/arm64` (published to `beruto/netmon:<tag>` and `:latest` on Docker
Hub): 64-bit covers every supported OS install, while 32-bit stragglers
are served by the static armv6 binary instead of a QEMU-emulated image build.

CD pushes to Docker Hub with a scoped Personal Access Token stored as the
`DOCKERHUB_TOKEN` repository secret — never your password, never in code.
Create it at Docker Hub → Account Settings → Personal Access Tokens
(Read & Write scope), then add it at GitHub → repo Settings → Secrets and
variables → Actions → New repository secret. That's the only secret the
pipeline needs (releases use the automatic `GITHUB_TOKEN`).
