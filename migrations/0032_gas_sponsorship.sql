-- Gas sponsorship usage ledger. Single source of truth for both the global
-- circuit breaker (sum per chain/window) and the per-subject rolling quota
-- (sum per quota_subject/chain/window). Wei-based: no tiers, no pre-aggregated
-- metrics table -- SUM on read, add a rollup only if volume demands it.
CREATE TABLE IF NOT EXISTS sponsorship_usage (
    id            BIGINT      PRIMARY KEY GENERATED ALWAYS AS IDENTITY,
    -- The on-chain EOA from the signed intent. Always recorded for audit,
    -- regardless of which quota_key the policy uses.
    user_address  TEXT        NOT NULL,
    -- The identity the per-user quota counts against: the EOA (quota_key=address)
    -- or the Better Auth userId from the verified JWT sub (quota_key=user).
    quota_subject TEXT        NOT NULL,
    chain_id      BIGINT      NOT NULL,
    tx_hash       TEXT        NOT NULL,
    gas_used      NUMERIC     NOT NULL,
    gas_price     NUMERIC     NOT NULL,
    -- gas_used * gas_price, denormalized for cheap windowed SUM aggregation.
    eth_spent     NUMERIC     NOT NULL,
    sponsored_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- One row per sponsored tx; guards against double-counting on retries.
    CONSTRAINT sponsorship_usage_tx_hash_unique UNIQUE (tx_hash)
);

-- Per-subject rolling-window quota lookups.
CREATE INDEX IF NOT EXISTS idx_sponsorship_usage_subject_time
    ON sponsorship_usage (quota_subject, chain_id, sponsored_at DESC);

-- Global circuit-breaker window lookups.
CREATE INDEX IF NOT EXISTS idx_sponsorship_usage_chain_time
    ON sponsorship_usage (chain_id, sponsored_at DESC);
