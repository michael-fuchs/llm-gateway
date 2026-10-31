//! The usage database: what each call used and cost, and which requests were refused.

use std::path::Path;
use std::sync::Mutex;

use jiff::Timestamp;
use rusqlite::{Connection, params};
use rusqlite_migration::{M, Migrations};

use crate::money::Eur;

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(include_str!("../migrations/0001-usage.sql"))])
}

/// A call that reached the provider and was answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    pub at: Timestamp,
    pub caller: String,
    pub sphere: String,
    pub provider: String,
    pub model: String,
    pub inputs: u64,
    pub input_tokens: u64,
    pub cost: Eur,
}

/// A request the gateway refused before contacting the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub at: Timestamp,
    pub caller: String,
    pub sphere: String,
    pub model: String,
    pub reason: String,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Migration(#[from] rusqlite_migration::Error),
}

/// The SQLite database holding usage and refusals.
pub struct UsageStore {
    conn: Mutex<Connection>,
}

impl UsageStore {
    /// Opens the database at `path`, creating and migrating it as needed.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        migrations().to_latest(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// An empty database in memory, for tests.
    pub fn in_memory() -> Result<Self, StoreError> {
        let mut conn = Connection::open_in_memory()?;
        migrations().to_latest(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave a half-written row: each write is one
        // statement. So a poisoned lock is still safe to use.
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn record_usage(&self, usage: &Usage) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO usage (at, caller, sphere, provider, model, inputs, input_tokens, cost_nano_eur) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                usage.at.as_millisecond(),
                usage.caller,
                usage.sphere,
                usage.provider,
                usage.model,
                usage.inputs as i64,
                usage.input_tokens as i64,
                usage.cost.nano(),
            ],
        )?;
        Ok(())
    }

    pub fn record_refusal(&self, refusal: &Refusal) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO refusal (at, caller, sphere, model, reason) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                refusal.at.as_millisecond(),
                refusal.caller,
                refusal.sphere,
                refusal.model,
                refusal.reason,
            ],
        )?;
        Ok(())
    }

    /// The estimated cost of all calls at or after `since`.
    pub fn spent_since(&self, since: Timestamp) -> Result<Eur, StoreError> {
        let nano: i64 = self.conn().query_row(
            "SELECT coalesce(sum(cost_nano_eur), 0) FROM usage WHERE at >= ?1",
            [since.as_millisecond()],
            |row| row.get(0),
        )?;
        Ok(Eur::from_nano(nano))
    }

    /// All recorded calls, oldest first.
    pub fn usage(&self) -> Result<Vec<Usage>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT at, caller, sphere, provider, model, inputs, input_tokens, cost_nano_eur \
             FROM usage ORDER BY at, id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Usage {
                at: timestamp(row.get(0)?),
                caller: row.get(1)?,
                sphere: row.get(2)?,
                provider: row.get(3)?,
                model: row.get(4)?,
                inputs: row.get::<_, i64>(5)? as u64,
                input_tokens: row.get::<_, i64>(6)? as u64,
                cost: Eur::from_nano(row.get(7)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// All recorded refusals, oldest first.
    pub fn refusals(&self) -> Result<Vec<Refusal>, StoreError> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT at, caller, sphere, model, reason FROM refusal ORDER BY at, id")?;
        let rows = stmt.query_map([], |row| {
            Ok(Refusal {
                at: timestamp(row.get(0)?),
                caller: row.get(1)?,
                sphere: row.get(2)?,
                model: row.get(3)?,
                reason: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn timestamp(millis: i64) -> Timestamp {
    Timestamp::from_millisecond(millis).unwrap_or(Timestamp::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_valid() {
        migrations().validate().unwrap();
    }

    #[test]
    fn sums_costs_since_a_point_in_time() {
        let store = UsageStore::in_memory().unwrap();
        let at = |s: &str| s.parse::<Timestamp>().unwrap();
        for (when, nano) in [
            ("2026-09-30T23:59:59Z", 7),
            ("2026-10-01T00:00:00Z", 100),
            ("2026-10-31T12:00:00Z", 20),
        ] {
            store
                .record_usage(&Usage {
                    at: at(when),
                    caller: "mnemograph".into(),
                    sphere: "work".into(),
                    provider: "mistral".into(),
                    model: "mistral-embed".into(),
                    inputs: 1,
                    input_tokens: 1,
                    cost: Eur::from_nano(nano),
                })
                .unwrap();
        }
        assert_eq!(
            store.spent_since(at("2026-10-01T00:00:00Z")).unwrap(),
            Eur::from_nano(120)
        );
        assert_eq!(
            store.spent_since(at("2026-11-01T00:00:00Z")).unwrap(),
            Eur::ZERO
        );
        assert_eq!(store.usage().unwrap().len(), 3);
    }
}
