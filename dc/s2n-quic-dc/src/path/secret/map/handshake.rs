// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use super::{entry::ApplicationData, Entry, Map};
use crate::{
    packet::secret_control as control,
    path::secret::{receiver, schedule, sender},
};
use parking_lot::Mutex;
use s2n_quic_core::{
    dc::{self, ApplicationParams, DatagramInfo},
    endpoint, ensure, event,
};
use std::{error::Error, net::SocketAddr, sync::Arc};
use zeroize::Zeroizing;

const TLS_EXPORTER_LABEL: &str = "EXPERIMENTAL EXPORTER s2n-quic-dc";
const TLS_EXPORTER_CONTEXT: &str = "";
const TLS_EXPORTER_LENGTH: usize = schedule::EXPORT_SECRET_LEN;

#[derive(Clone)]
pub struct HandshakingPath {
    inner: Arc<Mutex<HandshakingPathInner>>,
}

struct HandshakingPathInner {
    peer: SocketAddr,
    dc_version: dc::Version,
    parameters: ApplicationParams,
    endpoint_type: s2n_quic_core::endpoint::Type,
    secret: Option<schedule::Secret>,
    entry: Option<Arc<Entry>>,
    application_data: Option<ApplicationData>,
    /// Remote peer's PeerInfo bytes from the DcPeerInfo transport parameter.
    peer_info: Option<bytes::Bytes>,
    /// Raw bytes of the peer's `DcDataAddresses` transport parameter, decoded
    /// and validated lazily in `validate_peer_data_addrs`.
    peer_data_addrs_bytes: Option<bytes::Bytes>,
    /// Decoded and validated peer data addresses, populated by
    /// `validate_peer_data_addrs` and consumed by `on_peer_stateless_reset_tokens`.
    peer_data_addrs: Option<Vec<SocketAddr>>,
    map: Map,

    error: Option<Box<dyn Error + Send + Sync>>,
}

impl HandshakingPath {
    fn new(connection_info: &dc::ConnectionInfo, map: Map) -> Self {
        let endpoint_type = match connection_info.endpoint_type {
            event::api::EndpointType::Server { .. } => endpoint::Type::Server,
            event::api::EndpointType::Client { .. } => endpoint::Type::Client,
        };

        let peer_info = connection_info.peer_info.clone();
        let peer_data_addrs_bytes = connection_info.peer_data_addrs.clone();

        HandshakingPath {
            inner: Arc::new(Mutex::new(HandshakingPathInner {
                peer: connection_info.remote_address.clone().into(),
                dc_version: connection_info.dc_version,
                parameters: connection_info.application_params.clone(),
                endpoint_type,
                secret: None,
                entry: None,
                application_data: None,
                peer_info,
                peer_data_addrs_bytes,
                peer_data_addrs: None,
                map,
                error: None,
            })),
        }
    }

    pub fn entry(&self) -> Option<Arc<Entry>> {
        self.inner.lock().entry.clone()
    }

    pub fn take_error(&self) -> Option<Box<dyn Error + Send + Sync>> {
        self.inner.lock().error.take()
    }
}

impl dc::Endpoint for Map {
    type Path = HandshakingPath;

    fn new_path(&mut self, connection_info: &dc::ConnectionInfo) -> Option<Self::Path> {
        Some(HandshakingPath::new(connection_info, self.clone()))
    }

    fn on_possible_secret_control_packet(
        &mut self,
        // TODO: Maybe we should confirm that the sender IP at least matches the IP for the
        //       corresponding control secret?
        datagram_info: &DatagramInfo,
        payload: &mut [u8],
    ) -> bool {
        let payload = s2n_codec::DecoderBufferMut::new(payload);
        match control::Packet::decode(payload) {
            Ok((packet, tail)) => {
                // Probably a bug somewhere? There shouldn't be anything trailing in the buffer
                // after we decode a secret control packet.
                ensure!(tail.is_empty(), false);

                // If we successfully decoded a control packet, pass it into our map to handle.
                self.handle_control_packet(&packet, &datagram_info.remote_address.clone().into());

                true
            }
            Err(_) => false,
        }
    }

    fn advertised_peer_info(&self) -> Option<bytes::Bytes> {
        self.advertised_peer_info()
    }

    fn local_data_addrs(&self) -> Option<bytes::Bytes> {
        self.advertised_data_addrs()
    }
}

impl dc::Path for HandshakingPath {
    fn on_path_secrets_ready(
        &mut self,
        session: &impl s2n_quic_core::crypto::tls::TlsSession,
    ) -> Result<Vec<s2n_quic_core::stateless_reset::Token>, s2n_quic_core::transport::Error> {
        self.inner.lock().on_path_secrets_ready(session)
    }

    fn on_peer_stateless_reset_tokens<'a>(
        &mut self,
        stateless_reset_tokens: impl Iterator<Item = &'a s2n_quic_core::stateless_reset::Token>,
    ) {
        self.inner
            .lock()
            .on_peer_stateless_reset_tokens(stateless_reset_tokens)
    }

    fn on_dc_handshake_complete(&mut self) {
        self.inner.lock().on_dc_handshake_complete();
    }

    fn on_mtu_updated(&mut self, mtu: u16) {
        self.inner.lock().on_mtu_updated(mtu);
    }
}

/// Validate and normalize the peer's advertised data addresses, learned from the
/// `DcDataAddresses` transport parameter, against the `peer` address the handshake completed
/// against.
///
/// Wildcard IPs in the address list are replaced with the peer's handshake IP, since a peer
/// that bound to `[::]` is reachable at the address we connected to. Each address is then
/// normalized to the handshake peer's address family — an IPv4 data addr learned from an IPv6
/// handshake is mapped to IPv4-mapped IPv6 — so it can be sent on the (typically dual-stack
/// IPv6) socket we communicate with the peer over; otherwise the send path would hand the
/// kernel an `AF_INET` sockaddr on an IPv6 socket. An empty list is left as-is (the peer simply
/// advertised nothing); per-send context creation handles the no-addrs case.
///
/// Returns `TRANSPORT_PARAMETER_ERROR` if the peer advertised more than
/// [`MAX_PEER_DATA_ADDRS`](super::MAX_PEER_DATA_ADDRS) addresses, a wildcard port, or a loopback
/// address from a non-loopback peer.
fn validate_peer_data_addrs(
    peer: SocketAddr,
    addrs: &mut [SocketAddr],
) -> Result<(), s2n_quic_core::transport::Error> {
    use std::net::IpAddr;

    use s2n_quic_core::transport::Error;

    if addrs.is_empty() {
        return Ok(());
    }

    if addrs.len() > super::MAX_PEER_DATA_ADDRS {
        ::tracing::error!(
            count = addrs.len(),
            max = super::MAX_PEER_DATA_ADDRS,
            %peer,
            "peer advertised too many data addrs"
        );
        return Err(
            Error::TRANSPORT_PARAMETER_ERROR.with_reason("peer advertised too many data addrs")
        );
    }

    let peer_ip = peer.ip();
    let peer_is_loopback = peer_ip.is_loopback();

    for addr in addrs.iter_mut() {
        if addr.port() == 0 {
            ::tracing::error!(%addr, %peer, "peer data addr has wildcard port");
            return Err(
                Error::TRANSPORT_PARAMETER_ERROR.with_reason("peer data addr has wildcard port")
            );
        }

        // A peer that bound to a wildcard IP (e.g. `[::]`) is reachable at the address we
        // completed the handshake against; substitute that.
        let ip = if addr.ip().is_unspecified() {
            peer_ip
        } else {
            addr.ip()
        };

        if !peer_is_loopback && ip.is_loopback() {
            ::tracing::error!(
                %addr, %peer,
                "peer data addr is loopback but handshake addr is not"
            );
            return Err(Error::TRANSPORT_PARAMETER_ERROR
                .with_reason("peer data addr is loopback but handshake addr is not"));
        }

        // Match the handshake peer's address family so the address is sendable on the socket
        // we reached the peer over (see the function docs).
        let ip = match (peer_ip, ip) {
            (IpAddr::V6(_), IpAddr::V4(v4)) => IpAddr::V6(v4.to_ipv6_mapped()),
            (IpAddr::V4(_), IpAddr::V6(_)) => {
                ::tracing::error!(%addr, %peer, "peer data addr family does not match handshake addr");
                return Err(Error::TRANSPORT_PARAMETER_ERROR
                    .with_reason("peer data addr family does not match handshake addr"));
            }
            _ => ip,
        };

        *addr = SocketAddr::new(ip, addr.port());
    }

    ::tracing::debug!(%peer, ?addrs, "validated peer data addresses");

    Ok(())
}

impl HandshakingPathInner {
    /// Decode and validate the peer's advertised data addresses from the raw
    /// `DcDataAddresses` transport parameter bytes (see [`validate_peer_data_addrs`]).
    fn validate_peer_data_addrs(&mut self) -> Result<(), s2n_quic_core::transport::Error> {
        let Some(bytes) = self.peer_data_addrs_bytes.take() else {
            return Ok(());
        };

        // Decode the raw transport parameter bytes; a malformed encoding is treated
        // the same as an absent parameter (the peer simply advertised nothing).
        let mut addrs = match super::data_addresses::decode(&bytes) {
            Ok(addrs) => addrs,
            Err(err) => {
                ::tracing::debug!(
                    peer = %self.peer,
                    error = %err,
                    "ignoring malformed DcDataAddresses transport parameter"
                );
                return Ok(());
            }
        };

        validate_peer_data_addrs(self.peer, &mut addrs)?;
        self.peer_data_addrs = Some(addrs);
        Ok(())
    }

    fn on_path_secrets_ready(
        &mut self,
        session: &impl s2n_quic_core::crypto::tls::TlsSession,
    ) -> Result<Vec<s2n_quic_core::stateless_reset::Token>, s2n_quic_core::transport::Error> {
        // Validate and normalize the peer's advertised data addresses before doing any
        // other work, so a malformed `DcDataAddresses` transport parameter aborts the
        // handshake fast rather than after deriving secrets.
        self.validate_peer_data_addrs()?;

        let request = super::ApplicationDataRequest {
            tls: session,
            peer_info: self.peer_info.as_ref(),
            peer: self.peer,
            endpoint_type: self.endpoint_type,
        };
        match self.map.store.application_data(request) {
            Ok(application_data) => {
                self.application_data = application_data;
            }
            Err(err) => {
                self.error = Some(err.inner);
                return Err(s2n_quic_core::transport::Error::APPLICATION_ERROR.with_reason(err.msg));
            }
        };

        let mut material = Zeroizing::new([0; TLS_EXPORTER_LENGTH]);
        session
            .tls_exporter(
                TLS_EXPORTER_LABEL.as_bytes(),
                TLS_EXPORTER_CONTEXT.as_bytes(),
                &mut *material,
            )
            .map_err(|_| {
                s2n_quic_core::transport::Error::INTERNAL_ERROR.with_reason("tls exporter failed")
            })?;

        let cipher_suite = match session.cipher_suite() {
            s2n_quic_core::crypto::tls::CipherSuite::TLS_AES_128_GCM_SHA256 => {
                schedule::Ciphersuite::AES_GCM_128_SHA256
            }
            s2n_quic_core::crypto::tls::CipherSuite::TLS_AES_256_GCM_SHA384 => {
                schedule::Ciphersuite::AES_GCM_256_SHA384
            }
            _ => {
                return Err(s2n_quic_core::transport::Error::INTERNAL_ERROR
                    .with_reason("unsupported ciphersuite"))
            }
        };

        let secret =
            schedule::Secret::new(cipher_suite, self.dc_version, self.endpoint_type, &material);

        let stateless_reset = self.map.store.signer().sign(secret.id());
        self.secret = Some(secret);

        Ok(vec![stateless_reset.into()])
    }

    fn on_peer_stateless_reset_tokens<'a>(
        &mut self,
        stateless_reset_tokens: impl Iterator<Item = &'a s2n_quic_core::stateless_reset::Token>,
    ) {
        // TODO: support multiple stateless reset tokens
        let sender = sender::State::new(
            stateless_reset_tokens
                .into_iter()
                .next()
                .unwrap()
                .into_inner(),
        );

        let receiver = receiver::State::new();
        let socket_sender_count = self.map.store.socket_sender_count();
        let peer_data_addrs = core::mem::take(&mut self.peer_data_addrs);

        let entry = Entry::new_with_socket_senders(
            self.peer,
            self.secret
                .take()
                .expect("peer tokens are only received after secrets are ready"),
            sender,
            receiver,
            self.parameters.clone(),
            self.map.store.get_time(),
            self.application_data.take(),
            peer_data_addrs.unwrap_or_default().into(),
            socket_sender_count,
        );
        let entry = Arc::new(entry);
        self.entry = Some(entry.clone());
        self.map.store.on_new_path_secrets(entry);
    }

    fn on_dc_handshake_complete(&mut self) {
        let entry = self.entry.clone().expect(
            "the dc handshake cannot be complete without \
        on_peer_stateless_reset_tokens creating a map entry",
        );
        self.map.store.on_handshake_complete(entry);
    }

    fn on_mtu_updated(&mut self, mtu: u16) {
        if let Some(entry) = self.entry.as_ref() {
            entry.update_max_datagram_size(mtu);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::validate_peer_data_addrs;
    use std::net::SocketAddr;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn validate(peer: &str, addrs: &[&str]) -> Result<Vec<SocketAddr>, ()> {
        let mut addrs: Vec<SocketAddr> = addrs.iter().map(|s| addr(s)).collect();
        validate_peer_data_addrs(addr(peer), &mut addrs)
            .map(|()| addrs)
            .map_err(|_| ())
    }

    /// An IPv4 data addr from an IPv6 peer is rewritten to IPv4-mapped IPv6 so the send path
    /// can target it on the dual-stack IPv6 socket. This is the regression behind a v4 sockaddr
    /// being handed to an IPv6 socket.
    #[test]
    fn ipv4_addr_from_ipv6_peer_is_v6_mapped() {
        let out = validate("[2001:db8::1]:443", &["10.0.0.1:9000"]).unwrap();
        assert_eq!(out, vec![addr("[::ffff:10.0.0.1]:9000")]);
    }

    /// Families that already match (or a v4 peer) are left untouched.
    #[test]
    fn matching_family_is_unchanged() {
        assert_eq!(
            validate("[2001:db8::1]:443", &["[2001:db8::2]:9000"]).unwrap(),
            vec![addr("[2001:db8::2]:9000")]
        );
        assert_eq!(
            validate("10.0.0.5:443", &["10.0.0.1:9000"]).unwrap(),
            vec![addr("10.0.0.1:9000")]
        );
    }

    /// A wildcard advertised IP is replaced with the handshake peer IP, then family-normalized.
    #[test]
    fn wildcard_ip_uses_peer_ip() {
        assert_eq!(
            validate("203.0.113.7:443", &["0.0.0.0:9000"]).unwrap(),
            vec![addr("203.0.113.7:9000")]
        );
        // Wildcard v6 against a v6 peer keeps the port and adopts the peer IP.
        assert_eq!(
            validate("[2001:db8::1]:443", &["[::]:9000"]).unwrap(),
            vec![addr("[2001:db8::1]:9000")]
        );
    }

    #[test]
    fn empty_list_is_ok() {
        assert_eq!(validate("10.0.0.5:443", &[]).unwrap(), Vec::new());
    }

    #[test]
    fn wildcard_port_is_rejected() {
        assert!(validate("10.0.0.5:443", &["10.0.0.1:0"]).is_err());
    }

    /// A loopback data addr from a non-loopback peer is rejected; a loopback peer may advertise
    /// loopback addrs (local testing).
    #[test]
    fn loopback_from_non_loopback_peer_is_rejected() {
        assert!(validate("10.0.0.5:443", &["127.0.0.1:9000"]).is_err());
        assert!(validate("127.0.0.1:443", &["127.0.0.1:9000"]).is_ok());
    }

    #[test]
    fn too_many_addrs_is_rejected() {
        let many: Vec<String> = (0..=super::super::MAX_PEER_DATA_ADDRS)
            .map(|i| format!("10.0.0.1:{}", 9000 + i))
            .collect();
        let refs: Vec<&str> = many.iter().map(|s| s.as_str()).collect();
        assert!(validate("10.0.0.5:443", &refs).is_err());
    }

    /// Each handshake runs the `make_application_data` callback with this side's role, so that a
    /// map that is both the client and the server of a path, as in a loopback, tells the two
    /// sides of a handshake apart.
    #[test]
    fn application_data_request_carries_the_endpoint_type() {
        use super::{HandshakingPath, Map};
        use crate::path::secret::stateless_reset;
        use s2n_quic_core::{
            crypto::tls::{ChainError, CipherSuite, TlsExportError, TlsSession},
            dc::{self, Path as _},
            endpoint,
            event::IntoEvent as _,
            inet::{SocketAddress, SocketAddressV4},
            time,
        };
        use std::sync::{Arc, Mutex};

        /// A TLS session whose exporter leaves its output as it is, which is enough for the path
        /// to derive its secret.
        struct StubTlsSession;

        impl TlsSession for StubTlsSession {
            fn tls_exporter(
                &self,
                _label: &[u8],
                _context: &[u8],
                _output: &mut [u8],
            ) -> Result<(), TlsExportError> {
                Ok(())
            }

            fn cipher_suite(&self) -> CipherSuite {
                CipherSuite::TLS_AES_128_GCM_SHA256
            }

            fn peer_cert_chain_der(&self) -> Result<Vec<Vec<u8>>, ChainError> {
                Ok(vec![])
            }
        }

        let map = Map::builder()
            .with_signer(stateless_reset::Signer::random())
            .with_capacity(10)
            .with_clock(time::NoopClock)
            .with_subscriber(crate::event::tracing::Subscriber::default())
            .build()
            .unwrap();
        map.test_stop_cleaner();

        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = seen.clone();
        map.register_make_application_data(Box::new(
            move |request: super::super::ApplicationDataRequest| {
                recorder.lock().unwrap().push(request.endpoint_type);
                Ok(None)
            },
        ));

        let peer = SocketAddress::from(SocketAddressV4::new([127, 0, 0, 1], 4433));
        for endpoint_type in [endpoint::Type::Server, endpoint::Type::Client] {
            let connection_info = dc::ConnectionInfo::new(
                &peer,
                dc::SUPPORTED_VERSIONS[0],
                dc::testing::TEST_APPLICATION_PARAMS,
                endpoint_type.into_event(),
                None,
                None,
            );
            let mut path = HandshakingPath::new(&connection_info, map.clone());
            path.on_path_secrets_ready(&StubTlsSession).unwrap();
        }

        assert_eq!(
            *seen.lock().unwrap(),
            [endpoint::Type::Server, endpoint::Type::Client]
        );
    }
}
