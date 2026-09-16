SELECT target, ts, success FROM checks
WHERE ts >= ?1 ORDER BY target ASC, ts ASC, id ASC LIMIT ?2
