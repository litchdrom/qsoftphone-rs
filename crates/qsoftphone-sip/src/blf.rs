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
use rsipstack::sip::{Header, HeadersExt, Method, Request, Response, Uri};
use rsipstack::transaction::key::{TransactionKey, TransactionRole};
use rsipstack::transaction::transaction::Transaction;
use tracing::{info, warn};

use qsoftphone_core::model::account::BlfEntry;
use qsoftphone_core::sip::blf::{self, BlfState, Tracker};

use crate::call::{aor_for, aor_of, credential_of};
use crate::digest;
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
    /// The extension the lamp is about.
    ///
    /// The extension that is **watched** — not necessarily the number
    /// that is dialled for it.
    pub extension: String,
    /// The address of record the subscription is for.
    pub aor: String,
    /// The number that is dialled when the lamp is pressed.
    ///
    /// Often **not** the extension itself: on many dialplans the
    /// extension `101` is reached at `00101`, or through a queue at
    /// `200`, or through a feature code at `*300`.
    pub dial: String,
    /// What the lamp says.
    pub state: BlfState,
}

/// The subscription of one watched extension.
///
/// It lives in the session, because the `NOTIFY` that updates it arrives
/// on the session's endpoint and has to be matched to it.
#[derive(Debug, Clone)]
pub struct BlfSubscription {
    /// The extension the lamp is about.
    pub extension: String,
    /// The address of record we subscribed for.
    pub aor: String,
    /// The number that is dialled when the lamp is pressed.
    pub dial: String,
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
            dial: self.dial.clone(),
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
    /// `entry` has **two** values: the extension to watch, and the
    /// number to dial for it. The subscription is for the first; the
    /// second is what pressing the lamp does, and it never goes on the
    /// wire.
    ///
    /// The subscription is registered **before** the SUBSCRIBE goes
    /// out: the server is allowed to send the first `NOTIFY` before the
    /// `202 Accepted` has been read, and a lamp that missed it would
    /// show the wrong thing.
    pub fn watch(&self, account_id: &str, entry: &BlfEntry) -> Result<(), BlfError> {
        let session = self
            .sessions
            .get(account_id)
            .ok_or(BlfError::NotRegistered)?;

        let extension = entry.watch.trim().to_string();
        // The address of record carries the **port**: a registrar on
        // `193.178.169.164:5161` is not on `193.178.169.164:5060`, and
        // a SUBSCRIBE that goes to the wrong port never comes back.
        let aor = aor_for(&extension, &session.account);

        let request = subscribe_request(&session, &aor, 1, EXPIRES_SECONDS, None, None, None)?;
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
            extension: extension.clone(),
            aor: aor.clone(),
            dial: entry.dial_target().to_string(),
            call_id: call_id.clone(),
            from_tag: from_tag.clone(),
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
        let spec = SubscribeSpec {
            aor: aor.clone(),
            cseq: 1,
            expires: EXPIRES_SECONDS,
            call_id: Some(call_id.clone()),
            from_tag: Some(from_tag),
            to_tag: None,
        };
        self.handle.spawn(async move {
            // The request goes out with the Call-ID the subscription was
            // registered under: the `NOTIFY` comes back on it. A request
            // built with another Call-ID would leave the lamp deaf.
            match send_subscribe(&session_for_task, &spec).await {
                Ok(cseq) => session_for_task.set_blf_cseq(&call_id, cseq),
                Err(e) => {
                    warn!(error = %e, "the Busy Lamp Field subscription could not be sent");
                }
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

    /// What one extension's lamp says.
    ///
    /// `Unavailable` when nothing has been said about it — which is not
    /// the same as "free", and is what an extension whose subscription
    /// has not been established says.
    pub fn state_of(&self, account_id: &str, extension: &str) -> BlfState {
        self.lamps(account_id)
            .into_iter()
            .find(|lamp| lamp.extension == extension)
            .map(|lamp| lamp.state)
            .unwrap_or(BlfState::Unavailable)
    }
}

/// The pieces of a SUBSCRIBE.
///
/// They are kept apart from the request so that the request can be
/// built **again** — with a new CSeq and a new branch — when the server
/// answers with a challenge. The dialog does not change: the Call-ID
/// and the two tags are the same.
#[derive(Clone, Debug)]
struct SubscribeSpec {
    /// The address of record that is watched.
    aor: String,
    /// The CSeq of the request.
    cseq: u32,
    /// How long the subscription is asked for.
    expires: u32,
    /// The Call-ID of the subscription.
    call_id: Option<String>,
    /// The tag on our `From`.
    from_tag: Option<String>,
    /// The tag on the server's `To`.
    to_tag: Option<String>,
}

/// Send the SUBSCRIBE and wait for the answer.
///
/// The `202 Accepted` is what says the subscription is established. The
/// server's `To` tag comes out of it: the refresh is an **in-dialog**
/// SUBSCRIBE (RFC 6665 §4), and an in-dialog request has to carry the
/// tags of the dialog it belongs to.
///
/// A server that wants credentials answers `401 Unauthorized` first
/// (RFC 3261 §8.1.3.2): the challenge is answered with the account's
/// credentials, and the request goes out again — the **same** dialog,
/// with a new CSeq and a new branch. Asterisk does exactly this, and a
/// SUBSCRIBE that is not answered twice is a subscription that is never
/// made.
///
/// The CSeq that was actually used comes back, because a request that
/// was sent twice used two of them.
async fn send_subscribe(
    session: &Arc<AccountSession>,
    spec: &SubscribeSpec,
) -> Result<u32, BlfError> {
    let request = subscribe_request(
        session,
        &spec.aor,
        spec.cseq,
        spec.expires,
        spec.call_id.clone(),
        spec.from_tag.clone(),
        spec.to_tag.clone(),
    )?;

    let first = send_once(session, &request).await?;
    let (response, cseq) = match digest::challenge_of(&first) {
        None => (first, spec.cseq),
        Some((challenge, proxy)) => {
            let credential = credential_of(&session.account).ok_or_else(|| {
                BlfError::Sip("the account has no credentials to answer with".to_string())
            })?;
            let authorization = digest::authorization_header(
                challenge,
                &request.method,
                &request.uri.to_string(),
                &credential.username,
                &credential.password,
                proxy,
            )
            .ok_or_else(|| {
                BlfError::Sip("the digest challenge could not be answered".to_string())
            })?;

            // The retry: the same dialog, the credentials, a new CSeq
            // and a new branch. The CSeq goes up because a request that
            // is sent again is a new request (RFC 3261 §8.1.3.2).
            let retry = SubscribeSpec {
                cseq: spec.cseq + 1,
                call_id: Some(
                    request
                        .call_id_header()
                        .map_err(|e| BlfError::Sip(e.to_string()))?
                        .value()
                        .to_string(),
                ),
                from_tag: Some(
                    request
                        .from_header()
                        .map_err(|e| BlfError::Sip(e.to_string()))?
                        .tag()
                        .map_err(|e| BlfError::Sip(e.to_string()))?
                        .map(|tag| tag.value().to_string())
                        .unwrap_or_default(),
                ),
                ..spec.clone()
            };
            let mut retry_request = subscribe_request(
                session,
                &retry.aor,
                retry.cseq,
                retry.expires,
                retry.call_id.clone(),
                retry.from_tag.clone(),
                retry.to_tag.clone(),
            )?;
            retry_request.headers.push(authorization);

            info!("the server asked for credentials: the subscription is sent again");
            (send_once(session, &retry_request).await?, retry.cseq)
        }
    };

    // The answer. A `202 Accepted` means the subscription is there; a
    // `4xx` or `5xx` means the server will not say anything, and the
    // lamp stays dark.
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
        return Ok(cseq);
    }
    warn!(code, "the server refused the Busy Lamp Field subscription");
    Err(BlfError::Sip(format!("the server answered {code}")))
}

/// Send one request and wait for its answer.
async fn send_once(session: &Arc<AccountSession>, request: &Request) -> Result<Response, BlfError> {
    let key = TransactionKey::from_request(request, TransactionRole::Client)
        .map_err(|e| BlfError::Sip(e.to_string()))?;
    let mut transaction =
        Transaction::new_client(key, request.clone(), session.endpoint.inner.clone(), None);
    transaction
        .send()
        .await
        .map_err(|e| BlfError::Sip(e.to_string()))?;

    while let Some(message) = transaction.receive().await {
        if let rsipstack::sip::SipMessage::Response(response) = message {
            return Ok(response);
        }
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
        let spec = SubscribeSpec {
            aor: aor.clone(),
            cseq: cseq + 1,
            expires: EXPIRES_SECONDS,
            call_id: Some(call_id.clone()),
            from_tag: Some(from_tag),
            to_tag: Some(to_tag),
        };
        match send_subscribe(&session, &spec).await {
            Ok(cseq) => {
                // The CSeq that was really used: a refresh that was
                // challenged used two of them.
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
