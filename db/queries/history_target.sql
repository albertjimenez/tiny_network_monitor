SELECT id, ts, target, success, latency_ms, error FROM checks
WHERE target = ?1 ORDER BY ts DESC LIMIT ?2
