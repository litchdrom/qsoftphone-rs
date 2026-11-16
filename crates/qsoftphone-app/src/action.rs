//! What a status change does.
//!
//! ## The button is the switch, and the action is what it is wired to
//!
//! An account's status — **Ready**, **Online**, **Pause** — is the
//! state the account is in. Setting it runs the account's action, and
//! the action is one of four kinds:
//!
//! | Kind | What happens |
//! |---|---|
//! | dial a code | DTMF if a call is up on the account, a call to the code otherwise |
//! | run a script | the command, as the user typed it |
//! | run an HTTP query | a `GET` of the URL |
//! | use a plugin | nothing yet — §8 puts the plugin host out of scope |
//!
//! The point of the action is that the *state* lives outside the
//! softphone: a user who is ready tells the PBX so with a feature
//! code, and a user who is online tells a CRM so with an HTTP query.
//!
//! ## Nothing waits for the interface
//!
//! The two kinds that take time — the script and the HTTP query — run
//! on a thread of their own. A script that takes ten seconds and a
//! server that does not answer are both things the user must not be
//! kept waiting for, and §3 of the brief says the interface thread
//! never waits for anything. What they say arrives on a channel and is
//! read at the top of the frame.
//!
//! The two kinds that do not wait — the code and the plugin — are the
//! app's own business: a code needs a call, and a plugin is not there
//! yet.

use std::process::Command;
use std::sync::mpsc::Sender;

use qsoftphone_core::model::status::StatusAction;

/// What an action says when it is done.
#[derive(Debug, Clone)]
pub struct ActionReport {
    /// The account the action ran for.
    pub account: String,
    /// What happened, in a sentence.
    pub result: Result<String, String>,
}

/// Run an action that takes time, on a thread of its own.
///
/// The thread is not joined and nothing waits for it: the report comes
/// back on the channel when it is ready, and the interface carries on
/// in the meantime.
pub fn spawn(action: &StatusAction, account: &str, sender: Sender<ActionReport>) {
    match action {
        StatusAction::Script(command) => {
            let command = command.clone();
            let account = account.to_string();
            std::thread::Builder::new()
                .name("qsoftphone-action".to_string())
                .spawn(move || {
                    let result = run_script(&command);
                    let _ = sender.send(ActionReport { account, result });
                })
                .ok();
        }
        StatusAction::Http(url) => {
            let url = url.clone();
            let account = account.to_string();
            std::thread::Builder::new()
                .name("qsoftphone-action".to_string())
                .spawn(move || {
                    let result = run_http(&url);
                    let _ = sender.send(ActionReport { account, result });
                })
                .ok();
        }
        // The code is the app's own business: it needs a call, and the
        // plugin is not there yet.
        StatusAction::None | StatusAction::Dial(_) | StatusAction::Plugin(_) => {}
    }
}

/// Run a command, the way the user typed it.
///
/// The command goes through the system's own shell, so that a path
/// with a space in it, a redirect or a pipeline works the way it does
/// in a console — a screen capture is usually a command with arguments
/// and a path in it, and splitting those by hand would be a worse
/// parser than the one the user already has.
fn run_script(command: &str) -> Result<String, String> {
    let command = command.trim();
    if command.is_empty() {
        return Err("there is no command to run.".to_string());
    }

    let mut process = if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", command])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    } else {
        Command::new("sh")
            .args(["-c", command])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    }
    .map_err(|e| format!("the command could not be started: {e}"))?;

    let status = process
        .wait()
        .map_err(|e| format!("the command could not be waited for: {e}"))?;
    if status.success() {
        Ok("the command ran.".to_string())
    } else {
        Err(match status.code() {
            Some(code) => format!("the command failed: it exited with {code}."),
            None => "the command was stopped before it finished.".to_string(),
        })
    }
}

/// Fetch a URL.
fn run_http(url: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("there is no URL to fetch.".to_string());
    }

    match ureq::get(url).call() {
        Ok(response) => Ok(format!("the query answered {}.", response.status())),
        Err(ureq::Error::StatusCode(code)) => Err(format!("the query answered {code}.")),
        Err(e) => Err(format!("the query did not go through: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn a_script_that_runs_says_so() {
        // The command that does nothing at all is the one that has to
        // work on every machine.
        let command = if cfg!(target_os = "windows") {
            "cmd /C exit 0"
        } else {
            "true"
        };
        let result = run_script(command);
        assert!(result.is_ok(), "the command did not run: {result:?}");
    }

    #[test]
    fn a_script_that_fails_says_why() {
        let command = if cfg!(target_os = "windows") {
            "cmd /C exit 3"
        } else {
            "exit 3"
        };
        let result = run_script(command);
        assert!(
            result.is_err(),
            "the failing command was reported as a success"
        );
        let error = result.unwrap_err();
        assert!(
            error.contains("3"),
            "the error does not say the code: {error}"
        );
    }

    #[test]
    fn an_empty_command_is_not_run() {
        let result = run_script("   ");
        assert!(result.is_err());
    }

    #[test]
    fn the_script_runs_on_a_thread_of_its_own() {
        // The report comes back on the channel, and the caller waits
        // for it there — not by standing on the interface thread.
        let (sender, receiver) = channel();
        spawn(
            &StatusAction::Script("echo hello".to_string()),
            "Office",
            sender,
        );
        let report = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the script reported back");
        assert_eq!(report.account, "Office");
        assert!(report.result.is_ok(), "{:?}", report.result);
    }

    #[test]
    fn an_http_query_to_nothing_says_so() {
        // A URL that cannot be reached is an error, and the error is
        // what the user is told.
        let result = run_http("http://127.0.0.1:1/nothing");
        assert!(result.is_err(), "the query to nothing succeeded");
    }

    #[test]
    fn an_empty_url_is_not_fetched() {
        let result = run_http("  ");
        assert!(result.is_err());
    }
}
