//! One account's SIP session.
//!
//! A session is the endpoint (one socket, one Via branch space), the
//! dialog layer that lives on it, and the dialogs that are currently
//! up. The registrar creates it and publishes it in the
//! [`SessionRegistry`]; the call layer finds it there.
//!
//! The split matters: the endpoint has to outlive a single
//! registration — a registered account is what a call is placed from,
//! and it is what an incoming INVITE arrives on.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use rsipstack::dialog::dialog_layer::DialogLayer;
use rsipstack::dialog::invite_dialog::InviteDialog;
use rsipstack::platform::CancellationToken;
use rsipstack::transaction::endpoint::Endpoint;

use qsoftphone_core::model::account::Account;
use qsoftphone_media::{MediaConfig, MediaSession};

use crate::inspector::PublicAddress;

/// One account's SIP session.
pub struct AccountSession {
    /// The account this session belongs to.
    pub account: Account,
    /// The endpoint: the socket and the transaction layer.
    pub endpoint: Endpoint,
    /// The dialog layer. One per endpoint.
    ///
    /// It is an `Arc` because `do_invite_async` needs one: the
    /// outbound INVITE has to be driven on the runtime, and the
    /// dialog layer is what owns it.
    pub dialog_layer: Arc<DialogLayer>,
    /// The address the registrar sees us at.
    pub public_address: PublicAddress,
    /// Cancelled when the account goes away.
    ///
    /// The calls wait on it. A call whose transport has gone cannot be
    /// carried on, and it must not be left on the screen either.
    pub token: CancellationToken,
    /// The dialogs we can still act on, keyed by SIP Call-ID.
    dialogs: Mutex<HashMap<String, InviteDialog>>,
    /// The media of the calls that are up, keyed by SIP Call-ID.
    ///
    /// The session is a handle: it is cheap to clone, and the mute
    /// flags are atomics. That is what lets the interface read the
    /// status of a call while the call is still being set up on
    /// another thread.
    media: Mutex<HashMap<String, MediaSession>>,
    /// What the media of a call should be.
    ///
    /// The settings are global, as they were in the C++: the `media`
    /// section of the config file held the codec priorities for the
    /// whole application, not for one account.
    media_config: Arc<RwLock<MediaConfig>>,
}

impl AccountSession {
    /// A session for an account and the endpoint it owns.
    pub fn new(
        account: Account,
        endpoint: Endpoint,
        public_address: PublicAddress,
        media_config: Arc<RwLock<MediaConfig>>,
    ) -> Arc<Self> {
        let dialog_layer = Arc::new(DialogLayer::new(endpoint.inner.clone()));
        Arc::new(Self {
            account,
            endpoint,
            dialog_layer,
            public_address,
            token: CancellationToken::new(),
            dialogs: Mutex::new(HashMap::new()),
            media: Mutex::new(HashMap::new()),
            media_config,
        })
    }

    /// End every call of this account.
    ///
    /// The sound card is closed and the RTP threads stop. This has to
    /// happen when the account is unregistered: a call that is still
    /// open would keep the microphone and the speaker, and the **next**
    /// call would find the device already in use — which is a buffer
    /// overrun on the capture side and a corrupted microphone.
    ///
    /// The token goes with it, so the calls watching it end too.
    pub fn close(&self) {
        self.token.cancel();
        let mut media = self.media.lock().expect("media lock");
        for (_, session) in media.drain() {
            session.stop();
        }
        self.dialogs.lock().expect("dialog lock").clear();
    }

    /// What the media of a call should be.
    pub fn media_config(&self) -> MediaConfig {
        self.media_config.read().expect("media settings").clone()
    }

    /// Remember a dialog so the UI can act on it later.
    pub fn remember(&self, call_id: &str, dialog: InviteDialog) {
        self.dialogs
            .lock()
            .expect("dialog lock")
            .insert(call_id.to_string(), dialog);
    }

    /// The dialog for a call, if it is still up.
    pub fn dialog(&self, call_id: &str) -> Option<InviteDialog> {
        self.dialogs
            .lock()
            .expect("dialog lock")
            .get(call_id)
            .cloned()
    }

    /// Forget a dialog that has ended.
    pub fn forget(&self, call_id: &str) {
        self.dialogs.lock().expect("dialog lock").remove(call_id);
    }

    /// The calls that are currently up.
    pub fn call_ids(&self) -> Vec<String> {
        self.dialogs
            .lock()
            .expect("dialog lock")
            .keys()
            .cloned()
            .collect()
    }

    /// Remember the media of a call.
    ///
    /// The session is created before the offer is built: the SDP has
    /// to name a port that is really bound, and the socket is what
    /// binds it.
    pub fn remember_media(&self, call_id: &str, media: MediaSession) {
        self.media
            .lock()
            .expect("media lock")
            .insert(call_id.to_string(), media);
    }

    /// The media of a call, if it is still up.
    pub fn media(&self, call_id: &str) -> Option<MediaSession> {
        self.media.lock().expect("media lock").get(call_id).cloned()
    }

    /// Forget the media of a call that has ended.
    ///
    /// The session drops the sound card and stops the RTP threads on
    /// the way out.
    pub fn forget_media(&self, call_id: &str) {
        if let Some(media) = self.media.lock().expect("media lock").remove(call_id) {
            media.stop();
        }
    }
}

/// The live sessions, keyed by account id.
///
/// The registrar publishes into it, the call layer reads from it. It is
/// cloned by handle, so both hold the same map.
#[derive(Clone, Default)]
pub struct SessionRegistry {
    inner: Arc<Mutex<HashMap<String, Arc<AccountSession>>>>,
    /// What the media of a call should be.
    media_config: Arc<RwLock<MediaConfig>>,
}

impl SessionRegistry {
    /// A new, empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// The media settings, read.
    pub fn media_config(&self) -> MediaConfig {
        self.media_config.read().expect("media settings").clone()
    }

    /// The media settings, set.
    ///
    /// The registrar and the call layer both read them, so they are
    /// shared: a codec the user picks has to reach the offer the
    /// registrar's side builds.
    pub fn set_media_config(&self, config: MediaConfig) {
        *self.media_config.write().expect("media settings") = config;
    }

    /// The shared handle to the media settings.
    pub fn media_config_handle(&self) -> Arc<RwLock<MediaConfig>> {
        self.media_config.clone()
    }

    /// Publish a session under its account id.
    pub fn insert(&self, account_id: &str, session: Arc<AccountSession>) {
        self.inner
            .lock()
            .expect("session lock")
            .insert(account_id.to_string(), session);
    }

    /// The session for an account, if it has one.
    pub fn get(&self, account_id: &str) -> Option<Arc<AccountSession>> {
        self.inner
            .lock()
            .expect("session lock")
            .get(account_id)
            .cloned()
    }

    /// Drop a session — the account was unregistered, or the app is
    /// shutting down.
    ///
    /// The session is **closed** on the way out: the calls of that
    /// account end, and their sound cards are closed. A session that
    /// was only dropped would leave the microphone open, and the next
    /// call would find the device in use.
    pub fn remove(&self, account_id: &str) {
        if let Some(session) = self.inner.lock().expect("session lock").remove(account_id) {
            session.close();
        }
    }

    /// How many sessions are live.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("session lock").len()
    }

    /// Whether no session is live.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsipstack::platform::CancellationToken;
    use rsipstack::sip::Transport;

    #[test]
    fn a_registry_starts_empty() {
        let registry = SessionRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.get("acct-1").is_none());
    }

    #[tokio::test]
    async fn a_session_can_be_published_and_found() {
        let token = CancellationToken::new();
        let endpoint = crate::build_endpoint(Transport::Udp, 0, token.clone(), None)
            .await
            .expect("endpoint");
        let account = Account::new("Office", "example.com", "104", "pw", "udp");
        let registry = SessionRegistry::new();
        let session = AccountSession::new(
            account,
            endpoint,
            PublicAddress::default(),
            registry.media_config_handle(),
        );

        registry.insert("acct-1", session.clone());

        assert_eq!(registry.len(), 1);
        assert!(registry.get("acct-1").is_some());
        assert!(registry.get("acct-2").is_none());

        // The registry is a handle, not a copy: the session the call
        // layer finds is the one the registrar published.
        assert!(Arc::ptr_eq(
            &registry.get("acct-1").expect("session"),
            &session
        ));

        registry.remove("acct-1");
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn closing_a_session_stops_its_calls() {
        // The bug this fixes: the user unregisters in the middle of a
        // call, and the call's sound card stays open. The next call
        // then finds the device in use — a buffer overrun on the
        // capture side and a corrupted microphone.
        let token = CancellationToken::new();
        let endpoint = crate::build_endpoint(Transport::Udp, 0, token.clone(), None)
            .await
            .expect("endpoint");
        let account = Account::new("Office", "example.com", "104", "pw", "udp");
        let registry = SessionRegistry::new();
        let session = AccountSession::new(
            account,
            endpoint,
            PublicAddress::default(),
            registry.media_config_handle(),
        );

        // The transport, not the sound card: the test must not open a
        // microphone on the machine it runs on.
        let media = MediaSession::new(&MediaConfig::default()).expect("media");
        media.start_transport();
        assert!(media.is_running(), "the media is not running");
        session.remember_media("call-1", media.clone());

        session.close();

        assert!(!media.is_running(), "the media is still running");
        assert!(
            session.media("call-1").is_none(),
            "the media was left on the account"
        );
        assert!(session.token.is_cancelled(), "the calls were not told");
    }

    #[tokio::test]
    async fn removing_a_session_closes_it() {
        // The registry is where the registrar puts a session and where
        // it takes it away again. Taking it away has to **close** it:
        // a session that was only dropped would leave its sound card
        // open.
        let token = CancellationToken::new();
        let endpoint = crate::build_endpoint(Transport::Udp, 0, token.clone(), None)
            .await
            .expect("endpoint");
        let account = Account::new("Office", "example.com", "104", "pw", "udp");
        let registry = SessionRegistry::new();
        let session = AccountSession::new(
            account,
            endpoint,
            PublicAddress::default(),
            registry.media_config_handle(),
        );

        let media = MediaSession::new(&MediaConfig::default()).expect("media");
        media.start_transport();
        session.remember_media("call-1", media.clone());

        registry.insert("acct-1", session.clone());
        registry.remove("acct-1");

        assert!(registry.is_empty());
        assert!(!media.is_running(), "the media is still running");
        assert!(session.token.is_cancelled(), "the calls were not told");
    }
}
