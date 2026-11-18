//! What a status change does.
//!
//! ## The button is the switch, and the hook is what it is wired to
//!
//! A status — **Ready**, **Online**, **Pause**, or one of the user's
//! own — is the state the account is in. Setting it runs the **hook**
//! that belongs to the status, and the hook is one of four kinds:
//!
//! | Kind | What happens |
//! |---|---|
//! | dial a code | DTMF if a call is up on the account, a call to the code otherwise |
//! | run a script | the command, as the user typed it |
//! | run an HTTP query | the URL, with the method and the body the user chose |
//! | use a plugin | nothing yet — §8 puts the plugin host out of scope |
//!
//! The point of the hook is that the *state* lives outside the
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
use std::time::Duration;

use qsoftphone_core::model::status::{HttpMethod, StatusAction};
use qsoftphone_core::wake::Wake;

/// How long the closing of the app waits for a script.
///
/// A script that stops a screen capture has to have finished before
/// the process goes — otherwise the capture is left running with
/// nobody watching it. A hook that hangs must not keep the window from
/// closing either, so the wait has an end.
pub const LEAVE_DEADLINE: Duration = Duration::from_secs(2);

/// What an action says when it is done.
#[derive(Debug, Clone)]
pub struct ActionReport {
    /// The account the action ran for.
    pub account: String,
    /// What happened, in a sentence.
    pub result: Result<String, String>,
}

/// The way back to the app.
///
/// The report goes on the channel, and the window is asked to be drawn
/// again: a script that took thirty seconds has something to say, and
/// the status line has to say it without waiting for the next poll.
#[derive(Clone)]
pub struct Reporter {
    sender: Sender<ActionReport>,
    wake: Wake,
}

impl Reporter {
    /// A reporter for the given channel.
    pub fn new(sender: Sender<ActionReport>, wake: Wake) -> Self {
        Self { sender, wake }
    }

    /// Say what happened.
    pub fn report(&self, account: String, result: Result<String, String>) {
        let _ = self.sender.send(ActionReport { account, result });
        self.wake.wake();
    }
}

/// Run an action that takes time, on a thread of its own.
///
/// The thread is not joined and nothing waits for it: the report comes
/// back on the channel when it is ready, and the interface carries on
/// in the meantime.
pub fn spawn(action: &StatusAction, account: &str, reporter: Reporter) {
    match action {
        StatusAction::Script(command) => {
            let command = command.clone();
            let account = account.to_string();
            std::thread::Builder::new()
                .name("qsoftphone-action".to_string())
                .spawn(move || {
                    let result = run_script(&command);
                    reporter.report(account, result);
                })
                .ok();
        }
        StatusAction::Http { method, url, body } => {
            let method = *method;
            let url = url.clone();
            let body = body.clone();
            let account = account.to_string();
            std::thread::Builder::new()
                .name("qsoftphone-action".to_string())
                .spawn(move || {
                    let result = run_http(method, &url, &body);
                    reporter.report(account, result);
                })
                .ok();
        }
        // The code is the app's own business: it needs a call, and the
        // plugin is not there yet.
        StatusAction::None | StatusAction::Dial(_) | StatusAction::Plugin(_) => {}
    }
}

/// Run a command on a thread of its own, and say what it did.
///
/// This is the seam for the user's **own** commands: the capture that
/// starts when the agent becomes ready, and the one that stops it.
/// The command is run the way the user typed it — through the system's
/// own shell, with the quotes they wrote in it left alone.
pub fn spawn_script(command: &str, account: &str, reporter: Reporter) {
    let command = command.trim().to_string();
    let account = account.to_string();
    std::thread::Builder::new()
        .name("qsoftphone-script".to_string())
        .spawn(move || {
            let result = run_script(&command);
            reporter.report(account, result);
        })
        .ok();
}

/// Run a command on a thread of its own and wait for it, up to a
/// deadline.
///
/// This is what the app does when it is closing: the capture has to
/// have stopped before the process goes — otherwise it is left running
/// with nobody watching it — and a command that hangs must not keep the
/// window from closing either, so the wait has an end.
///
/// The report is not sent anywhere: the app is on its way out, and
/// there is no status line left to say it on.
pub fn spawn_script_and_wait(command: &str, deadline: Duration) {
    let (sender, receiver) = std::sync::mpsc::channel();
    // Nobody is watching this one: the app is on its way out, and there
    // is no window left to wake.
    spawn_script(command, "", Reporter::new(sender, Wake::new()));
    // The thread sends its report when it is done, and the channel is
    // what says it is done.
    let _ = receiver.recv_timeout(deadline);
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

    let mut process = shell(command)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
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

/// The shell that runs the command, with the command in it.
///
/// On Windows the command is handed to `cmd` as **one line**, with the
/// quotes the user wrote in it left alone: the path of a script with
/// a space in it — `powershell -File "C:\Program Files\…"` — has to
/// survive, and the ordinary way of passing an argument would put a
/// backslash in front of every quote and make of the path another
/// name.
#[cfg(windows)]
fn shell(command: &str) -> Command {
    use std::os::windows::process::CommandExt;

    let mut builder = Command::new("cmd");
    builder.raw_arg("/C").raw_arg(command);
    builder
}

/// The shell that runs the command, with the command in it.
#[cfg(not(windows))]
fn shell(command: &str) -> Command {
    let mut builder = Command::new("sh");
    builder.arg("-c").arg(command);
    builder
}

/// Fetch a URL, with the method the user chose.
///
/// The method is not a detail: a GET that fetches is a GET that can be
/// repeated by anything that reads the log, and a CRM that is told to
/// change a call's state wants a PATCH. The body goes with the
/// methods that carry one — a GET and a DELETE carry none.
///
/// The answer is the status code and nothing else: a hook that reports
/// the body of the answer would put a CRM's answer on the status line
/// of a softphone.
fn run_http(method: HttpMethod, url: &str, body: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("there is no URL to fetch.".to_string());
    }
    let body = body.trim();

    let answer = match method {
        HttpMethod::Get => ureq::get(url).call().map(|response| response.status()),
        HttpMethod::Delete => ureq::delete(url).call().map(|response| response.status()),
        HttpMethod::Post => ureq::post(url).send(body).map(|response| response.status()),
        HttpMethod::Put => ureq::put(url).send(body).map(|response| response.status()),
        HttpMethod::Patch => ureq::patch(url)
            .send(body)
            .map(|response| response.status()),
    };

    match answer {
        Ok(code) => Ok(format!("the query answered {code}.")),
        Err(ureq::Error::StatusCode(code)) => Err(format!("the query answered {code}.")),
        Err(e) => Err(format!("the query did not go through: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::channel;

    /// A server that answers one request and says what it saw.
    ///
    /// It is a socket and a thread: a hook that fetches a URL is worth
    /// testing against something that answers, and a server of one's
    /// own is the only one that can be asked what method the request
    /// carried.
    fn one_request_server() -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the server binds");
        let port = listener
            .local_addr()
            .expect("the server has an address")
            .port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("the server accepts");
            // The head, then the body the head asks for.
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => head.push(byte[0]),
                }
            }
            let text = String::from_utf8_lossy(&head).to_string();
            let length: usize = text
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    if name.trim().eq_ignore_ascii_case("content-length") {
                        value.trim().parse().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let mut body = vec![0u8; length];
            if length > 0 {
                let _ = stream.read_exact(&mut body);
            }
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = stream.flush();
            format!("{text}{}", String::from_utf8_lossy(&body))
        });
        (format!("http://127.0.0.1:{port}/state"), handle)
    }

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
    fn a_quoted_path_in_the_command_survives() {
        // The path of a file with a space in it has to survive: a
        // screen capture is a command that names a file, and the file
        // is often under "Program Files" or "My Documents".
        //
        // The command goes to the shell as one line, so the quotes the
        // user wrote in it are the shell's own.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let folder = std::env::temp_dir().join(format!("qsoftphone action {stamp}"));
        std::fs::create_dir_all(&folder).expect("the folder is there");
        let marker = folder.join("marker.txt");

        let command = if cfg!(target_os = "windows") {
            format!("echo done>\"{}\"", marker.display())
        } else {
            format!("echo done > '{}'", marker.display())
        };
        let result = run_script(&command);
        assert!(result.is_ok(), "{result:?}");
        assert!(
            marker.exists(),
            "the command did not reach the file: {command}"
        );
    }

    #[test]
    fn the_script_runs_on_a_thread_of_its_own() {
        // The report comes back on the channel, and the caller waits
        // for it there — not by standing on the interface thread.
        let (sender, receiver) = channel();
        spawn(
            &StatusAction::Script("echo hello".to_string()),
            "Office",
            Reporter::new(sender, Wake::new()),
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
        let result = run_http(HttpMethod::Get, "http://127.0.0.1:1/nothing", "");
        assert!(result.is_err(), "the query to nothing succeeded");
    }

    #[test]
    fn an_empty_url_is_not_fetched() {
        let result = run_http(HttpMethod::Get, "  ", "");
        assert!(result.is_err());
    }

    #[test]
    fn a_get_is_a_get() {
        let (url, server) = one_request_server();
        let result = run_http(HttpMethod::Get, &url, "");
        let request = server.join().expect("the server is done");
        assert!(result.is_ok(), "{result:?}");
        assert!(
            request.starts_with("GET /state "),
            "the request was not a GET:\n{request}"
        );
    }

    #[test]
    fn a_post_carries_its_method_and_its_body() {
        // The user asked for this: a query that changes something is
        // not a GET, and it says what it changes.
        let (url, server) = one_request_server();
        let result = run_http(HttpMethod::Post, &url, r#"{"state":"online"}"#);
        let request = server.join().expect("the server is done");
        assert!(result.is_ok(), "{result:?}");
        assert!(
            request.starts_with("POST /state "),
            "the request was not a POST:\n{request}"
        );
        assert!(
            request.ends_with(r#"{"state":"online"}"#),
            "the body did not arrive:\n{request}"
        );
    }

    #[test]
    fn a_patch_is_a_patch() {
        let (url, server) = one_request_server();
        let result = run_http(HttpMethod::Patch, &url, "{\"state\":\"pause\"}");
        let request = server.join().expect("the server is done");
        assert!(result.is_ok(), "{result:?}");
        assert!(
            request.starts_with("PATCH /state "),
            "the request was not a PATCH:\n{request}"
        );
        assert!(
            request.ends_with("{\"state\":\"pause\"}"),
            "the body did not arrive:\n{request}"
        );
    }

    #[test]
    fn a_delete_carries_no_body() {
        let (url, server) = one_request_server();
        let result = run_http(HttpMethod::Delete, &url, "");
        let request = server.join().expect("the server is done");
        assert!(result.is_ok(), "{result:?}");
        assert!(
            request.starts_with("DELETE /state "),
            "the request was not a DELETE:\n{request}"
        );
    }

    #[test]
    fn an_http_hook_runs_on_a_thread_of_its_own() {
        let (url, server) = one_request_server();
        let (sender, receiver) = channel();
        spawn(
            &StatusAction::Http {
                method: HttpMethod::Post,
                url: url.clone(),
                body: "{}".to_string(),
            },
            "Office",
            Reporter::new(sender, Wake::new()),
        );
        let report = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the query reported back");
        assert_eq!(report.account, "Office");
        assert!(report.result.is_ok(), "{:?}", report.result);
        let _ = server.join();
    }
}
