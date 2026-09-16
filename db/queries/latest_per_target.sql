SELECT id, ts, target, success, latency_ms, error FROM checks
WHERE id IN (SELECT MAX(id) FROM checks GROUP BY target)
ORDER BY target
