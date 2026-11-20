// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! stream Endpoint: shared infrastructure for the process.

use crate::{
    acceptor,
    counter::GaugedQueueReceiver,
    endpoint::{
        frame::SubmissionSender,
        id::{
            Id, IdJoin, IdMap, LocalRecvSocketId, LocalSendSocketId, LocalSenderId,
            RecvDispatchWorkerId, RecvIoWorkerId, SendWorkerId,
        },
    },
    intrusive::Entry,
    packet,
    socket::{
        channel::{intrusive::sync as sync_queue, GaugedSender, UnboundedSender},
        pool::{descriptor, SyncReusePool},
    },
    stream::Stream,
    time::precision,
    tracing::*,
};
use core::time::Duration;
use s2n_quic_core::{time, varint::VarInt};
use std::sync::{atomic::AtomicU64, Arc};

pub(crate) mod ack;
pub(crate) mod assemble;
pub(crate) mod combinator;
pub mod counters;
pub(crate) mod dbg;
pub(crate) mod decode;
pub(crate) mod dispatch;
pub mod edt;
pub(crate) mod error;
pub(crate) mod frame;
pub(crate) mod frame_trace;
pub mod id;
pub(crate) mod inflight;
pub(crate) mod msg;
pub(crate) mod range_codec;
pub(crate) mod recv;
pub(crate) mod routing;
pub(crate) mod send;
pub mod socket;
pub(crate) mod tasks;
pub(crate) mod ups;
pub(crate) mod waker;
pub(crate) mod worker;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use error::Error;
pub use s2n_quic_platform::features::Gso;

/// The maximum time a stream will be open without activity from the peer
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Cooldown after a peer is marked dead before new flows are allowed.
pub const DEFAULT_DEAD_PEER_COOLDOWN: Duration = Duration::from_secs(30);
/// The maximum length of a single packet
pub const MAX_DATAGRAM_SIZE: usize = 1 << 15; // 32k

type BatchSender =
    GaugedSender<sync_queue::Sender<combinator::FrameBatch>, Entry<combinator::FrameBatch>>;
type BatchReceiver = sync_queue::Receiver<combinator::FrameBatch>;
type AckMsgReceiver = sync_queue::Receiver<msg::Sender>;

#[cfg(test)]
impl Endpoint {
    /// An endpoint that doesn't run any worker or hold any socket, for tests of
    /// `stream::Client::connect`: `connect` reads only the endpoint's path-secret map, clock,
    /// dead-peer cooldown, metrics, credit pools and frame sender, and a stream it returns sends
    /// nothing until its first write. The returned receiver keeps the frame channel open while
    /// the test holds it.
    pub(crate) fn without_workers(
        path_secret_map: crate::path::secret::Map,
        clock: crate::time::DefaultClock,
        dead_peer_cooldown: Duration,
    ) -> (Self, frame::SubmissionReceiver) {
        let counters = crate::counter::Registry::default();
        let (frame_tx, frame_rx) = frame::submission_channel(1);
        let reader_metrics = Arc::new(crate::stream::metrics::ReaderMetrics::new(
            &counters,
            "stream.reader",
        ));
        let writer_metrics = Arc::new(crate::stream::metrics::WriterMetrics::new(
            &counters,
            "stream.writer",
        ));
        let client_metrics = Arc::new(crate::stream::metrics::ClientMetrics::new(&counters));
        let send_credit_pool = crate::sync::Arc::new(crate::credit::Pool::new(Default::default()));
        let recv_credit_pool = crate::sync::Arc::new(crate::credit::Pool::new(Default::default()));
        let endpoint = Self {
            frame_tx,
            path_secret_map,
            acceptor_registry: acceptor::Registry::new(),
            counters,
            next_binding_id: AtomicU64::new(0),
            data_addrs: Vec::new(),
            dead_peer_cooldown,
            clock,
            reader_metrics,
            writer_metrics,
            client_metrics,
            send_credit_pool,
            recv_credit_pool,
            recv_rings: Vec::new(),
        };
        (endpoint, frame_rx)
    }
}

pub struct Endpoint {
    /// Frame submission channel (writers submit frame inputs here)
    pub frame_tx: SubmissionSender,
    /// Path secret map (shared with PSK providers)
    pub path_secret_map: crate::path::secret::Map,
    /// Queue allocator for flow queues
    /// Acceptor registry for server-side stream dispatch
    pub acceptor_registry: acceptor::Registry<Stream>,
    /// Counters associated with this endpoint
    pub counters: crate::counter::Registry,
    /// Endpoint-wide stream ID counter
    pub next_binding_id: AtomicU64,
    /// Recv socket addresses advertised to peers during handshake.
    /// Each recv worker has its own distinct address.
    pub data_addrs: Vec<std::net::SocketAddr>,
    /// Cooldown period during which new flows are rejected after a peer is marked dead.
    pub dead_peer_cooldown: Duration,
    /// Clock created once at endpoint construction — bach-aware in test contexts.
    pub clock: crate::time::DefaultClock,
    /// Per-outcome sojourn metrics for stream read halves (shared across all server streams).
    pub reader_metrics: Arc<crate::stream::metrics::ReaderMetrics>,
    /// Per-outcome sojourn metrics for stream write halves (shared across all server streams).
    pub writer_metrics: Arc<crate::stream::metrics::WriterMetrics>,
    /// Metrics for client-side connect operations (queue-pair allocation timing/blocked counts).
    pub client_metrics: Arc<crate::stream::metrics::ClientMetrics>,
    /// Endpoint-wide credit pool gating outbound stream admission. All Writers acquire
    /// from this pool before submitting frames so cross-stream priority and global
    /// admission limits can be enforced. The matching `Distributor` runs on the
    /// frame-dispatch worker.
    ///
    /// Wrapped in `crate::sync::Arc` (rather than `std::sync::Arc`) so the type matches
    /// the `Pool`'s internal `Arc` flavor under `--features loom`.
    pub send_credit_pool: crate::sync::Arc<crate::credit::Pool>,
    /// Endpoint-wide credit pool reserved for the Reader side. Wired here to keep the
    /// `Distributor` task colocated with the rest of the endpoint plumbing; the Reader
    /// integration is a follow-up.
    pub recv_credit_pool: crate::sync::Arc<crate::credit::Pool>,
    /// io_uring recv-ring lifetime guards (empty unless the io_uring recv backend adopted sockets).
    /// Each guard signals its ring thread to stop and joins it on drop (the ring thread owns its
    /// socket, so the fd closes there after the join), so they must live as long as the endpoint.
    /// Held purely for its `Drop`; never read.
    #[allow(dead_code)]
    recv_rings: Vec<RecvRingHandle>,
}

// ── Pipeline Setup ────────────────────────────────────────────────────────

/// Per-poll budgets for each pipeline sub-task.
///
/// Each budget controls how many items a task processes per executor poll before yielding.
/// Lower values improve fairness across tasks; higher values improve throughput under load.
#[derive(Clone, Copy, Debug)]
pub struct Budgets {
    /// Budget for the submission router (shards drained per poll).
    pub submission_router: usize,
    /// Budget for the frame-dispatch batcher+distributor task.
    pub frame_dispatch: usize,
    /// Budget for the send-worker context resolver task.
    pub context_resolver: usize,
    /// Budget for the send-worker ACK processor task.
    pub ack_processor: usize,
    /// Budget for the send-worker TX wheel drain task.
    pub tx_wheel: usize,
    /// Budget for the send-worker PTO wheel drain task.
    pub pto_wheel: usize,
    /// Budget for the send-worker idle wheel drain task.
    pub idle_wheel: usize,
    /// Budget for per-socket assembler+send tasks.
    pub assembler: usize,
    /// Budget for the completion dispatcher task (acked frames).
    pub completion_acked: usize,
    /// Budget for the completion dispatcher task (cancelled frames).
    pub completion_cancelled: usize,
    /// Budget for the per-socket recv task.
    pub socket_recv: usize,
    /// Budget for the per-worker packet dispatch task.
    pub packet_dispatch: usize,
    /// Budget for the per-worker ACK burst drain task (`ack_burst_task`), i.e.
    /// how many pending recv contexts are encoded/sent per poll.
    pub ack_burst: usize,
    /// Budget for the waker drain task (wakers fired per poll).
    pub waker_drain: usize,
    /// Budget for the ACK completion drain task (entries returned from assembler per poll).
    pub ack_completion: usize,
    /// Budget for the per-worker invalidation drain task.
    pub invalidation: usize,
    /// Per-poll budget for each credit-pool [`Distributor`](crate::credit::Distributor) task.
    /// Bounds work-per-pass; a budget that is too small surfaces as
    /// `!credit.<dir>.distributor.budget_exhausted` increments. Two distributors run
    /// (one per direction); they each get this budget independently.
    pub credit_distributor: usize,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            submission_router: 32,
            frame_dispatch: 128,
            context_resolver: 128,
            ack_processor: 256,
            tx_wheel: tasks::DEFAULT_DISPATCH_BUDGET,
            pto_wheel: tasks::DEFAULT_DISPATCH_BUDGET,
            idle_wheel: 1,
            assembler: tasks::DEFAULT_DISPATCH_BUDGET,
            completion_acked: 128,
            completion_cancelled: tasks::DEFAULT_DISPATCH_BUDGET,
            socket_recv: tasks::DEFAULT_RECV_BUDGET,
            packet_dispatch: 4096,
            ack_burst: 256,
            waker_drain: 512,
            ack_completion: tasks::DEFAULT_DISPATCH_BUDGET,
            invalidation: 1,
            credit_distributor: 256,
        }
    }
}

/// Assigns spawner thread indices to pipeline roles.
///
/// Each field is a list of worker IDs (indices into the spawner's thread pool). The spawner
/// must have at least `max(all IDs) + 1` threads. Overlapping IDs are allowed (e.g. recv_io
/// and recv_dispatch on the same worker) but typically kept separate for isolation.
#[derive(Debug)]
pub struct WorkerLayout {
    /// Which worker runs the frame dispatch task (single).
    pub frame_dispatch: usize,
    /// Workers that run send (context resolver + assembler + socket send).
    /// Send sockets are distributed round-robin across these workers.
    pub send: Vec<usize>,
    /// Workers that run recv IO (socket read + decode + fan-out to dispatch).
    /// Recv sockets are distributed round-robin across these workers.
    pub recv_io: Vec<usize>,
    /// Workers that run recv dispatch (decrypt + dedup + frame routing to queues).
    /// Packets are hash-routed to these workers by (credentials.id, source_sender_id).
    pub recv_dispatch: Vec<usize>,
    /// Workers that run waker drain tasks (fire wakers offloaded from dispatch/send workers).
    /// Multiple workers are supported for sharding if the single-thread budget is exceeded.
    pub waker_drain: Vec<usize>,
    /// Worker that runs background housekeeping tasks (e.g. invalidation validation).
    pub background: usize,
}

/// Selects how recv sockets are driven to read inbound datagrams.
///
/// The choice only affects *how* packets are pulled off the wire — every backend produces the same
/// [`descriptor::Filled`] segments and feeds the same `FanOutRouter`, so nothing downstream changes.
///
/// [`descriptor::Filled`]: crate::socket::pool::descriptor::Filled
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecvBackend {
    /// Use io_uring when it is available at runtime, otherwise fall back to the syscall backend.
    ///
    /// Availability is probed once at endpoint startup; if the kernel lacks io_uring, the
    /// `kernel.io_uring_disabled` sysctl forbids it, or the locked-memory limit is too low, the
    /// endpoint logs the reason and uses [`Syscall`](Self::Syscall). On non-Linux targets io_uring
    /// does not exist, so `Auto` is always `Syscall`.
    #[default]
    Auto,
    /// Prefer the io_uring recv backend. A dedicated ring thread submits a backlog of multishot
    /// receives and reaps completions, rather than cooperatively busy-polling the socket.
    ///
    /// If io_uring is unavailable at runtime the endpoint logs the reason and falls back to
    /// [`Syscall`](Self::Syscall) — `IoUring` is a preference, not a hard requirement, so a
    /// misconfigured host degrades instead of failing to start.
    IoUring,
    /// Force the cooperative syscall recv path (the historical behavior): a per-socket task busy-polls
    /// `recvmsg` and fans out the resulting segments.
    Syscall,
}

/// Resolve [`RecvBackend`] to a concrete decision: `true` = drive recv sockets with io_uring rings,
/// `false` = use the cooperative syscall recv path. `Auto`/`IoUring` probe io_uring once and fall
/// back (logging why) when it is unavailable; on non-Linux targets io_uring does not exist so the
/// answer is always `false`.
fn resolve_recv_backend(backend: RecvBackend) -> bool {
    match backend {
        RecvBackend::Syscall => false,
        #[cfg(target_os = "linux")]
        RecvBackend::Auto | RecvBackend::IoUring => match crate::socket::recv::uring::probe() {
            Ok(()) => {
                debug!("io_uring recv backend enabled");
                true
            }
            Err(err) => {
                warn!(%err, "io_uring recv backend unavailable; falling back to syscall recv path");
                false
            }
        },
        #[cfg(not(target_os = "linux"))]
        RecvBackend::Auto | RecvBackend::IoUring => false,
    }
}

/// A running recv-ring lifetime guard held by the [`Endpoint`]. On Linux it is the real
/// `recv::uring::RecvRing` (joins its thread on drop); off Linux io_uring does not exist, so it is an
/// uninhabited placeholder that is never constructed.
#[cfg(target_os = "linux")]
type RecvRingHandle = crate::socket::recv::uring::RecvRing;
#[cfg(not(target_os = "linux"))]
type RecvRingHandle = core::convert::Infallible;

/// Try to adopt `socket` with a dedicated io_uring recv ring driving `router`. Returns `Ok(())` when a
/// ring was spawned (the ring thread now owns both `socket` and `router`); returns
/// `Err((socket, router))` — handing both back — when the socket has no real fd or the ring could not
/// be created, so the caller falls back to the syscall recv path. Linux-only; the non-Linux build
/// never calls this (`use_uring` is always false there). The ring records the socket's `socket.rx.*`
/// counters in `counter_registry` under `recv.{idx}`, as the syscall recv path does.
#[cfg(target_os = "linux")]
fn try_spawn_recv_ring<S, Router>(
    idx: usize,
    socket: S,
    recv_pool: crate::socket::pool::Pool,
    router: Router,
    rings: &mut Vec<RecvRingHandle>,
    counter_registry: &crate::counter::Registry,
) -> Result<(), (S, Router)>
where
    S: crate::socket::recv::BindOnWorker + Send + 'static,
    Router: crate::socket::recv::router::Router + Send + 'static,
{
    use crate::socket::recv::uring;
    let reuse = crate::socket::pool::SyncReuseRing::new();
    match uring::spawn(
        idx,
        socket,
        uring::DEFAULT_RING_DEPTH,
        recv_pool,
        reuse,
        router,
        uring::Counters::register(counter_registry, idx),
    ) {
        Ok(ring) => {
            rings.push(ring);
            Ok(())
        }
        Err(uring::SpawnError::Recoverable(err, socket, router)) => {
            // Setup failed before socket/router were committed — fall back to the syscall path.
            debug!(%err, idx, "io_uring recv ring not used for this socket; using syscall recv path");
            Err((socket, router))
        }
        Err(uring::SpawnError::Fatal(err)) => {
            // All fallible io_uring setup (ring + buffer-ring registration) now happens before the
            // thread is spawned and surfaces as `Recoverable`, so the only way to reach `Fatal` is the
            // OS refusing to create the ring thread itself (resource exhaustion). The socket/router
            // were already moved into the dropped closure and cannot be recovered, so this socket
            // could never receive. Rather than silently bring up an endpoint with a dead recv path,
            // fail fast — a thread-spawn failure at construction is a hard environment problem the
            // operator must see.
            panic!("io_uring recv ring thread spawn failed for recv socket {idx}: {err}");
        }
    }
}

/// Non-Linux stub: io_uring does not exist, so a recv ring can never be spawned. `use_uring` is always
/// `false` off Linux, so this is never actually called — it exists only so the (platform-independent)
/// recv-socket distribution loop type-checks. Always hands socket + router back for the syscall path.
#[cfg(not(target_os = "linux"))]
fn try_spawn_recv_ring<S, Router>(
    _idx: usize,
    socket: S,
    _recv_pool: crate::socket::pool::Pool,
    router: Router,
    _rings: &mut Vec<RecvRingHandle>,
    _counter_registry: &crate::counter::Registry,
) -> Result<(), (S, Router)>
where
    S: crate::socket::recv::BindOnWorker + Send + 'static,
    Router: crate::socket::recv::router::Router + Send + 'static,
{
    Err((socket, router))
}

/// Configuration for the stream pipeline.
pub struct Config {
    /// Counter registry the endpoint records all of its metrics against.
    ///
    /// [`Registry`](crate::counter::Registry) is a handle: cloning it yields another handle onto the
    /// same underlying storage. To share one registry across the endpoint, the [`fs`](crate::fs) IO
    /// scheduler, and an application's own instrumentation, keep a clone and pass another clone here
    /// — a single reporter draining any of the clones then samples everything. Pass
    /// [`crate::counter::Registry::default()`] for a private registry when no sharing is needed.
    pub counters: crate::counter::Registry,
    /// Worker layout — maps pipeline roles to spawner thread indices.
    pub layout: WorkerLayout,
    /// Which backend drives the recv sockets (io_uring vs. cooperative syscall busy-poll).
    pub recv_backend: RecvBackend,
    /// Buffer pool for outbound (send) packets.
    pub send_pool: crate::socket::pool::Pool,
    /// Buffer pool for inbound (recv) packets.
    pub recv_pool: crate::socket::pool::Pool,
    /// Path-secret map shared with PSK providers.
    pub path_secret_map: crate::path::secret::Map,
    /// GSO capability probed for the local host.
    pub gso: s2n_quic_platform::features::Gso,
    /// Server-side acceptor registry.
    pub acceptor_registry: acceptor::Registry<Stream>,
    /// Overall bandwidth cap applied by the frame-dispatch pacing stage.
    ///
    /// The [`Paced`] combinator in the dispatch pipeline enforces this rate across all
    /// send sockets combined. Set to a very high value (e.g. `Rate::new(100.0)` for
    /// 100 Gbps) to effectively disable pacing when the network is not a bottleneck.
    ///
    /// [`Paced`]: crate::socket::channel::Paced
    pub overall_send_rate: crate::socket::rate::Rate,
    /// Per-poll budgets for each pipeline task.
    pub budgets: Budgets,
    /// Number of shards for the frame submission channel.
    pub submission_shards: usize,
    /// Rate limit for UnknownPathSecret responses (pacing rate).
    pub ups_rate: crate::socket::rate::Rate,
    /// Dedup LRU capacity for UnknownPathSecret responses.
    pub ups_dedup_capacity: usize,
    /// Dedup suppression window for UnknownPathSecret responses.
    pub ups_dedup_window: core::time::Duration,
    /// Cooldown period during which new flows are rejected after a peer is marked dead.
    pub dead_peer_cooldown: core::time::Duration,
    /// Number of descriptors to pre-allocate into each send descriptor reuse pool at startup.
    ///
    /// Pre-warming avoids hitting the system allocator on the first `N` sends. Each send-side
    /// [`UnsyncReusePool`] (one per socket) is seeded with this many descriptors.
    ///
    /// [`UnsyncReusePool`]: crate::socket::pool::UnsyncReusePool
    pub initial_tx_descriptor_allocs: usize,
    /// Number of descriptors to pre-allocate into each recv descriptor reuse pool at startup.
    ///
    /// Pre-warming avoids hitting the system allocator on the first `N` receives. Each recv-side
    /// [`SyncReuseLocal`] (one per recv-IO worker) is seeded with this many descriptors.
    ///
    /// [`SyncReuseLocal`]: crate::socket::pool::SyncReuseLocal
    pub initial_rx_descriptor_allocs: usize,
    /// Configuration for the endpoint-wide outbound credit pool. Capacity and per-priority
    /// caps are exposed here so deployments can experiment with admission limits.
    pub send_credit_pool_config: crate::credit::Config,
    /// Configuration for the endpoint-wide inbound credit pool. Reserved for the Reader-side
    /// integration; the pool is constructed and run today but not yet acquired against.
    pub recv_credit_pool_config: crate::credit::Config,
}

// ── setup_endpoint ────────────────────────────────────────────────────────

/// Assembles the stream pipeline from pre-opened sockets and spawns worker tasks.
///
/// This is the top-level composition function. It creates all inter-task channels, builds a
/// [`Worker`] for each spawner thread, and calls [`Worker::spawn`]. No pipeline logic lives
/// here — every stage is implemented in the task functions in [`tasks`].
///
/// # Worker layout
///
/// The [`WorkerLayout`] in [`Config`] assigns pipeline roles to spawner thread indices:
///
/// * **frame_dispatch** (single): routes submitted frames to send workers via PickTwo.
/// * **send** workers: context resolution, assembly, and socket transmission.
/// * **recv_io** workers: socket reads, segment decoding, and hash-based fan-out.
/// * **recv_dispatch** workers: decryption, deduplication, and frame dispatch to queues.
///
/// Recv IO tasks fan out decoded packets to recv_dispatch workers by hashing
/// (credentials.id, source_sender_id), ensuring a given peer always lands in the same
/// recv::Cache for coherent ACK space and packet-number deduplication.
pub fn setup_endpoint<SendSocket, RecvSocket, UpsSocket, R>(
    runtime: R,
    config: Config,
    send_sockets: Vec<SendSocket>,
    recv_sockets: Vec<RecvSocket>,
    ups_socket: UpsSocket,
) -> Endpoint
where
    SendSocket: crate::socket::send::Socket + Send + 'static,
    RecvSocket: crate::socket::recv::BindOnWorker + Send + 'static,
    UpsSocket: crate::socket::send::Socket + Send + 'static,
    R: crate::runtime::Runtime,
{
    let num_recv_dispatch = config.layout.recv_dispatch.len();
    let counter_registry = config.counters.clone();

    let send_sockets: IdMap<_, _> = LocalSendSocketId::range(send_sockets.len())
        .zip(send_sockets)
        .map(|(key, socket)| {
            let local_addr = socket
                .local_addr()
                .expect("send socket must have a local address");
            let socket = crate::socket::cached_addr::CachedAddr::new(socket, local_addr);
            let socket = crate::socket::tracing::Tracing::new(socket, key);
            let socket = socket::Metered::new(
                socket,
                counter_registry.register_nominal("socket.tx.ops", format_args!("send.{key}")),
                counter_registry.register_nominal("socket.tx.bytes", format_args!("send.{key}")),
                counter_registry.register_nominal("!socket.tx.errors", format_args!("send.{key}")),
            );
            (key, socket)
        })
        .collect();

    // Recv sockets stay UNWRAPPED and unbound here (see `crate::socket::recv::BindOnWorker`). They
    // are carried to the worker that will poll them and only there — on the worker, so tokio's
    // readiness `AsyncFd` registers with the worker's own reactor rather than the setup runtime's —
    // are they bound and wrapped (CachedAddr/Tracing/Metered) at the recv-task spawn site in
    // `Worker::spawn`. Runtimes that need no readiness registration (busy-poll, bach) bind as identity.
    let recv_sockets: IdMap<_, _> = id::LocalRecvSocketId::range(recv_sockets.len())
        .zip(recv_sockets)
        .collect();

    debug!(
        ?config.layout,
        ?send_sockets,
        "setting up endpoint"
    );

    if num_recv_dispatch.is_power_of_two() {
        setup_endpoint_inner::<_, _, _, _, routing::PowerOfTwoRoute>(
            runtime,
            config,
            counter_registry,
            send_sockets,
            recv_sockets,
            ups_socket,
        )
    } else {
        setup_endpoint_inner::<_, _, _, _, routing::ModuloRoute>(
            runtime,
            config,
            counter_registry,
            send_sockets,
            recv_sockets,
            ups_socket,
        )
    }
}

fn setup_endpoint_inner<SendSocket, RecvSocket, UpsSocket, R, RecvRoute>(
    runtime: R,
    config: Config,
    counter_registry: crate::counter::Registry,
    send_sockets: IdMap<LocalSendSocketId, SendSocket>,
    recv_sockets: IdMap<id::LocalRecvSocketId, RecvSocket>,
    ups_socket: UpsSocket,
) -> Endpoint
where
    SendSocket: crate::socket::send::Socket + Send + 'static,
    RecvSocket: crate::socket::recv::BindOnWorker + Send + 'static,
    UpsSocket: crate::socket::send::Socket + Send + 'static,
    R: crate::runtime::Runtime,
    RecvRoute: routing::SenderRoute,
{
    use crate::socket::channel::intrusive;

    let Config {
        // The registry is already extracted and threaded through as `counter_registry`; drop the
        // duplicate handle in the config here so the destructuring stays exhaustive.
        counters: _,
        layout,
        recv_backend,
        send_pool,
        recv_pool,
        path_secret_map,
        gso,
        acceptor_registry,
        overall_send_rate,
        budgets,
        submission_shards,
        ups_rate,
        ups_dedup_capacity,
        ups_dedup_window,
        dead_peer_cooldown,
        initial_tx_descriptor_allocs,
        initial_rx_descriptor_allocs,
        send_credit_pool_config,
        recv_credit_pool_config,
    } = config;

    let clock = runtime.clock();

    // Resolve the effective recv backend once at startup. `IoUring`/`Auto` only take effect on Linux
    // when the runtime io_uring probe succeeds; otherwise we log why and use the syscall path. The
    // resolved value gates, per recv socket, whether we spawn a dedicated ring thread (for sockets
    // that expose a real fd) or push the socket to a cooperative recv_io worker.
    let use_uring = resolve_recv_backend(recv_backend);

    let num_workers = runtime.worker_count().max(1);
    let num_send = send_sockets.len();

    assert!(
        num_send.is_power_of_two(),
        "number of sender sockets must be a power of two"
    );
    assert!(
        submission_shards.is_power_of_two(),
        "submission shard count must be a power of two"
    );
    assert!(
        !layout.send.is_empty(),
        "at least one send worker is required"
    );
    assert!(
        !layout.recv_io.is_empty(),
        "at least one recv_io worker is required"
    );
    assert!(
        !layout.recv_dispatch.is_empty(),
        "at least one recv_dispatch worker is required"
    );

    // Collect all recv socket addresses — advertised to peers so senders can target
    // individual recv workers directly.
    let data_addrs: Vec<std::net::SocketAddr> = recv_sockets
        .iter()
        .map(|(_, s)| {
            s.local_addr()
                .expect("recv socket must have a local address")
        })
        .collect();
    let source_control_port = data_addrs.first().map_or(0, |a| a.port());

    // Frame submission channel: all writers share one sharded sender; one dispatch task drains it.
    let (frame_tx, frame_rx) = frame::submission_channel(submission_shards);

    // Shared flow-queue allocator and dispatch counters -------------------------
    let (freed_batch_tx, freed_batch_rx) = crate::queue::freed_batch_channel();
    let counters = counters::Dispatch::new(&counter_registry);

    // Endpoint-wide credit pools (one per direction) ----------------------------
    // The send pool gates outbound stream admission via the Writer; the recv pool
    // is constructed today for symmetry but is not yet acquired against (Reader
    // integration is a follow-up).
    //
    // Bind the refill pacer (the liveness floor) to the endpoint's configured send rate. The pacer
    // guarantees the pool keeps delivering credit at that rate even when no release arrives,
    // breaking the concurrent-stream-overcommit wedge. The config default enables refill (with a
    // placeholder rate); here we substitute the real rate. A caller that explicitly *disabled*
    // refill (`refill: None`, via `Config::without_refill`) is preserved — `.map` only rewrites the
    // rate of an already-enabled refill, it never re-enables a disabled one.
    let refill_rate = overall_send_rate;
    let send_credit_pool_config = crate::credit::Config {
        refill: send_credit_pool_config
            .refill
            .map(|_| crate::credit::Refill { rate: refill_rate }),
        ..send_credit_pool_config
    };
    let recv_credit_pool_config = crate::credit::Config {
        refill: recv_credit_pool_config
            .refill
            .map(|_| crate::credit::Refill { rate: refill_rate }),
        ..recv_credit_pool_config
    };
    let send_credit_pool = crate::sync::Arc::new(crate::credit::Pool::with_counters(
        send_credit_pool_config,
        crate::credit::Counters::new_with_prefix(&counter_registry, "credit.send"),
    ));
    let recv_credit_pool = crate::sync::Arc::new(crate::credit::Pool::with_counters(
        recv_credit_pool_config,
        crate::credit::Counters::new_with_prefix(&counter_registry, "credit.recv"),
    ));
    // Pull-based pool-state gauges (available / outstanding / returned_pending / in_flight). The
    // reporter samples these closures once per interval; the leak signal is `in_flight` flooring
    // positive while the pool is idle. See `credit::Pool::register_gauges`.
    send_credit_pool.register_gauges(&counter_registry, "credit.send");
    recv_credit_pool.register_gauges(&counter_registry, "credit.recv");
    let send_credit_distributor = crate::credit::Distributor::new(send_credit_pool.clone());
    let recv_credit_distributor = crate::credit::Distributor::new(recv_credit_pool.clone());

    // Per-send-worker batch channels -----------------------------------------------
    let num_send_workers = layout.send.len();
    let (worker_batch_txs, worker_batch_rxs): (
        IdMap<id::SendWorkerId, _>,
        IdMap<id::SendWorkerId, _>,
    ) = SendWorkerId::range(num_send_workers)
        .map(|id| {
            let (tx, rx) = intrusive::sync::new::<combinator::FrameBatch>();
            let gauge = counter_registry
                .register_queue_gauge_nominal("q.resolver", format_args!("send.{id}"));
            let tx = GaugedSender::new(tx, gauge);
            ((id, tx), (id, rx))
        })
        .unzip();
    let (worker_ack_txs, worker_ack_rxs): (IdMap<id::SendWorkerId, _>, IdMap<id::SendWorkerId, _>) =
        SendWorkerId::range(num_send_workers)
            .map(|id| {
                let (tx, rx) = intrusive::sync::new::<msg::Sender>();
                let gauge = counter_registry
                    .register_queue_gauge_nominal("q.ack", format_args!("send.{id}"));
                let tx = GaugedSender::new(tx, gauge);
                ((id, tx), (id, rx))
            })
            .unzip();

    // UPS channel: recv_dispatch workers → background (UnknownPathSecret responses)
    // It carries the ReplayDetected and StaleKey responses too, and `ups::Pending` keeps at most
    // one response in it for each credential and peer sender id, with a shard of keys for each
    // recv_dispatch worker.
    let (ups_tx, ups_rx) = intrusive::sync::new::<ups::Response>();
    let ups_queue_gauge = counter_registry.register_queue_gauge("q.ups");
    let ups_pending = ups::Pending::new(
        layout.recv_dispatch.len(),
        counter_registry.register("ups.pending_suppressed"),
    );
    let ups_tx = ups_pending.sender(GaugedSender::new(ups_tx, ups_queue_gauge.clone()));
    let ups_rx = ups_pending.receiver(ups_rx);

    // Invalidation channels: recv IO → background (raw segments) and background → workers
    let (invalidation_raw_tx, invalidation_raw_rx) = intrusive::sync::new::<descriptor::Filled>();
    let (invalidation_send_txs, invalidation_send_rxs): (
        IdMap<id::SendWorkerId, _>,
        IdMap<id::SendWorkerId, _>,
    ) = SendWorkerId::range(num_send_workers)
        .map(|id| {
            let (tx, rx) = intrusive::sync::new::<tasks::Invalidation>();
            let gauge = counter_registry
                .register_queue_gauge_nominal("q.invalidation", format_args!("send.{id}"));
            let tx = GaugedSender::new(tx, gauge);
            ((id, tx), (id, rx))
        })
        .unzip();
    let (invalidation_recv_txs, invalidation_recv_rxs): (
        IdMap<id::RecvDispatchWorkerId, _>,
        IdMap<id::RecvDispatchWorkerId, _>,
    ) = RecvDispatchWorkerId::range(layout.recv_dispatch.len())
        .map(|id| {
            let (tx, rx) = intrusive::sync::new::<tasks::Invalidation>();
            let gauge = counter_registry
                .register_queue_gauge_nominal("q.invalidation", format_args!("recv.{id}"));
            let tx = GaugedSender::new(tx, gauge);
            ((id, tx), (id, rx))
        })
        .unzip();
    let (peer_dead_tx, peer_dead_rx) = intrusive::sync::new::<tasks::PeerDead>();
    let peer_dead_tx = GaugedSender::new(
        peer_dead_tx,
        counter_registry.register_queue_gauge("q.peer_dead"),
    );

    let mut sender_id_to_worker: IdMap<LocalSenderId, id::SendWorkerId> = IdMap::default();

    // Set the socket sender count on the map so path-secret entries allocate
    // per-socket transmission schedules for pick-two load balancing.
    path_secret_map.set_socket_sender_count(num_send);

    // Build workers -------------------------------------------------------------
    let mut workers: Vec<Worker<SendSocket, RecvSocket, UpsSocket, R::Clock, _, RecvRoute, _>> = {
        let mut v = Vec::with_capacity(num_workers);
        v.extend((0..num_workers).map(|id| {
            Worker::new(
                id,
                budgets,
                num_send,
                clock.clone(),
                dead_peer_cooldown,
                counter_registry.clone(),
                initial_rx_descriptor_allocs,
            )
        }));
        v
    };

    // Distribute send sockets across send workers round-robin.
    for (socket_id, socket) in send_sockets.into_iter() {
        let raw_idx = socket_id.as_usize();
        let sender_idx = LocalSenderId::new(VarInt::new(raw_idx as u64).unwrap());
        let worker_id = layout.send[raw_idx % num_send_workers];
        sender_id_to_worker.extend(core::iter::once((
            sender_idx,
            id::SendWorkerId::new(raw_idx % num_send_workers),
        )));
        workers[worker_id].send_sockets.push(SendSocketParts {
            socket,
            sender_idx,
            source_control_port,
            gso: gso.clone(),
            pool: send_pool.clone(),
            clock: clock.clone(),
            initial_tx_descriptor_allocs,
        });
    }

    // Build per-socket senders: each socket ID maps to its owning worker's channel.
    let socket_senders: IdMap<LocalSenderId, BatchSender> = sender_id_to_worker
        .iter()
        .map(|(sender_id, &worker_idx)| (sender_id, worker_batch_txs[worker_idx].clone()))
        .collect();

    // ── Waker offload ─────────────────────────────────────────────────────────
    // One slot per producer (recv_dispatch + send workers + background peer-dead fanout task
    // + the two credit-pool distributors), partitioned across waker_drain workers.
    let num_recv_dispatch = layout.recv_dispatch.len();
    let num_waker_slots = num_recv_dispatch + num_send_workers + 1 + 2;
    let num_waker_drains = layout.waker_drain.len().max(1);
    let (mut waker_sinks, waker_drains) = waker::new(num_waker_slots, num_waker_drains);
    // Layout: [recv_dispatch.. | send_workers.. | bg | credit_send | credit_recv]
    let recv_credit_distributor_waker_sink = waker_sinks
        .pop()
        .expect("recv credit waker sink must exist");
    let send_credit_distributor_waker_sink = waker_sinks
        .pop()
        .expect("send credit waker sink must exist");
    let mut send_and_bg_waker_sinks = waker_sinks.split_off(num_recv_dispatch);
    let bg_waker_sink = send_and_bg_waker_sinks
        .pop()
        .expect("background waker sink must exist");
    let send_waker_sinks = send_and_bg_waker_sinks;

    assert_eq!(send_waker_sinks.len(), num_send_workers);
    let send_waker_sinks: IdMap<SendWorkerId, _> = SendWorkerId::range(send_waker_sinks.len())
        .zip(send_waker_sinks)
        .collect();

    assert_eq!(waker_sinks.len(), num_recv_dispatch);
    let waker_sinks: IdMap<RecvDispatchWorkerId, _> =
        RecvDispatchWorkerId::range(num_recv_dispatch)
            .zip(waker_sinks)
            .collect();

    // Frame-dispatch task on its designated worker. Bundles the credit-pool
    // distributors so they spawn alongside frame_dispatch on the same thread.
    workers[layout.frame_dispatch].frame_dispatch = Some(FrameDispatchParts {
        frame_rx,
        socket_senders,
        clock: clock.clone(),
        overall_send_rate,
        send_credit_distributor,
        recv_credit_distributor,
        send_credit_waker_sink: send_credit_distributor_waker_sink,
        recv_credit_waker_sink: recv_credit_distributor_waker_sink,
        send_credit_pool: send_credit_pool.clone(),
    });

    for (idx, drain) in waker_drains.into_iter().enumerate() {
        let worker_id = layout.waker_drain[idx % layout.waker_drain.len()];
        let prev = workers[worker_id].waker_drain.replace(drain);
        assert!(
            prev.is_none(),
            "worker {worker_id} assigned multiple waker drain tasks"
        );
    }

    // ACK completion channels: one per recv dispatch worker. Send workers route completed
    // ACK entries back to the recv worker that submitted them.
    let (ack_completion_txs, ack_completion_rxs): (
        IdMap<RecvDispatchWorkerId, _>,
        IdMap<RecvDispatchWorkerId, _>,
    ) = RecvDispatchWorkerId::range(num_recv_dispatch)
        .map(|id| {
            let (tx, rx) = crate::socket::channel::intrusive::sync::new::<msg::Sender>();
            let gauge = counter_registry
                .register_queue_gauge_nominal("q.dispatch", format_args!("recv.dispatch.{id}"));
            let tx = GaugedSender::new(tx, gauge);
            ((id, tx), (id, rx))
        })
        .unzip();
    let ack_completions_tx = routing::AckCompletionSender::new(ack_completion_txs);

    // Assign per-send-worker batch/ack receivers.
    let stream_clock = crate::time::DefaultClock::default();
    let reader_metrics = Arc::new(crate::stream::metrics::ReaderMetrics::new(
        &counter_registry,
        "stream.reader",
    ));
    let writer_metrics = Arc::new(crate::stream::metrics::WriterMetrics::new(
        &counter_registry,
        "stream.writer",
    ));
    let client_metrics = Arc::new(crate::stream::metrics::ClientMetrics::new(
        &counter_registry,
    ));
    for (send_worker_id, batch_rx, ack_rx, invalidation_rx, waker_sink) in (
        worker_batch_rxs,
        worker_ack_rxs,
        invalidation_send_rxs,
        send_waker_sinks,
    )
        .join()
    {
        let worker_id = layout.send[send_worker_id.as_usize()];
        workers[worker_id].send_worker = Some(SendWorkerParts {
            idx: send_worker_id,
            batch_rx,
            batch_gauge: counter_registry
                .register_queue_gauge_nominal("q.resolver", format_args!("send.{send_worker_id}")),
            ack_rx,
            ack_gauge: counter_registry
                .register_queue_gauge_nominal("q.ack", format_args!("send.{send_worker_id}")),
            random: crate::xorshift::Rng::new(),
            frame_tx: frame_tx.clone(),
            ack_completions_tx: ack_completions_tx.clone(),
            freed_batch_tx: freed_batch_tx.clone(),
            waker_sink,
            invalidation_rx,
            peer_dead_tx: peer_dead_tx.clone(),
            stream_clock: stream_clock.clone(),
            reader_metrics: reader_metrics.clone(),
            writer_metrics: writer_metrics.clone(),
            send_credit_pool: send_credit_pool.clone(),
        });
    }

    // Build ACK sender after socket distribution so sender_id_to_worker is populated.
    let ack_sender = routing::AckSender::new(worker_ack_txs, &sender_id_to_worker);

    // ── Recv dispatch queues ─────────────────────────────────────────────────
    // One dispatch queue per recv_dispatch worker. Recv IO tasks fan out to all of these
    // using a hash of (credentials.id, source_sender_id) for peer affinity.
    let (dispatch_txs, dispatch_rxs): (
        IdMap<RecvDispatchWorkerId, PacketSender>,
        IdMap<RecvDispatchWorkerId, PacketReceiver>,
    ) = RecvDispatchWorkerId::range(num_recv_dispatch)
        .map(|id| {
            let (tx, rx) =
                intrusive::sync::new::<packet::datagram::decoder::Packet<descriptor::Filled>>();
            let gauge = counter_registry
                .register_queue_gauge_nominal("q.dispatch_rx", format_args!("recv.{id}"));
            ((id, GaugedSender::new(tx, gauge)), (id, rx))
        })
        .unzip();

    let ack_route = RecvRoute::new(num_send);
    for (recv_dispatch_id, dispatch_rx, ack_completion_rx, invalidation_rx, waker_sink) in (
        dispatch_rxs,
        ack_completion_rxs,
        invalidation_recv_rxs,
        waker_sinks,
    )
        .join()
    {
        let worker_id = layout.recv_dispatch[recv_dispatch_id.as_usize()];
        workers[worker_id].recv_dispatch = Some(RecvDispatchParts {
            packet_rx: dispatch_rx,
            packet_gauge: counter_registry.register_queue_gauge_nominal(
                "q.dispatch_rx",
                format_args!("recv.{recv_dispatch_id}"),
            ),
            path_secret_map: path_secret_map.clone(),
            acceptor_registry: acceptor_registry.clone(),
            frame_tx: frame_tx.clone(),
            ack_sender: ack_sender.clone(),
            ack_completion_rx,
            recv_dispatch_idx: recv_dispatch_id,
            freed_batch_tx: freed_batch_tx.clone(),
            counters: counters.clone(),
            clock: clock.clone(),
            stream_clock: stream_clock.clone(),
            route: ack_route,
            waker_sink,
            ups_tx: ups_tx.clone(),
            invalidation_rx,
            reader_metrics: reader_metrics.clone(),
            writer_metrics: writer_metrics.clone(),
            send_credit_pool: send_credit_pool.clone(),
            recv_credit_pool: recv_credit_pool.clone(),
        });
    }

    // Create one sync reuse pool per recv_io worker.
    let num_recv_io_workers = layout.recv_io.len();
    let recycle_pools: IdMap<RecvIoWorkerId, _> = RecvIoWorkerId::range(num_recv_io_workers)
        .map(|id| (id, SyncReusePool::new()))
        .collect();

    // io_uring recv-ring guards. A ring thread owns the socket it drives (keeping the fd open) and is
    // joined when its guard drops, so the guards must live as long as the endpoint. Empty unless the
    // uring backend is active and a socket exposes a real fd.
    let mut recv_rings: Vec<RecvRingHandle> = Vec::new();

    // Distribute recv sockets: io_uring-adopted sockets get a ring thread (which owns the socket); the
    // rest go to recv_io workers round-robin (cooperative syscall busy-poll). Round-robin only counts
    // the syscall-assigned sockets so workers stay balanced when some sockets are adopted by rings.
    let mut syscall_recv_idx = 0usize;
    for (recv_socket_id, socket) in recv_sockets.into_iter() {
        let router = worker::FanOutRouter::<_, RecvRoute, _>::new(
            dispatch_txs.clone(),
            invalidation_raw_tx.clone(),
            &counter_registry,
        );

        // Try io_uring first when enabled. On success the ring thread owns both socket and router; on
        // failure (uring disabled, no fd, or setup error) both are handed back so the syscall path can
        // use them — the router is built exactly once either way.
        let (socket, router) = if use_uring {
            match try_spawn_recv_ring(
                recv_socket_id.as_usize(),
                socket,
                recv_pool.clone(),
                router,
                &mut recv_rings,
                &counter_registry,
            ) {
                Ok(()) => continue,
                Err((socket, router)) => (socket, router),
            }
        } else {
            (socket, router)
        };

        // Syscall path: a recv_io worker busy-polls this socket.
        let recv_io_idx = RecvIoWorkerId::new(syscall_recv_idx % num_recv_io_workers);
        let worker_id = layout.recv_io[recv_io_idx.as_usize()];
        syscall_recv_idx += 1;
        workers[worker_id].recv_sockets.push(RecvSocketParts {
            idx: recv_socket_id,
            socket,
            recv_pool: recv_pool.clone(),
            router,
        });
    }

    // Assign sync reuse pools to their workers — but only to workers that actually own recv sockets.
    // The reuse pool exists solely to feed `socket_recv` tasks (and its `recycle_drain` task drains
    // descriptors back into it), so a recv_io worker with no sockets has no use for one. Skipping the
    // assignment means such a worker spawns *nothing* (no `socket_recv`, no `recycle_drain`), so the
    // executor can spin its thread down. This is the common case under the io_uring recv backend,
    // where rings own every socket on their own threads and the recv_io workers are left empty.
    for (recv_io_idx, recycle_pool) in recycle_pools.into_iter() {
        let worker_id = layout.recv_io[recv_io_idx.as_usize()];
        if !workers[worker_id].recv_sockets.is_empty() {
            workers[worker_id].recycle_pool = Some(recycle_pool);
        }
    }

    // Background worker — invalidation validation + future housekeeping.
    workers[layout.background].background = Some(BackgroundParts {
        raw_rx: invalidation_raw_rx,
        peer_dead_rx,
        peer_dead_queue_gauge: counter_registry.register_queue_gauge("q.peer_dead"),
        freed_batch_rx,
        ack_sender: ack_sender.clone(),
        path_secret_map: path_secret_map.clone(),
        send_txs: invalidation_send_txs,
        recv_txs: invalidation_recv_txs,
        sender_id_to_worker,
        ups_rx,
        ups_queue_gauge: counter_registry.register_queue_gauge("q.ups"),
        ups_socket,
        ups_rate,
        ups_dedup_capacity,
        ups_dedup_window,
        acceptor_cleaner: acceptor_registry.cleaner(),
        waker_sink: bg_waker_sink,
    });

    // Spawn all workers ---------------------------------------------------------
    for worker in workers {
        worker.spawn(&runtime);
    }

    Endpoint {
        frame_tx,
        path_secret_map,
        acceptor_registry,
        counters: counter_registry,
        next_binding_id: AtomicU64::new(0),
        data_addrs,
        dead_peer_cooldown,
        clock: stream_clock,
        reader_metrics,
        writer_metrics,
        client_metrics,
        send_credit_pool,
        recv_credit_pool,
        recv_rings,
    }
}

// ── Worker parts ──────────────────────────────────────────────────────────

/// All the ingredients needed to spawn the frame-dispatch task on a worker.
struct FrameDispatchParts<Clk> {
    frame_rx: frame::SubmissionReceiver,
    /// Per-socket-id senders: indexed by socket ID, each routes to the owning worker.
    socket_senders: IdMap<LocalSenderId, BatchSender>,
    /// Clock used by the pacing stage.
    clock: Clk,
    /// Overall bandwidth cap for the pacing stage.
    overall_send_rate: crate::socket::rate::Rate,
    /// Send-direction credit-pool distributor. Spawned alongside the frame-dispatch
    /// task on the same worker so distribution and submission share a thread.
    send_credit_distributor: crate::credit::Distributor,
    /// Send credit pool handle, passed to `frame_dispatch` so the pick-two router can
    /// return credit when it has to drop an undeliverable batch (worker teardown).
    send_credit_pool: crate::sync::Arc<crate::credit::Pool>,
    /// Recv-direction credit-pool distributor. Reserved for the Reader integration;
    /// spawned today so the close path is wired and `recv_credit_pool.release` works.
    recv_credit_distributor: crate::credit::Distributor,
    /// Waker sinks used by the two distributors.
    send_credit_waker_sink: waker::Sink,
    recv_credit_waker_sink: waker::Sink,
}

/// Per-worker state for context resolution and ACK processing.
struct SendWorkerParts {
    idx: SendWorkerId,
    batch_rx: BatchReceiver,
    batch_gauge: crate::counter::QueueGauge,
    ack_rx: AckMsgReceiver,
    ack_gauge: crate::counter::QueueGauge,
    random: crate::xorshift::Rng,
    frame_tx: SubmissionSender,
    ack_completions_tx: routing::AckCompletionSender<
        GaugedSender<sync_queue::Sender<msg::Sender>, crate::intrusive::Queue<msg::Sender>>,
    >,
    freed_batch_tx: crate::queue::FreedBatchTx,
    waker_sink: waker::Sink,
    invalidation_rx: sync_queue::Receiver<tasks::Invalidation>,
    peer_dead_tx: PeerDeadSender,
    stream_clock: crate::time::DefaultClock,
    reader_metrics: Arc<crate::stream::metrics::ReaderMetrics>,
    writer_metrics: Arc<crate::stream::metrics::WriterMetrics>,
    send_credit_pool: crate::sync::Arc<crate::credit::Pool>,
}

/// Per-socket ingredients for the socket send task.
pub(crate) struct SendSocketParts<Socket, Clk> {
    socket: Socket,
    sender_idx: LocalSenderId,
    source_control_port: u16,
    gso: s2n_quic_platform::features::Gso,
    pool: crate::socket::pool::Pool,
    clock: Clk,
    initial_tx_descriptor_allocs: usize,
}

type PacketSender = GaugedSender<
    sync_queue::Sender<packet::datagram::decoder::Packet<descriptor::Filled>>,
    Entry<packet::datagram::decoder::Packet<descriptor::Filled>>,
>;
type PacketReceiver = sync_queue::Receiver<packet::datagram::decoder::Packet<descriptor::Filled>>;

/// Ingredients for a recv IO worker (socket read + decode + fan-out).
struct RecvSocketParts<Socket, Route, Inv> {
    idx: LocalRecvSocketId,
    socket: Socket,
    recv_pool: crate::socket::pool::Pool,
    router: worker::FanOutRouter<PacketSender, Route, Inv>,
}

/// Ingredients for a recv dispatch worker (decrypt + dedup + frame dispatch).
struct RecvDispatchParts<Clk, AckSnd, Route> {
    packet_rx: PacketReceiver,
    packet_gauge: crate::counter::QueueGauge,
    path_secret_map: crate::path::secret::Map,
    acceptor_registry: acceptor::Registry<Stream>,
    frame_tx: SubmissionSender,
    ack_sender: AckSnd,
    ack_completion_rx: sync_queue::Receiver<msg::Sender>,
    /// Index into the AckCompletionSender's staging array (0..num_recv_dispatch).
    recv_dispatch_idx: RecvDispatchWorkerId,
    freed_batch_tx: crate::queue::FreedBatchTx,
    counters: Arc<counters::Dispatch>,
    clock: Clk,
    /// Concrete clock used for stream construction (checked once at endpoint startup).
    stream_clock: crate::time::DefaultClock,
    route: Route,
    waker_sink: waker::Sink,
    ups_tx: UpsSender,
    invalidation_rx: sync_queue::Receiver<tasks::Invalidation>,
    reader_metrics: Arc<crate::stream::metrics::ReaderMetrics>,
    writer_metrics: Arc<crate::stream::metrics::WriterMetrics>,
    /// Endpoint-wide send credit pool, plumbed through to `Writer::new_server` so
    /// server-side stream Writers can register their `Slot` against it.
    send_credit_pool: crate::sync::Arc<crate::credit::Pool>,
    /// Endpoint-wide recv credit pool. Dispatch releases bytes here on each new
    /// payload arrival, and Readers register their `Slot` against it for
    /// window-extension acquires.
    recv_credit_pool: crate::sync::Arc<crate::credit::Pool>,
}

/// Ingredients for the background worker (invalidation validation + future housekeeping).
struct BackgroundParts<UpsSocket> {
    raw_rx: sync_queue::Receiver<descriptor::Filled>,
    peer_dead_rx: sync_queue::Receiver<tasks::PeerDead>,
    peer_dead_queue_gauge: crate::counter::QueueGauge,
    freed_batch_rx: crate::queue::FreedBatchRx,
    ack_sender: routing::AckSender<
        GaugedSender<sync_queue::Sender<msg::Sender>, crate::intrusive::Entry<msg::Sender>>,
    >,
    path_secret_map: crate::path::secret::Map,
    send_txs: IdMap<id::SendWorkerId, InvalidationSender>,
    recv_txs: IdMap<id::RecvDispatchWorkerId, InvalidationSender>,
    sender_id_to_worker: IdMap<LocalSenderId, id::SendWorkerId>,
    ups_rx: ups::PendingReceiver<sync_queue::Receiver<ups::Response>>,
    ups_queue_gauge: crate::counter::QueueGauge,
    ups_socket: UpsSocket,
    ups_rate: crate::socket::rate::Rate,
    ups_dedup_capacity: usize,
    ups_dedup_window: core::time::Duration,
    acceptor_cleaner: acceptor::Cleaner<Stream>,
    waker_sink: waker::Sink,
}

type InvalidationSender =
    GaugedSender<sync_queue::Sender<tasks::Invalidation>, Entry<tasks::Invalidation>>;
type PeerDeadSender = GaugedSender<sync_queue::Sender<tasks::PeerDead>, Entry<tasks::PeerDead>>;

type UpsSender =
    ups::PendingSender<GaugedSender<sync_queue::Sender<ups::Response>, Entry<ups::Response>>>;

// ── Worker ────────────────────────────────────────────────────────────────

struct Worker<SendSocket, RecvSocket, UpsSocket, Clk, AckSnd, Route, Inv> {
    id: usize,
    budgets: Budgets,
    total_sender_ids: usize,
    dead_peer_cooldown: core::time::Duration,
    clock: Clk,
    counter_registry: crate::counter::Registry,
    frame_dispatch: Option<FrameDispatchParts<Clk>>,
    /// Per-worker batch/ack receiver (one per send worker).
    send_worker: Option<SendWorkerParts>,
    /// Send sockets assigned to this worker.
    send_sockets: Vec<SendSocketParts<SendSocket, Clk>>,
    /// Recv IO: socket read + decode + fan-out (one or more per worker).
    recv_sockets: Vec<RecvSocketParts<RecvSocket, Route, Inv>>,
    /// Recv dispatch: decrypt + dedup + frame routing (at most one per worker).
    recv_dispatch: Option<RecvDispatchParts<Clk, AckSnd, Route>>,
    /// Recv-side descriptor reuse state, localized on the worker thread.
    recycle_pool: Option<SyncReusePool>,
    /// Waker drain task assigned to this worker.
    waker_drain: Option<waker::Drain>,
    /// Background worker parts (invalidation validation).
    background: Option<BackgroundParts<UpsSocket>>,
    /// Number of descriptors to pre-allocate into each recv reuse pool on startup.
    initial_rx_descriptor_allocs: usize,
}

impl<SendSocket, RecvSocket, UpsSocket, Clk, AckSnd, Route, Inv>
    Worker<SendSocket, RecvSocket, UpsSocket, Clk, AckSnd, Route, Inv>
where
    SendSocket: crate::socket::send::Socket + Send + 'static,
    RecvSocket: crate::socket::recv::BindOnWorker + Send + 'static,
    UpsSocket: crate::socket::send::Socket + Send + 'static,
    Clk: time::Clock + precision::Clock + Clone + Send + 'static,
    AckSnd: UnboundedSender<Entry<msg::Sender>> + Clone + Send + 'static,
    Route: routing::SenderRoute,
    Inv: UnboundedSender<Entry<descriptor::Filled>> + Send + 'static,
{
    #[inline]
    fn new(
        id: usize,
        budgets: Budgets,
        total_sender_ids: usize,
        clock: Clk,
        dead_peer_cooldown: core::time::Duration,
        counter_registry: crate::counter::Registry,
        initial_rx_descriptor_allocs: usize,
    ) -> Self {
        Self {
            id,
            budgets,
            total_sender_ids,
            dead_peer_cooldown,
            clock,
            counter_registry,
            frame_dispatch: None,
            send_worker: None,
            send_sockets: Vec::new(),
            recv_sockets: Vec::new(),
            recv_dispatch: None,
            recycle_pool: None,
            waker_drain: None,
            background: None,
            initial_rx_descriptor_allocs,
        }
    }

    #[inline]
    fn spawn<R: crate::runtime::Runtime>(self, runtime: &R) {
        use crate::{runtime::Spawner as _, socket::channel::ReceiverExt as _};

        let Self {
            id,
            budgets,
            total_sender_ids,
            dead_peer_cooldown,
            clock,
            counter_registry,
            frame_dispatch,
            send_worker,
            send_sockets,
            recv_sockets,
            recv_dispatch,
            recycle_pool,
            waker_drain,
            background,
            initial_rx_descriptor_allocs,
        } = self;

        runtime.spawn_local(id, move |mut local| {
            if let Some(fd) = frame_dispatch {
                // Clone the clock before `frame_dispatch` consumes `fd.clock` by value — the two
                // credit distributors spawned below each need their own handle for the refill pacer.
                let credit_clock = fd.clock.clone();
                tasks::frame_dispatch(
                    &mut local,
                    fd.frame_rx,
                    fd.socket_senders,
                    crate::xorshift::Rng::new(),
                    fd.clock,
                    fd.overall_send_rate,
                    budgets,
                    counter_registry.clone(),
                    fd.send_credit_pool,
                );
                tasks::spawn_credit_distributor(
                    &mut local,
                    fd.send_credit_distributor,
                    budgets.credit_distributor,
                    fd.send_credit_waker_sink,
                    &counter_registry,
                    "send",
                    credit_clock.clone(),
                );
                tasks::spawn_credit_distributor(
                    &mut local,
                    fd.recv_credit_distributor,
                    budgets.credit_distributor,
                    fd.recv_credit_waker_sink,
                    &counter_registry,
                    "recv",
                    credit_clock,
                );
            }

            if let Some(sw) = send_worker {
                let batch_rx = GaugedQueueReceiver::new(
                    sw.batch_rx,
                    sw.batch_gauge
                        .receiver("task.context_resolver")
                        .with_function("endpoint::Worker::spawn"),
                );
                let ack_rx = GaugedQueueReceiver::new(
                    sw.ack_rx,
                    sw.ack_gauge
                        .receiver("task.ack_processor")
                        .with_function("endpoint::Worker::spawn"),
                );
                tasks::send_worker(
                    &mut local,
                    sw.idx,
                    batch_rx,
                    ack_rx,
                    sw.invalidation_rx,
                    total_sender_ids,
                    send_sockets.into(),
                    clock.clone(),
                    sw.random,
                    sw.frame_tx,
                    sw.ack_completions_tx,
                    sw.freed_batch_tx,
                    sw.waker_sink,
                    sw.peer_dead_tx,
                    dead_peer_cooldown,
                    budgets,
                    counter_registry.clone(),
                    sw.stream_clock,
                    sw.reader_metrics,
                    sw.writer_metrics,
                    sw.send_credit_pool,
                );
            }

            let mut recycle_pool = recycle_pool.map(SyncReusePool::into_local);

            // Prime the recv-side reuse pool before handing out any handles.
            if let (Some(recycle_pool), Some(first_socket)) =
                (recycle_pool.as_mut(), recv_sockets.first())
            {
                recycle_pool.prime(&first_socket.recv_pool, initial_rx_descriptor_allocs);
            }

            // Spawn the recycle drain task if this worker has one.
            debug_assert!(
                recv_sockets.is_empty() || recycle_pool.is_some(),
                "recv_io workers with sockets must have a recycle pool"
            );
            if let Some(recycle_drain) = recycle_pool.as_mut().and_then(|pool| pool.take_drain()) {
                let rx = tasks::recycle_drain(recycle_drain);
                let task_counter = counter_registry
                    .register_task("task.recycle_drain")
                    .with_registration_metadata(
                        "task.recycle_drain",
                        "Drains recycled descriptors from dispatch workers into local pool",
                        "endpoint::Worker::spawn",
                    );
                local.spawn_receiver_task(
                    rx.drain_budgeted_metered(None, task_counter.clone()),
                    None,
                    task_counter,
                );
            }

            for rs in recv_sockets {
                let recv_idx = rs.idx;
                let variant = format!("recv.{recv_idx}");
                let recycle = recycle_pool
                    .as_ref()
                    .expect("recv socket workers should have a recycle pool")
                    .handle();
                // Bind the recv socket to THIS worker's runtime (tokio registers its readiness
                // `AsyncFd` with the worker's own reactor here; other runtimes bind as identity), then
                // apply the socket instrumentation stack. Both happen on the worker so the readiness
                // registration is same-runtime — see `crate::socket::recv::BindOnWorker`.
                let socket = rs
                    .socket
                    .bind_on_worker()
                    .expect("bind recv socket to worker runtime");
                let local_addr = crate::socket::LocalAddr::local_addr(&socket)
                    .expect("recv socket must have a local address");
                let socket = crate::socket::cached_addr::CachedAddr::new(socket, local_addr);
                let socket = crate::socket::tracing::Tracing::new(socket, recv_idx);
                let socket = socket::Metered::new(
                    socket,
                    counter_registry
                        .register_nominal("socket.rx.ops", format_args!("recv.{recv_idx}")),
                    counter_registry
                        .register_nominal("socket.rx.bytes", format_args!("recv.{recv_idx}")),
                    counter_registry
                        .register_nominal("!socket.rx.errors", format_args!("recv.{recv_idx}")),
                );
                let rx = tasks::socket_recv(socket, rs.recv_pool, recycle, rs.router);
                let task_counter = counter_registry
                    .register_nominal_task("task.socket_recv", &variant)
                    .with_registration_metadata(
                        "task.socket_recv",
                        "Reads UDP datagrams and routes packets into dispatch queues",
                        "endpoint::Worker::spawn",
                    );
                local.spawn_receiver_task(
                    rx.drain_budgeted_metered(Some(budgets.socket_recv), task_counter.clone()),
                    Some(budgets.socket_recv),
                    task_counter,
                );
            }

            if let Some(rd) = recv_dispatch {
                let packet_rx = GaugedQueueReceiver::new(
                    rd.packet_rx,
                    rd.packet_gauge
                        .receiver("task.packet_dispatch")
                        .with_function("endpoint::Worker::spawn"),
                );
                let recv_dispatch_idx = rd.recv_dispatch_idx;
                let recv_cache = std::rc::Rc::new(std::cell::RefCell::new(
                    crate::stream::endpoint::recv::Cache::new(recv_dispatch_idx),
                ));
                let (ack_burst_tx, ack_burst_rx) =
                    crate::socket::channel::intrusive::unsync::new_with_adapter::<
                        crate::stream::endpoint::recv::AckBurstAdapter,
                    >();

                // Recv idle wheel — expires inactive recv contexts.
                let variant = format!("recv.dispatch.{recv_dispatch_idx}");
                let q_recv_idle_wheel =
                    counter_registry.register_queue_gauge_nominal("q.idle_wheel", &variant);
                let (recv_idle_wheel_tx, recv_idle_wheel_rx) =
                    crate::socket::channel::intrusive::unsync::new_with_adapter::<
                        crate::stream::endpoint::recv::IdleWheelAdapter,
                    >();
                {
                    let recv_cache = recv_cache.clone();
                    let clock = rd.clock.clone();
                    let idle_expired = counter_registry.register("idle.recv.expired");
                    let idle_rescheduled = counter_registry.register("idle.recv.rescheduled");
                    let idle_lifetime =
                        counter_registry.register_nominal_timer("idle.recv.lifetime", &variant);
                    let task_counter = counter_registry
                        .register_nominal_task("task.recv_idle_wheel", &variant)
                        .with_registration_metadata(
                            "task.recv_idle_wheel",
                            "Expires or re-schedules idle recv contexts",
                            "endpoint::Worker::spawn",
                        );
                    local.spawn_receiver_task(
                        tasks::recv_idle_wheel_drain(
                            recv_idle_wheel_rx,
                            recv_idle_wheel_tx.clone(),
                            clock,
                            q_recv_idle_wheel,
                            recv_cache,
                            idle_expired,
                            idle_rescheduled,
                            idle_lifetime,
                            budgets.idle_wheel,
                            task_counter.clone(),
                        ),
                        Some(budgets.idle_wheel),
                        task_counter,
                    );
                }

                let rx = tasks::packet_dispatch(
                    packet_rx,
                    recv_cache.clone(),
                    ack_burst_tx,
                    recv_idle_wheel_tx,
                    rd.path_secret_map,
                    rd.acceptor_registry,
                    rd.frame_tx,
                    rd.ack_sender.clone(),
                    rd.freed_batch_tx,
                    rd.counters.clone(),
                    rd.clock,
                    rd.route,
                    rd.waker_sink,
                    rd.ups_tx,
                    rd.stream_clock,
                    rd.reader_metrics,
                    rd.writer_metrics,
                    rd.send_credit_pool,
                    rd.recv_credit_pool,
                );
                let variant = format!("recv.dispatch.{recv_dispatch_idx}");
                let task_counter = counter_registry
                    .register_nominal_task("task.packet_dispatch", &variant)
                    .with_registration_metadata(
                        "task.packet_dispatch",
                        "Decrypts, validates, and routes inbound packets",
                        "endpoint::Worker::spawn",
                    );
                local.spawn_receiver_task(
                    rx.drain_budgeted_metered(Some(budgets.packet_dispatch), task_counter.clone()),
                    Some(budgets.packet_dispatch),
                    task_counter,
                );
                let rx = tasks::ack_burst(
                    crate::socket::channel::FlattenList::new(ack_burst_rx.into_list_receiver()),
                    rd.ack_sender.clone(),
                    recv_dispatch_idx,
                    rd.counters.clone(),
                );
                let task_counter = counter_registry
                    .register_nominal_task("task.ack_burst", &variant)
                    .with_registration_metadata(
                        "task.ack_burst",
                        "Encodes and submits ACK bursts from recv contexts",
                        "endpoint::Worker::spawn",
                    );
                local.spawn_receiver_task(
                    rx.drain_budgeted_metered(Some(budgets.ack_burst), task_counter.clone()),
                    Some(budgets.ack_burst),
                    task_counter,
                );
                let ack_completion_gauge =
                    counter_registry.register_queue_gauge_nominal("q.dispatch", &variant);
                let ack_completion_rx = crate::counter::GaugedReceiver::new(
                    rd.ack_completion_rx,
                    ack_completion_gauge
                        .receiver("task.ack_completion")
                        .with_function("endpoint::Worker::spawn"),
                );
                let rx = tasks::ack_completion(
                    ack_completion_rx,
                    recv_cache.clone(),
                    rd.ack_sender,
                    rd.counters.clone(),
                );
                let task_counter = counter_registry
                    .register_nominal_task("task.ack_completion", &variant)
                    .with_registration_metadata(
                        "task.ack_completion",
                        "Finalizes ACK send completions and retries stale acknowledgements",
                        "endpoint::Worker::spawn",
                    );
                local.spawn_receiver_task(
                    rx.drain_budgeted_metered(Some(budgets.ack_completion), task_counter.clone()),
                    Some(budgets.ack_completion),
                    task_counter,
                );

                let rx = tasks::recv_invalidation(rd.invalidation_rx, recv_cache);
                let task_counter = counter_registry
                    .register_nominal_task("task.invalidation", &variant)
                    .with_registration_metadata(
                        "task.invalidation",
                        "Purges invalidated recv contexts",
                        "endpoint::Worker::spawn",
                    );
                local.spawn_receiver_task(
                    rx.drain_budgeted_metered(Some(budgets.invalidation), task_counter.clone()),
                    Some(budgets.invalidation),
                    task_counter,
                );
            }

            if let Some(drain) = waker_drain {
                let variant = format!("waker.{id}");
                let rx = tasks::waker_drain(drain);
                let task_counter = counter_registry
                    .register_nominal_task("task.waker_drain", &variant)
                    .with_registration_metadata(
                        "task.waker_drain",
                        "Invokes deferred wakers enqueued by dispatch workers",
                        "endpoint::Worker::spawn",
                    );
                local.spawn_receiver_task(
                    rx.drain_budgeted_metered(Some(budgets.waker_drain), task_counter.clone()),
                    Some(budgets.waker_drain),
                    task_counter,
                );
            }

            if let Some(bg) = background {
                let invalidation_counters = tasks::ValidatorInvalidationCounters {
                    unknown_path_secret_validated: counter_registry
                        .register("invalidation.validator.ups.validated"),
                    stale_key_validated: counter_registry
                        .register("invalidation.validator.stale_key.validated"),
                    replay_detected_validated: counter_registry
                        .register("invalidation.validator.replay_detected.validated"),
                };
                let rx = tasks::invalidation_validator(
                    bg.raw_rx,
                    bg.path_secret_map.clone(),
                    bg.send_txs,
                    bg.recv_txs,
                    bg.sender_id_to_worker,
                    invalidation_counters,
                );
                let task_counter = counter_registry
                    .register_nominal_task("task.invalidation_validator", "background")
                    .with_registration_metadata(
                        "task.invalidation_validator",
                        "Validates invalidation datagrams and fan-outs revocation events",
                        "endpoint::Worker::spawn",
                    );
                local.spawn_receiver_task(
                    rx.drain_budgeted_metered(None, task_counter.clone()),
                    None,
                    task_counter,
                );

                let peer_dead_counters = tasks::PeerDeadCounters {
                    events: counter_registry.register("peer_dead.events"),
                    broadcasted: counter_registry.register("peer_dead.broadcasted"),
                };
                let peer_dead_rx = crate::counter::GaugedReceiver::new(
                    bg.peer_dead_rx,
                    bg.peer_dead_queue_gauge
                        .receiver("task.peer_dead_broadcast")
                        .with_function("endpoint::Worker::spawn"),
                );
                // A client path the idle wheel marks dead asks the handshake client registered
                // on the map, where one is, for a new handshake with the peer in the background.
                let path_secret_map = bg.path_secret_map;
                let request_rehandshake = move |peer| {
                    let reason = crate::psk::io::HandshakeReason::Remote;
                    let _ = path_secret_map.request_handshake(peer, reason);
                };
                let rx = tasks::peer_dead_broadcast(
                    peer_dead_rx,
                    bg.waker_sink,
                    peer_dead_counters,
                    request_rehandshake,
                );
                let task_counter = counter_registry
                    .register_nominal_task("task.peer_dead_broadcast", "background")
                    .with_registration_metadata(
                        "task.peer_dead_broadcast",
                        "Marks peers dead and performs credential-wide reset fanout",
                        "endpoint::Worker::spawn",
                    );
                local.spawn_receiver_task(
                    rx.drain_budgeted_metered(None, task_counter.clone()),
                    None,
                    task_counter,
                );

                let ups_counters = ups::Counters::new(&counter_registry);
                let ups_rx = crate::counter::GaugedReceiver::new(
                    bg.ups_rx,
                    bg.ups_queue_gauge
                        .receiver("task.ups_send")
                        .with_function("endpoint::Worker::spawn"),
                );
                let rx = tasks::ups_send(
                    ups_rx,
                    bg.ups_socket,
                    clock.clone(),
                    bg.ups_rate,
                    bg.ups_dedup_capacity,
                    bg.ups_dedup_window,
                    ups_counters,
                );
                let task_counter =
                    counter_registry.register_nominal_task("task.ups_send", "background");
                local.spawn(rx.drain_budgeted_metered(None, task_counter));

                // Freed-batch LB routing: pick socket via sender_load_score, forward
                // to the target send worker's ack channel.
                {
                    use crate::socket::channel::{ReceiverExt as _, UnboundedSender as _};
                    let mut ack_sender = bg.ack_sender;
                    let socket_count = total_sender_ids;
                    let mut rng = crate::xorshift::Rng::new();
                    let rx = crate::socket::channel::Map::new(
                        bg.freed_batch_rx,
                        move |mut entry: crate::intrusive::Entry<msg::Sender>| {
                            // Pick-two LB using sender_load_score atomics
                            let chosen = if socket_count <= 1 {
                                LocalSenderId::from_index(0)
                            } else {
                                let path_entry = entry.path_secret_entry().clone();
                                let idx1 = LocalSenderId::from_index(rng.next_usize(socket_count));
                                let idx2 = {
                                    let mut raw = rng.next_usize(socket_count - 1);
                                    if raw >= idx1.as_usize() {
                                        raw += 1;
                                    }
                                    LocalSenderId::from_index(raw)
                                };
                                let s1 = path_entry.sender_load_score(idx1);
                                let s2 = path_entry.sender_load_score(idx2);
                                if s1 <= s2 {
                                    idx1
                                } else {
                                    idx2
                                }
                            };

                            // Set the chosen sender and forward
                            if let msg::Sender::PendingFreed {
                                ref mut local_sender_id,
                                ..
                            } = *entry
                            {
                                *local_sender_id = chosen;
                            }
                            let _ = ack_sender.send(entry);
                        },
                    );
                    let task_counter = counter_registry
                        .register_nominal_task("task.freed_batch_router", "background")
                        .with_registration_metadata(
                            "task.freed_batch_router",
                            "Routes freed-batch tokens to send workers with LB",
                            "endpoint::Worker::spawn",
                        );
                    local.spawn_receiver_task(
                        rx.drain_budgeted_metered(None, task_counter.clone()),
                        None,
                        task_counter,
                    );
                }

                local.spawn(bg.acceptor_cleaner);
            }
        });
    }
}
