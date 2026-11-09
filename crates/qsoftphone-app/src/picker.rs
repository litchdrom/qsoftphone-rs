//! The native file dialog.
//!
//! The C++ has one: `src/ui/SettingsWindow.qml` opens a QML
//! `FileDialog` — "Pick a ringtone WAV file", with a WAV filter and an
//! "All files" escape — and writes what comes back into the
//! `ringtone_file` setting. This is that dialog, for the same purpose.
//!
//! ## Why it is on a thread of its own
//!
//! The dialog is a window of the **operating system**, and it stays
//! open until the user has chosen a file or given up. Opening it on the
//! interface thread would freeze the window for as long as the user
//! takes to look at it (§3: the interface never waits). So the dialog
//! is opened on a worker thread and what it says arrives on a channel,
//! which the app drains at the top of the frame — the same pattern as
//! the registration state, the call events and the media.
//!
//! That is not a workaround: `rfd`'s own asynchronous path does
//! exactly this, and on Windows it has to — the COM apartment the
//! dialog lives in is per **thread**, and `rfd` initialises COM on
//! whichever thread opens the dialog.
//!
//! ## What the dialog cannot do
//!
//! It cannot be *parented* to the app's own window. `eframe` keeps the
//! raw window handle private (`raw_window_handle` is `pub(crate)` in
//! `epi.rs`), so there is no handle to hand to `rfd`. The dialog is
//! therefore a window of its own: it comes to the front, because the
//! process that owns the foreground may always bring its own windows
//! forward, but it is not modal to the app's window.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

/// The title the C++ gives the dialog.
pub const DIALOG_TITLE: &str = "Pick a ringtone WAV file";

/// The name of the WAV filter.
pub const WAV_FILTER: &str = "WAV files";

/// The name of the escape-hatch filter.
pub const ALL_FILES_FILTER: &str = "All files";

/// What the dialog was opened for.
///
/// The target travels with the answer, because the dialog is modal to
/// the *user*, not to the app: the window keeps running while it is
/// open, and the user can pick another account, or another page, before
/// they come back with a file. The answer has to say what it was for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The app's own ringtone — the settings page.
    AppRingtone,
    /// One account's ringtone — the account editor.
    ///
    /// The account's **id** and not its index: the list can be
    /// re-ordered while the dialog is open, and a file picked for one
    /// account must not end up on another.
    AccountRingtone {
        /// The id of the account the dialog was opened for.
        account_id: String,
    },
}

/// What the dialog says when it comes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    /// The user chose a file, and it is one the softphone can play.
    File {
        /// What the dialog was opened for.
        target: Target,
        /// The file the user chose.
        path: PathBuf,
    },
    /// The user closed the dialog without choosing anything.
    Cancelled {
        /// What the dialog was opened for.
        target: Target,
    },
    /// The file was chosen but cannot be played — or the dialog itself
    /// could not be opened.
    Failed {
        /// What the dialog was opened for.
        target: Target,
        /// Why it could not be used.
        error: String,
    },
}

/// The dialog, and the thread it runs on.
pub struct Picker {
    /// Where the answers arrive.
    results: Receiver<Picked>,
    /// The way back to the worker.
    sender: Sender<Picked>,
    /// Whether a dialog is open right now.
    ///
    /// One at a time: two dialogs on top of each other is a window
    /// nobody can find, and the second one would answer for the first.
    open: bool,
}

impl Picker {
    /// A picker with no dialog open.
    pub fn new() -> Self {
        let (sender, results) = channel();
        Self {
            results,
            sender,
            open: false,
        }
    }

    /// Open the dialog.
    ///
    /// Returns `false` when a dialog is already open: the second one
    /// would be a window on top of a window, and the answer would
    /// arrive for the wrong target.
    pub fn open(&mut self, target: Target, start_in: Option<PathBuf>) -> bool {
        if self.open {
            return false;
        }
        self.open = true;

        let sender = self.sender.clone();
        let spawned = std::thread::Builder::new()
            .name("qsoftphone-file-dialog".to_string())
            .spawn(move || {
                let picked = pick(target, start_in);
                let _ = sender.send(picked);
            });
        if let Err(error) = spawned {
            // The thread could not be started, so no answer will ever
            // come. The flag has to go back, or the picker would be
            // closed for the rest of the session.
            self.open = false;
            tracing::warn!(error = %error, "the file dialog thread could not be started");
        }
        true
    }

    /// The answer, if there is one waiting.
    pub fn take(&mut self) -> Option<Picked> {
        let picked = self.results.try_recv().ok()?;
        // The dialog that was open has now answered.
        self.open = false;
        Some(picked)
    }

    /// Whether a dialog is open right now.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Hand an answer in.
    ///
    /// This is how the tests answer: a test cannot open a real
    /// dialog — it would open one on the machine the suite runs on,
    /// and wait for a person who is not there. The worker sends on the
    /// channel itself.
    #[cfg(test)]
    pub fn deliver(&self, picked: Picked) {
        let _ = self.sender.send(picked);
    }
}

impl Default for Picker {
    fn default() -> Self {
        Self::new()
    }
}

/// Open the dialog and bring back what the user said.
fn pick(target: Target, start_in: Option<PathBuf>) -> Picked {
    let mut dialog = rfd::FileDialog::new()
        .set_title(DIALOG_TITLE)
        .add_filter(WAV_FILTER, &["wav"]);

    // The escape hatch: the C++ has "All files (*.*)" next to its WAV
    // filter, and a ringtone is not always a `.wav` on disk.
    //
    // On macOS the panel merges every filter into one flat list of
    // allowed types, and there a `*` is not a wildcard but a literal
    // extension — so the escape hatch is left off there and the WAV
    // filter stands on its own.
    #[cfg(not(target_os = "macos"))]
    {
        dialog = dialog.add_filter(ALL_FILES_FILTER, &["*"]);
    }

    if let Some(directory) = start_in {
        dialog = dialog.set_directory(directory);
    }

    let Some(path) = dialog.pick_file() else {
        return Picked::Cancelled { target };
    };

    // The file has to be one the softphone can actually play: 16-bit
    // PCM. The check is here, on the worker, because reading a file is
    // not something the interface thread should wait for — and because
    // a file that cannot be played is worth refusing **here**, where
    // the user can be told why, instead of at the first incoming call.
    match playable(&path) {
        Ok(()) => Picked::File { target, path },
        Err(error) => Picked::Failed { target, error },
    }
}

/// Whether the file is a WAV the softphone can play.
///
/// The same reader the ringtone itself uses, so what is accepted here
/// is exactly what will ring later.
pub fn playable(path: &Path) -> Result<(), String> {
    match qsoftphone_media::read_wav(path) {
        Ok(_) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_travels_from_the_worker_to_the_app() {
        let mut picker = Picker::new();
        assert!(!picker.is_open());

        let target = Target::AppRingtone;
        picker.deliver(Picked::File {
            target: target.clone(),
            path: PathBuf::from(r"C:\sounds\desk.wav"),
        });

        let picked = picker.take().expect("the answer did not arrive");
        assert_eq!(
            picked,
            Picked::File {
                target: target.clone(),
                path: PathBuf::from(r"C:\sounds\desk.wav"),
            }
        );
        assert!(picker.take().is_none(), "there was a second answer");
    }

    #[test]
    fn a_cancelled_dialog_says_so() {
        let mut picker = Picker::new();
        picker.deliver(Picked::Cancelled {
            target: Target::AppRingtone,
        });
        let picked = picker.take().expect("the answer did not arrive");
        assert_eq!(
            picked,
            Picked::Cancelled {
                target: Target::AppRingtone
            }
        );
    }

    #[test]
    fn an_account_pick_carries_the_account_it_was_for() {
        let mut picker = Picker::new();
        let target = Target::AccountRingtone {
            account_id: "acct-1".to_string(),
        };
        picker.deliver(Picked::File {
            target: target.clone(),
            path: PathBuf::from("office.wav"),
        });
        let picked = picker.take().expect("the answer did not arrive");
        match picked {
            Picked::File { target: got, path } => {
                assert_eq!(got, target);
                assert_eq!(path, PathBuf::from("office.wav"));
            }
            other => panic!("the wrong answer came back: {other:?}"),
        }
    }

    #[test]
    fn a_file_that_is_not_a_wav_is_refused() {
        // A file that is not a WAV at all.
        let path = std::env::temp_dir().join("qsoftphone-picker-not-a-wav.bin");
        std::fs::write(&path, b"this is not a RIFF file").expect("could not write the file");
        let error = playable(&path).expect_err("the file was accepted");
        assert!(
            error.contains("not a WAV"),
            "the reason should say what is wrong: {error}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_file_that_is_there_but_cannot_be_read_is_refused() {
        let path = std::env::temp_dir().join("qsoftphone-picker-missing.wav");
        let _ = std::fs::remove_file(&path);
        assert!(playable(&path).is_err(), "a missing file was accepted");
    }

    #[test]
    fn a_wav_file_is_accepted() {
        // The built-in tone is a real WAV; the media crate writes one
        // out, and the ringtone reader has to take it back.
        let path = std::env::temp_dir().join("qsoftphone-picker-good.wav");
        let samples: Vec<i16> = (0..800).map(|i| (i * 100) as i16).collect();
        std::fs::write(&path, wav(&samples, 8000, 1)).expect("could not write the WAV");
        assert!(playable(&path).is_ok(), "a good WAV was refused");
        let _ = std::fs::remove_file(&path);
    }

    /// A WAV file with the given samples.
    ///
    /// The same shape the media crate's own tests write: 16-bit PCM,
    /// which is the only thing the ringtone reader takes.
    fn wav(samples: &[i16], rate: u32, channels: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&((36 + samples.len() * 2) as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        bytes.extend_from_slice(&(channels * 2).to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&((samples.len() * 2) as u32).to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }
}
