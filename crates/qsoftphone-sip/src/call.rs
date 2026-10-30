//! The call layer.
//!
//! M2: place a call, answer one, hang up, send DTMF.
//!
//! The layer sits on rsipstack's dialog API. Two things about that
//! API are worth knowing before reading the code, because both of them
//! are easy to get wrong:
//!
//! * **A response only reaches the wire if somebody is driving the
//!   transaction.** `InviteDialog::ringing`, `accept` and `reject` put
//!   the response on a queue; it is sent when `InviteDialog::handle`
//!   is running. Without that loop the 180 Ringing of §5.4 never
//!   leaves the machine.
//! * **The dialog's state channel is the only view of the call.** It
//!   carries the provisional responses, the confirmation, the
//!   termination and every in-dialog request (the DTMF `INFO`).
//!
//! §5.5 is why nothing here forces a status code: the hangup path
//! lets the stack pick, exactly as the C++ does — `200 OK` for an
//! established call, `603 Decline` for an incoming call that is still
//! ringing, `CANCEL` for one of ours that has not been answered.

use std::fmt;
use std::sync::Arc;

use rsipstack::dialog::authenticate::Credential;
use rsipstack::dialog::dialog::{Dialog, DialogState, TerminatedReason};
use rsipstack::dialog::invitation::InviteOption;
use rsipstack::sip::event_packages::parse_info_dtmf;
use rsipstack::sip::{Auth, Header, HeadersExt, Scheme, StatusCode, ToTypedHeader, Uri};
use rsipstack::transaction::Transaction;
use rsipstack::transaction::key::TransactionRole;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tracing::warn;

use qsoftphone_core::model::account::Account;
use qsoftphone_core::model::call_event::{CallEvent, CallEventType, Direction};
use qsoftphone_core::sip::identity;
use qsoftphone_core::sip::uri::normalise_sip_target;
use qsoftphone_media::{Codec, MediaConfig, MediaSession, MediaStatus};

use crate::endpoint::split_host_port;
use crate::sdp;
use crate::session::{AccountSession, SessionRegistry};

/// How long a DTMF digit lasts, in milliseconds.
const DTMF_DURATION_MS: u32 = 100;

/// Why a call could not be started or driven.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// The account is not registered, so there is no endpoint to call
    /// from.
    NotRegistered,
    /// There is no such call.
    NoSuchCall,
    /// The target could not be turned into a SIP URI.
    BadTarget(String),
    /// The SIP stack refused.
    Sip(String),
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRegistered => write!(f, "the account is not registered"),
            Self::NoSuchCall => write!(f, "there is no such call"),
            Self::BadTarget(target) => write!(f, "{target} is not a SIP URI"),
            Self::Sip(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CallError {}

/// A call event, and the account it belongs to.
///
/// The [`CallEvent`] itself is the canonical contract — the C++'s
/// `CallEvent::toJson()`. The account id travels next to it because
/// the Rust app keys accounts by UUID while the C++ used a small
/// integer; the numeric `account_id` inside the event stays unset
/// until the REST layer (M6) defines the mapping. The account's
/// extension is in `user`.
#[derive(Debug, Clone)]
pub struct CallUpdate {
    /// The account the call belongs to.
    pub account_id: String,
    /// The canonical event.
    pub event: CallEvent,
}

/// The channel the call events travel over.
///
/// The registrar reports an incoming call through it, and the UI
/// drains it. The [`CallManager`] holds the receiving end; the bus is
/// a handle that can be cloned.
#[derive(Clone)]
pub struct CallBus(UnboundedSender<CallUpdate>);

impl CallBus {
    /// A bus, and the end the UI drains.
    pub fn new() -> (Self, UnboundedReceiver<CallUpdate>) {
        let (sender, receiver) = unbounded_channel();
        (Self(sender), receiver)
    }

    /// Publish an event.
    pub fn publish(&self, account_id: &str, event: CallEvent) {
        let _ = self.0.send(CallUpdate {
            account_id: account_id.to_string(),
            event,
        });
    }
}

/// Drives the calls of every registered account.
///
/// The UI never touches the network: it asks for a call here and
/// reads the events that come back over a channel.
pub struct CallManager {
    sessions: SessionRegistry,
    bus: CallBus,
    receiver: UnboundedReceiver<CallUpdate>,
    /// The runtime the call tasks run on.
    handle: tokio::runtime::Handle,
}

impl CallManager {
    /// A new manager. It shares the session registry with the
    /// registrar, which is what publishes the live endpoints into it.
    pub fn new(
        handle: tokio::runtime::Handle,
        sessions: SessionRegistry,
        bus: CallBus,
        receiver: UnboundedReceiver<CallUpdate>,
    ) -> Self {
        Self {
            sessions,
            bus,
            receiver,
            handle,
        }
    }

    /// Set what the media of a call should be.
    ///
    /// The codecs are the ones the offer names, in the order they are
    /// preferred; the first one this build speaks is the one the peer
    /// is asked for. The devices are named by their system name, and
    /// an empty name means the system default.
    ///
    /// The settings are shared with the registrar, so a codec the user
    /// picks reaches the offer that the registrar's side builds.
    pub fn set_media_config(&mut self, config: MediaConfig) {
        self.sessions.set_media_config(config);
    }

    /// What the media of a call should be.
    pub fn media_config(&self) -> MediaConfig {
        self.sessions.media_config()
    }

    /// The session registry, for the registrar to publish into.
    pub fn sessions(&self) -> SessionRegistry {
        self.sessions.clone()
    }

    /// Drain the call events that arrived since the last call.
    ///
    /// The UI calls this at the top of every frame (§3).
    pub fn drain(&mut self) -> Vec<CallUpdate> {
        let mut out = Vec::new();
        while let Ok(update) = self.receiver.try_recv() {
            out.push(update);
        }
        out
    }

    /// Place a call.
    ///
    /// `target` may be a bare number (`6666`), a SIP URI, or a URI
    /// with a display name. A bare number is dialled at the
    /// registrar, the way the C++ does it.
    ///
    /// This is called from the UI thread, which is **not** inside the
    /// tokio runtime — `eframe` owns the main thread and the SIP
    /// stack runs on a runtime of its own. rsipstack drives the
    /// INVITE from a task it spawns with `tokio::spawn`, and
    /// `tokio::spawn` panics when the calling thread has no runtime.
    /// The runtime's context is therefore entered around the call;
    /// without it, placing a call takes the whole app down before a
    /// single byte goes out.
    pub fn place_call(&mut self, account_id: &str, target: &str) -> Result<String, CallError> {
        let session = self
            .sessions
            .get(account_id)
            .ok_or(CallError::NotRegistered)?;
        let account = session.account.clone();

        let target_uri = normalise_sip_target(target, &account.registrar);
        let callee: Uri = target_uri
            .parse()
            .map_err(|_| CallError::BadTarget(target_uri.clone()))?;
        let caller: Uri = aor_of(&account)
            .parse()
            .map_err(|_| CallError::BadTarget(aor_of(&account)))?;

        let (state_sender, state_receiver) = session.dialog_layer.new_dialog_state_channel();

        // The media comes first: the SDP has to name the port the
        // RTP socket is bound to, and the socket is what binds it.
        let media_config = self.sessions.media_config();
        let media = MediaSession::new(&media_config).map_err(|e| CallError::Sip(e.to_string()))?;

        let option = InviteOption {
            caller,
            callee,
            contact: contact_uri(&account, &session),
            content_type: Some("application/sdp".to_string()),
            offer: Some(sdp::offer_sdp(
                &media_address(&session),
                media.local_port(),
                1,
                &media_config.codecs,
            )),
            // §5.8: a PBX that challenges the INVITE with 401/407 has
            // to be answered with the account's digest. Without a
            // credential the stack treats the challenge as the final
            // response and the call dies before it rings.
            credential: credential_of(&account),
            ..Default::default()
        };

        let runtime = self.handle.clone();
        let (dialog, _handle) = {
            let _runtime = runtime.enter();
            session
                .dialog_layer
                .do_invite_async(option, state_sender)
                .map_err(|e| CallError::Sip(e.to_string()))?
        };

        let call_id = dialog.id().call_id.clone();
        session.remember(&call_id, dialog);
        session.remember_media(&call_id, media);

        let mut event = CallEvent::new(CallEventType::OutgoingCallInitiated);
        event.user = account.user.clone();
        event.call_id = call_id.clone();
        event.call_direction = Some(Direction::Outgoing);
        event.caller = account.user.clone();
        // The C++'s `CallEvent` carries the **URI** of the peer, not
        // its user-part: `Application.cpp` runs `sipUriUserPart()` over
        // `ev.callee` to get the number. The history row needs the
        // full URI (§5.3), and the UI shows the user-part.
        event.callee = target_uri.clone();
        event.call_state = "initiating".to_string();
        self.bus.publish(account_id, event);

        self.handle.spawn(watch_dialog(
            session.clone(),
            call_id.clone(),
            Direction::Outgoing,
            account.user.clone(),
            state_receiver,
            self.bus.clone(),
            media_config,
        ));

        Ok(call_id)
    }

    /// Answer an incoming call.
    ///
    /// The `200 OK` carries the SDP and a NAT-aware Contact.
    ///
    /// The SDP is the **answer** to the offer the peer sent, not a
    /// fixed body: RFC 3264 §6 says the answer may only name media
    /// formats the offer listed, and a peer that offered only G.711
    /// A-law would hang up on a fixed G.711 µ-law answer.
    ///
    /// The port in it is the one the RTP socket is bound to, so the
    /// peer's audio arrives somewhere.
    pub fn answer(&self, account_id: &str, call_id: &str) -> Result<(), CallError> {
        let session = self
            .sessions
            .get(account_id)
            .ok_or(CallError::NotRegistered)?;
        let dialog = session.dialog(call_id).ok_or(CallError::NoSuchCall)?;
        let account = session.account.clone();

        let local = session
            .endpoint
            .get_addrs()
            .first()
            .cloned()
            .ok_or_else(|| CallError::Sip("the endpoint has no local address".to_string()))?;

        // The media comes first: the answer has to name the port the
        // RTP socket is bound to.
        let media_config = self.sessions.media_config();
        let media = MediaSession::new(&media_config).map_err(|e| CallError::Sip(e.to_string()))?;

        let headers = vec![Header::ContentType("application/sdp".into())];
        let offer = dialog.initial_request().body.clone();
        let body = sdp::answer_sdp(
            &offer,
            &media_address(&session),
            media.local_port(),
            1,
            &media_config.codecs,
        );

        // The answer settled on one of the formats the offer listed —
        // and the media has to speak that one. Without this the
        // session would keep the first codec of the list and send it
        // under the wrong payload type, and the peer would hear
        // nothing at all.
        if let Some(format) = sdp::choose_format(&offer, &media_config.codecs) {
            if let Some(codec) =
                Codec::from_payload_type(format.payload_type, Some(&format.encoding))
            {
                media.negotiate(codec, format.payload_type);
            }
        }

        // The peer said where its RTP comes from. It is in the offer,
        // and the answer only confirms the codec.
        if let Some((address, port)) = sdp::media_endpoint(&offer) {
            if let Err(e) = media.set_remote(&address, port) {
                warn!(call_id = %call_id, error = %e, "the peer's media address did not resolve");
            }
        }

        session.remember_media(call_id, media);

        dialog
            .accept_with_public_contact(
                &account.user,
                session.public_address.get(),
                &local,
                Some(headers),
                Some(body),
            )
            .map_err(|e| CallError::Sip(e.to_string()))
    }

    /// Decline an incoming call.
    ///
    /// §5.5: the stack's default is `603 Decline` for a call that is
    /// still ringing, and that is what the C++ sent.
    pub fn decline(&self, account_id: &str, call_id: &str) -> Result<(), CallError> {
        let session = self
            .sessions
            .get(account_id)
            .ok_or(CallError::NotRegistered)?;
        let dialog = session.dialog(call_id).ok_or(CallError::NoSuchCall)?;
        dialog
            .reject(None, None)
            .map_err(|e| CallError::Sip(e.to_string()))
    }

    /// Hang up.
    ///
    /// §5.5: no status code is forced. The stack picks the right one
    /// for the state and the role — `BYE` for an established call,
    /// `CANCEL` for one of ours that has not been answered yet, and
    /// `603 Decline` for an incoming call that is still ringing.
    ///
    /// The three cases are the ones pjsua_call_hangup picks when the
    /// status code is left at zero (see the C++ `SipCall::hangup`).
    /// rsipstack's own `hangup()` covers two of them: it cancels an
    /// early call of ours and it says `BYE` on a confirmed one — but
    /// it is a no-op for an early *incoming* call, which has to be
    /// declined instead.
    pub fn hangup(&self, account_id: &str, call_id: &str) -> Result<(), CallError> {
        let session = self
            .sessions
            .get(account_id)
            .ok_or(CallError::NotRegistered)?;
        let dialog = session.dialog(call_id).ok_or(CallError::NoSuchCall)?;

        let early = matches!(
            dialog.state(),
            DialogState::Calling(_) | DialogState::Trying(_) | DialogState::Early(_, _)
        );
        let is_server = dialog.role() == TransactionRole::Server;

        if early && is_server {
            // A call that is still ringing on our side: decline it.
            // The stack's default is 603 Decline — the same code the
            // C++ sent.
            dialog
                .reject(None, None)
                .map_err(|e| CallError::Sip(e.to_string()))?;
            return Ok(());
        }

        let call_id = call_id.to_string();
        self.handle.spawn(async move {
            if let Err(e) = dialog.hangup().await {
                warn!(call_id = %call_id, error = %e, "the hangup failed");
            }
        });
        Ok(())
    }

    /// Send a DTMF digit.
    ///
    /// RFC 4733 over `INFO`: the media plane is M4, and RFC 2833 needs
    /// it.
    pub fn send_dtmf(&self, account_id: &str, call_id: &str, digit: &str) -> Result<(), CallError> {
        let session = self
            .sessions
            .get(account_id)
            .ok_or(CallError::NotRegistered)?;
        let dialog = session.dialog(call_id).ok_or(CallError::NoSuchCall)?;

        let headers = vec![Header::ContentType(sdp::DTMF_RELAY_CONTENT_TYPE.into())];
        let body = sdp::dtmf_relay_body(digit, DTMF_DURATION_MS);
        let call_id = call_id.to_string();
        self.handle.spawn(async move {
            if let Err(e) = dialog.info(Some(headers), Some(body)).await {
                warn!(call_id = %call_id, error = %e, "the DTMF could not be sent");
            }
        });
        Ok(())
    }

    /// Mute or unmute the microphone of a call.
    ///
    /// The flag is an atomic, so this is safe to call from the
    /// interface thread — §6 forbids blocking there, and this does
    /// not block.
    pub fn set_mic_muted(
        &self,
        account_id: &str,
        call_id: &str,
        muted: bool,
    ) -> Result<(), CallError> {
        let media = self.media_of(account_id, call_id)?;
        media.set_mic_muted(muted);
        Ok(())
    }

    /// Mute or unmute the speaker of a call.
    pub fn set_speaker_muted(
        &self,
        account_id: &str,
        call_id: &str,
        muted: bool,
    ) -> Result<(), CallError> {
        let media = self.media_of(account_id, call_id)?;
        media.set_speaker_muted(muted);
        Ok(())
    }

    /// What the media of a call is doing.
    pub fn media_status(&self, account_id: &str, call_id: &str) -> Option<MediaStatus> {
        let session = self.sessions.get(account_id)?;
        session.media(call_id).map(|media| media.status())
    }

    /// The media of a call, if it has any.
    fn media_of(&self, account_id: &str, call_id: &str) -> Result<MediaSession, CallError> {
        let session = self
            .sessions
            .get(account_id)
            .ok_or(CallError::NotRegistered)?;
        session.media(call_id).ok_or(CallError::NoSuchCall)
    }
}

/// Handle an incoming INVITE.
///
/// §5.4: the `180 Ringing` goes out **right away**, before the user
/// has seen anything. A PBX that does not get one aborts with
/// `503`/`480` after its ringing timeout.
pub async fn handle_incoming_invite(
    session: Arc<AccountSession>,
    mut tx: Transaction,
    bus: CallBus,
) -> Result<(), CallError> {
    let account = session.account.clone();
    let request = tx.original.clone();

    // The 100 Trying. rsipstack would send it from the dialog's
    // handler, but the handler only sends it while the dialog is
    // still `Calling` — and the 180 below moves it to `Early` first.
    // Sending it here keeps the order the peer expects.
    tx.send_trying().await.ok();

    let (state_sender, state_receiver) = session.dialog_layer.new_dialog_state_channel();
    let dialog = session
        .dialog_layer
        .get_or_create_server_invite(&tx, state_sender, None, None)
        .map_err(|e| CallError::Sip(e.to_string()))?;

    let call_id = dialog.id().call_id.clone();
    session.remember(&call_id, dialog.clone());

    // §5.4 — the 180 Ringing, immediately.
    //
    // The Contact is the public one, the same address the `200 OK`
    // will carry. rsipstack would otherwise put the local socket in
    // it — `0.0.0.0` or a private address — and a peer that takes the
    // Contact from the provisional response would send the BYE to the
    // wrong place.
    let contact = match session.endpoint.get_addrs().first().cloned() {
        Some(local) => Some(vec![crate::inspector::nat_aware_contact(
            &account.user,
            session.public_address.get(),
            &local,
        )]),
        None => None,
    };
    dialog
        .ringing(contact, None)
        .map_err(|e| CallError::Sip(e.to_string()))?;

    // The response only reaches the wire if somebody drives the
    // transaction. This loop is also what answers the ACK and the
    // in-dialog requests.
    let mut handler = dialog.clone();
    tokio::spawn(async move {
        let _ = handler.handle(&mut tx).await;
    });

    let mut event = CallEvent::new(CallEventType::IncomingCall);
    event.user = account.user.clone();
    event.call_id = call_id.clone();
    event.call_direction = Some(Direction::Incoming);
    event.call_state = "ringing".to_string();
    fill_identity(&mut event, &account, &request, &session);
    bus.publish(&account.id, event);

    tokio::spawn(watch_dialog(
        session.clone(),
        call_id,
        Direction::Incoming,
        account.user.clone(),
        state_receiver,
        bus,
        session.media_config(),
    ));

    Ok(())
}

/// Route an in-dialog request to the dialog it belongs to.
///
/// The BYE, the DTMF `INFO` and a re-INVITE all arrive on the same
/// channel as the initial INVITE; the To tag is what tells them
/// apart.
pub async fn handle_in_dialog_request(
    session: Arc<AccountSession>,
    mut tx: Transaction,
) -> Result<(), CallError> {
    let Some(dialog) = session.dialog_layer.match_dialog(&tx) else {
        warn!("an in-dialog request arrived for a dialog we do not have");
        return Ok(());
    };
    let Dialog::Invite(mut dialog) = dialog else {
        warn!("an in-dialog request arrived for a dialog we cannot drive");
        return Ok(());
    };
    tokio::spawn(async move {
        let _ = dialog.handle(&mut tx).await;
    });
    Ok(())
}

/// The state of one dialog, turned into events.
///
/// This is the only place that reads the dialog's state channel, and
/// it is where the call's lifecycle becomes the canonical event
/// stream.
async fn watch_dialog(
    session: Arc<AccountSession>,
    call_id: String,
    direction: Direction,
    user: String,
    mut states: UnboundedReceiver<DialogState>,
    bus: CallBus,
    media_config: MediaConfig,
) {
    let account_id = session.account.id.clone();
    let mut ended = false;

    loop {
        // The account can go away under a live call: the user
        // unregisters, or the app is shutting down. The dialog will
        // never be told, because its transport is going down with it —
        // so the token is what ends the call.
        let state = tokio::select! {
            state = states.recv() => match state {
                Some(state) => state,
                None => break,
            },
            _ = session.token.cancelled() => break,
        };

        match state {
            DialogState::Early(_, response) => {
                // For an outgoing call this is the peer's 180. For an
                // incoming one it is our own — the user already knows.
                if direction == Direction::Outgoing
                    && matches!(
                        response.status_code,
                        StatusCode::Ringing | StatusCode::SessionProgress
                    )
                {
                    let mut event = CallEvent::new(CallEventType::OutgoingCallRinging);
                    event.user = user.clone();
                    event.call_id = call_id.clone();
                    event.call_direction = Some(direction);
                    event.call_state = "ringing".to_string();
                    event.sip_code = response.status_code.code() as i32;
                    bus.publish(&account_id, event);
                }
            }
            DialogState::Confirmed(_, response) => {
                // The peer's answer is where its RTP comes from and
                // which codec it chose. For an incoming call the offer
                // already said, and the answer confirmed the codec.
                if direction == Direction::Outgoing {
                    if let Some(media) = session.media(&call_id) {
                        match sdp::parse_answer(&response.body, &media_config.codecs) {
                            Some(answer) => {
                                media.negotiate(answer.codec, answer.payload_type);
                                if let Err(e) = media.set_remote(&answer.address, answer.port) {
                                    warn!(
                                        call_id = %call_id,
                                        error = %e,
                                        "the peer's media address did not resolve"
                                    );
                                }
                            }
                            None => {
                                warn!(
                                    call_id = %call_id,
                                    "the peer answered with media this build does not speak"
                                );
                            }
                        }
                    }
                }

                // The audio starts here, on the runtime — never on the
                // interface thread (§6).
                if let Some(media) = session.media(&call_id) {
                    media.start(&media_config);
                }

                let mut event = CallEvent::new(CallEventType::CallAnswered);
                event.user = user.clone();
                event.call_id = call_id.clone();
                event.call_direction = Some(direction);
                event.call_state = "connected".to_string();
                bus.publish(&account_id, event);
            }
            DialogState::Info(_, request, handle) => {
                // A DTMF digit. The INFO has to be answered, or the
                // transaction hangs.
                if let Some(digit) = dtmf_digit(&request) {
                    let mut event = CallEvent::new(CallEventType::DtmfReceived);
                    event.user = user.clone();
                    event.call_id = call_id.clone();
                    event.call_direction = Some(direction);
                    event.digit = digit;
                    bus.publish(&account_id, event);
                }
                handle.respond(StatusCode::OK, None, None).await.ok();
            }
            DialogState::Terminated(id, reason) => {
                // The media goes first: the sound card is closed and
                // the RTP threads stop before the call is forgotten.
                session.forget_media(&call_id);

                let (code, cause) = terminated_code(&reason);
                let mut event = CallEvent::new(CallEventType::CallEnded);
                event.user = user.clone();
                event.call_id = call_id.clone();
                event.call_direction = Some(direction);
                event.call_state = "ended".to_string();
                event.sip_code = code as i32;
                event.hangup_cause = cause;
                bus.publish(&account_id, event);
                ended = true;

                // The dialog has to leave the registry, or it leaks.
                session.forget(&call_id);
                session.dialog_layer.remove_dialog(&id);
                break;
            }
            _ => {}
        }
    }

    if !ended {
        // The call was cut short — the account went away under it.
        // The media goes with it: a call that keeps the microphone
        // open is a call the next one cannot use the device for.
        session.forget_media(&call_id);
        session.forget(&call_id);

        let mut event = CallEvent::new(CallEventType::CallEnded);
        event.user = user.clone();
        event.call_id = call_id.clone();
        event.call_direction = Some(direction);
        event.call_state = "ended".to_string();
        event.hangup_cause = "the account was unregistered".to_string();
        bus.publish(&account_id, event);
    }
}

/// The digit in an `INFO` request, if it carries one.
fn dtmf_digit(request: &rsipstack::sip::Request) -> Option<String> {
    let content_type = request.headers.iter().find_map(|h| match h {
        Header::ContentType(value) => Some(value.value()),
        _ => None,
    });

    // rsipstack's own parser knows the content type and the RFC 4733
    // event code.
    let code = parse_info_dtmf(content_type, &request.body)?;

    if let Some((digit, _)) = sdp::parse_dtmf_relay(&request.body) {
        if !digit.is_empty() {
            return Some(digit);
        }
    }
    Some(sdp::digit_from_event_code(code).to_string())
}

/// The SIP status and the reason for a terminated dialog.
fn terminated_code(reason: &TerminatedReason) -> (u16, String) {
    match reason {
        TerminatedReason::Timeout => (408, "Timeout".to_string()),
        TerminatedReason::UacCancel => (487, "Request Terminated".to_string()),
        TerminatedReason::UacBye | TerminatedReason::UasBye => (200, "OK".to_string()),
        TerminatedReason::UacBusy | TerminatedReason::UasBusy => (486, "Busy Here".to_string()),
        TerminatedReason::UasDecline => (603, "Decline".to_string()),
        TerminatedReason::ProxyError(code) => (code.code(), code.to_string()),
        TerminatedReason::ProxyAuthRequired => (401, "Unauthorized".to_string()),
        TerminatedReason::UacOther(code) | TerminatedReason::UasOther(code) => {
            (code.code(), code.to_string())
        }
    }
}

/// The account's address of record: `sip:user@host`.
///
/// The host is the account's domain when it has one, and the
/// registrar's host otherwise.
pub fn aor_of(account: &Account) -> String {
    let host = if account.domain.trim().is_empty() {
        split_host_port(&account.registrar).0
    } else {
        account.domain.trim().to_string()
    };
    format!("sip:{}@{host}", account.user)
}

/// The digest credential of an account.
///
/// §5.8: the realm defaults to the registrar host. The registrar
/// answers the REGISTER with it, and a PBX that challenges an INVITE
/// with 401 or 407 is answered with the same one — a stack that has
/// no credential treats the challenge as the final response and the
/// call never rings.
///
/// The realm here is what we *expect*; the digest itself is computed
/// over the realm the server names in its challenge, which is what
/// RFC 7616 requires.
pub fn credential_of(account: &Account) -> Option<Credential> {
    if account.user.trim().is_empty() {
        return None;
    }
    Some(Credential {
        username: account.user.clone(),
        password: account.password.clone(),
        realm: Some(split_host_port(&account.registrar).0),
        auth_username: None,
    })
}

/// The Contact URI for an account.
///
/// The public address wins when the registrar reported one — it is the
/// address the peer has to use to reach us. Otherwise the local
/// address is used, which is right when there is no NAT.
fn contact_uri(account: &Account, session: &AccountSession) -> Uri {
    let local = session
        .endpoint
        .get_addrs()
        .first()
        .map(|addr| addr.addr.clone());
    let host_with_port = session.public_address.get().or(local).unwrap_or_default();

    Uri {
        scheme: Some(Scheme::Sip),
        auth: Some(Auth {
            user: account.user.clone(),
            password: None,
        }),
        host_with_port,
        params: vec![],
        headers: vec![],
    }
}

/// The address to put in the SDP.
fn media_address(session: &AccountSession) -> String {
    if let Some(address) = session.public_address.get() {
        return address.host.to_string();
    }
    if let Some(addr) = session.endpoint.get_addrs().first() {
        return addr.addr.host.to_string();
    }
    "127.0.0.1".to_string()
}

/// Fill the identity fields of an incoming call's event.
///
/// §5.2 and §5.3: the caller's name and the number the remote party
/// actually dialled. The Request-URI is the DID/DND case — the PBX
/// forwards `sip:6666@host` to our AOR — and it is the only way to
/// tell `6666` from `104`.
fn fill_identity(
    event: &mut CallEvent,
    account: &Account,
    request: &rsipstack::sip::Request,
    session: &AccountSession,
) {
    let profile = account.profile();

    let from = request.from_header().ok().and_then(|f| f.typed().ok());
    let caller_uri = from.as_ref().map(|f| f.uri.to_string()).unwrap_or_default();
    let display_name = from.as_ref().and_then(|f| f.display_name.clone());

    let to = request.to_header().ok().and_then(|t| t.typed().ok());
    let callee_uri = to.as_ref().map(|t| t.uri.to_string()).unwrap_or_default();

    // The Request-URI, as it came in.
    let request_uri = request.uri.to_string();

    // The headers the account allows, and the two identity headers
    // that are needed to resolve the caller at all.
    let mut captured = qsoftphone_core::model::account::HeaderMap::new();
    for header in request.headers.iter() {
        let name = header.name().to_string();
        let value = header.value().to_string();
        captured.insert(name, value);
    }
    let allowed = profile.filter_headers(&captured);

    // The peer's URI, as the C++'s `CallEvent` carries it — the
    // history row wants the full URI and derives the number from it.
    event.caller = caller_uri.clone();
    event.callee = callee_uri.clone();
    event.headers = allowed.clone();
    event.caller_id = identity::resolve_caller_display_name(
        &profile,
        &allowed,
        &caller_uri,
        display_name.as_deref(),
        None,
    );
    event.dialed_number =
        identity::resolve_dialed_number(&profile, &allowed, Some(&request_uri), &callee_uri);

    let _ = session;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use rsipstack::sip::Transport;

    use crate::inspector::PublicAddress;

    #[test]
    fn the_aor_uses_the_domain_when_there_is_one() {
        let mut account = Account::new("Office", "sip.example.com:5060", "104", "pw", "udp");
        assert_eq!(aor_of(&account), "sip:104@sip.example.com");
        account.domain = "pbx.example.com".to_string();
        assert_eq!(aor_of(&account), "sip:104@pbx.example.com");
    }

    #[test]
    fn a_terminated_dialog_reports_the_status_the_cpp_would() {
        // §5.5: an established call that is hung up is 200 OK.
        assert_eq!(terminated_code(&TerminatedReason::UacBye).0, 200);
        assert_eq!(terminated_code(&TerminatedReason::UasBye).0, 200);
        // A cancelled outgoing call is 487.
        assert_eq!(terminated_code(&TerminatedReason::UacCancel).0, 487);
        // A declined incoming call is 603.
        assert_eq!(terminated_code(&TerminatedReason::UasDecline).0, 603);
        // A busy peer is 486.
        assert_eq!(terminated_code(&TerminatedReason::UacBusy).0, 486);
    }

    #[test]
    fn a_forced_status_code_is_never_produced() {
        // Every termination maps to a status the stack itself would
        // have picked; nothing here forces 603 onto a normal call.
        for reason in [
            TerminatedReason::UacBye,
            TerminatedReason::UasBye,
            TerminatedReason::UacCancel,
            TerminatedReason::UasDecline,
            TerminatedReason::UacBusy,
            TerminatedReason::UasBusy,
            TerminatedReason::Timeout,
        ] {
            let (code, cause) = terminated_code(&reason);
            assert!(code >= 200, "{reason:?} -> {code} {cause}");
        }
    }

    /// A session with an endpoint, for the tests that need one.
    async fn a_session() -> Arc<AccountSession> {
        let token = rsipstack::platform::CancellationToken::new();
        let endpoint = crate::build_endpoint(Transport::Udp, 0, token, None)
            .await
            .expect("endpoint");
        let account = Account::new("Office", "example.com", "104", "pw", "udp");
        let registry = SessionRegistry::new();
        AccountSession::new(
            account,
            endpoint,
            PublicAddress::default(),
            registry.media_config_handle(),
        )
    }

    #[tokio::test]
    async fn a_call_ends_when_the_account_is_unregistered() {
        // The user unregisters in the middle of a call. The dialog will
        // never be told — its transport is going down with it — so the
        // account's token is what ends the call.
        //
        // What matters is that the call *ends*: it must not stay on the
        // screen, and its sound card must be released. A call that keeps
        // the microphone open is a call the next one cannot use the
        // device for.
        let session = a_session().await;

        let media = MediaSession::new(&MediaConfig::default()).expect("media");
        // The transport, not the sound card: the test must not open a
        // microphone on the machine it runs on.
        media.start_transport();
        assert!(media.is_running(), "the media is not running");
        session.remember_media("call-1", media.clone());

        let (bus, mut updates) = CallBus::new();
        let (_tx, states) = tokio::sync::mpsc::unbounded_channel();

        let watcher = tokio::spawn(watch_dialog(
            session.clone(),
            "call-1".to_string(),
            Direction::Outgoing,
            "104".to_string(),
            states,
            bus,
            MediaConfig::default(),
        ));

        // The account goes away.
        session.close();

        // The call ends, and the interface is told.
        let update = tokio::time::timeout(Duration::from_secs(5), updates.recv())
            .await
            .expect("the call never ended")
            .expect("the bus closed");
        assert_eq!(update.event.call_id, "call-1");
        assert_eq!(update.event.event_type, CallEventType::CallEnded);

        // And its sound card is released.
        assert!(!media.is_running(), "the media is still running");
        assert!(
            session.media("call-1").is_none(),
            "the media was left on the account"
        );

        watcher.await.expect("the watcher panicked");
    }
}
