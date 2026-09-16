SELECT id, ts, target, success, latency_ms, error FROM checks
WHERE ts >= ?1 AND target = ?2 ORDER BY ts DESC LIMIT ?3
