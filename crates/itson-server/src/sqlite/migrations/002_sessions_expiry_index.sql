-- Eight-byte big-endian timestamps preserve unsigned expiry ordering.
CREATE INDEX sessions_by_expiry ON sessions(expires_at);
