//! The history service — the UI's side of SQLite.
//!
//! §6: no blocking in the UI thread, ever. So the UI never touches the
//! database for the history: it sends a command over a channel, a task
//! on the runtime does the work, and the answer comes back over
//! another channel and is drained at the top of the frame (§3).
//!
//! The SQLite call itself is blocking I/O, so it goes to tokio's
//! blocking pool rather than onto a runtime worker.

use std::sync::Arc;

use qsoftphone_core::model::history::{CallRecord, Direction};
use qsoftphone_core::storage::Db;
use qsoftphone_core::storage::history::HistoryStore;
use tokio::runtime::Handle;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tracing::warn;

/// How many rows the history shows at most.
const MAX_ROWS: usize = 500;

/// What the UI asks the history for.
enum Command {
    /// Store a finished call.
    Record(CallRecord),
    /// Ask for the rows that match a search.
    Query {
        text: String,
        direction: Option<Direction>,
    },
}

/// What the history answers with.
pub enum HistoryUpdate {
    /// The rows that match the current query, newest first.
    Rows(Vec<CallRecord>),
    /// The history could not be read or written.
    Failed(String),
}

/// The history, as the UI sees it.
pub struct HistoryService {
    commands: UnboundedSender<Command>,
    updates: UnboundedReceiver<HistoryUpdate>,
}

impl HistoryService {
    /// Start the history worker on the runtime.
    pub fn new(handle: Handle, db: Arc<Db>) -> Self {
        let (commands, inbox) = unbounded_channel();
        let (outbox, updates) = unbounded_channel();
        handle.spawn(async move {
            worker(db, inbox, outbox).await;
        });
        Self { commands, updates }
    }

    /// Store a finished call. The write happens on the runtime.
    pub fn record(&self, record: CallRecord) {
        let _ = self.commands.send(Command::Record(record));
    }

    /// Ask for the rows that match a search.
    pub fn query(&self, text: &str, direction: Option<Direction>) {
        let _ = self.commands.send(Command::Query {
            text: text.to_string(),
            direction,
        });
    }

    /// The answers that arrived since the last frame.
    pub fn drain(&mut self) -> Vec<HistoryUpdate> {
        let mut out = Vec::new();
        while let Ok(update) = self.updates.try_recv() {
            out.push(update);
        }
        out
    }
}

/// The worker: every command is answered with the rows that match the
/// current query, so a new call shows up without the UI asking again.
async fn worker(
    db: Arc<Db>,
    mut inbox: UnboundedReceiver<Command>,
    outbox: UnboundedSender<HistoryUpdate>,
) {
    let mut text = String::new();
    let mut direction: Option<Direction> = None;

    while let Some(command) = inbox.recv().await {
        let record = match command {
            Command::Record(record) => Some(record),
            Command::Query {
                text: new_text,
                direction: new_direction,
            } => {
                text = new_text;
                direction = new_direction;
                None
            }
        };

        let search_text = text.clone();
        let search_direction = direction;
        let db = db.clone();

        // SQLite is blocking I/O: the blocking pool, not a runtime
        // worker.
        let outcome = tokio::task::spawn_blocking(move || {
            let store = HistoryStore::new(&db);
            if let Some(record) = record {
                store.record(&record)?;
            }
            store.search(&search_text, search_direction, MAX_ROWS)
        })
        .await;

        match outcome {
            Ok(Ok(rows)) => {
                let _ = outbox.send(HistoryUpdate::Rows(rows));
            }
            Ok(Err(e)) => {
                warn!(error = %e, "the history query failed");
                let _ = outbox.send(HistoryUpdate::Failed(e.to_string()));
            }
            Err(e) => {
                warn!(error = %e, "the history task failed");
                let _ = outbox.send(HistoryUpdate::Failed(e.to_string()));
            }
        }
    }
}
