//! The SQLite handle and its schema.
//!
//! `rusqlite` is built with the `bundled` feature, so SQLite is
//! compiled into the binary — no system SQLite and no runtime DLL to
//! ship. The schema mirrors the C++ app's tables
//! (`src/core/history/CallHistory.cpp`).

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use crate::error::{Error, Result};

/// A SQLite database with the app's schema applied.
#[derive(Debug)]
pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// Open (creating if needed) a database file and apply the schema.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        Self::from_connection(Connection::open(path)?)
    }

    /// An in-memory database. Used by the tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;",
        )?;
        let db = Db {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    /// Run the schema.
    ///
    /// `CREATE TABLE IF NOT EXISTS` throughout, so opening an existing
    /// database is a no-op and the schema can grow over time.
    fn migrate(&self) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS accounts (
                    id                   TEXT    PRIMARY KEY,
                    label                TEXT    NOT NULL DEFAULT '',
                    registrar            TEXT    NOT NULL DEFAULT '',
                    user                 TEXT    NOT NULL DEFAULT '',
                    password             TEXT    NOT NULL DEFAULT '',
                    transport            TEXT    NOT NULL DEFAULT 'udp',
                    domain               TEXT    NOT NULL DEFAULT '',
                    outbound_proxy       TEXT    NOT NULL DEFAULT '',
                    caller_id_header     TEXT    NOT NULL DEFAULT '',
                    dialed_number_header TEXT    NOT NULL DEFAULT '',
                    capture_headers      TEXT    NOT NULL DEFAULT '[]',
                    crm_preset_id        TEXT    NOT NULL DEFAULT '',
                    created_at           INTEGER NOT NULL DEFAULT 0,
                    updated_at           INTEGER NOT NULL DEFAULT 0
                );

                CREATE TABLE IF NOT EXISTS contacts (
                    id           INTEGER PRIMARY KEY AUTOINCREMENT,
                    phone        TEXT    NOT NULL,
                    display_name TEXT    NOT NULL DEFAULT '',
                    organization TEXT    NOT NULL DEFAULT '',
                    job_title    TEXT    NOT NULL DEFAULT '',
                    email        TEXT    NOT NULL DEFAULT ''
                );
                CREATE INDEX IF NOT EXISTS idx_contacts_phone
                    ON contacts(phone);

                CREATE TABLE IF NOT EXISTS calls (
                    id            INTEGER PRIMARY KEY AUTOINCREMENT,
                    account_id    INTEGER,
                    call_id       TEXT,
                    direction     TEXT,
                    remote_number TEXT,
                    remote_uri    TEXT,
                    remote_name   TEXT,
                    started_at    INTEGER,
                    ended_at      INTEGER,
                    duration_sec  INTEGER,
                    sip_code      INTEGER,
                    hangup_cause  TEXT,
                    answered      INTEGER,
                    recorded      INTEGER
                );
                CREATE INDEX IF NOT EXISTS idx_calls_started_at
                    ON calls(started_at);
                CREATE INDEX IF NOT EXISTS idx_calls_call_id
                    ON calls(call_id);
                "#,
            )?;
            Ok(())
        })
    }

    /// Run a closure with the connection.
    ///
    /// The connection is behind a mutex so that the stores can be
    /// shared across the UI thread and the SIP runtime without
    /// `rusqlite`'s `Connection` being `Sync`.
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Message("database mutex poisoned".into()))?;
        f(&conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_in_memory_applies_the_schema() {
        let db = Db::open_in_memory().unwrap();
        db.with_conn(|conn| {
            let tables: Vec<String> = conn
                .prepare(
                    "SELECT name FROM sqlite_master
                     WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                     ORDER BY name",
                )?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(tables, vec!["accounts", "calls", "contacts"]);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn opening_twice_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("qsoftphone-db-{}", uuid::Uuid::new_v4()));
        let path = dir.join("accounts.sqlite");
        let db = Db::open(&path).unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO accounts (id, label) VALUES (?1, ?2)",
                rusqlite::params!["a", "A"],
            )?;
            Ok(())
        })
        .unwrap();
        drop(db);

        let db2 = Db::open(&path).unwrap();
        let count = db2
            .with_conn(|conn| {
                let n: i64 = conn.query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0))?;
                Ok(n)
            })
            .unwrap();
        assert_eq!(count, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
