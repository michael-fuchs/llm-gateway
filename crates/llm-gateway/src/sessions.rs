//! The owner's browser sessions for the usage view, stored in the usage database.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jiff::Timestamp;
use rand::RngCore;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::usage::{StoreError, UsageStore};

/// A logged-in owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The owner's user ID at the identity provider.
    pub owner_id: String,
    /// How to show the owner, e.g. their email address.
    pub owner_name: String,
}

impl UsageStore {
    /// Starts a session and returns its token, which only the browser keeps.
    pub fn create_session(
        &self,
        session: &Session,
        lifetime: Duration,
    ) -> Result<String, StoreError> {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token = URL_SAFE_NO_PAD.encode(bytes);
        let now = Timestamp::now().as_millisecond();
        let conn = self.conn();
        conn.execute("DELETE FROM session WHERE expires_at <= ?1", [now])?;
        conn.execute(
            "INSERT INTO session (token_hash, owner_id, owner_name, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                hash(&token),
                session.owner_id,
                session.owner_name,
                now + lifetime.as_millis() as i64
            ],
        )?;
        Ok(token)
    }

    /// The session for a token, if it exists and has not expired.
    pub fn session(&self, token: &str) -> Result<Option<Session>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT owner_id, owner_name FROM session WHERE token_hash = ?1 AND expires_at > ?2",
                params![hash(token), Timestamp::now().as_millisecond()],
                |row| {
                    Ok(Session {
                        owner_id: row.get(0)?,
                        owner_name: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn end_session(&self, token: &str) -> Result<(), StoreError> {
        self.conn()
            .execute("DELETE FROM session WHERE token_hash = ?1", [hash(token)])?;
        Ok(())
    }
}

fn hash(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_last_until_they_expire_or_end() {
        let store = UsageStore::in_memory().unwrap();
        let owner = Session {
            owner_id: "user-1".into(),
            owner_name: "owner@example.org".into(),
        };
        let token = store
            .create_session(&owner, Duration::from_secs(60))
            .unwrap();
        assert_eq!(store.session(&token).unwrap(), Some(owner.clone()));
        assert_eq!(store.session("other").unwrap(), None);
        store.end_session(&token).unwrap();
        assert_eq!(store.session(&token).unwrap(), None);

        let expired = store.create_session(&owner, Duration::ZERO).unwrap();
        assert_eq!(store.session(&expired).unwrap(), None);
    }

    #[test]
    fn only_a_hash_of_the_token_is_stored() {
        let store = UsageStore::in_memory().unwrap();
        let owner = Session {
            owner_id: "user-1".into(),
            owner_name: "owner".into(),
        };
        let token = store
            .create_session(&owner, Duration::from_secs(60))
            .unwrap();
        let stored: String = store
            .conn()
            .query_row("SELECT token_hash FROM session", [], |row| row.get(0))
            .unwrap();
        assert_ne!(stored, token);
    }
}
