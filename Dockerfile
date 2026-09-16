# syntax=docker/dockerfile:1

# ---------------------------------------------------------------------------
# Builder: compile a fully static musl binary.
# `rust:alpine` targets musl natively, so `cargo build --release` already
# yields a static binary with no dynamic loader dependency.
# SQLite is compiled in (rusqlite `bundled` feature) and the dashboard HTML
# is embedded at compile time via `include_str!`, so the runtime image needs
# no libraries, no shell, and no extra data files besides the binary.
# ---------------------------------------------------------------------------
FROM rust:1.98-alpine AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY static ./static
COPY config.json ./config.json
COPY db ./db

RUN cargo build --release

# ---------------------------------------------------------------------------
# Runtime: scratch + the static binary only.
# ---------------------------------------------------------------------------
FROM scratch AS runtime

# Created automatically by Docker even on scratch; the SQLite file
# (plus its -wal/-shm sidecars) is auto-created here on first boot, so no
# manual `sqlite3` step is ever needed. Persist it with a named volume
# (see compose.yaml) or a bind-mounted *directory*.
WORKDIR /data

COPY --from=builder /app/target/release/network_packet_drop /network_packet_drop
COPY --from=builder /app/config.json /config.json

EXPOSE 3000

ENV PORT=3000 \
    DB_PATH=/data/netmon.db \
    CONFIG_PATH=/config.json \
    CHECK_INTERVAL_SECS=10 \
    CHECK_TIMEOUT_SECS=5

ENTRYPOINT ["/network_packet_drop"]

# Self-check: the binary GETs /api/health on $PORT and exits 0/1.
# (Exec form — no shell exists in scratch.)
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD ["/network_packet_drop", "healthcheck"]
