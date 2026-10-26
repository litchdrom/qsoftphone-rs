//! The local contact book.
//!
//! §5.2 falls back to this when no caller-id header is configured.
//! The lookup matches on digits so that a contact stored as
//! `+15551234567` is found when the caller dialled `5551234567` or
//! `2345567`.

use rusqlite::{Row, params};

use crate::error::Result;
use crate::storage::db::Db;

/// One contact.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    /// Row id; zero before insert.
    pub id: i64,
    pub phone: String,
    pub display_name: String,
    pub organization: String,
    pub job_title: String,
    pub email: String,
}

/// CRUD over the `contacts` table.
pub struct Contacts<'a> {
    db: &'a Db,
}

/// The shortest suffix we are willing to match on.
///
/// The C++ `ContactsService::bestMatchForPhone()` is still a TODO
/// stub, so there is no C++ rule to copy — this threshold is ours.
/// Seven digits is the shortest national significant number
/// (ITU-T E.123), and anything shorter would match far too many
/// contacts in a real address book.
const MIN_SUFFIX_DIGITS: usize = 7;

impl<'a> Contacts<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// Insert or update a contact. Returns the row id.
    pub fn upsert(&self, contact: &Contact) -> Result<i64> {
        self.db.with_conn(|conn| {
            if contact.id > 0 {
                conn.execute(
                    "UPDATE contacts
                     SET phone = ?1, display_name = ?2, organization = ?3,
                         job_title = ?4, email = ?5
                     WHERE id = ?6",
                    params![
                        contact.phone,
                        contact.display_name,
                        contact.organization,
                        contact.job_title,
                        contact.email,
                        contact.id,
                    ],
                )?;
                return Ok(contact.id);
            }
            conn.execute(
                "INSERT INTO contacts (phone, display_name, organization, job_title, email)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    contact.phone,
                    contact.display_name,
                    contact.organization,
                    contact.job_title,
                    contact.email,
                ],
            )?;
            Ok(conn.last_insert_rowid())
        })
    }

    /// Every contact, ordered by display name.
    pub fn list(&self) -> Result<Vec<Contact>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, phone, display_name, organization, job_title, email
                 FROM contacts
                 ORDER BY display_name COLLATE NOCASE, id",
            )?;
            let rows = stmt.query_map([], row_to_contact)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// Delete a contact.
    pub fn delete(&self, id: i64) -> Result<bool> {
        self.db.with_conn(|conn| {
            let removed = conn.execute("DELETE FROM contacts WHERE id = ?1", params![id])?;
            Ok(removed > 0)
        })
    }

    /// The display name of the best contact match for `phone`.
    ///
    /// An exact digit match wins. Failing that, the longest suffix
    /// match of at least [`MIN_SUFFIX_DIGITS`] digits wins, so a
    /// contact saved in international format is still found when the
    /// caller dialled the local form.
    pub fn best_match_for_phone(&self, phone: &str) -> Result<Option<String>> {
        let needle = normalise_number(phone);
        if needle.is_empty() {
            return Ok(None);
        }

        let mut best: Option<(usize, String)> = None;
        for contact in self.list()? {
            if contact.display_name.trim().is_empty() {
                continue;
            }
            let hay = normalise_number(&contact.phone);
            if hay.is_empty() {
                continue;
            }
            if hay == needle {
                return Ok(Some(contact.display_name));
            }
            if needle.len() >= MIN_SUFFIX_DIGITS && hay.ends_with(&needle) {
                let len = needle.len();
                if best.as_ref().map(|(l, _)| len > *l).unwrap_or(true) {
                    best = Some((len, contact.display_name));
                }
            }
        }
        Ok(best.map(|(_, name)| name))
    }
}

/// Strip everything but digits, keeping a leading `+`.
pub fn normalise_number(phone: &str) -> String {
    let trimmed = phone.trim();
    let plus = trimmed.starts_with('+');
    let digits: String = trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
    if plus && !digits.is_empty() {
        format!("+{digits}")
    } else {
        digits
    }
}

fn row_to_contact(row: &Row<'_>) -> rusqlite::Result<Contact> {
    Ok(Contact {
        id: row.get(0)?,
        phone: row.get(1)?,
        display_name: row.get(2)?,
        organization: row.get(3)?,
        job_title: row.get(4)?,
        email: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(db: &Db) -> Contacts<'_> {
        let contacts = Contacts::new(db);
        contacts
            .upsert(&Contact {
                phone: "+15551234567".into(),
                display_name: "Alice Smith".into(),
                ..Default::default()
            })
            .unwrap();
        contacts
    }

    #[test]
    fn normalise_keeps_digits_and_a_leading_plus() {
        assert_eq!(normalise_number("+1 (555) 123-4567"), "+15551234567");
        assert_eq!(normalise_number("555 123 4567"), "5551234567");
        assert_eq!(normalise_number(""), "");
    }

    #[test]
    fn an_exact_match_wins() {
        let db = Db::open_in_memory().unwrap();
        let contacts = seed(&db);
        assert_eq!(
            contacts.best_match_for_phone("+15551234567").unwrap(),
            Some("Alice Smith".into())
        );
    }

    #[test]
    fn a_long_suffix_match_is_found() {
        let db = Db::open_in_memory().unwrap();
        let contacts = seed(&db);
        assert_eq!(
            contacts.best_match_for_phone("5551234567").unwrap(),
            Some("Alice Smith".into())
        );
        assert_eq!(
            contacts.best_match_for_phone("1234567").unwrap(),
            Some("Alice Smith".into())
        );
    }

    #[test]
    fn a_short_suffix_does_not_match() {
        let db = Db::open_in_memory().unwrap();
        let contacts = seed(&db);
        // Six digits and fewer are too short to be a national number.
        assert_eq!(contacts.best_match_for_phone("234567").unwrap(), None);
        assert_eq!(contacts.best_match_for_phone("4567").unwrap(), None);
    }

    #[test]
    fn an_unknown_number_does_not_match() {
        let db = Db::open_in_memory().unwrap();
        let contacts = seed(&db);
        assert_eq!(contacts.best_match_for_phone("+15550000000").unwrap(), None);
    }

    #[test]
    fn the_longest_suffix_wins() {
        let db = Db::open_in_memory().unwrap();
        let contacts = Contacts::new(&db);
        contacts
            .upsert(&Contact {
                phone: "1112223333".into(),
                display_name: "Short".into(),
                ..Default::default()
            })
            .unwrap();
        contacts
            .upsert(&Contact {
                phone: "9992223333".into(),
                display_name: "Long".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            contacts.best_match_for_phone("2223333").unwrap(),
            Some("Long".into())
        );
    }

    #[test]
    fn contacts_without_a_name_are_ignored() {
        let db = Db::open_in_memory().unwrap();
        let contacts = Contacts::new(&db);
        contacts
            .upsert(&Contact {
                phone: "5551234567".into(),
                display_name: String::new(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(contacts.best_match_for_phone("5551234567").unwrap(), None);
    }
}
