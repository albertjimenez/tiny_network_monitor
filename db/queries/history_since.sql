SELECT id, ts, target, success, latency_ms, error FROM checks
WHERE ts >= ?1 ORDER BY ts DESC LIMIT ?2
