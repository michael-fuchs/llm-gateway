-- One row per call that reached the provider and was answered.
CREATE TABLE usage (
    id INTEGER PRIMARY KEY,
    at INTEGER NOT NULL,            -- Unix time in milliseconds
    caller TEXT NOT NULL,
    sphere TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    inputs INTEGER NOT NULL,        -- number of texts in the request
    input_tokens INTEGER NOT NULL,
    cost_nano_eur INTEGER NOT NULL  -- estimated cost in 10^-9 EUR
) STRICT;
CREATE INDEX usage_at ON usage (at);

-- One row per request the gateway refused for a reason the owner should see
-- (budget exhausted now; rate limits and sphere rules later).
CREATE TABLE refusal (
    id INTEGER PRIMARY KEY,
    at INTEGER NOT NULL,            -- Unix time in milliseconds
    caller TEXT NOT NULL,
    sphere TEXT NOT NULL,
    model TEXT NOT NULL,
    reason TEXT NOT NULL            -- machine-readable code, e.g. budget_exhausted
) STRICT;
CREATE INDEX refusal_at ON refusal (at);
