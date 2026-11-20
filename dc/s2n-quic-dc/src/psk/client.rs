// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use super::io::{self, HandshakeFailed};
use crate::{path::secret, psk::io::HandshakeReason};
use s2n_quic::{
    provider::{event::Subscriber as Sub, tls::Provider as Prov},
    server::Name,
};
use std::{net::SocketAddr, sync::Arc};
use tokio::runtime::Runtime;
use tokio_util::sync::DropGuard;

mod builder;

pub use crate::path::secret::HandshakeKind;
pub use builder::Builder;

#[derive(Clone)]
pub struct Provider {
    state: Arc<State>,
}

struct State {
    // This is always present in production, but for testing purposes we sometimes run within the
    // deterministic simulation framework. In that case there's no runtime for us to push work
    // into.
    runtime: Option<(Arc<Runtime>, DropGuard)>,
    map: secret::Map,
    client: io::Client,
    local_addr: SocketAddr,
}

fn make_runtime() -> (Arc<Runtime>, DropGuard) {
    let runtime = Arc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
    );

    let token = tokio_util::sync::CancellationToken::new();
    let cancelled = token.clone().cancelled_owned();
    let rt = runtime.clone();
    std::thread::Builder::new()
        .name(String::from("hs-client"))
        .spawn(move || {
            rt.block_on(cancelled);
        })
        .unwrap();

    (runtime, token.drop_guard())
}

impl State {
    fn new_runtime<
        Provider: Prov + Send + Sync + 'static,
        Subscriber: Sub + Send + Sync + 'static,
        Event: s2n_quic::provider::event::Subscriber,
    >(
        addr: SocketAddr,
        map: secret::Map,
        tls_materials_provider: Provider,
        subscriber: Subscriber,
        builder: Builder<Event>,
    ) -> io::Result<Self> {
        let (runtime, rt_guard) = make_runtime();
        let guard = runtime.enter();
        let client = io::Client::bind::<Provider, Subscriber, Event>(
            addr,
            map.clone(),
            tls_materials_provider,
            subscriber,
            builder,
        )?;
        drop(guard);

        Ok(Self {
            map,
            runtime: Some((runtime, rt_guard)),
            local_addr: client.local_addr()?,
            client,
        })
    }
}

impl Provider {
    /// Returns a [`Builder`] which is able to configure the [`Provider`]
    pub fn builder() -> Builder<impl s2n_quic::provider::event::Subscriber> {
        Builder::default()
    }

    pub fn new<
        Provider: Prov + Send + Sync + 'static,
        Subscriber: Sub + Send + Sync + 'static,
        Event: s2n_quic::provider::event::Subscriber,
    >(
        addr: SocketAddr,
        map: secret::Map,
        tls_materials_provider: Provider,
        subscriber: Subscriber,
        builder: Builder<Event>,
        server_name: Name,
    ) -> io::Result<Self> {
        let state = State::new_runtime(
            addr,
            map.clone(),
            tls_materials_provider,
            subscriber,
            builder,
        )?;
        let state = Arc::new(state);

        // The map asks for a handshake in the background: the cleaner's periodic rehandshake, an
        // accepted UnknownPathSecret or ReplayDetected, and a client path the endpoint's send
        // idle wheel marks dead.
        // Avoid holding onto the state unintentionally after it's no longer needed.
        let weak = Arc::downgrade(&state);
        map.register_request_handshake(Box::new(move |peer, reason| {
            let state = weak.upgrade()?;
            let runtime = state.runtime.as_ref().map(|v| &v.0)?;
            let client = state.client.clone();
            let server_name = server_name.clone();
            Some(runtime.spawn(async move {
                if let Err(HandshakeFailed { .. }) = client.connect(peer, reason, server_name).await
                {
                    // failure has already been logged, no further action required.
                }
            }))
        }));

        Ok(Self { state })
    }

    /// Handshake asynchronously with a peer.
    ///
    /// This method can be called with any async runtime.
    #[inline]
    pub async fn handshake_with(
        &self,
        peer: SocketAddr,
        server_name: Name,
    ) -> std::io::Result<HandshakeKind> {
        let (_peer, kind) = self.handshake_with_entry(peer, server_name).await?;
        Ok(kind)
    }

    /// Handshake asynchronously with a peer, returning an entry for secret derivation
    ///
    /// This method can be called with any async runtime.
    #[inline]
    #[doc(hidden)]
    pub async fn handshake_with_entry(
        &self,
        peer: SocketAddr,
        server_name: Name,
    ) -> std::io::Result<(secret::map::Peer, HandshakeKind)> {
        if let Some(entry) = self.state.map.get_tracked(peer) {
            return Ok((entry, HandshakeKind::Cached));
        }

        // Unconditionally request a background handshake. This schedules any re-handshaking
        // needed. We put this after get_tracked because that saves us a global lock to check
        // presence in the map in the happy path.
        if self.state.runtime.is_some() {
            let _ = self.background_handshake_with(peer, server_name.clone());
        }

        let state = self.state.clone();
        if let Some((runtime, _)) = self.state.runtime.as_ref() {
            runtime
                .spawn(async move {
                    state
                        .client
                        .connect(peer, HandshakeReason::User, server_name)
                        .await
                })
                .await??;
        } else {
            state
                .client
                .connect(peer, HandshakeReason::User, server_name)
                .await?;
        }

        // already recorded a metric above in get_tracked.
        let peer = self.state.map.get_untracked(peer).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("handshake failed to exchange credentials for {peer}"),
            )
        })?;

        Ok((peer, HandshakeKind::Fresh))
    }

    /// Handshake with a peer in the background.
    #[inline]
    pub fn background_handshake_with(
        &self,
        peer: SocketAddr,
        server_name: Name,
    ) -> std::io::Result<HandshakeKind> {
        if self.state.map.contains(&peer) {
            return Ok(HandshakeKind::Cached);
        }

        let client = self.state.client.clone();
        if let Some((runtime, _)) = self.state.runtime.as_ref() {
            // Drop the JoinHandle -- we're not actually going to block on the join handle's
            // result. The future will keep running in the background.
            runtime.spawn(async move {
                if let Err(HandshakeFailed { .. }) = client
                    .connect(peer, HandshakeReason::User, server_name)
                    .await
                {
                    // error already logged
                }
            });
        } else {
            panic!("background_handshake_with not supported with deterministic testing");
        }

        // Technically this might not be true (the handshake may get deduplicated), but it's close
        // enough to accurate that we're OK claiming it's true.
        Ok(HandshakeKind::Fresh)
    }

    /// Handshake synchronously with a peer.
    ///
    /// This method will block the calling thread and will panic if called from within a Tokio
    /// runtime.
    // We duplicate the implementation of this method with handshake_with so that we preserve the fast
    // path (not interacting with the runtime at all) for cached handshakes.
    #[inline]
    pub fn blocking_handshake_with(
        &self,
        peer: SocketAddr,
        server_name: Name,
    ) -> std::io::Result<HandshakeKind> {
        // Unconditionally request a background handshake. This schedules any re-handshaking
        // needed.
        if self.state.runtime.is_some() {
            let _ = self.background_handshake_with(peer, server_name.clone());
        }

        if self.state.map.contains(&peer) {
            return Ok(HandshakeKind::Cached);
        }

        let fut = self
            .state
            .client
            .connect(peer, HandshakeReason::User, server_name);
        if let Some((runtime, _)) = self.state.runtime.as_ref() {
            runtime.block_on(fut)?
        } else {
            panic!("blocking_handshake_with not supported with deterministic testing");
        }

        debug_assert!(self.state.map.contains(&peer));

        Ok(HandshakeKind::Fresh)
    }

    /// Replaces `stale`, the cached entry for `peer` that a caller found dead and silent, with
    /// the entry of a new handshake, and returns the peer's entry.
    ///
    /// After a successful handshake, the client holds that success for a random period, and a
    /// handshake requested within it returns that success without handshaking. This evicts
    /// `stale` and drops that hold, then handshakes, so the handshake builds a new entry.
    ///
    /// Concurrent calls for the same peer share one handshake: the handshake queue runs one
    /// handshake for a peer at a time, which the other calls join. Evicting `stale` leaves a
    /// newer entry for the peer in place, and a call that finds one, as one that comes after the
    /// shared handshake does, keeps that handshake's hold and gets the newer entry back as
    /// cached. A failed handshake stays held, so the calls after it get its error until its
    /// backoff ends, without handshaking.
    pub(crate) async fn handshake_replacing(
        &self,
        peer: SocketAddr,
        stale: &Arc<secret::map::Entry>,
        server_name: Name,
    ) -> std::io::Result<(secret::map::Peer, HandshakeKind)> {
        self.state.client.evict_and_forget_completed(peer, stale);
        self.handshake_with_entry(peer, server_name).await
    }

    /// This forces a handshake with the given peer, ignoring whether there's already an entry or
    /// not.
    #[inline]
    #[doc(hidden)]
    pub async fn unconditionally_handshake_with_entry(
        &self,
        peer: SocketAddr,
        server_name: Name,
    ) -> std::io::Result<secret::map::Peer> {
        let state = self.state.clone();
        if let Some((runtime, _)) = self.state.runtime.as_ref() {
            runtime
                .spawn(async move {
                    state
                        .client
                        .connect(peer, HandshakeReason::User, server_name)
                        .await
                })
                .await??;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "missing runtime for handshake client",
            ));
        }

        // Don't bother recording metrics on access.
        let peer = self.state.map.get_untracked(peer).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("handshake failed to exchange credentials for {peer}"),
            )
        })?;

        Ok(peer)
    }

    // FIXME: Remove Result (breaking change)
    #[inline]
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.state.local_addr)
    }

    pub fn map(&self) -> &secret::Map {
        &self.state.map
    }

    /// Returns true if the handshake client holds an entry for this peer: a handshake in flight,
    /// or a failure it holds. This is only available in test builds.
    #[cfg(test)]
    pub(crate) fn has_pending_entry(&self, peer: SocketAddr) -> bool {
        self.state.client.has_pending_entry(peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        path::secret::{stateless_reset::Signer, Map},
        testing::{init_tracing, server_name, NoopSubscriber, TestTlsProvider},
    };
    use s2n_quic_core::time::StdClock;
    use std::{io::ErrorKind, time::Duration};

    fn test_map() -> Map {
        Map::builder()
            .with_signer(Signer::new(b"default"))
            .with_capacity(50_000)
            .with_clock(StdClock::default())
            .with_subscriber(NoopSubscriber {})
            .build()
            .unwrap()
    }

    /// The case `handshake_replacing` covers, with a real handshake. While the handshake queue
    /// still holds the success of the handshake that built an evicted entry, the next handshake
    /// returns that success without handshaking, and fails since the map has no entry.
    /// `handshake_replacing` drops the hold, so its handshake builds a new entry, here for an
    /// entry that another caller evicted first.
    #[tokio::test]
    async fn handshake_replacing_replaces_an_evicted_entry() {
        init_tracing();

        let tls = TestTlsProvider {};
        let server = crate::psk::server::Provider::builder()
            .start(
                "[::1]:0".parse().unwrap(),
                tls.clone(),
                NoopSubscriber {},
                test_map(),
            )
            .await
            .unwrap();
        let server_addr = server.local_addr();

        // A success hold far longer than the test, so the first handshake's success is still
        // held when its entry is evicted.
        let client = Provider::builder()
            .with_success_jitter(Duration::from_secs(1_000_000))
            .start(
                "[::]:0".parse().unwrap(),
                test_map(),
                tls,
                NoopSubscriber {},
                server_name(),
            )
            .unwrap();

        let (first, kind) = client
            .handshake_with_entry(server_addr, server_name())
            .await
            .unwrap();
        assert!(matches!(kind, HandshakeKind::Fresh));
        let first = first.into_raw();
        let first_id = *first.id();

        client.map().evict_entry(&first);
        assert!(!client.map().contains(&server_addr));

        // While the hold stands, the held success comes back without a handshake, and the map
        // has no entry for the peer.
        let error = client
            .handshake_with_entry(server_addr, server_name())
            .await
            .map(|_| ())
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound, "{error}");

        let (second, kind) = client
            .handshake_replacing(server_addr, &first, server_name())
            .await
            .unwrap();
        assert!(matches!(kind, HandshakeKind::Fresh));
        assert_ne!(*second.into_raw().id(), first_id);
    }

    /// A thousand concurrent `handshake_replacing` calls for one dead entry share one handshake.
    /// The first evicts the entry and drops the held success; the others join its handshake or
    /// find the entry it inserted, and none drops that handshake's hold, so none handshakes
    /// again. Each call gets the same new entry, and the map holds that entry alone.
    #[tokio::test]
    async fn concurrent_replacements_share_one_handshake() {
        init_tracing();

        let tls = TestTlsProvider {};
        let server = crate::psk::server::Provider::builder()
            .start(
                "[::1]:0".parse().unwrap(),
                tls.clone(),
                NoopSubscriber {},
                test_map(),
            )
            .await
            .unwrap();
        let server_addr = server.local_addr();

        // With the cleaner stopped, an entry a second handshake replaced would stay in the map's
        // list of secrets, and `secrets_len` would count it.
        let map = test_map();
        map.test_stop_cleaner();
        // A success hold far longer than the test, so a call that dropped the new handshake's
        // hold would let the next call handshake again.
        let client = Provider::builder()
            .with_success_jitter(Duration::from_secs(1_000_000))
            .start(
                "[::]:0".parse().unwrap(),
                map.clone(),
                tls,
                NoopSubscriber {},
                server_name(),
            )
            .unwrap();

        let (first, _kind) = client
            .handshake_with_entry(server_addr, server_name())
            .await
            .unwrap();
        let first = first.into_raw();
        let first_id = *first.id();
        assert_eq!(map.secrets_len(), 1);

        let calls = tokio::task::LocalSet::new();
        let ids = calls
            .run_until(async {
                let handles: Vec<_> = (0..1_000)
                    .map(|_| {
                        let client = client.clone();
                        let first = first.clone();
                        tokio::task::spawn_local(async move {
                            let (entry, _kind) = client
                                .handshake_replacing(server_addr, &first, server_name())
                                .await
                                .expect("each call gets the peer's new entry");
                            *entry.into_raw().id()
                        })
                    })
                    .collect();
                let mut ids = Vec::with_capacity(handles.len());
                for handle in handles {
                    ids.push(handle.await.unwrap());
                }
                ids
            })
            .await;

        let second_id = ids[0];
        assert_ne!(second_id, first_id, "the calls replaced the dead entry");
        assert!(
            ids.iter().all(|id| *id == second_id),
            "every call gets the one new entry"
        );
        assert_eq!(
            map.secrets_len(),
            1,
            "one handshake replaced the dead entry, and none ran after it"
        );
    }

    /// The provider registers a handshake request on its map, so a background request for a
    /// peer, such as the one the endpoint makes for a client path it marks dead, handshakes with
    /// the peer and inserts an entry for it.
    #[tokio::test]
    async fn map_request_handshake_handshakes_in_the_background() {
        init_tracing();

        let tls = TestTlsProvider {};
        let server = crate::psk::server::Provider::builder()
            .start(
                "[::1]:0".parse().unwrap(),
                tls.clone(),
                NoopSubscriber {},
                test_map(),
            )
            .await
            .unwrap();
        let server_addr = server.local_addr();

        let client = Provider::builder()
            .start(
                "[::]:0".parse().unwrap(),
                test_map(),
                tls,
                NoopSubscriber {},
                server_name(),
            )
            .unwrap();
        assert!(!client.map().contains(&server_addr));

        let handshake = client
            .map()
            .request_handshake(server_addr, HandshakeReason::Remote)
            .expect("the provider registers a handshake request on its map");
        handshake.await.unwrap();

        assert!(client.map().contains(&server_addr));
    }

    /// Many background requests for one peer share one handshake. The handshake queue runs one
    /// handshake for a peer at a time and holds its success, so a burst of requests, such as one
    /// for each ReplayDetected notice a client accepts, doesn't start a handshake for each
    /// request.
    #[tokio::test]
    async fn repeated_background_requests_share_one_handshake() {
        init_tracing();

        let tls = TestTlsProvider {};
        let server = crate::psk::server::Provider::builder()
            .start(
                "[::1]:0".parse().unwrap(),
                tls.clone(),
                NoopSubscriber {},
                test_map(),
            )
            .await
            .unwrap();
        let server_addr = server.local_addr();

        // With the cleaner stopped, an entry a second handshake replaced would stay in the map's
        // list of secrets, and `secrets_len` would count it.
        let map = test_map();
        map.test_stop_cleaner();

        // A success hold far longer than the test, so a request that comes after the handshake
        // finished gets the held success and doesn't handshake again.
        let client = Provider::builder()
            .with_success_jitter(Duration::from_secs(1_000_000))
            .start(
                "[::]:0".parse().unwrap(),
                map.clone(),
                tls,
                NoopSubscriber {},
                server_name(),
            )
            .unwrap();

        let handshakes: Vec<_> = (0..1_000)
            .map(|_| {
                client
                    .map()
                    .request_handshake(server_addr, HandshakeReason::Remote)
                    .expect("the provider registers a handshake request on its map")
            })
            .collect();
        for handshake in handshakes {
            handshake.await.unwrap();
        }

        assert!(client.map().contains(&server_addr));
        assert_eq!(
            map.secrets_len(),
            1,
            "the requests shared one handshake, and none ran after it"
        );
    }
}
