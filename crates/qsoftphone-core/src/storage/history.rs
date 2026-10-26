//! Call history persistence.
//!
//! The schema matches the C++ `calls` table
//! (`src/core/history/CallHistory.cpp`). The C++ app also carries a
//! one-shot SQL repair for rows written by old versions that stored
//! the angle-bracket shape (`<sip:6666@host>`) in `remote_number`.
//! That repair is not needed here: this table is new, and
//! `CallRecord::new` derives `remote_number` from the URI through
//! `sip_uri_user_part`, so the junk shape cannot be written.

use rusqlite::{Row, params};

use crate::error::Result;
use crate::model::history::{CallRecord, Direction};
use crate::storage::db::Db;

/// CRUD over the `calls` table.
pub struct HistoryStore<'a> {
    db: &'a Db,
}

impl<'a> HistoryStore<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// Insert a finished call. Returns the new row id.
    pub fn record(&self, record: &CallRecord) -> Result<i64> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO calls
                    (account_id, call_id, direction, remote_number, remote_uri,
                     remote_name, dialed_number, started_at, ended_at, duration_sec,
                     sip_code, hangup_cause, answered, recorded)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    record.account_id,
                    record.call_id,
                    record.direction.as_str(),
                    record.remote_number,
                    record.remote_uri,
                    record.remote_name,
                    record.dialed_number,
                    record.started_at,
                    record.ended_at,
                    record.duration_sec,
                    record.sip_code,
                    record.hangup_cause,
                    record.answered as i64,
                    record.recorded as i64,
                ],
            )?;
            Ok(conn.last_insert_rowid())
        })
    }

    /// The most recent calls, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<CallRecord>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, account_id, call_id, direction, remote_number, remote_uri,
                        remote_name, dialed_number, started_at, ended_at, duration_sec,
                        sip_code,
                        hangup_cause, answered, recorded
                 FROM calls
                 ORDER BY started_at DESC, id DESC
                 LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], row_to_record)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// One call by its SIP call id.
    pub fn by_call_id(&self, call_id: &str) -> Result<Option<CallRecord>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, account_id, call_id, direction, remote_number, remote_uri,
                        remote_name, dialed_number, started_at, ended_at, duration_sec,
                        sip_code,
                        hangup_cause, answered, recorded
                 FROM calls WHERE call_id = ?1 ORDER BY id DESC LIMIT 1",
            )?;
            let mut rows = stmt.query(params![call_id])?;
            match rows.next()? {
                Some(row) => Ok(Some(row_to_record(row)?)),
                None => Ok(None),
            }
        })
    }

    /// Total number of stored calls.
    pub fn total_calls(&self) -> Result<i64> {
        self.db.with_conn(|conn| {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM calls", [], |r| r.get(0))?;
            Ok(n)
        })
    }

    /// Number of missed calls.
    pub fn missed_calls(&self) -> Result<i64> {
        self.db.with_conn(|conn| {
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM calls WHERE direction = ?1",
                params![Direction::Missed.as_str()],
                |r| r.get(0),
            )?;
            Ok(n)
        })
    }
}

fn row_to_record(row: &Row<'_>) -> rusqlite::Result<CallRecord> {
    let direction: String = row.get(3)?;
    Ok(CallRecord {
        id: row.get(0)?,
        account_id: row.get(1)?,
        call_id: row.get(2)?,
        direction: Direction::parse(&direction).unwrap_or_default(),
        remote_number: row.get(4)?,
        remote_uri: row.get(5)?,
        remote_name: row.get(6)?,
        dialed_number: row.get(7)?,
        started_at: row.get(8)?,
        ended_at: row.get(9)?,
        duration_sec: row.get(10)?,
        sip_code: row.get(11)?,
        hangup_cause: row.get(12)?,
        answered: row.get::<_, i64>(13)? != 0,
        recorded: row.get::<_, i64>(14)? != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips() {
        let db = Db::open_in_memory().unwrap();
        let store = HistoryStore::new(&db);

        let record = CallRecord::from_incoming(
            1,
            "call-1",
            "sip:6666@193.178.169.164",
            "6666",
            "Alice",
            1_000,
            61_000,
            200,
            "Normal",
            true,
        );
        let id = store.record(&record).unwrap();
        assert!(id > 0);

        let fetched = store.by_call_id("call-1").unwrap().unwrap();
        assert_eq!(fetched.remote_number, "6666");
        assert_eq!(fetched.remote_uri, "sip:6666@193.178.169.164");
        assert_eq!(fetched.direction, Direction::Incoming);
        assert_eq!(fetched.duration_sec, 60);
        assert!(fetched.answered);
        assert_eq!(store.total_calls().unwrap(), 1);
    }

    #[test]
    fn the_dialed_number_round_trips() {
        let db = Db::open_in_memory().unwrap();
        let store = HistoryStore::new(&db);

        store
            .record(&CallRecord::from_incoming(
                1,
                "call-did",
                "sip:asterisk@host",
                "6666",
                "Alice",
                0,
                0,
                0,
                "",
                false,
            ))
            .unwrap();

        let fetched = store.by_call_id("call-did").unwrap().unwrap();
        assert_eq!(fetched.remote_number, "asterisk");
        assert_eq!(fetched.dialed_number, "6666");
    }

    #[test]
    fn history_never_stores_the_local_aor_as_the_remote_number() {
        let db = Db::open_in_memory().unwrap();
        let store = HistoryStore::new(&db);

        // The To-URI is our AOR; the caller dialled 6666.
        store
            .record(&CallRecord::from_incoming(
                1,
                "call-2",
                "sip:6666@host",
                "6666",
                "",
                0,
                0,
                0,
                "",
                false,
            ))
            .unwrap();

        let recent = store.recent(10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].remote_number, "6666");
    }

    #[test]
    fn recent_returns_newest_first() {
        let db = Db::open_in_memory().unwrap();
        let store = HistoryStore::new(&db);
        for i in 0..3 {
            store
                .record(&CallRecord::from_outgoing(
                    1,
                    format!("call-{i}"),
                    "sip:6666@host",
                    "",
                    i * 1_000,
                    0,
                    200,
                    "",
                    true,
                ))
                .unwrap();
        }
        let recent = store.recent(2).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].call_id, "call-2");
        assert_eq!(recent[1].call_id, "call-1");
    }

    #[test]
    fn missed_calls_are_counted_separately() {
        let db = Db::open_in_memory().unwrap();
        let store = HistoryStore::new(&db);
        store
            .record(&CallRecord::missed(
                1,
                "call-m",
                "sip:6666@host",
                "6666",
                "",
                0,
            ))
            .unwrap();
        store
            .record(&CallRecord::from_outgoing(
                1,
                "call-o",
                "sip:7777@host",
                "",
                0,
                0,
                200,
                "",
                true,
            ))
            .unwrap();
        assert_eq!(store.total_calls().unwrap(), 2);
        assert_eq!(store.missed_calls().unwrap(), 1);
    }
}
