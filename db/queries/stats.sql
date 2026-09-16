SELECT target,
       COUNT(*) as total,
       SUM(success) as success,
       AVG(CASE WHEN success = 1 THEN latency_ms ELSE NULL END) as avg_lat,
       MAX(CASE WHEN success = 1 THEN ts ELSE NULL END) as last_ok,
       MAX(CASE WHEN success = 0 THEN ts ELSE NULL END) as last_fail
FROM checks WHERE ts >= ?1 GROUP BY target ORDER BY target
