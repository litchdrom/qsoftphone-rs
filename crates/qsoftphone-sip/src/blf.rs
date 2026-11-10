//! The Busy Lamp Field — the lamps that say whether an extension is
//! free, busy or ringing.
//!
//! On Asterisk the lamp is built on RFC 3903. The watcher **subscribes**
//! to `dialog-info+simulimit` for an address of record, and the server
//! answers with a `dialog-info+xml` document that lists that address's
//! dialogs. Every time the state changes the server sends another
//! `NOTIFY`.
//!
//! rsipstack 0.7.9 has no out-of-dialog SUBSCRIBE of its own, so the
//! request is built here: [`rsipstack::transaction::endpoint::EndpointInner`]
//! makes the request, and a client transaction carries it.
//!
//! The document itself is parsed in the core
//! ([`qsoftphone_core::sip::blf`]) — it is a pure function over strings,
//! and §6 is where it belongs.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use rsipstack::sip::headers::{Accept, Contact, Event, Expires, SubscriptionState};
use rsipstack::sip::{Header, HeadersExt, Method, Request, Uri};
use rsipstack::transaction::key::{TransactionKey, TransactionRole};
use rsipstack::transaction::transaction::Transaction;
use tracing::{info, warn};

use qsoftphone_core::sip::blf::{self, BlfState, Tracker};

use crate::call::aor_of;
use crate::session::{AccountSession, SessionRegistry};

/// The event package a Busy Lamp Field subscription asks for.
///
/// `dialog-info+simulimit` is the package PJSUA's own BLF uses, and the
/// one Asterisk's `res_pjsip_pubsub` serves: it says what the
/// **address of record** is doing, not what each of its devices is
/// doing separately.
pub const EVENT_PACKAGE: &str = "dialog-info+simulimit";

/// The content type the document comes in.
pub const EVENT_CONTENT_TYPE: &str = "application/dialog-info+xml";

/// How long a subscription lasts, and how long before it is refreshed.
///
/// The refresh is a SUBSCRIBE with a longer life; the server would
/// otherwise stop sending the state and the lamp would go dark while
/// the extension is still on a call.
const EXPIRES_SECONDS: u32 = 3600;

/// How long before the expiry the subscription is refreshed.
const REFRESH_AFTER: Duration = Duration::from_secs(3000);

/// Why a lamp could not be lit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlfError {
    /// The account is not registered, so there is no endpoint to
    /// subscribe on.
    NotRegistered,
    /// The address of record is not a SIP URI.
    BadTarget(String),
    /// The stack refused.
    Sip(String),
}

impl fmt::Display for BlfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlfError::NotRegistered => write!(f, "the account is not registered"),
            BlfError::BadTarget(target) => write!(f, "{target} is not a SIP URI"),
            BlfError::Sip(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BlfError {}

/// One lamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlfLamp {
    /// The extension, as the user knows it — `105`.
    pub extension: String,
    /// The address of record the subscription is for.
    pub aor: String,
    /// What the lamp says.
    pub state: BlfState,
}

/// The subscription of one watched extension.
///
/// It lives in the session, because the `NOTIFY` that updates it arrives
/// on the session's endpoint and has to be matched to it.
#[derive(Debug, Clone)]
pub struct BlfSubscription {
    /// The extension, as the user knows it.
    pub extension: String,
    /// The address of record we subscribed for.
    pub aor: String,
    /// The Call-ID of the SUBSCRIBE.
    ///
    /// The `NOTIFY`s carry it, and it is how the two are matched.
    pub call_id: String,
    /// The tag we put on our `From`.
    pub from_tag: String,
    /// The tag the server put on its `To`, once it has answered.
    pub to_tag: String,
    /// The CSeq of the last SUBSCRIBE we sent.
    pub cseq: u32,
    /// What is known about the dialogs of that address.
    pub tracker: Tracker,
}

impl BlfSubscription {
    /// The lamp, as the panel shows it.
    pub fn lamp(&self) -> BlfLamp {
        BlfLamp {
            extension: self.extension.clone(),
            aor: self.aor.clone(),
            state: self.tracker.state(),
        }
    }
}

/// The Busy Lamp Field of every registered account.
///
/// The UI never touches the network: it asks for a lamp here, and reads
/// the state back from the session.
pub struct BlfManager {
    sessions: SessionRegistry,
    /// The runtime the subscription tasks run on.
    handle: tokio::runtime::Handle,
}

impl BlfManager {
    /// A manager for the given sessions.
    pub fn new(sessions: SessionRegistry, handle: tokio::runtime::Handle) -> BlfManager {
        BlfManager { sessions, handle }
    }

    /// Watch an extension.
    ///
    /// `aor` is the address of record to subscribe for — `sip:105@pbx`
    /// for extension 105 on that server. The subscription is registered
    /// **before** the SUBSCRIBE goes out: the server is allowed to send
    /// the first `NOTIFY` before the `202 Accepted` has been read, and
    /// a lamp that missed it would show the wrong thing.
    pub fn watch(&self, account_id: &str, extension: &str, aor: &str) -> Result<(), BlfError> {
        let session = self
            .sessions
            .get(account_id)
            .ok_or(BlfError::NotRegistered)?;

        let request = subscribe_request(&session, aor, 1, EXPIRES_SECONDS, None, None, None)?;
        let call_id = request
            .call_id_header()
            .map_err(|e| BlfError::Sip(e.to_string()))?
            .value()
            .to_string();
        let from_tag = request
            .from_header()
            .map_err(|e| BlfError::Sip(e.to_string()))?
            .tag()
            .map_err(|e| BlfError::Sip(e.to_string()))?
            .map(|tag| tag.value().to_string())
            .unwrap_or_default();

        session.remember_blf(BlfSubscription {
            extension: extension.to_string(),
            aor: aor.to_string(),
            call_id: call_id.clone(),
            from_tag,
            to_tag: String::new(),
            cseq: 1,
            tracker: Tracker::new(),
        });

        // The SUBSCRIBE goes out on the runtime: the endpoint lives
        // there, and the interface thread is not on it (§3).
        //
        // The task is spawned **onto** that runtime, so it runs in its
        // context — which is what the stack's own `tokio::spawn` calls
        // need. There is no `enter()` guard here: it is not `Send`, and
        // a guard held across an await would take the task with it.
        let session_for_task = session.clone();
        let aor = aor.to_string();
        let extension = extension.to_string();
        self.handle.spawn(async move {
            // The **same** request goes out: the Call-ID it carries is
            // the one the subscription was registered under, and the
            // `NOTIFY` comes back on it. A request built twice would
            // carry two Call-IDs, and the lamp would never hear from
            // the server.
            if let Err(e) = send_subscribe(&session_for_task, request).await {
                warn!(error = %e, "the Busy Lamp Field subscription could not be sent");
            }
            refresh_loop(session_for_task, extension, aor).await;
        });

        Ok(())
    }

    /// Stop watching an extension.
    pub fn unwatch(&self, account_id: &str, extension: &str) {
        if let Some(session) = self.sessions.get(account_id) {
            session.forget_blf(extension);
        }
    }

    /// The lamps of an account.
    pub fn lamps(&self, account_id: &str) -> Vec<BlfLamp> {
        match self.sessions.get(account_id) {
            Some(session) => session.blf_lamps(),
            None => Vec::new(),
        }
    }
}

/// Send the SUBSCRIBE and wait for the answer.
///
/// The `202 Accepted` is what says the subscription is established. The
/// server's `To` tag comes out of it: the refresh is an **in-dialog**
/// SUBSCRIBE (RFC 6665 §4), and an in-dialog request has to carry the
/// tags of the dialog it belongs to.
async fn send_subscribe(session: &Arc<AccountSession>, request: Request) -> Result<(), BlfError> {
    let key = TransactionKey::from_request(&request, TransactionRole::Client)
        .map_err(|e| BlfError::Sip(e.to_string()))?;
    let mut transaction =
        Transaction::new_client(key, request, session.endpoint.inner.clone(), None);
    transaction
        .send()
        .await
        .map_err(|e| BlfError::Sip(e.to_string()))?;

    // The answer. A `202 Accepted` means the subscription is there; a
    // `4xx` or `5xx` means the server will not say anything, and the
    // lamp stays dark.
    while let Some(message) = transaction.receive().await {
        let rsipstack::sip::SipMessage::Response(response) = message else {
            continue;
        };
        let code = response.status_code.code();
        if code < 300 {
            // The server's tag, for the refresh.
            if let Ok(to) = response.to_header() {
                if let Ok(Some(tag)) = to.tag() {
                    if let Ok(call_id) = response.call_id_header() {
                        session.set_blf_to_tag(call_id.value(), tag.value());
                    }
                }
            }
            info!(code, "the Busy Lamp Field subscription was accepted");
            return Ok(());
        }
        warn!(code, "the server refused the Busy Lamp Field subscription");
        return Err(BlfError::Sip(format!("the server answered {code}")));
    }

    Err(BlfError::Sip(
        "the subscription was not answered".to_string(),
    ))
}

/// Keep the subscription alive.
///
/// A subscription that is not refreshed ends, and a lamp that has gone
/// dark says "free" when the truth is "we stopped listening" — which is
/// the one thing a lamp must never say.
async fn refresh_loop(session: Arc<AccountSession>, extension: String, aor: String) {
    loop {
        tokio::time::sleep(REFRESH_AFTER).await;

        // The refresh is an in-dialog SUBSCRIBE: the same Call-ID, the
        // same tags, a new CSeq (RFC 6665 §4).
        let Some((call_id, cseq, from_tag, to_tag)) = session.blf_refresh_of(&extension) else {
            // The extension is not watched any more.
            return;
        };
        let cseq = cseq + 1;
        let request = match subscribe_request(
            &session,
            &aor,
            cseq,
            EXPIRES_SECONDS,
            Some(call_id.clone()),
            Some(from_tag),
            Some(to_tag),
        ) {
            Ok(request) => request,
            Err(e) => {
                warn!(error = %e, "the Busy Lamp Field subscription could not be refreshed");
                return;
            }
        };
        match send_subscribe(&session, request).await {
            Ok(()) => {
                session.set_blf_cseq(&call_id, cseq);
                info!("the Busy Lamp Field subscription was refreshed");
            }
            Err(e) => {
                warn!(error = %e, "the Busy Lamp Field subscription could not be refreshed");
                return;
            }
        }
    }
}

/// Build the SUBSCRIBE that watches an address of record.
///
/// The request is built by the stack — it knows the Via, the User-Agent
/// and the branch — and the four headers that make it a subscription
/// are added here: `Event`, `Accept`, `Expires` and the `Contact` the
/// server has to send the `NOTIFY`s to.
///
/// `call_id` and `to_tag` are what make a request a **refresh** rather
/// than a new subscription: the same Call-ID, and the tag the server
/// put on its `To` — RFC 6665 §4.
fn subscribe_request(
    session: &Arc<AccountSession>,
    aor: &str,
    cseq: u32,
    expires: u32,
    call_id: Option<String>,
    from_tag: Option<String>,
    to_tag: Option<String>,
) -> Result<Request, BlfError> {
    let account = &session.account;
    let uri: Uri = aor
        .parse()
        .map_err(|_| BlfError::BadTarget(aor.to_string()))?;

    let caller: Uri = aor_of(account)
        .parse()
        .map_err(|_| BlfError::BadTarget(aor_of(account)))?;

    let via = session
        .endpoint
        .inner
        .get_via(None, None)
        .map_err(|e| BlfError::Sip(e.to_string()))?;

    // Our tag on the `From`. A refresh carries the **same** one: the
    // dialog is identified by the two tags, and a new one would make a
    // new dialog.
    let from = match from_tag {
        Some(tag) if !tag.is_empty() => rsipstack::sip::typed::From {
            display_name: None,
            uri: caller,
            params: vec![rsipstack::sip::uri::Param::Tag(tag.into())],
        },
        _ => rsipstack::sip::typed::From {
            display_name: None,
            uri: caller,
            params: vec![],
        }
        .with_tag(rsipstack::transaction::make_tag()),
    };

    // The server's tag, when this is a refresh: an in-dialog request
    // carries the tags of the dialog it belongs to.
    let params = match to_tag {
        Some(tag) if !tag.is_empty() => vec![rsipstack::sip::uri::Param::Tag(tag.into())],
        _ => vec![],
    };
    let to = rsipstack::sip::typed::To {
        display_name: None,
        uri: uri.clone(),
        params,
    };

    let call_id = match call_id {
        Some(call_id) => Some(rsipstack::sip::headers::CallId::new(call_id)),
        None => Some(rsipstack::transaction::make_call_id(
            None,
            rsipstack::transaction::CallIdFormat::UuidWithSuffix,
        )),
    };

    let mut request =
        session
            .endpoint
            .inner
            .make_request(Method::Subscribe, uri, via, from, to, cseq, call_id);

    request
        .headers
        .push(Header::Event(Event::new(EVENT_PACKAGE)));
    request
        .headers
        .push(Header::Accept(Accept::new(EVENT_CONTENT_TYPE)));
    request
        .headers
        .push(Header::Expires(Expires::new(expires.to_string())));
    request.headers.push(Header::Contact(Contact::new(
        crate::call::contact_uri(account, session).to_string(),
    )));

    Ok(request)
}

/// Answer a `NOTIFY` and read the state out of it.
///
/// The `200 OK` goes out **first**: the server is waiting for it, and a
/// `NOTIFY` that is not answered is a subscription the server gives up
/// on. The document is read afterwards — the answer does not depend on
/// it.
pub async fn handle_notify(
    session: Arc<AccountSession>,
    mut transaction: Transaction,
) -> Result<(), BlfError> {
    let request = transaction.original.clone();
    let call_id = request
        .call_id_header()
        .map_err(|e| BlfError::Sip(e.to_string()))?
        .value()
        .to_string();

    let mut response = crate::incoming::make_response(&request, rsipstack::sip::StatusCode::OK);
    response
        .headers
        .push(Header::SubscriptionState(SubscriptionState::new(format!(
            "active;expires={EXPIRES_SECONDS}"
        ))));
    transaction
        .respond(response)
        .await
        .map_err(|e| BlfError::Sip(e.to_string()))?;

    let body = String::from_utf8_lossy(&request.body).to_string();
    let Some(info) = blf::parse(&body) else {
        warn!("the Busy Lamp Field document was not read");
        return Ok(());
    };

    let Some(state) = session.apply_blf(&call_id, &info) else {
        // A NOTIFY for a subscription we do not have: somebody else's
        // business, or one that has already ended.
        return Ok(());
    };
    info!(call_id, state = ?state, "the Busy Lamp Field state arrived");
    Ok(())
}
