-- Browser sessions of the owner for the usage view. Only a hash of the session token is
-- stored, so the database alone does not allow taking over a session.
CREATE TABLE session (
    token_hash TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,         -- the owner's user ID at the identity provider
    owner_name TEXT NOT NULL,       -- for display, e.g. the email address
    expires_at INTEGER NOT NULL     -- Unix time in milliseconds
) STRICT;
