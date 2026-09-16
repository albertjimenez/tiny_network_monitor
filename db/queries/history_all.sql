SELECT id, ts, target, success, latency_ms, error FROM checks
ORDER BY ts DESC LIMIT ?1
