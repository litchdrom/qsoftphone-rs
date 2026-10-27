//! Endpoint construction.
//!
//! One endpoint per account: each account has its own transport, its
//! own local socket and its own registrar, and they must not share a
//! Via branch space.

use std::net::SocketAddr;

use rsipstack::platform::CancellationToken;
use rsipstack::sip::Transport;
use rsipstack::transaction::endpoint::MessageInspector;
use rsipstack::transaction::{Endpoint, EndpointBuilder};
use rsipstack::transport::TransportLayer;
use rsipstack::transport::udp::UdpConnection;

pub const USER_AGENT: &str = concat!("qsoftphone-rs/", env!("CARGO_PKG_VERSION"));

/// The error `build_endpoint` returns.
///
/// rsipstack's error carries the whole message buffer, which makes it a
/// large `Err` variant. It is boxed here so the `Result` stays small —
/// the caller only ever renders it into a log line and the UI's status
/// text.
pub type EndpointError = Box<rsipstack::Error>;

/// Split a `host[:port]` value.
///
/// Accepts a bare host, a host and a port, a bracketed IPv6 literal,
/// and a full `sip:` URI with parameters — users type all of these.
/// The port is optional; the default SIP port is applied later.
pub fn split_host_port(value: &str) -> (String, Option<u16>) {
    let value = value.trim();
    // Drop a scheme, if there is one.
    let value = value
        .strip_prefix("sips:")
        .or_else(|| value.strip_prefix("sip:"))
        .unwrap_or(value);
    // Drop any URI parameters.
    let value = value.split(';').next().unwrap_or(value);

    if let Some(rest) = value.strip_prefix('[') {
        // [::1]:5060
        if let Some((host, tail)) = rest.split_once(']') {
            let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
            return (host.to_string(), port);
        }
        return (rest.to_string(), None);
    }

    // More than one colon and no brackets: an unbracketed IPv6
    // literal, which cannot carry a port.
    if value.matches(':').count() > 1 {
        return (value.to_string(), None);
    }

    match value.rsplit_once(':') {
        Some((host, port)) => match port.parse::<u16>() {
            Ok(port) => (host.to_string(), Some(port)),
            // A colon that is not a port — an unbracketed IPv6
            // literal, most likely.
            Err(_) => (value.to_string(), None),
        },
        None => (value.to_string(), None),
    }
}

/// The `;transport=` URI parameter for a transport, if it needs one.
///
/// UDP is the default and needs no parameter; adding one would change
/// the string the digest is computed over.
pub fn transport_param(transport: Transport) -> Option<&'static str> {
    match transport {
        Transport::Udp => None,
        Transport::Tcp => Some("tcp"),
        Transport::Tls => Some("tls"),
        _ => None,
    }
}

/// Parse the account's transport setting.
///
/// Anything unrecognised is UDP, which is what the brief's account
/// model defaults to.
pub fn parse_transport(value: &str) -> Transport {
    match value.trim().to_ascii_lowercase().as_str() {
        "tcp" => Transport::Tcp,
        "tls" => Transport::Tls,
        _ => Transport::Udp,
    }
}

/// Build an endpoint for one account.
///
/// UDP binds a local socket up front. If the configured port is
/// already taken the socket falls back to an ephemeral port — a
/// second copy of the app should not refuse to start because the
/// first one has 5060.
///
/// TCP and TLS are dialled on demand by the transport layer — it
/// reads the transport off the registrar's URI — so they start with
/// an empty transport layer and the registrar's address decides.
///
/// The optional inspector sees every message in and out; that is where
/// [`crate::digest::DigestInterop`] patches the RFC 7616 algorithm
/// spelling.
pub async fn build_endpoint(
    transport: Transport,
    port: u16,
    cancel_token: CancellationToken,
    inspector: Option<Box<dyn MessageInspector>>,
) -> Result<Endpoint, EndpointError> {
    let transport_layer = TransportLayer::new(cancel_token.clone());

    if matches!(transport, Transport::Udp) {
        let preferred = SocketAddr::from(([0, 0, 0, 0], port));
        let connection = match UdpConnection::create_connection(
            preferred,
            None,
            Some(cancel_token.clone()),
        )
        .await
        {
            Ok(connection) => connection,
            Err(e) if port != 0 => {
                tracing::warn!(error = %e, port, "the SIP port is in use; using an ephemeral one");
                UdpConnection::create_connection(
                    SocketAddr::from(([0, 0, 0, 0], 0)),
                    None,
                    Some(cancel_token.clone()),
                )
                .await
                .map_err(Box::new)?
            }
            Err(e) => return Err(Box::new(e)),
        };
        transport_layer.add_transport(connection.into());
    }

    let mut builder = EndpointBuilder::new();
    builder
        .with_user_agent(USER_AGENT)
        .with_transport_layer(transport_layer)
        .with_cancel_token(cancel_token);
    if let Some(inspector) = inspector {
        builder.with_inspector(inspector);
    }
    Ok(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_and_port_are_split() {
        assert_eq!(
            split_host_port("example.com:5060"),
            ("example.com".into(), Some(5060))
        );
        assert_eq!(split_host_port("example.com"), ("example.com".into(), None));
    }

    #[test]
    fn a_scheme_and_parameters_are_tolerated() {
        assert_eq!(
            split_host_port("sip:example.com:5060;transport=tcp"),
            ("example.com".into(), Some(5060))
        );
        assert_eq!(
            split_host_port("sips:example.com"),
            ("example.com".into(), None)
        );
    }

    #[test]
    fn ipv6_literals_are_handled() {
        assert_eq!(split_host_port("[::1]:5060"), ("::1".into(), Some(5060)));
        assert_eq!(split_host_port("[::1]"), ("::1".into(), None));
    }

    #[test]
    fn a_colon_that_is_not_a_port_is_kept() {
        assert_eq!(split_host_port("::1"), ("::1".into(), None));
        assert_eq!(split_host_port("2001:db8::1"), ("2001:db8::1".into(), None));
    }

    #[test]
    fn the_transport_is_parsed_leniently() {
        assert!(matches!(parse_transport("udp"), Transport::Udp));
        assert!(matches!(parse_transport("TCP"), Transport::Tcp));
        assert!(matches!(parse_transport("tls"), Transport::Tls));
        assert!(matches!(parse_transport("nonsense"), Transport::Udp));
        assert!(matches!(parse_transport(""), Transport::Udp));
    }

    #[test]
    fn udp_needs_no_transport_parameter() {
        assert_eq!(transport_param(Transport::Udp), None);
        assert_eq!(transport_param(Transport::Tcp), Some("tcp"));
        assert_eq!(transport_param(Transport::Tls), Some("tls"));
    }
}
