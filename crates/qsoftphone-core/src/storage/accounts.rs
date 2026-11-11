//! SIP account CRUD.
//!
//! The accounts live in SQLite rather than in the JSON settings file:
//! they carry a password and a policy block, and they are the one
//! thing in the app that is written concurrently from more than one
//! place (the settings UI and the REST API).

use rusqlite::{Row, params};

use crate::error::Result;
use crate::model::account::{Account, BlfEntry, now_ms};
use crate::storage::db::Db;

/// CRUD over the `accounts` table.
pub struct AccountStore<'a> {
    db: &'a Db,
}

impl<'a> AccountStore<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// Every account, ordered by label.
    pub fn list(&self) -> Result<Vec<Account>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, label, registrar, user, password, transport, domain,
                        outbound_proxy, caller_id_header, dialed_number_header,
                        capture_headers, custom_headers, blf_extensions, crm_preset_id,
                        ringtone_file, created_at, updated_at
                 FROM accounts
                 ORDER BY label COLLATE NOCASE, id",
            )?;
            let rows = stmt.query_map([], row_to_account)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// One account by id.
    pub fn get(&self, id: &str) -> Result<Option<Account>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, label, registrar, user, password, transport, domain,
                        outbound_proxy, caller_id_header, dialed_number_header,
                        capture_headers, custom_headers, blf_extensions, crm_preset_id,
                        ringtone_file, created_at, updated_at
                 FROM accounts WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id])?;
            match rows.next()? {
                Some(row) => Ok(Some(row_to_account(row)?)),
                None => Ok(None),
            }
        })
    }

    /// Insert or update an account.
    ///
    /// An account with an empty id gets one generated. `updated_at` is
    /// always refreshed; `created_at` is kept on update.
    pub fn upsert(&self, account: &Account) -> Result<Account> {
        let mut account = account.clone();
        if account.id.trim().is_empty() {
            account.id = uuid::Uuid::new_v4().to_string();
        }
        if account.created_at == 0 {
            account.created_at = now_ms();
        }
        account.updated_at = now_ms();

        let capture = serde_json::to_string(&account.capture_headers)?;
        let custom = serde_json::to_string(&account.custom_headers)?;
        let blf = serde_json::to_string(&account.blf_extensions)?;

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO accounts
                    (id, label, registrar, user, password, transport, domain,
                     outbound_proxy, caller_id_header, dialed_number_header,
                     capture_headers, custom_headers, blf_extensions, crm_preset_id,
                     ringtone_file, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
                 ON CONFLICT(id) DO UPDATE SET
                    label                = excluded.label,
                    registrar            = excluded.registrar,
                    user                 = excluded.user,
                    password             = excluded.password,
                    transport            = excluded.transport,
                    domain               = excluded.domain,
                    outbound_proxy       = excluded.outbound_proxy,
                    caller_id_header     = excluded.caller_id_header,
                    dialed_number_header = excluded.dialed_number_header,
                    capture_headers      = excluded.capture_headers,
                    custom_headers       = excluded.custom_headers,
                    blf_extensions       = excluded.blf_extensions,
                    crm_preset_id        = excluded.crm_preset_id,
                    ringtone_file        = excluded.ringtone_file,
                    updated_at           = excluded.updated_at",
                params![
                    account.id,
                    account.label,
                    account.registrar,
                    account.user,
                    account.password,
                    account.transport,
                    account.domain,
                    account.outbound_proxy,
                    account.caller_id_header,
                    account.dialed_number_header,
                    capture,
                    custom,
                    blf,
                    account.crm_preset_id,
                    account.ringtone_file,
                    account.created_at,
                    account.updated_at,
                ],
            )?;
            Ok(())
        })?;

        Ok(account)
    }

    /// Delete an account. Returns whether a row was removed.
    pub fn delete(&self, id: &str) -> Result<bool> {
        self.db.with_conn(|conn| {
            let removed = conn.execute("DELETE FROM accounts WHERE id = ?1", params![id])?;
            Ok(removed > 0)
        })
    }

    /// How many accounts are configured.
    pub fn count(&self) -> Result<usize> {
        self.db.with_conn(|conn| {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0))?;
            Ok(n as usize)
        })
    }
}

fn row_to_account(row: &Row<'_>) -> rusqlite::Result<Account> {
    let id: String = row.get(0)?;
    let capture: String = row.get(10)?;
    let capture_headers = match serde_json::from_str::<Vec<String>>(&capture) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                account_id = %id,
                error = %e,
                "account has malformed capture_headers; treating as empty"
            );
            Vec::new()
        }
    };
    // The custom headers are a list of pairs. A column that cannot be
    // read is an account whose headers are lost, not an account that
    // cannot be loaded — so the malformed value is a warning and the
    // account still comes back.
    let custom: String = row.get(11)?;
    let custom_headers = match serde_json::from_str::<Vec<(String, String)>>(&custom) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                account_id = %id,
                error = %e,
                "account has malformed custom_headers; treating as empty"
            );
            Vec::new()
        }
    };
    // The extensions whose lamps this account watches.
    //
    // The column was a list of strings before the watched extension and
    // the number to dial became two values: `["105", "106"]`. That old
    // shape is **read**, not thrown away — an account whose list is
    // dropped is an account whose lamps go dark and whose speed dials
    // disappear, and the user is told nothing except a line in the log.
    // Each old entry is an extension to watch, with no number of its
    // own, which is exactly what an empty one means.
    let blf: String = row.get(12)?;
    let blf_extensions = match serde_json::from_str::<Vec<BlfEntry>>(&blf) {
        Ok(v) => v,
        Err(e) => match serde_json::from_str::<Vec<String>>(&blf) {
            Ok(old) => {
                tracing::info!(
                    account_id = %id,
                    "the watched extensions were read in the old format"
                );
                old.into_iter()
                    .map(|watch| BlfEntry {
                        watch,
                        dial: String::new(),
                    })
                    .collect()
            }
            Err(_) => {
                tracing::warn!(
                    account_id = %id,
                    error = %e,
                    "account has malformed blf_extensions; treating as empty"
                );
                Vec::new()
            }
        },
    };
    Ok(Account {
        id,
        label: row.get(1)?,
        registrar: row.get(2)?,
        user: row.get(3)?,
        password: row.get(4)?,
        transport: row.get(5)?,
        domain: row.get(6)?,
        outbound_proxy: row.get(7)?,
        caller_id_header: row.get(8)?,
        dialed_number_header: row.get(9)?,
        capture_headers,
        custom_headers,
        blf_extensions,
        crm_preset_id: row.get(13)?,
        ringtone_file: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crud_round_trip() {
        let db = Db::open_in_memory().unwrap();
        let store = AccountStore::new(&db);

        assert_eq!(store.count().unwrap(), 0);
        assert!(store.list().unwrap().is_empty());

        let account = Account::new("Office", "example.com", "104", "s3cret", "udp");
        let stored = store.upsert(&account).unwrap();
        assert_eq!(stored.id, account.id);

        assert_eq!(store.count().unwrap(), 1);
        let fetched = store.get(&account.id).unwrap().unwrap();
        assert_eq!(fetched.label, "Office");
        assert_eq!(fetched.user, "104");
        assert_eq!(fetched.password, "s3cret");
        assert_eq!(fetched.transport, "udp");
    }

    #[test]
    fn update_keeps_the_creation_timestamp() {
        let db = Db::open_in_memory().unwrap();
        let store = AccountStore::new(&db);

        let mut account = Account::new("Office", "example.com", "104", "", "udp");
        let created = account.created_at;
        store.upsert(&account).unwrap();

        account.label = "Home".into();
        let updated = store.upsert(&account).unwrap();
        assert_eq!(updated.created_at, created);
        assert_eq!(updated.label, "Home");
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn delete_removes_the_account() {
        let db = Db::open_in_memory().unwrap();
        let store = AccountStore::new(&db);
        let account = Account::new("Office", "example.com", "104", "", "udp");
        store.upsert(&account).unwrap();

        assert!(store.delete(&account.id).unwrap());
        assert!(!store.delete(&account.id).unwrap());
        assert_eq!(store.count().unwrap(), 0);
        assert!(store.get(&account.id).unwrap().is_none());
    }

    #[test]
    fn the_ringtone_survives_a_round_trip() {
        let db = Db::open_in_memory().unwrap();
        let store = AccountStore::new(&db);

        let mut account = Account::new("Office", "example.com", "104", "", "udp");
        account.ringtone_file = r"C:\sounds\office.wav".to_string();
        store.upsert(&account).unwrap();

        let fetched = store.get(&account.id).unwrap().unwrap();
        assert_eq!(fetched.ringtone_file, r"C:\sounds\office.wav");

        // And it is there for every account in the list, too.
        let listed = store.list().unwrap();
        assert_eq!(listed[0].ringtone_file, r"C:\sounds\office.wav");
    }

    #[test]
    fn capture_headers_survive_a_round_trip() {
        let db = Db::open_in_memory().unwrap();
        let store = AccountStore::new(&db);

        let mut account = Account::new("Office", "example.com", "104", "", "udp");
        account.capture_headers = vec!["X-Ticket".into(), "X-Region".into()];
        account.caller_id_header = "X-Caller-Name".into();
        store.upsert(&account).unwrap();

        let fetched = store.get(&account.id).unwrap().unwrap();
        assert_eq!(fetched.capture_headers, vec!["X-Ticket", "X-Region"]);
        assert_eq!(fetched.caller_id_header, "X-Caller-Name");
    }

    #[test]
    fn the_custom_headers_survive_a_round_trip() {
        let db = Db::open_in_memory().unwrap();
        let store = AccountStore::new(&db);

        let mut account = Account::new("Office", "example.com", "104", "", "udp");
        account.custom_headers = vec![
            ("X-var1".to_string(), "one".to_string()),
            ("X-var2".to_string(), "$called".to_string()),
        ];
        store.upsert(&account).unwrap();

        let fetched = store.get(&account.id).unwrap().unwrap();
        assert_eq!(fetched.custom_headers.len(), 2);
        assert_eq!(fetched.custom_headers[0].0, "X-var1");
        assert_eq!(fetched.custom_headers[0].1, "one");
        assert_eq!(fetched.custom_headers[1].0, "X-var2");
        assert_eq!(
            fetched.custom_headers[1].1, "$called",
            "the template is stored as it was typed — it is resolved per call"
        );
    }

    #[test]
    fn an_account_with_no_custom_headers_has_none() {
        let db = Db::open_in_memory().unwrap();
        let store = AccountStore::new(&db);

        let account = Account::new("Office", "example.com", "104", "", "udp");
        store.upsert(&account).unwrap();

        let fetched = store.get(&account.id).unwrap().unwrap();
        assert!(fetched.custom_headers.is_empty());
    }

    #[test]
    fn the_watched_extensions_survive_a_round_trip() {
        let db = Db::open_in_memory().unwrap();
        let store = AccountStore::new(&db);

        let mut account = Account::new("Office", "example.com", "104", "", "udp");
        account.blf_extensions = vec![
            BlfEntry {
                watch: "101".to_string(),
                dial: "00101".to_string(),
            },
            BlfEntry {
                watch: "102".to_string(),
                dial: String::new(),
            },
        ];
        store.upsert(&account).unwrap();

        let fetched = store.get(&account.id).unwrap().unwrap();
        assert_eq!(fetched.blf_extensions.len(), 2);
        assert_eq!(fetched.blf_extensions[0].watch, "101");
        assert_eq!(fetched.blf_extensions[0].dial, "00101");
        assert_eq!(fetched.blf_extensions[1].watch, "102");
        assert_eq!(fetched.blf_extensions[1].dial, "");
    }

    #[test]
    fn the_watched_extensions_are_read_out_of_the_old_format() {
        // The column used to be a list of strings: `["105", "106"]`.
        // A list that cannot be read is a list that is **lost** — the
        // lamps go dark and the speed dials disappear — so the old
        // shape is read, not thrown away.
        let db = Db::open_in_memory().unwrap();
        let store = AccountStore::new(&db);

        let account = Account::new("Office", "example.com", "104", "", "udp");
        store.upsert(&account).unwrap();

        db.with_conn(|conn| {
            conn.execute(
                "UPDATE accounts SET blf_extensions = ?1 WHERE id = ?2",
                params!["[\"105\",\"106\"]", account.id],
            )?;
            Ok(())
        })
        .unwrap();

        let fetched = store.get(&account.id).unwrap().unwrap();
        assert_eq!(fetched.blf_extensions.len(), 2);
        assert_eq!(fetched.blf_extensions[0].watch, "105");
        assert_eq!(
            fetched.blf_extensions[0].dial, "",
            "the old list had no number of its own"
        );
        assert_eq!(fetched.blf_extensions[1].watch, "106");
        assert_eq!(fetched.blf_extensions[1].dial, "");
    }

    #[test]
    fn an_account_with_no_watched_extensions_watches_nothing() {
        let db = Db::open_in_memory().unwrap();
        let store = AccountStore::new(&db);

        let account = Account::new("Office", "example.com", "104", "", "udp");
        store.upsert(&account).unwrap();

        let fetched = store.get(&account.id).unwrap().unwrap();
        assert!(fetched.blf_extensions.is_empty());
    }

    #[test]
    fn an_account_without_an_id_gets_one() {
        let db = Db::open_in_memory().unwrap();
        let store = AccountStore::new(&db);
        let mut account = Account::new("Office", "example.com", "104", "", "udp");
        account.id.clear();
        let stored = store.upsert(&account).unwrap();
        assert_eq!(stored.id.len(), 36);
    }

    #[test]
    fn accounts_are_listed_by_label() {
        let db = Db::open_in_memory().unwrap();
        let store = AccountStore::new(&db);
        store
            .upsert(&Account::new("zebra", "a", "1", "", "udp"))
            .unwrap();
        store
            .upsert(&Account::new("Alpha", "b", "2", "", "udp"))
            .unwrap();
        let labels: Vec<String> = store.list().unwrap().into_iter().map(|a| a.label).collect();
        assert_eq!(labels, vec!["Alpha", "zebra"]);
    }
}
