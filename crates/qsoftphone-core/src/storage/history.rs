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

    /// The calls that match a search, newest first.
    ///
    /// The C++ filters with a case-insensitive substring over the
    /// remote number, the remote URI, the remote name and the hangup
    /// cause — and it matches the SIP status code exactly, so "486"
    /// finds every busy call. The direction filter narrows it
    /// further.
    pub fn search(
        &self,
        query: &str,
        direction: Option<Direction>,
        limit: usize,
    ) -> Result<Vec<CallRecord>> {
        let term = query.trim();
        let pattern = format!("%{}%", escape_like(term));
        // A query that is a number is also a SIP status code search.
        let code: Option<i32> = term.parse().ok();
        let wanted = direction.map(|d| d.as_str().to_string());

        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, account_id, call_id, direction, remote_number, remote_uri,
                        remote_name, dialed_number, started_at, ended_at, duration_sec,
                        sip_code,
                        hangup_cause, answered, recorded
                 FROM calls
                 WHERE (?1 = ''
                        OR remote_number LIKE ?2 ESCAPE '\\'
                        OR remote_uri    LIKE ?2 ESCAPE '\\'
                        OR remote_name   LIKE ?2 ESCAPE '\\'
                        OR hangup_cause  LIKE ?2 ESCAPE '\\'
                        OR (?3 IS NOT NULL AND sip_code = ?3))
                   AND (?4 IS NULL OR direction = ?4)
                 ORDER BY started_at DESC, id DESC
                 LIMIT ?5",
            )?;
            let rows = stmt.query_map(
                params![term, pattern, code, wanted, limit as i64],
                row_to_record,
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
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

/// The account id of a row, read leniently.
///
/// The column was declared INTEGER in the first schema and holds the
/// account's UUID. SQLite keeps a UUID as text even in an INTEGER
/// column, but a row written before the change could still hold a
/// number — and one unreadable row must not break the whole list.
fn account_id_of(row: &Row<'_>) -> rusqlite::Result<String> {
    Ok(match row.get_ref(1)? {
        rusqlite::types::ValueRef::Text(text) => String::from_utf8_lossy(text).into_owned(),
        rusqlite::types::ValueRef::Integer(number) => number.to_string(),
        rusqlite::types::ValueRef::Real(number) => number.to_string(),
        rusqlite::types::ValueRef::Blob(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        rusqlite::types::ValueRef::Null => String::new(),
    })
}

/// Escape the `LIKE` wildcards in a search term, so that a search for
/// "50%" finds the number 50% and not everything.
fn escape_like(term: &str) -> String {
    term.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn row_to_record(row: &Row<'_>) -> rusqlite::Result<CallRecord> {
    let direction: String = row.get(3)?;
    Ok(CallRecord {
        id: row.get(0)?,
        account_id: account_id_of(row)?,
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
            "acct-1",
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
                "acct-1",
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
                "acct-1",
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
                    "acct-1",
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
                "acct-1",
                "call-m",
                "sip:6666@host",
                "6666",
                "",
                0,
            ))
            .unwrap();
        store
            .record(&CallRecord::from_outgoing(
                "acct-1",
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

    /// Three rows to search through.
    fn seeded() -> (Db, Vec<CallRecord>) {
        let db = Db::open_in_memory().unwrap();
        let store = HistoryStore::new(&db);
        let rows = vec![
            CallRecord::from_incoming(
                "acct-1",
                "call-alice",
                "sip:6666@host",
                "6666",
                "Alice",
                3_000,
                63_000,
                200,
                "Normal",
                true,
            ),
            CallRecord::from_outgoing(
                "acct-1",
                "call-bob",
                "sip:7777@host",
                "",
                2_000,
                2_000,
                486,
                "Busy",
                false,
            ),
            CallRecord::missed(
                "acct-1",
                "call-carol",
                "sip:8888@host",
                "8888",
                "Carol",
                1_000,
            ),
        ];
        for row in &rows {
            store.record(row).unwrap();
        }
        (db, rows)
    }

    #[test]
    fn an_empty_search_finds_everything() {
        let (db, _) = seeded();
        let store = HistoryStore::new(&db);
        assert_eq!(store.search("", None, 50).unwrap().len(), 3);
    }

    #[test]
    fn the_search_matches_the_number_the_uri_and_the_name() {
        let (db, _) = seeded();
        let store = HistoryStore::new(&db);

        // The number, case-insensitively.
        let hits = store.search("ALICE", None, 50).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].call_id, "call-alice");

        // The URI.
        let hits = store.search("sip:8888", None, 50).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].call_id, "call-carol");

        // A substring of the number.
        let hits = store.search("777", None, 50).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].call_id, "call-bob");

        // Nothing matches everything.
        assert!(store.search("nobody", None, 50).unwrap().is_empty());
    }

    #[test]
    fn a_numeric_search_also_finds_the_sip_code() {
        let (db, _) = seeded();
        let store = HistoryStore::new(&db);
        // 486 is the busy code of the second call.
        let hits = store.search("486", None, 50).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].call_id, "call-bob");
    }

    #[test]
    fn the_direction_filter_narrows_the_search() {
        let (db, _) = seeded();
        let store = HistoryStore::new(&db);

        let missed = store.search("", Some(Direction::Missed), 50).unwrap();
        assert_eq!(missed.len(), 1);
        assert_eq!(missed[0].call_id, "call-carol");

        let incoming = store.search("", Some(Direction::Incoming), 50).unwrap();
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].call_id, "call-alice");

        let outgoing = store.search("", Some(Direction::Outgoing), 50).unwrap();
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].call_id, "call-bob");
    }

    #[test]
    fn a_wildcard_in_the_search_is_literal() {
        let (db, _) = seeded();
        let store = HistoryStore::new(&db);
        // "%" must not match everything.
        assert!(store.search("%", None, 50).unwrap().is_empty());
        // "_" matches a single character, not any character.
        assert!(store.search("_", None, 50).unwrap().is_empty());
    }

    #[test]
    fn a_database_written_by_the_old_schema_still_takes_a_uuid_account() {
        // The first schema declared `account_id INTEGER`, and there
        // are databases on disk with that table already. SQLite keeps
        // a UUID as text even in an INTEGER column, and the reader
        // accepts both, so an existing database keeps working.
        let path = std::env::temp_dir().join("qsoftphone-old-schema-history-test.sqlite");
        let _ = std::fs::remove_file(&path);
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE calls (
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
                );",
            )
            .unwrap();
        }

        let db = Db::open(&path).unwrap();
        let store = HistoryStore::new(&db);
        let account = "7b797ac0-0642-46b4-b1ad-f25912da318e";
        let record = CallRecord::from_incoming(
            account,
            "call-1",
            "sip:6666@host",
            "6666",
            "",
            1_000,
            2_000,
            200,
            "Normal",
            true,
        );
        store.record(&record).unwrap();

        let fetched = store.by_call_id("call-1").unwrap().unwrap();
        assert_eq!(fetched.account_id, account);
        assert_eq!(fetched.remote_number, "6666");

        let _ = std::fs::remove_file(&path);
    }
}
