//! The REGISTER flow.
//!
//! M1: register, keep registered, unregister, and report the state.
//!
//! The refresh loop is written here rather than using
//! `Registration::keep_alive`, because the loop needs the *granted*
//! expiry — the registrar may grant less than we asked for — and
//! `keep_alive`'s callback cannot reach back into the registration it
//! is driving.
//!
//! §5.8: the digest realm defaults to the registrar host. rsipstack
//! takes the realm from the server's challenge, which is what RFC
//! 7616 requires; the credential's realm records what we expect, so
//! a registrar that moves realms is visible in the log.

use std::collections::HashMap;
use std::net::SocketAddr;

use rsipstack::dialog::registration::{Registration, backoff_interval, refresh_interval};
use rsipstack::platform::CancellationToken;
use rsipstack::sip::{StatusCodeKind, Transport, Uri};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use tracing::{Instrument, info, warn};

use qsoftphone_core::logging::account_span;
use qsoftphone_core::model::account::Account;

use crate::call::{CallBus, credential_of};
use crate::endpoint::{build_endpoint, parse_transport, split_host_port, transport_param};
use crate::inspector::SessionInspector;
use crate::session::{AccountSession, SessionRegistry};
use crate::state::{RegistrationEvent, RegistrationState};

/// How long we ask the registrar to keep the binding, in seconds.
pub const DEFAULT_EXPIRES: u32 = 300;

/// The REGISTER Request-URI: the registrar's domain (RFC 3261 §10.2).
///
/// The port is only spelled out when the account gives one, so the
/// string the digest is computed over stays as close to what the
/// operator wrote as possible. TLS is the exception: its default port
/// is 5061, not 5060.
pub fn registrar_uri(registrar: &str, transport: Transport) -> Option<Uri> {
    let (host, port) = split_host_port(registrar);
    if host.is_empty() {
        return None;
    }

    let mut uri = format!("sip:{host}");
    let port = match (port, transport) {
        (Some(port), _) => Some(port),
        (None, Transport::Tls) => Some(5061),
        (None, _) => None,
    };
    if let Some(port) = port {
        uri.push_str(&format!(":{port}"));
    }
    if let Some(param) = transport_param(transport) {
        uri.push_str(&format!(";transport={param}"));
    }
    Uri::try_from(uri).ok()
}

/// A running registration.
pub struct RegistrationTask {
    /// Cancelling this stops the loop and unregisters.
    token: CancellationToken,
    /// The task doing the work.
    handle: JoinHandle<()>,
}

impl RegistrationTask {
    /// Stop the registration and unregister from the registrar.
    pub async fn stop(self) {
        self.token.cancel();
        let _ = self.handle.await;
    }
}

/// Keeps a set of accounts registered.
///
/// The UI never touches the network: it starts and stops tasks here,
/// and reads the state that comes back over a channel.
pub struct RegistrationManager {
    tasks: HashMap<String, RegistrationTask>,
    sender: UnboundedSender<RegistrationEvent>,
    receiver: UnboundedReceiver<RegistrationEvent>,
    /// Where the live endpoints are published, so the call layer can
    /// place a call from a registered account.
    sessions: SessionRegistry,
    /// Where the incoming calls are reported.
    calls: CallBus,
    /// The runtime the registration tasks run on.
    ///
    /// The UI thread is not inside the runtime, so tasks have to be
    /// spawned through its handle.
    handle: tokio::runtime::Handle,
}

impl RegistrationManager {
    pub fn new(handle: tokio::runtime::Handle, sessions: SessionRegistry, calls: CallBus) -> Self {
        let (sender, receiver) = unbounded_channel();
        Self {
            tasks: HashMap::new(),
            sender,
            receiver,
            sessions,
            calls,
            handle,
        }
    }

    /// Start registering an account. Does nothing if it is already
    /// registered.
    pub fn start(&mut self, account: &Account, port: u16) {
        if self.tasks.contains_key(&account.id) {
            return;
        }

        let token = CancellationToken::new();
        let child = token.clone();
        let sender = self.sender.clone();
        let sessions = self.sessions.clone();
        let calls = self.calls.clone();
        let account_id = account.id.clone();
        let account = account.clone();

        let handle = self.handle.spawn(async move {
            // §6: the account id has to be on every log line this task
            // writes, so the whole task runs inside the span. The span
            // is made on the runtime thread — the UI thread does no
            // work beyond handing the task over (§3).
            let span = account_span(&account);
            run_registration(account, port, sender, child, sessions, calls)
                .instrument(span)
                .await
        });

        self.tasks
            .insert(account_id.clone(), RegistrationTask { token, handle });
    }

    /// Stop registering an account, unregistering first.
    ///
    /// The shutdown is asynchronous: the UI thread must not wait for
    /// the network.
    pub fn stop(&mut self, account_id: &str) {
        if let Some(task) = self.tasks.remove(account_id) {
            self.handle.spawn(async move {
                task.stop().await;
            });
        }
    }

    /// Stop everything.
    pub fn stop_all(&mut self) {
        let ids: Vec<String> = self.tasks.keys().cloned().collect();
        for id in ids {
            self.stop(&id);
        }
    }

    /// Drain the state changes that arrived since the last call.
    ///
    /// The UI calls this at the top of every frame (§3).
    pub fn drain(&mut self) -> Vec<RegistrationEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.receiver.try_recv() {
            out.push(event);
        }
        out
    }
}

fn send(sender: &UnboundedSender<RegistrationEvent>, account: &Account, state: RegistrationState) {
    let _ = sender.send(RegistrationEvent {
        account_id: account.id.clone(),
        user: account.user.clone(),
        state,
    });
}

/// Cancels the endpoint's token when the registration task ends.
///
/// The transport must not outlive the task, whichever way the task
/// ends.
struct EndpointGuard(CancellationToken);

impl Drop for EndpointGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn run_registration(
    account: Account,
    port: u16,
    sender: UnboundedSender<RegistrationEvent>,
    token: CancellationToken,
    sessions: SessionRegistry,
    calls: CallBus,
) {
    // The endpoint gets a token of its own. Cancelling the task token
    // would take the transport down with it, and the final
    // `Expires: 0` REGISTER still needs a live transport to go out —
    // otherwise the unregister races the shutdown and never lands.
    let endpoint_token = CancellationToken::new();
    let _guard = EndpointGuard(endpoint_token.clone());

    run_registration_inner(
        account,
        port,
        sender,
        token,
        endpoint_token,
        sessions,
        calls,
    )
    .await;
}

async fn run_registration_inner(
    account: Account,
    port: u16,
    sender: UnboundedSender<RegistrationEvent>,
    token: CancellationToken,
    endpoint_token: CancellationToken,
    sessions: SessionRegistry,
    calls: CallBus,
) {
    let transport = parse_transport(&account.transport);

    // The inspector does two jobs: it patches the RFC 7616 algorithm
    // spelling, and it notes the public address the registrar reports
    // — the address the Contact of an INVITE has to name.
    let inspector = SessionInspector::new();
    let public_address = inspector.public_address();

    let endpoint = match build_endpoint(
        transport,
        port,
        endpoint_token.clone(),
        Some(Box::new(inspector)),
    )
    .await
    {
        Ok(endpoint) => endpoint,
        Err(e) => {
            warn!(error = %e, "could not open the transport");
            send(
                &sender,
                &account,
                RegistrationState::Failed {
                    reason: e.to_string(),
                },
            );
            return;
        }
    };

    // The endpoint has to be running before anything is sent.
    let inner = endpoint.inner.clone();
    tokio::spawn(async move {
        let _ = inner.serve().await;
    });

    // The address we are listening on. The Contact the registrar binds
    // is derived from it, unless the registrar reports a different
    // public address (received/rport) — so this is the first thing to
    // look at when the server cannot reach us.
    info!(addrs = ?endpoint.get_addrs(), "the transport is open");

    // The session: the endpoint plus the dialog layer. It is
    // published so the call layer can place a call from this account.
    let session = AccountSession::new(
        account.clone(),
        endpoint,
        public_address,
        sessions.media_config_handle(),
    );
    sessions.insert(&account.id, session.clone());

    // Answer what the registrar sends us, and take the calls that
    // arrive. Asterisk pings a registered endpoint with OPTIONS, and
    // a UA that stays silent is marked unreachable — the registration
    // looks fine and the calls fail.
    match session.endpoint.incoming_transactions() {
        Ok(incoming) => {
            tokio::spawn(crate::incoming::serve(
                incoming,
                endpoint_token.clone(),
                session.clone(),
                calls,
            ));
        }
        Err(e) => {
            warn!(error = %e, "the incoming channel was already taken");
        }
    }

    let Some(uri) = registrar_uri(&account.registrar, transport) else {
        send(
            &sender,
            &account,
            RegistrationState::Failed {
                reason: format!("`{}` is not a usable registrar", account.registrar),
            },
        );
        return;
    };

    if !account.domain.trim().is_empty() {
        // The SIP domain is a separate setting from the registrar host
        // (§5.8) and is only applied once the domain can differ — the
        // brief puts that in M6. Until then the registrar host is used
        // for both, which is the same on a single-tenant PBX.
        warn!(
            domain = %account.domain,
            "the SIP domain is not applied yet; the registrar host is used for the From/URI"
        );
    }

    let credential = credential_of(&account);

    let mut registration = Registration::new(session.endpoint.inner.clone(), credential);

    if let Some(proxy) = parse_outbound_proxy(&account.outbound_proxy) {
        registration.outbound_proxy = Some(proxy);
    }

    send(&sender, &account, RegistrationState::Registering);

    let mut failures: u32 = 0;
    loop {
        match registration
            .register(uri.clone(), Some(DEFAULT_EXPIRES))
            .await
        {
            Ok(response) if response.status_code.kind() == StatusCodeKind::Successful => {
                failures = 0;
                let expires = registration.expires();
                info!(expires, "registered");
                send(&sender, &account, RegistrationState::Registered { expires });
            }
            Ok(response) => {
                failures += 1;
                let status: u16 = response.status_code.into();
                let reason = response.wire_reason.clone().unwrap_or_default();
                warn!(status, reason, "the registrar rejected the registration");
                send(
                    &sender,
                    &account,
                    RegistrationState::Rejected { status, reason },
                );
            }
            Err(e) => {
                failures += 1;
                warn!(error = %e, "the registration attempt failed");
                send(
                    &sender,
                    &account,
                    RegistrationState::Failed {
                        reason: e.to_string(),
                    },
                );
            }
        }

        let wait = if failures == 0 {
            refresh_interval(DEFAULT_EXPIRES)
        } else {
            backoff_interval(failures - 1)
        };

        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = token.cancelled() => break,
        }
    }

    // The final expires=0 REGISTER is ours to send: the task owns the
    // lifecycle, so the registrar must not keep the binding.
    match registration.register(uri, Some(0)).await {
        Ok(response) => info!(status = response.status_code.code(), "unregistered"),
        Err(e) => warn!(error = %e, "the unregister failed"),
    }

    send(&sender, &account, RegistrationState::Idle);
}

/// Parse the account's outbound proxy.
///
/// It is a bare `host:port` — the address the packets are sent to,
/// while the registrar's domain stays in the SIP headers.
fn parse_outbound_proxy(value: &str) -> Option<SocketAddr> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let (host, port) = split_host_port(value);
    if host.is_empty() {
        return None;
    }
    let port = port?;
    let host = host.trim_matches(['[', ']']);
    format!("{host}:{port}").parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registrar_uri_is_the_registrar_domain() {
        let uri = registrar_uri("example.com:5060", Transport::Udp).unwrap();
        assert_eq!(uri.to_string(), "sip:example.com:5060");
    }

    #[test]
    fn a_bare_host_gets_no_port() {
        let uri = registrar_uri("example.com", Transport::Udp).unwrap();
        assert_eq!(uri.to_string(), "sip:example.com");
    }

    #[test]
    fn tls_gets_the_tls_default_port() {
        let uri = registrar_uri("example.com", Transport::Tls).unwrap();
        let rendered = uri.to_string();
        assert!(rendered.starts_with("sip:example.com:5061"), "{rendered}");
        // rsipstack normalises the transport parameter to upper case
        // on the wire.
        assert!(
            rendered.to_ascii_lowercase().contains("transport=tls"),
            "{rendered}"
        );
    }

    #[test]
    fn tcp_carries_the_transport_parameter() {
        let uri = registrar_uri("example.com:5060", Transport::Tcp).unwrap();
        let rendered = uri.to_string();
        assert!(
            rendered.to_ascii_lowercase().contains("transport=tcp"),
            "{rendered}"
        );
    }

    #[test]
    fn an_empty_registrar_is_rejected() {
        assert!(registrar_uri("", Transport::Udp).is_none());
        assert!(registrar_uri("   ", Transport::Udp).is_none());
    }

    #[test]
    fn the_outbound_proxy_is_parsed() {
        assert_eq!(
            parse_outbound_proxy("10.0.0.1:5080"),
            Some("10.0.0.1:5080".parse().unwrap())
        );
        assert_eq!(parse_outbound_proxy(""), None);
        assert_eq!(parse_outbound_proxy("example.com"), None);
    }
}
