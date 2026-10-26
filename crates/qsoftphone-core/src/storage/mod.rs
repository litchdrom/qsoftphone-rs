//! Persistence: SQLite for accounts, contacts and call history.

pub mod accounts;
pub mod contacts;
pub mod db;
pub mod history;

pub use accounts::AccountStore;
pub use contacts::{Contact, Contacts};
pub use db::Db;
pub use history::HistoryStore;
