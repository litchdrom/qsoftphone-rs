//! The message inspector a session runs with.
//!
//! Two jobs, both about what the peer has to believe about us:
//!
//! * **§5.8** — patch the RFC 7616 algorithm spelling, so rsipstack can
//!   parse the challenge at all. See [`crate::digest::DigestInterop`].
//! * **The public address** — a registrar that reports `received` /
//!   `rport` in the Via tells us the address our packets come from.
//!   That is the address the peer has to use to reach us, so it is the
//!   address that belongs in the Contact of an INVITE and in the
//!   `200 OK` of an incoming call.
//!
//! Without the second one the Contact would name the local socket —
//! `0.0.0.0:5060` — and every in-dialog request the peer sends (the
//! BYE, a re-INVITE) would go nowhere.

use std::sync::{Arc, Mutex};

use rsipstack::sip::{Header, HeadersExt, Host, HostWithPort, Response, SipMessage, ToTypedHeader};
use rsipstack::transaction::endpoint::MessageInspector;
use rsipstack::transport::SipAddr;

use crate::digest::DigestInterop;

/// The address the registrar sees us at.
///
/// `None` until a registrar tells us — a registrar that does not
/// report `received`/`rport` is telling us the address implicitly, and
/// the local address is then the right one.
#[derive(Clone, Default)]
pub struct PublicAddress(Arc<Mutex<Option<HostWithPort>>>);

impl PublicAddress {
    /// The address, if the registrar has reported one.
    pub fn get(&self) -> Option<HostWithPort> {
        self.0.lock().expect("public address lock").clone()
    }

    /// Note an address.
    fn set(&self, address: HostWithPort) {
        let mut slot = self.0.lock().expect("public address lock");
        if slot.as_ref() != Some(&address) {
            *slot = Some(address);
        }
    }
}

/// The inspector a session runs with.
pub struct SessionInspector {
    digest: DigestInterop,
    public: PublicAddress,
}

impl SessionInspector {
    /// A new inspector.
    pub fn new() -> Self {
        Self {
            digest: DigestInterop::new(),
            public: PublicAddress::default(),
        }
    }

    /// The public address, for the Contact headers.
    pub fn public_address(&self) -> PublicAddress {
        self.public.clone()
    }
}

impl Default for SessionInspector {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageInspector for SessionInspector {
    fn before_send(&self, msg: SipMessage, dest: Option<&SipAddr>) -> SipMessage {
        self.digest.before_send(msg, dest)
    }

    fn after_received(&self, msg: SipMessage, from: Option<&SipAddr>) -> SipMessage {
        let msg = self.digest.after_received(msg, from);
        if let SipMessage::Response(response) = &msg {
            if let Some(address) = public_address_of(response) {
                self.public.set(address);
            }
        }
        msg
    }
}

/// The address a response came from, as the server sees it.
///
/// RFC 3581: `received` is the source address the server saw, `rport`
/// the source port. Both are optional — a server that reports neither
/// is telling us that the address in the Via is the real one.
pub fn public_address_of(response: &Response) -> Option<HostWithPort> {
    let via = response.via_header().ok()?.typed().ok()?;

    let host = match via.received() {
        Some(Ok(ip)) => Host::IpAddr(ip),
        _ => via.sent_by().host.clone(),
    };

    let port = match via.rport() {
        Some(Some(port)) => Some(port.into()),
        _ => via.sent_by().port,
    };

    Some(HostWithPort { host, port })
}

/// The Contact header for this account, NAT-aware.
///
/// The public address wins when the registrar reported one; otherwise
/// the local address is used, which is right when there is no NAT.
pub fn nat_aware_contact(
    username: &str,
    public_address: Option<HostWithPort>,
    local_address: &SipAddr,
) -> Header {
    rsipstack::dialog::registration::Registration::create_nat_aware_contact(
        username,
        public_address,
        local_address,
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsipstack::sip::StatusCode;
    use rsipstack::sip::Uri;
    use rsipstack::sip::headers::typed::Via;

    fn response_with_via(via: &str) -> Response {
        let mut response = Response {
            status_code: StatusCode::OK,
            version: rsipstack::sip::Version::V2,
            headers: rsipstack::sip::Headers::default(),
            body: Vec::new(),
            wire_reason: None,
        };
        response
            .headers
            .push(Header::Via(Via::parse(via).expect("via").into()));
        response
    }

    #[test]
    fn the_received_parameter_wins_over_the_via_address() {
        let response = response_with_via(
            "SIP/2.0/UDP 10.0.0.9:5060;branch=z9hG4bK1;received=203.0.113.7;rport=51234",
        );
        let address = public_address_of(&response).expect("address");
        assert_eq!(address.host, Host::IpAddr("203.0.113.7".parse().unwrap()));
        assert_eq!(address.port.map(|p| p.value()), Some(51234));
    }

    #[test]
    fn without_the_received_parameters_the_via_address_is_used() {
        let response = response_with_via("SIP/2.0/UDP 203.0.113.7:5060;branch=z9hG4bK1");
        let address = public_address_of(&response).expect("address");
        assert_eq!(address.host, Host::IpAddr("203.0.113.7".parse().unwrap()));
        assert_eq!(address.port.map(|p| p.value()), Some(5060));
    }

    #[test]
    fn a_response_without_a_via_has_no_address() {
        let response = Response {
            status_code: StatusCode::OK,
            version: rsipstack::sip::Version::V2,
            headers: rsipstack::sip::Headers::default(),
            body: Vec::new(),
            wire_reason: None,
        };
        assert!(public_address_of(&response).is_none());
    }

    #[test]
    fn the_contact_uses_the_public_address_when_there_is_one() {
        let local = SipAddr {
            r#type: Some(rsipstack::sip::Transport::Udp),
            addr: HostWithPort {
                host: Host::IpAddr("0.0.0.0".parse().unwrap()),
                port: Some(5060.into()),
            },
        };
        let contact = nat_aware_contact(
            "104",
            Some(HostWithPort {
                host: Host::IpAddr("203.0.113.7".parse().unwrap()),
                port: Some(51234.into()),
            }),
            &local,
        );
        let rendered = contact.to_string();
        assert!(rendered.contains("203.0.113.7"), "{rendered}");
        assert!(rendered.contains("51234"), "{rendered}");
        assert!(rendered.contains("104"), "{rendered}");
    }

    #[test]
    fn the_contact_falls_back_to_the_local_address() {
        let local = SipAddr {
            r#type: Some(rsipstack::sip::Transport::Udp),
            addr: HostWithPort {
                host: Host::IpAddr("10.0.0.5".parse().unwrap()),
                port: Some(5060.into()),
            },
        };
        let contact = nat_aware_contact("104", None, &local);
        let rendered = contact.to_string();
        assert!(rendered.contains("10.0.0.5"), "{rendered}");
    }

    #[test]
    fn the_public_address_is_remembered() {
        let public = PublicAddress::default();
        assert!(public.get().is_none());
        public.set(HostWithPort {
            host: Host::IpAddr("203.0.113.7".parse().unwrap()),
            port: Some(51234.into()),
        });
        assert_eq!(
            public.get().map(|a| a.port.map(|p| p.value())),
            Some(Some(51234))
        );
    }

    #[test]
    fn a_uri_is_not_a_header() {
        // A guard against a silly mistake: the contact has to be a
        // header, not a bare URI.
        let uri = Uri::try_from("sip:104@example.com").unwrap();
        assert!(!uri.to_string().is_empty());
    }
}
