// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! stream Client: outbound connection establishment
//!
//! The Client is constructed from an `Arc<Endpoint>` and a PSK client provider. It provides
//! [`connect`](Client::connect) which performs a handshake if needed and returns a [`Stream`].
//! The Client holds its own clone of the queue allocator to avoid synchronization on the hot path.
//!
//! Flow initialization is lazy: `connect` allocates local queues and returns immediately.
//! The [`Writer`](crate::stream::Writer) sends `QueueInit` on the first write, potentially
//! with early data.

use crate::{
    path::secret::map::entry::QueueState,
    psk,
    stream::{endpoint::Endpoint, Reader, Stream, Writer},
};
use s2n_quic::server::Name;
use s2n_quic_core::varint::VarInt;
use std::{
    future::{poll_fn, Future},
    io,
    net::SocketAddr,
    pin::pin,
    sync::Arc,
    time::Instant,
};

pub mod rpc;

/// Client for making outbound `s2n-quic-dc` stream connections.
///
/// `Client` wraps a shared [`Endpoint`] and a PSK provider to open bidirectional [`Stream`]s
/// to a remote server. Each call to [`connect`](Self::connect) performs a TLS handshake if
/// no path secret exists yet, then allocates local queues and returns a ready-to-use stream.
///
/// `Client` is cheap to clone: it holds an `Arc` to the shared endpoint and an independent
/// copy of the queue allocator, so hot-path connection creation does not require global
/// synchronization.
///
/// # Expectations and guarantees
///
/// - Every clone shares the same underlying endpoint and path-secret map.
/// - Flow initialization is lazy. The [`Writer`](crate::stream::Writer) sends `QueueInit`
///   (with optional early data) on the first write after `connect` returns.
/// - The TLS handshake, if needed, is performed inside `connect` and is transparent to
///   the caller.
///
/// # Footguns
///
/// - The PSK provider's map **must** be the same `Arc` instance as the endpoint's map.
///   [`new`](Self::new) panics if they differ.
/// - `connect` only ensures a path secret exists. It does not guarantee the server is
///   reachable or that the stream will complete successfully.
///
/// # Example
///
/// ```ignore
/// use s2n_quic_dc::stream::{Client, Stream};
/// use s2n_quic_core::varint::VarInt;
/// use std::net::SocketAddr;
///
/// async fn open_stream(
///     mut client: Client,
///     server: SocketAddr,
/// ) -> std::io::Result<Stream> {
///     let acceptor_id = VarInt::from_u8(0);
///     client.connect(server, acceptor_id, Priority::default()).await
/// }
/// ```
#[derive(Clone)]
pub struct Client {
    endpoint: Arc<Endpoint>,
    psk: psk::client::Provider,
    server_name: Name,
}

impl Client {
    /// Creates a new `Client` from a shared [`Endpoint`] and PSK provider.
    ///
    /// `server_name` is the TLS server name used during handshakes with the peer.
    ///
    /// # Panics
    ///
    /// Panics if the PSK provider's map is not the same `Arc` instance as the endpoint's map.
    /// Both must point to the same shared path-secret store.
    pub fn new(endpoint: Arc<Endpoint>, psk: psk::client::Provider, server_name: Name) -> Self {
        assert_eq!(
            endpoint.path_secret_map,
            *psk.map(),
            "PSK provider map must be the same instance as the endpoint map"
        );
        Self {
            endpoint,
            psk,
            server_name,
        }
    }

    /// Opens a new bidirectional stream to `peer`.
    ///
    /// If no path secret already exists for `peer`, a TLS handshake is performed first.
    /// Once a path secret is available, local queues are allocated and a [`Stream`] is
    /// returned immediately.
    ///
    /// `acceptor_id` identifies which acceptor on the server should handle this stream.
    ///
    /// # Semantics
    ///
    /// - Flow initialization is lazy. The [`Writer`](crate::stream::Writer) sends `QueueInit`
    ///   (possibly with early data) on the first write.
    /// - A successful return does not mean the server has accepted the stream yet; it only
    ///   means local setup succeeded.
    ///
    /// # Footguns
    ///
    /// - If the TLS handshake fails, this returns an error and no stream is created.
    /// - Using an `acceptor_id` not registered on the server causes the server to reject
    ///   the stream, which surfaces as a later write or read error.
    pub async fn connect(
        &mut self,
        peer: SocketAddr,
        acceptor_id: VarInt,
        priority: crate::credit::Priority,
    ) -> io::Result<Stream> {
        let handshake_start = Instant::now();
        let peer_addr = peer;
        let (peer, mut handshake_kind) = self
            .psk
            .handshake_with_entry(peer_addr, self.server_name.clone())
            .await?;
        let mut path_secret_entry = peer.into_raw();

        // A cached entry whose peer was marked dead, past the dead-peer cooldown, and hasn't
        // answered for a whole idle timeout can't recover by itself: only a packet from the peer
        // clears the mark, and each flow we open on it goes unanswered and marks it again. Evict
        // it and handshake again, so the peer gets fresh keys and queue state, in place of
        // refusing flows until the process restarts.
        if matches!(handshake_kind, crate::path::secret::HandshakeKind::Cached)
            && path_secret_entry.is_dead_and_silent(
                self.endpoint.clock.now(),
                self.endpoint.path_secret_map.entry_age(&path_secret_entry),
                self.endpoint.dead_peer_cooldown,
            )
        {
            // Connects that find the same entry at once share one handshake, and a later one
            // gets that handshake's entry, as `handshake_replacing` says.
            let (peer, kind) = self
                .psk
                .handshake_replacing(peer_addr, &path_secret_entry, self.server_name.clone())
                .await?;
            path_secret_entry = peer.into_raw();
            handshake_kind = kind;
        }
        let handshake_elapsed = handshake_start.elapsed();
        let handshake_timer = match handshake_kind {
            crate::path::secret::HandshakeKind::Cached => {
                &self.endpoint.client_metrics.handshake_cached
            }
            crate::path::secret::HandshakeKind::Fresh => {
                &self.endpoint.client_metrics.handshake_fresh
            }
        };
        handshake_timer.record(handshake_elapsed);

        let now = self.endpoint.clock.now();
        if path_secret_entry.is_dead_during_cooldown(now, self.endpoint.dead_peer_cooldown) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "peer is in dead cooldown window",
            ));
        }

        let QueueState::Client(client_state) = path_secret_entry.queue_state() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "path secret entry has server queue state for client connect",
            ));
        };

        let alloc_start = Instant::now();
        let mut blocked = false;
        let alloc = {
            let mut alloc_fut =
                pin!(client_state.alloc(&path_secret_entry, self.endpoint.dead_peer_cooldown));
            poll_fn(|cx| {
                let poll = alloc_fut.as_mut().poll(cx);
                if poll.is_pending() {
                    blocked = true;
                }
                poll
            })
            .await
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionReset, "peer queue slots closed"))?;
        let alloc_elapsed = alloc_start.elapsed();
        if blocked {
            self.endpoint
                .client_metrics
                .alloc_blocked
                .record(alloc_elapsed);
        } else {
            self.endpoint
                .client_metrics
                .alloc_fast
                .record(alloc_elapsed);
        }

        let frame_tx = self.endpoint.frame_tx.clone();
        let writer = Writer::new_client(
            frame_tx.clone(),
            path_secret_entry.clone(),
            alloc.dest_queue_id,
            acceptor_id,
            alloc.control,
            self.endpoint.clock.clone(),
            self.endpoint.writer_metrics.clone(),
            self.endpoint.send_credit_pool.clone(),
            priority,
        );
        let reader = Reader::new_client(
            frame_tx,
            path_secret_entry,
            alloc.dest_queue_id,
            acceptor_id,
            alloc.stream,
            self.endpoint.clock.clone(),
            self.endpoint.reader_metrics.clone(),
            self.endpoint.recv_credit_pool.clone(),
            priority,
        );

        let stream = Stream::new(reader, writer);

        self.endpoint
            .client_metrics
            .connect_time
            .record(handshake_start.elapsed());

        Ok(stream)
    }

    /// Performs a single-round-trip RPC: sends `request` and collects `response`.
    ///
    /// Opens a stream to `peer` (with `acceptor_id`), writes the full request payload
    /// with FIN, then reads the full response and returns the caller-chosen output type.
    /// The stream is consumed after the exchange.
    ///
    /// # Footguns
    ///
    /// - The response buffer provided by [`Response::provide_storage`] must have sufficient
    ///   capacity. An error is returned if storage can never grow to fit the server's reply.
    /// - If the request write or response read fails, the stream is dropped without a clean
    ///   shutdown.
    pub async fn rpc<Req, Res>(
        &mut self,
        peer: SocketAddr,
        acceptor_id: VarInt,
        priority: crate::credit::Priority,
        request: Req,
        response: Res,
    ) -> io::Result<Res::Output>
    where
        Req: rpc::Request,
        Res: rpc::Response,
    {
        let stream = self.connect(peer, acceptor_id, priority).await?;
        rpc::from_stream(stream, request, response).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        credit::Priority,
        path::secret::{stateless_reset::Signer, Map},
        testing::{init_tracing, server_name, NoopSubscriber, TestTlsProvider},
    };
    use s2n_quic_core::time::StdClock;
    use std::time::Duration;

    /// `connect` replaces a cached entry that is dead and silent while the handshake queue still
    /// holds the success of the handshake that built it: it evicts the entry, drops that hold,
    /// handshakes again for a new entry, and records the second handshake on the fresh handshake
    /// timer, not the cached one.
    #[tokio::test]
    async fn connect_replaces_a_dead_and_silent_entry_inside_the_success_hold() {
        init_tracing();

        let tls = TestTlsProvider {};
        let server_map = Map::builder()
            .with_signer(Signer::new(b"default"))
            .with_capacity(50_000)
            .with_clock(StdClock::default())
            .with_subscriber(NoopSubscriber {})
            .build()
            .unwrap();
        let server = psk::server::Provider::builder()
            .start(
                "[::1]:0".parse().unwrap(),
                tls.clone(),
                NoopSubscriber {},
                server_map,
            )
            .await
            .unwrap();
        let server_addr = server.local_addr();

        // The client's map runs on a test clock, so the test moves an entry's age past an idle
        // timeout without waiting.
        let map_clock = crate::time::testing::Clock::new(Duration::from_secs(1_000));
        let map = Map::builder()
            .with_signer(Signer::new(b"default"))
            .with_capacity(50_000)
            .with_clock(map_clock.clone())
            .with_subscriber(NoopSubscriber {})
            .build()
            .unwrap();
        map.test_stop_cleaner();

        // A success hold far longer than the test, so the first handshake's success is still
        // held when `connect` evicts its entry.
        let handshakes = psk::client::Provider::builder()
            .with_success_jitter(Duration::from_secs(1_000_000))
            .start(
                "[::]:0".parse().unwrap(),
                map.clone(),
                tls,
                NoopSubscriber {},
                server_name(),
            )
            .unwrap();

        // The endpoint's clock counts from two minutes back. No packet updates the entry's last
        // activity here, so the time since that activity already passes an idle timeout.
        let start = std::time::Instant::now()
            .checked_sub(Duration::from_secs(120))
            .expect("the monotonic clock reads at least two minutes");
        let cooldown = crate::endpoint::DEFAULT_DEAD_PEER_COOLDOWN;
        let (endpoint, _frames) = crate::endpoint::Endpoint::without_workers(
            map.clone(),
            crate::time::DefaultClock::busy_poll_from(start),
            cooldown,
        );
        let endpoint = Arc::new(endpoint);
        let mut client = Client::new(endpoint.clone(), handshakes, server_name());
        let acceptor_id = VarInt::from_u8(0);

        let stream = client
            .connect(server_addr, acceptor_id, Priority::default())
            .await
            .expect("the first connect handshakes");
        let first_binding = stream.binding_id();
        drop(stream);
        let first = map
            .get_untracked(server_addr)
            .expect("the first connect's entry")
            .into_raw();
        let first_id = *first.id();
        let metrics = endpoint
            .counters
            .take_current_metrics_line()
            .expect("the first connect records its handshake");
        assert!(metrics.contains("Variant|fresh"), "{metrics}");
        assert!(!metrics.contains("Variant|cached"), "{metrics}");

        // The send idle wheel marks a peer dead when a flow goes unanswered. Mark it as of a
        // time just past the dead-peer cooldown.
        let now = endpoint.clock.now();
        let dead_at = now - (cooldown + Duration::from_secs(1));
        assert!(first.mark_dead_if_cooldown_elapsed(dead_at, cooldown));
        assert!(!first.is_dead_during_cooldown(now, cooldown));
        // The entry is new on the map's clock, so it hasn't been silent for an idle timeout yet.
        let age = map.entry_age(&first);
        assert!(!first.is_dead_and_silent(now, age, cooldown));

        map_clock.advance(first.idle_timeout());
        let now = endpoint.clock.now();
        assert!(
            first.is_dead_and_silent(now, map.entry_age(&first), cooldown),
            "the entry is dead, past the cooldown and silent for an idle timeout"
        );

        // `connect` gets the cached entry back, evicts it, drops the handshake queue's hold on
        // the first handshake's success and handshakes again. Without that drop, the second
        // handshake would return the held success while the map has no entry for the peer, and
        // this connect would fail with "handshake failed to exchange credentials".
        let stream = client
            .connect(server_addr, acceptor_id, Priority::default())
            .await
            .expect("connect replaces the dead and silent entry");
        // A stream takes its binding id from its entry's queue state, which starts each entry's
        // ids at the same value, so a stream on the new entry has the first stream's id, and one
        // on the evicted entry would have that entry's next id.
        assert_eq!(
            stream.binding_id(),
            first_binding,
            "the stream uses the new entry's queue state"
        );
        drop(stream);
        let second = map
            .get_untracked(server_addr)
            .expect("the second connect's entry")
            .into_raw();
        assert_ne!(
            *second.id(),
            first_id,
            "connect handshook again for a new entry"
        );
        let metrics = endpoint
            .counters
            .take_current_metrics_line()
            .expect("the second connect records its handshake");
        assert!(metrics.contains("Variant|fresh"), "{metrics}");
        assert!(!metrics.contains("Variant|cached"), "{metrics}");
    }

    /// `connect` to a peer that is down, with its entry dead past the cooldown and silent for an
    /// idle timeout, evicts the entry and handshakes, and returns that handshake's error in place
    /// of the cooldown refusal. The handshake times out, and the handshake queue doesn't hold a
    /// timed-out handshake's failure, so it keeps no entry for the peer and the next connect
    /// handshakes again.
    #[tokio::test]
    async fn connect_to_a_down_peer_returns_the_handshake_error() {
        init_tracing();

        // A loopback address that nothing listens on: bind a socket for a free port, then
        // close it.
        let peer_addr = std::net::UdpSocket::bind("[::1]:0")
            .unwrap()
            .local_addr()
            .unwrap();

        let map_clock = crate::time::testing::Clock::new(Duration::from_secs(1_000));
        let map = Map::builder()
            .with_signer(Signer::new(b"default"))
            .with_capacity(50_000)
            .with_clock(map_clock.clone())
            .with_subscriber(NoopSubscriber {})
            .build()
            .unwrap();
        map.test_stop_cleaner();

        let handshakes = psk::client::Provider::builder()
            .start(
                "[::]:0".parse().unwrap(),
                map.clone(),
                TestTlsProvider {},
                NoopSubscriber {},
                server_name(),
            )
            .unwrap();

        // The endpoint's clock counts from two minutes back. No packet updates the entry's last
        // activity here, so the time since that activity already passes an idle timeout.
        let start = std::time::Instant::now()
            .checked_sub(Duration::from_secs(120))
            .expect("the monotonic clock reads at least two minutes");
        let cooldown = crate::endpoint::DEFAULT_DEAD_PEER_COOLDOWN;
        let (endpoint, _frames) = crate::endpoint::Endpoint::without_workers(
            map.clone(),
            crate::time::DefaultClock::busy_poll_from(start),
            cooldown,
        );
        let endpoint = Arc::new(endpoint);
        let mut client = Client::new(endpoint.clone(), handshakes.clone(), server_name());
        let acceptor_id = VarInt::from_u8(0);

        // An entry for the peer, as an earlier handshake would leave it, marked dead as of a time
        // just past the cooldown and silent for an idle timeout.
        map.test_insert(peer_addr, s2n_quic_core::endpoint::Type::Client);
        let entry = map
            .get_untracked(peer_addr)
            .expect("the inserted entry")
            .into_raw();
        let now = endpoint.clock.now();
        let dead_at = now - (cooldown + Duration::from_secs(1));
        assert!(entry.mark_dead_if_cooldown_elapsed(dead_at, cooldown));
        // The test entry builder takes the entry's creation time from the process clock, which a
        // long test process reads past the map's test clock, so move the map's clock to one idle
        // timeout past the entry's creation, whatever the process clock reads.
        let map_now = s2n_quic_core::time::Clock::get_time(&map_clock);
        let behind = entry.creation_time().saturating_duration_since(map_now);
        map_clock.advance(behind + entry.idle_timeout());
        let now = endpoint.clock.now();
        assert!(
            entry.is_dead_and_silent(now, map.entry_age(&entry), cooldown),
            "the entry is dead, past the cooldown and silent for an idle timeout"
        );

        let error = client
            .connect(peer_addr, acceptor_id, Priority::default())
            .await
            .map(|_| ())
            .expect_err("nothing answers the handshake");
        assert!(
            !error.to_string().contains("dead cooldown"),
            "connect returns the handshake's error, not the cooldown refusal: {error}"
        );
        assert!(
            !map.contains(&peer_addr),
            "connect evicted the dead entry, and the failed handshake didn't insert one"
        );
        assert!(
            !handshakes.has_pending_entry(peer_addr),
            "the timed-out handshake's failure isn't held, so the next connect handshakes again"
        );
    }
}
