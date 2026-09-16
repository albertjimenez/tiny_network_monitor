SELECT success FROM checks WHERE target = ?1 AND ts >= ?2 ORDER BY ts DESC, id DESC LIMIT 1
